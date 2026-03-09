use anyhow::{Context, Result};
use quinn::ClientConfig;
use std::net::SocketAddr;
use std::sync::Arc;

use crate::state::AppState;

pub const PROBE_ALPN: &[u8] = b"dashdrop-probe/1";

pub async fn probe_addr(state: &AppState, remote_addr: SocketAddr) -> Result<()> {
    let mut rustls_cfg = state
        .identity
        .client_tls_config()
        .context("build probe client TLS config")?;
    Arc::get_mut(&mut rustls_cfg)
        .ok_or_else(|| anyhow::anyhow!("probe TLS config unexpectedly shared"))?
        .alpn_protocols = vec![PROBE_ALPN.to_vec()];
    let quinn_crypto = quinn::crypto::rustls::QuicClientConfig::try_from(rustls_cfg)
        .context("quinn probe client TLS")?;

    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(Some(
        std::time::Duration::from_secs(5)
            .try_into()
            .map_err(|e| anyhow::anyhow!("invalid probe duration: {e:?}"))?,
    ));
    transport.keep_alive_interval(None);

    let mut client_config = ClientConfig::new(Arc::new(quinn_crypto));
    client_config.transport_config(Arc::new(transport));

    let endpoint = state
        .endpoint
        .get()
        .ok_or_else(|| anyhow::anyhow!("QUIC endpoint not initialized yet"))?;
    let conn = endpoint
        .connect_with(client_config, remote_addr, "dashdrop")
        .context("initiate probe connection")?
        .await
        .context("probe handshake")?;
    conn.close(quinn::VarInt::from_u32(0), b"probe ok");
    Ok(())
}
