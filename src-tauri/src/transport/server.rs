use anyhow::{Context, Result};
use quinn::{Endpoint, EndpointConfig, ServerConfig};
use socket2::{Domain, Protocol, Socket, Type};
use std::net::{SocketAddrV4, SocketAddrV6, UdpSocket};
use std::sync::Arc;
use tauri::AppHandle;

use crate::crypto::Identity;

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

    let rate_limiter = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::<
        std::net::IpAddr,
        (u32, std::time::Instant),
    >::new()));

    // Accept connections in background
    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            let ip = incoming.remote_address().ip();
            {
                let mut rl = rate_limiter.lock().await;
                let (count, last_seen) = rl.entry(ip).or_insert((0, std::time::Instant::now()));
                if last_seen.elapsed() > std::time::Duration::from_secs(60) {
                    *count = 0;
                    *last_seen = std::time::Instant::now();
                }
                *count += 1;
                if *count > 20 {
                    tracing::warn!("Rate limit exceeded for IP: {}", ip);
                    incoming.ignore();
                    continue;
                }
            }

            let app2 = app.clone();
            let state2 = state.clone();
            tokio::spawn(async move {
                match incoming.await {
                    Ok(conn) => {
                        if let Some(protocol) = conn
                            .handshake_data()
                            .and_then(|data| {
                                data.downcast::<quinn::crypto::rustls::HandshakeData>().ok()
                            })
                            .and_then(|hs| hs.protocol)
                        {
                            if protocol.as_slice() == crate::transport::probe::PROBE_ALPN {
                                conn.close(quinn::VarInt::from_u32(0xD0), b"probe acknowledged");
                                return;
                            }
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
}
