use anyhow::{Context, Result};
use quinn::{ClientConfig, Connection};
use std::net::SocketAddr;
use std::sync::Arc;

use crate::state::AppState;

const TRANSFER_ALPN: &[u8] = b"dashdrop-transfer/1";

/// Connect to a remote DashDrop peer via QUIC.
pub async fn connect_to_peer(state: &AppState, remote_addr: SocketAddr) -> Result<Connection> {
    let mut rustls_cfg = state
        .identity
        .client_tls_config()
        .context("build client TLS config")?;
    Arc::get_mut(&mut rustls_cfg)
        .ok_or_else(|| anyhow::anyhow!("client TLS config unexpectedly shared"))?
        .alpn_protocols = vec![TRANSFER_ALPN.to_vec()];
    let quinn_crypto = quinn::crypto::rustls::QuicClientConfig::try_from(rustls_cfg)
        .context("quinn client TLS")?;

    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(Some(
        std::time::Duration::from_secs(30)
            .try_into()
            .map_err(|e| anyhow::anyhow!("invalid duration: {:?}", e))?,
    ));
    transport.keep_alive_interval(Some(std::time::Duration::from_secs(10)));

    let mut client_config = ClientConfig::new(Arc::new(quinn_crypto));
    client_config.transport_config(Arc::new(transport));

    let endpoint = state.endpoint.get().ok_or_else(|| {
        anyhow::anyhow!("QUIC endpoint not initialized yet. Server might be booting.")
    })?;

    let conn = endpoint
        .connect_with(client_config, remote_addr, "dashdrop")
        .context("initiate QUIC connection")?
        .await
        .context("QUIC handshake")?;

    tracing::debug!("Connected to {remote_addr}");
    Ok(conn)
}
