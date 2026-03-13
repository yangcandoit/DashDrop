use anyhow::{Context, Result};
use quinn::{Endpoint, EndpointConfig, ServerConfig};
use socket2::{Domain, Protocol, Socket, Type};
use std::collections::HashMap;
use std::net::IpAddr;
use std::net::{SocketAddrV4, SocketAddrV6, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::crypto::Identity;
use crate::runtime::host::RuntimeHost;

const INCOMING_IP_RATE_LIMIT_WINDOW_SECS: u64 = 60;
const INCOMING_IP_RATE_LIMIT_MAX_PER_WINDOW: u32 = 120;
const PREFERRED_QUIC_PORT: u16 = 53319;

struct ListenerBinding {
    socket: UdpSocket,
    listener_mode: String,
    listener_port_mode: String,
}

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
    host: Arc<dyn RuntimeHost>,
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

    let binding = bind_server_socket()?;
    let runtime = quinn::default_runtime()
        .ok_or_else(|| anyhow::anyhow!("no async runtime found for QUIC endpoint"))?;
    let endpoint = Endpoint::new(
        EndpointConfig::default(),
        Some(server_config),
        binding.socket,
        runtime,
    )
    .context("bind QUIC endpoint")?;
    let port = endpoint.local_addr()?.port();
    let listener_addrs = listener_addrs_for_mode(&binding.listener_mode, port);
    let firewall_rule_state = ensure_firewall_rule_state();

    tracing::info!(
        "QUIC server listening on port {port}, mode={}, port_mode={}, firewall_rule_state={}, addrs={}",
        binding.listener_mode,
        binding.listener_port_mode,
        firewall_rule_state,
        listener_addrs.join(", ")
    );
    *state.local_port.write().await = port;
    *state.listener_mode.write().await = binding.listener_mode;
    *state.listener_port_mode.write().await = binding.listener_port_mode;
    *state.firewall_rule_state.write().await = firewall_rule_state;
    *state.listener_addrs.write().await = listener_addrs;
    state
        .endpoint
        .set(endpoint.clone())
        .map_err(|_| anyhow::anyhow!("endpoint already set"))?;

    let rate_limiter = Arc::new(tokio::sync::Mutex::new(
        HashMap::<IpAddr, (u32, Instant)>::new(),
    ));

    // Accept connections in background
    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            let host2 = Arc::clone(&host);
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
                        if let Err(e) = super::handshake::handle_incoming(
                            conn,
                            Arc::clone(&host2),
                            state2.clone(),
                        )
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
                                &host2,
                                &state2,
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

fn bind_server_socket() -> Result<ListenerBinding> {
    bind_server_socket_with_preferred_port(PREFERRED_QUIC_PORT)
}

fn bind_server_socket_with_preferred_port(preferred_port: u16) -> Result<ListenerBinding> {
    let mut errors = Vec::new();

    match bind_dual_stack_socket(preferred_port) {
        Ok(socket) => {
            return Ok(ListenerBinding {
                socket,
                listener_mode: "dual_stack".to_string(),
                listener_port_mode: "fixed".to_string(),
            });
        }
        Err(error) => {
            tracing::warn!(
                "Failed to bind dual-stack UDP listener on fixed port {preferred_port}: {error:#}"
            );
            errors.push(error);
        }
    }

    match bind_ipv4_socket(preferred_port) {
        Ok(socket) => {
            return Ok(ListenerBinding {
                socket,
                listener_mode: "ipv4_only_fallback".to_string(),
                listener_port_mode: "fixed".to_string(),
            });
        }
        Err(error) => {
            tracing::warn!(
                "Failed to bind IPv4 UDP listener on fixed port {preferred_port}: {error:#}"
            );
            errors.push(error);
        }
    }

    match bind_dual_stack_socket(0) {
        Ok(socket) => {
            return Ok(ListenerBinding {
                socket,
                listener_mode: "dual_stack".to_string(),
                listener_port_mode: "fallback_random".to_string(),
            });
        }
        Err(error) => {
            tracing::warn!("Failed to bind dual-stack UDP listener on fallback port: {error:#}");
            errors.push(error);
        }
    }

    match bind_ipv4_socket(0) {
        Ok(socket) => Ok(ListenerBinding {
            socket,
            listener_mode: "ipv4_only_fallback".to_string(),
            listener_port_mode: "fallback_random".to_string(),
        }),
        Err(error) => {
            tracing::warn!("Failed to bind IPv4 UDP listener on fallback port: {error:#}");
            errors.push(error);
            Err(errors
                .into_iter()
                .next()
                .unwrap_or_else(|| anyhow::anyhow!("failed to bind QUIC UDP listener")))
        }
    }
}

fn bind_dual_stack_socket(port: u16) -> Result<UdpSocket> {
    let socket_v6 = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))
        .context("create IPv6 UDP socket")?;
    socket_v6
        .set_only_v6(false)
        .context("configure IPv6 socket for dual-stack")?;
    let bind_v6 = SocketAddrV6::new(std::net::Ipv6Addr::UNSPECIFIED, port, 0, 0);
    socket_v6
        .bind(&bind_v6.into())
        .with_context(|| format!("bind dual-stack UDP listener on port {port}"))?;
    socket_v6
        .set_nonblocking(true)
        .context("set dual-stack UDP socket nonblocking")?;
    Ok(socket_v6.into())
}

