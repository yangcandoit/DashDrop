use anyhow::{bail, Context, Result};
use quinn::Connection;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::AppHandle;

use crate::state::AppState;
use crate::transport::protocol::{
    read_message, write_message, DashMessage, ErrorCode, HelloPayload, RejectPayload,
    SUPPORTED_VERSIONS, WIRE_VERSION,
};

async fn reject_control_stream(
    send: &mut quinn::SendStream,
    reason: ErrorCode,
    context: &str,
) -> Result<()> {
    if let Err(e) = write_message(send, &DashMessage::Reject(RejectPayload { reason })).await {
        tracing::warn!("failed to send reject ({context}): {e:#}");
    }
    Ok(())
}

/// Called for each accepted incoming QUIC connection.
/// Performs: cert fp binding → Hello exchange → route to receiver.
pub async fn handle_incoming(conn: Connection, app: AppHandle, state: Arc<AppState>) -> Result<()> {
    // 1. Extract peer TLS certificate fingerprint
    let peer_cert_fp = extract_peer_fp(&conn)?;

    // 2. Auxiliary identity signal: compare against mDNS-by-IP and emit warning if mismatched.
    // Security decision is enforced by initiator-side fingerprint binding.
    let mismatch = {
        let devices = state.devices.read().await;
        let mut found: Option<(String, String)> = None;
        for device in devices.values() {
            for session in device.sessions.values() {
                if session
                    .addrs
                    .iter()
                    .any(|a| a.ip() == conn.remote_address().ip())
                {
                    // Found a matching mDNS device by IP
                    if device.fingerprint != peer_cert_fp {
                        found = Some((device.fingerprint.clone(), session.session_id.clone()));
                    }
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        found
    };

    if let Some((mdns_fp, session_id)) = mismatch {
        let remote_addr = conn.remote_address().to_string();
        tracing::warn!(
            transfer_id = "",
            peer_fp = %mdns_fp,
            phase = "incoming",
            cert_fp = %peer_cert_fp,
            "identity mismatch: mDNS fp != cert fp"
        );
        let _ = tauri::Emitter::emit(
            &app,
            "identity_mismatch",
            serde_json::json!({
                "remote_addr": remote_addr,
                "mdns_fp": mdns_fp.clone(),
                "cert_fp": peer_cert_fp.clone(),
                "phase": "incoming",
            }),
        );
        if let Ok(db) = state.db.lock() {
            let _ = crate::db::log_security_event(
                &db,
                "identity_mismatch",
                "incoming",
                Some(&mdns_fp),
                "mDNS identity does not match peer certificate fingerprint",
            );
        }

        let previous_trusted = state.is_trusted(&mdns_fp).await;
        let current_trusted = state.is_trusted(&peer_cert_fp).await;
        if previous_trusted && !current_trusted {
            let now_unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let dedupe_key = format!("{session_id}|{mdns_fp}|{peer_cert_fp}");
            let should_emit = {
                let mut alerts = state.fingerprint_change_alerts.lock().await;
                let last = alerts.get(&dedupe_key).copied().unwrap_or(0);
                if now_unix.saturating_sub(last) >= 60 {
                    alerts.insert(dedupe_key, now_unix);
                    true
                } else {
                    false
                }
            };
            if should_emit {
                let _ = tauri::Emitter::emit(
                    &app,
                    "fingerprint_changed",
                    serde_json::json!({
                        "session_id": session_id.clone(),
                        "previous_fp": mdns_fp.clone(),
                        "current_fp": peer_cert_fp.clone(),
                        "remote_addr": remote_addr,
                        "phase": "incoming",
                    }),
                );
                if let Ok(db) = state.db.lock() {
                    let _ = crate::db::log_security_event(
                        &db,
                        "fingerprint_changed",
                        "incoming",
                        Some(&mdns_fp),
                        &format!(
                            "trusted fingerprint changed on session {}: previous={} current={}",
                            session_id, mdns_fp, peer_cert_fp
                        ),
                    );
                }
            }
        }
    }

    // 3. Hello exchange: receive peer Hello, respond with our Hello
    let (mut send, mut recv) = conn.accept_bi().await.context("accept bi stream")?;

    // Read peer's Hello first
    let peer_hello = match read_message(&mut recv).await {
        Ok(DashMessage::Hello(h)) => h,
        Ok(other) => {
            reject_control_stream(
                &mut send,
                ErrorCode::Protocol(format!("expected Hello, got {:?}", other)),
                "incoming_hello_type",
            )
            .await?;
            return Ok(());
        }
        Err(e) => {
            reject_control_stream(
                &mut send,
                ErrorCode::Protocol(format!("read Hello failed: {e:#}")),
                "incoming_hello_read",
            )
            .await?;
            return Ok(());
        }
    };

    if peer_hello.wire_version != 0 {
        reject_control_stream(
            &mut send,
            ErrorCode::VersionMismatch,
            "incoming_wire_version",
        )
        .await?;
        return Ok(());
    }

    // Send our Hello back
    let our_hello = DashMessage::Hello(HelloPayload {
        wire_version: WIRE_VERSION,
        supported_versions: SUPPORTED_VERSIONS.to_vec(),
    });
    write_message(&mut send, &our_hello).await?;

    // Negotiate version
    let chosen = negotiate_version(&peer_hello.supported_versions);
    tracing::debug!("Negotiated protocol version: {:?}", chosen);

    let chosen_version = match chosen {
        Some(v) => v,
        None => {
            // No version intersection — reject
            let reject = DashMessage::Reject(RejectPayload {
                reason: ErrorCode::VersionMismatch,
            });
            write_message(&mut send, &reject).await?;
            bail!("no common protocol version");
        }
    };

    // 4. Wait for Offer on this stream
    let offer = match read_message(&mut recv).await {
        Ok(DashMessage::Offer(o)) => o,
        Ok(DashMessage::Cancel(_)) => {
            reject_control_stream(
                &mut send,
                ErrorCode::Cancelled,
                "incoming_cancel_before_offer",
            )
            .await?;
            return Ok(());
        }
        Ok(other) => {
            reject_control_stream(
                &mut send,
                ErrorCode::Protocol(format!("expected Offer, got {:?}", other)),
                "incoming_offer_type",
            )
            .await?;
            return Ok(());
        }
        Err(e) => {
            reject_control_stream(
                &mut send,
                ErrorCode::Protocol(format!("read Offer failed: {e:#}")),
                "incoming_offer_read",
            )
            .await?;
            return Ok(());
        }
    };

    if let Some(reason) = check_offer_rate_limit(&state, &peer_cert_fp).await {
        let _ = reject_control_stream(&mut send, reason, "incoming_offer_rate_limit").await;
        return Ok(());
    }

    tracing::info!(
        transfer_id = %offer.transfer_id,
        peer_fp = %peer_cert_fp,
        phase = "incoming_offer",
        sender_name = %offer.sender_name,
        item_count = offer.items.len(),
        total_size = offer.total_size,
        "incoming offer received"
    );

    // 5. Hand off to receiver state machine
    super::receiver::handle_offer(
        offer,
        conn,
        send,
        recv,
        peer_cert_fp,
        chosen_version,
        app,
        state,
    )
    .await
}

async fn check_offer_rate_limit(state: &Arc<AppState>, peer_fp: &str) -> Option<ErrorCode> {
    let limit = if state.is_trusted(peer_fp).await {
        10
    } else {
        3
    };
    let mut limiter = state.offer_rate_limits.lock().await;
    let (count, window_start) = limiter
        .entry(peer_fp.to_string())
        .or_insert((0, Instant::now()));
    if window_start.elapsed() > Duration::from_secs(60) {
        *count = 0;
        *window_start = Instant::now();
    }
    *count += 1;
    if *count > limit {
        Some(ErrorCode::RateLimited)
    } else {
        None
    }
}

/// Called by the sender side: perform Hello handshake as the initiator.
pub async fn do_hello_as_initiator(
    conn: &Connection,
) -> Result<(quinn::SendStream, quinn::RecvStream, u32)> {
    let (mut send, mut recv) = conn.open_bi().await.context("open bi stream")?;

    // Send our Hello first (as initiator)
    let our_hello = DashMessage::Hello(HelloPayload {
        wire_version: WIRE_VERSION,
        supported_versions: SUPPORTED_VERSIONS.to_vec(),
    });
    write_message(&mut send, &our_hello).await?;

    // Read peer Hello
    let peer_hello = match read_message(&mut recv).await? {
        DashMessage::Hello(h) => h,
        other => bail!("expected Hello, got {:?}", other),
    };

    if peer_hello.wire_version != 0 {
        bail!("unknown peer wire_version: {}", peer_hello.wire_version);
    }

    let chosen = negotiate_version(&peer_hello.supported_versions)
        .ok_or_else(|| anyhow::anyhow!("no common protocol version"))?;

    Ok((send, recv, chosen))
}

fn negotiate_version(peer_versions: &[u32]) -> Option<u32> {
    SUPPORTED_VERSIONS
        .iter()
        .rev()
        .find(|v| peer_versions.contains(v))
        .copied()
}

/// Extract the TLS peer certificate fingerprint from a QUIC connection.
pub fn extract_peer_fp(conn: &Connection) -> Result<String> {
    let identity = conn
        .peer_identity()
        .ok_or_else(|| anyhow::anyhow!("no peer identity"))?;

    let certs = identity
        .downcast::<Vec<rustls_pki_types::CertificateDer<'static>>>()
        .map_err(|_| anyhow::anyhow!("peer identity is not a certificate chain"))?;

    let end_entity = certs
        .first()
        .ok_or_else(|| anyhow::anyhow!("empty certificate chain"))?;

    Ok(crate::crypto::Identity::peer_fingerprint(
        end_entity.as_ref(),
    ))
}

/// Compare expected fingerprint with the certificate fingerprint on the active QUIC connection.
pub fn peer_fp_matches(conn: &Connection, expected_peer_fp: &str) -> Result<(bool, String)> {
    let actual = extract_peer_fp(conn)?;
    Ok((!is_identity_mismatch(expected_peer_fp, &actual), actual))
}

pub fn is_identity_mismatch(expected_peer_fp: &str, actual_peer_fp: &str) -> bool {
    expected_peer_fp != actual_peer_fp
}

#[cfg(test)]
mod tests {
    use super::is_identity_mismatch;

    #[test]
    fn detects_identity_mismatch() {
        assert!(is_identity_mismatch("expected-fp", "actual-fp"));
    }

    #[test]
    fn accepts_matching_identity() {
        assert!(!is_identity_mismatch("same-fp", "same-fp"));
    }
}
