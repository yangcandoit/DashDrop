use anyhow::{Context, Result};
use quinn::{Endpoint, EndpointConfig, ServerConfig};
use socket2::{Domain, Protocol, Socket, Type};
use std::collections::HashMap;
use std::net::IpAddr;
use std::net::{SocketAddrV4, SocketAddrV6, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::AppHandle;

use crate::crypto::Identity;

const INCOMING_IP_RATE_LIMIT_WINDOW_SECS: u64 = 60;
const INCOMING_IP_RATE_LIMIT_MAX_PER_WINDOW: u32 = 120;

fn ip_rate_limited(
    limiter: &mut HashMap<IpAddr, (u32, Instant)>,
    ip: IpAddr,
    now: Instant,
) -> bool {
    let (count, window_start) = limiter.entry(ip).or_insert((0, now));
    if now.duration_since(*window_start) > Duration::from_secs(INCOMING_IP_RATE_LIMIT_WINDOW_SECS) {
        *count = 0;
        *window_start = now;
    }
    *count = count.saturating_add(1);
    *count > INCOMING_IP_RATE_LIMIT_MAX_PER_WINDOW
}

/// Start the QUIC server and return the bound port.
pub async fn start_server(
    identity: Identity,
    app: AppHandle,
    state: Arc<crate::state::AppState>,
) -> Result<u16> {
    // Build rustls config, then wrap for quinn
    let rustls_cfg = identity
        .server_tls_config()
        .context("build server TLS config")?;
    let quinn_crypto = quinn::crypto::rustls::QuicServerConfig::try_from(rustls_cfg)
        .context("quinn server TLS")?;

    let mut server_config = ServerConfig::with_crypto(Arc::new(quinn_crypto));

    // QUIC transport parameters per ARCHITECTURE.md §3.6
    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(Some(
        std::time::Duration::from_secs(30)
            .try_into()
            .map_err(|e| anyhow::anyhow!("invalid duration: {:?}", e))?,
    ));
    transport.keep_alive_interval(Some(std::time::Duration::from_secs(10)));
    server_config.transport_config(Arc::new(transport));

    let (socket, listener_mode) = bind_server_socket()?;
    let runtime = quinn::default_runtime()
        .ok_or_else(|| anyhow::anyhow!("no async runtime found for QUIC endpoint"))?;
    let endpoint = Endpoint::new(
        EndpointConfig::default(),
        Some(server_config),
        socket,
        runtime,
    )
    .context("bind QUIC endpoint")?;
    let port = endpoint.local_addr()?.port();
    let listener_addrs = listener_addrs_for_mode(&listener_mode, port);

    tracing::info!(
        "QUIC server listening on port {port}, mode={listener_mode}, addrs={}",
        listener_addrs.join(", ")
    );
    *state.local_port.write().await = port;
    *state.listener_mode.write().await = listener_mode;
    *state.listener_addrs.write().await = listener_addrs;
    state
        .endpoint
        .set(endpoint.clone())
        .map_err(|_| anyhow::anyhow!("endpoint already set"))?;

    let rate_limiter = Arc::new(tokio::sync::Mutex::new(HashMap::<IpAddr, (u32, Instant)>::new()));

    // Accept connections in background
    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            let app2 = app.clone();
            let state2 = state.clone();
            let rate_limiter2 = rate_limiter.clone();
            tokio::spawn(async move {
                match incoming.await {
                    Ok(conn) => {
                        let is_probe = conn
                            .handshake_data()
                            .and_then(|data| {
                                data.downcast::<quinn::crypto::rustls::HandshakeData>().ok()
                            })
                            .and_then(|hs| hs.protocol)
                            .map(|protocol| {
                                protocol.as_slice() == crate::transport::probe::PROBE_ALPN
                            })
                            .unwrap_or(false);
                        if is_probe {
                            conn.close(quinn::VarInt::from_u32(0xD0), b"probe acknowledged");
                            return;
                        }

                        let ip = conn.remote_address().ip();
                        let limited = {
                            let mut rl = rate_limiter2.lock().await;
                            ip_rate_limited(&mut rl, ip, Instant::now())
                        };
                        if limited {
                            tracing::warn!("Rate limit exceeded for transfer IP: {}", ip);
                            conn.close(quinn::VarInt::from_u32(1), b"rate limited");
                            return;
                        }
                        tracing::debug!("Incoming connection from {:?}", conn.remote_address());
                        if let Err(e) =
                            super::handshake::handle_incoming(conn, app2.clone(), state2.clone())
                                .await
                        {
                            tracing::warn!("Incoming connection error: {e:#}");
                            if let Ok(db) = state2.db.lock() {
                                let _ = crate::db::log_security_event(
                                    &db,
                                    "handshake_failed",
                                    "incoming",
                                    None,
                                    &e.to_string(),
                                );
                            }
                            crate::transport::events::emit_transfer_error_with_detail(
                                &app2,
                                None,
                                "E_PROTOCOL",
                                "IncomingConnectionError",
                                "incoming",
                                0,
                                Some(&format!("{e:#}")),
                            );
                        }
                    }
                    Err(e) => tracing::warn!("QUIC accept error: {e}"),
                }
            });
        }
    });

    Ok(port)
}

