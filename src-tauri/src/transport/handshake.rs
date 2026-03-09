use anyhow::{bail, Context, Result};
use quinn::Connection;
use std::sync::Arc;
use tauri::AppHandle;

use crate::state::{AppState};
use crate::transport::protocol::{
    read_message, write_message, DashMessage, HelloPayload, RejectPayload,
    ErrorCode, WIRE_VERSION, SUPPORTED_VERSIONS,
};

/// Called for each accepted incoming QUIC connection.
/// Performs: cert fp binding → Hello exchange → route to receiver.
pub async fn handle_incoming(
    conn: Connection,
    app: AppHandle,
    state: Arc<AppState>,
) -> Result<()> {
    // 1. Extract peer TLS certificate fingerprint
    let peer_cert_fp = extract_peer_fp(&conn)?;

    // 2. Auxiliary identity signal: compare against mDNS-by-IP and emit warning if mismatched.
    // Security decision is enforced by initiator-side fingerprint binding.
    {
        let devices = state.devices.read().await;
        for device in devices.values() {
            for session in device.sessions.values() {
                if session.addrs.iter().any(|a| a.ip() == conn.remote_address().ip()) {
                    // Found a matching mDNS device by IP
                    if device.fingerprint != peer_cert_fp {
                        tracing::warn!(
                            "Identity mismatch: mDNS fp={} but cert fp={}",
                            device.fingerprint, peer_cert_fp
                        );
                        let _ = tauri::Emitter::emit(&app, "identity_mismatch", serde_json::json!({
                            "remote_addr": conn.remote_address().to_string(),
                            "mdns_fp": device.fingerprint,
                            "cert_fp": peer_cert_fp,
                            "phase": "incoming",
                        }));
                    }
                    break;
                }
            }
        }
    }

    // 3. Hello exchange: receive peer Hello, respond with our Hello
    let (mut send, mut recv) = conn.accept_bi().await.context("accept bi stream")?;

    // Read peer's Hello first
    let peer_hello = match read_message(&mut recv).await? {
        DashMessage::Hello(h) => h,
        other => bail!("expected Hello, got {:?}", other),
    };

    if peer_hello.wire_version != 0 {
        bail!("unknown wire_version: {}", peer_hello.wire_version);
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
            let reject = DashMessage::Reject(RejectPayload { reason: ErrorCode::VersionMismatch });
            write_message(&mut send, &reject).await?;
            bail!("no common protocol version");
        }
    };

    // 4. Wait for Offer on this stream
    let offer = match read_message(&mut recv).await? {
        DashMessage::Offer(o) => o,
        DashMessage::Cancel(_) => bail!("peer cancelled before offer"),
        other => bail!("expected Offer, got {:?}", other),
    };

    tracing::info!(
        "Incoming offer from '{}' (fp={}): {} items, {} bytes total",
        offer.sender_name, peer_cert_fp, offer.items.len(), offer.total_size
    );

    // 5. Hand off to receiver state machine
    super::receiver::handle_offer(
        offer, conn, send, recv,
        peer_cert_fp, chosen_version,
        app, state,
    ).await
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

    Ok(crate::crypto::Identity::peer_fingerprint(end_entity.as_ref()))
}

/// Compare expected fingerprint with the certificate fingerprint on the active QUIC connection.
pub fn peer_fp_matches(conn: &Connection, expected_peer_fp: &str) -> Result<(bool, String)> {
    let actual = extract_peer_fp(conn)?;
    Ok((actual == expected_peer_fp, actual))
}