fn bind_ipv4_socket(port: u16) -> Result<UdpSocket> {
    let socket_v4 = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
        .context("create IPv4 fallback UDP socket")?;
    let bind_v4 = SocketAddrV4::new(std::net::Ipv4Addr::UNSPECIFIED, port);
    socket_v4
        .bind(&bind_v4.into())
        .with_context(|| format!("bind IPv4 fallback UDP listener on port {port}"))?;
    socket_v4
        .set_nonblocking(true)
        .context("set IPv4 fallback UDP socket nonblocking")?;
    Ok(socket_v4.into())
}

fn listener_addrs_for_mode(mode: &str, port: u16) -> Vec<String> {
    match mode {
        "dual_stack" => vec![format!("[::]:{port}"), format!("0.0.0.0:{port}")],
        _ => vec![format!("0.0.0.0:{port}")],
    }
}

#[cfg(target_os = "windows")]
fn ensure_firewall_rule_state() -> String {
    match windows_is_elevated() {
        Ok(false) => "user_scope_unmanaged".to_string(),
        Ok(true) => match windows_ensure_firewall_rules() {
            Ok(()) => "managed".to_string(),
            Err(error) => {
                tracing::warn!("Failed to ensure Windows firewall rules: {error:#}");
                "unknown".to_string()
            }
        },
        Err(error) => {
            tracing::warn!("Failed to determine Windows elevation state: {error:#}");
            "unknown".to_string()
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn ensure_firewall_rule_state() -> String {
    "unknown".to_string()
}

#[cfg(target_os = "windows")]
fn windows_is_elevated() -> Result<bool> {
    let output = run_powershell(
        "[bool]([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)",
    )
    .context("query Windows elevation state")?;
    let stdout = String::from_utf8_lossy(&output.stdout)
        .trim()
        .to_ascii_lowercase();
    match stdout.as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(anyhow::anyhow!(
            "unexpected elevation probe output: {stdout}"
        )),
    }
}

#[cfg(target_os = "windows")]
fn windows_ensure_firewall_rules() -> Result<()> {
    const APP_RULE_NAME: &str = "DashDrop UDP App";
    const FIXED_PORT_RULE_NAME: &str = "DashDrop UDP Fixed Port 53319";

    let exe_path = std::env::current_exe().context("resolve current executable path")?;
    let exe_path = exe_path.display().to_string().replace('\'', "''");

    ensure_named_firewall_rule(
        APP_RULE_NAME,
        &format!(
            "New-NetFirewallRule -DisplayName '{APP_RULE_NAME}' -Direction Inbound -Action Allow -Protocol UDP -Program '{exe_path}' -Profile Any | Out-Null"
        ),
    )?;
    ensure_named_firewall_rule(
        FIXED_PORT_RULE_NAME,
        &format!(
            "New-NetFirewallRule -DisplayName '{FIXED_PORT_RULE_NAME}' -Direction Inbound -Action Allow -Protocol UDP -LocalPort {PREFERRED_QUIC_PORT} -Profile Any | Out-Null"
        ),
    )?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn ensure_named_firewall_rule(rule_name: &str, create_script: &str) -> Result<()> {
    let lookup_script = format!(
        "$rule = Get-NetFirewallRule -DisplayName '{rule_name}' -ErrorAction SilentlyContinue | Select-Object -First 1; if ($null -eq $rule) {{ 'false' }} else {{ 'true' }}"
    );
    let output = run_powershell(&lookup_script)
        .with_context(|| format!("lookup Windows firewall rule {rule_name}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout)
        .trim()
        .to_ascii_lowercase();
    if stdout == "true" {
        return Ok(());
    }
    if stdout != "false" {
        return Err(anyhow::anyhow!(
            "unexpected firewall lookup output for {rule_name}: {stdout}"
        ));
    }
    run_powershell(create_script)
        .with_context(|| format!("create Windows firewall rule {rule_name}"))?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn run_powershell(script: &str) -> Result<std::process::Output> {
    let output = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .context("spawn powershell")?;
    if output.status.success() {
        Ok(output)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(anyhow::anyhow!("powershell failed: {stderr}"))
    }
}

#[cfg(test)]
mod tests {
    use std::net::UdpSocket;

    #[test]
    fn listener_addrs_reflect_dual_stack_mode() {
        let addrs = super::listener_addrs_for_mode("dual_stack", 7001);
        assert_eq!(
            addrs,
            vec!["[::]:7001".to_string(), "0.0.0.0:7001".to_string()]
        );
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

    #[test]
    fn bind_server_prefers_fixed_port_when_available() {
        let probe = UdpSocket::bind("127.0.0.1:0").expect("reserve probe port");
        let preferred_port = probe.local_addr().expect("probe addr").port();
        drop(probe);

        let binding = super::bind_server_socket_with_preferred_port(preferred_port)
            .expect("bind preferred port");
        let bound_port = binding.socket.local_addr().expect("listener addr").port();

        assert_eq!(binding.listener_port_mode, "fixed");
        assert_eq!(bound_port, preferred_port);
    }

    #[test]
    fn bind_server_falls_back_to_random_port_when_fixed_is_occupied() {
        let blocker = UdpSocket::bind("127.0.0.1:0").expect("occupy UDP port");
        let preferred_port = blocker.local_addr().expect("blocker addr").port();

        let binding = super::bind_server_socket_with_preferred_port(preferred_port)
            .expect("bind fallback port");
        let bound_port = binding.socket.local_addr().expect("listener addr").port();

        assert_eq!(binding.listener_port_mode, "fallback_random");
        assert_ne!(bound_port, preferred_port);
    }
}