fn bind_server_socket() -> Result<(UdpSocket, String)> {
    if let Ok(socket_v6) = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP)) {
        if let Err(e) = socket_v6.set_only_v6(false) {
            tracing::warn!(
                "Failed to configure IPv6 UDP socket for dual-stack, falling back to IPv4-only: {e}"
            );
        } else {
            let bind_v6 = SocketAddrV6::new(std::net::Ipv6Addr::UNSPECIFIED, 0, 0, 0);
            if let Err(e) = socket_v6.bind(&bind_v6.into()) {
                tracing::warn!(
                    "Failed to bind IPv6 wildcard UDP listener, falling back to IPv4-only: {e}"
                );
            } else {
                socket_v6
                    .set_nonblocking(true)
                    .context("set dual-stack UDP socket nonblocking")?;
                let std_socket: UdpSocket = socket_v6.into();
                return Ok((std_socket, "dual_stack".to_string()));
            }
        }
    } else {
        tracing::warn!("Failed to create IPv6 UDP socket, falling back to IPv4-only");
    }

    let socket_v4 = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
        .context("create IPv4 fallback UDP socket")?;
    let bind_v4 = SocketAddrV4::new(std::net::Ipv4Addr::UNSPECIFIED, 0);
    socket_v4
        .bind(&bind_v4.into())
        .context("bind IPv4 fallback UDP listener")?;
    socket_v4
        .set_nonblocking(true)
        .context("set IPv4 fallback UDP socket nonblocking")?;
    let std_socket: UdpSocket = socket_v4.into();
    Ok((std_socket, "ipv4_only_fallback".to_string()))
}

fn listener_addrs_for_mode(mode: &str, port: u16) -> Vec<String> {
    match mode {
        "dual_stack" => vec![format!("[::]:{port}"), format!("0.0.0.0:{port}")],
        _ => vec![format!("0.0.0.0:{port}")],
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn listener_addrs_reflect_dual_stack_mode() {
        let addrs = super::listener_addrs_for_mode("dual_stack", 7001);
        assert_eq!(addrs, vec!["[::]:7001".to_string(), "0.0.0.0:7001".to_string()]);
    }

    #[test]
    fn listener_addrs_reflect_ipv4_fallback_mode() {
        let addrs = super::listener_addrs_for_mode("ipv4_only_fallback", 7001);
        assert_eq!(addrs, vec!["0.0.0.0:7001".to_string()]);
    }

    #[test]
    fn ip_rate_limit_triggers_only_after_threshold() {
        let mut limiter = std::collections::HashMap::new();
        let ip: std::net::IpAddr = "192.168.1.7".parse().unwrap();
        let now = std::time::Instant::now();
        for _ in 0..super::INCOMING_IP_RATE_LIMIT_MAX_PER_WINDOW {
            assert!(!super::ip_rate_limited(&mut limiter, ip, now));
        }
        assert!(super::ip_rate_limited(&mut limiter, ip, now));
    }
}
