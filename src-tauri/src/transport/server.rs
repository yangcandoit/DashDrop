use anyhow::{Context, Result};
use quinn::{Endpoint, ServerConfig};
use std::net::SocketAddr;
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

    let bind_addr: SocketAddr = "0.0.0.0:0".parse()?;
    let endpoint = Endpoint::server(server_config, bind_addr).context("bind QUIC endpoint")?;
    let port = endpoint.local_addr()?.port();

    tracing::info!("QUIC server listening on port {port}");
    *state.local_port.write().await = port;
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
                            crate::transport::events::emit_transfer_error(
                                &app2,
                                None,
                                &e.to_string(),
                                "IncomingConnectionError",
                                "incoming",
                                0,
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
