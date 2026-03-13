use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::crypto::Identity;
use crate::db;
use crate::persistence;
use crate::state::{AppConfig, AppState, TrustedPeer};

pub fn resolve_config_dir_from_base(base_config_dir: Option<PathBuf>) -> Result<PathBuf> {
    if let Ok(override_dir) = std::env::var("DASHDROP_CONFIG_DIR") {
        return Ok(PathBuf::from(override_dir));
    }

    let base =
        base_config_dir.ok_or_else(|| anyhow::anyhow!("failed to resolve app config directory"))?;
    Ok(base.join("dashdrop"))
}

pub fn resolve_headless_config_dir() -> Result<PathBuf> {
    if let Ok(override_dir) = std::env::var("DASHDROP_CONFIG_DIR") {
        return Ok(PathBuf::from(override_dir));
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return Ok(PathBuf::from(appdata).join("dashdrop"));
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        if let Ok(xdg_config_home) = std::env::var("XDG_CONFIG_HOME") {
            return Ok(PathBuf::from(xdg_config_home).join("dashdrop"));
        }
        if let Ok(home) = std::env::var("HOME") {
            return Ok(PathBuf::from(home).join(".config").join("dashdrop"));
        }
    }

    Err(anyhow::anyhow!(
        "failed to resolve headless config directory; set DASHDROP_CONFIG_DIR"
    ))
}

pub fn collect_external_share_paths_from_args() -> Vec<String> {
    collect_external_share_paths(std::env::args_os().skip(1))
}

pub fn collect_external_share_paths<I, S>(values: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let current_dir = std::env::current_dir().ok();
    let mut seen = HashSet::new();

    values
        .into_iter()
        .filter_map(|value| normalize_external_share_path(value.as_ref(), current_dir.as_deref()))
        .filter(|path| seen.insert(path.clone()))
        .collect()
}

pub fn collect_pairing_links_from_args() -> Vec<String> {
    collect_pairing_links(std::env::args().skip(1))
}

fn collect_pairing_links<I>(values: I) -> Vec<String>
where
    I: IntoIterator<Item = String>,
{
    values
        .into_iter()
        .filter(|value| value.trim_start().starts_with("dashdrop://pair?"))
        .collect()
}

fn normalize_external_share_path(value: &OsStr, current_dir: Option<&Path>) -> Option<String> {
    let raw_value = value.to_string_lossy();
    if raw_value.trim().is_empty() || raw_value.starts_with("-psn_") {
        return None;
    }

    if has_ascii_prefix(&raw_value, "file://") {
        return parse_file_url_path(&raw_value);
    }

    let raw_path = PathBuf::from(value);
    if raw_path.as_os_str().is_empty() {
        return None;
    }

    let candidate = if raw_path.is_absolute() {
        raw_path
    } else if let Some(base_dir) = current_dir {
        base_dir.join(raw_path)
    } else {
        raw_path
    };

    std::fs::canonicalize(candidate)
        .ok()
        .map(|path| path.to_string_lossy().to_string())
}

fn parse_file_url_path(raw_value: &str) -> Option<String> {
    let url_path = strip_ascii_prefix(raw_value, "file://")?;
    let url_path = if let Some(path) = strip_ascii_prefix(url_path, "localhost") {
        path
    } else {
        url_path
    };

    if !url_path.starts_with('/') {
        return None;
    }

    let decoded = percent_decode_utf8(strip_url_suffix(url_path))?;
    let candidate = normalize_local_file_url_path(decoded);

    std::fs::canonicalize(candidate)
        .ok()
        .map(|path| path.to_string_lossy().to_string())
}

fn strip_url_suffix(value: &str) -> &str {
    let query_index = value.find('?').unwrap_or(value.len());
    let fragment_index = value.find('#').unwrap_or(value.len());
    &value[..query_index.min(fragment_index)]
}

fn has_ascii_prefix(value: &str, prefix: &str) -> bool {
    strip_ascii_prefix(value, prefix).is_some()
}

fn strip_ascii_prefix<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    if value.len() < prefix.len() {
        return None;
    }
    let candidate = &value[..prefix.len()];
    if candidate.eq_ignore_ascii_case(prefix) {
        Some(&value[prefix.len()..])
    } else {
        None
    }
}

fn percent_decode_utf8(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                if index + 2 >= bytes.len() {
                    return None;
                }
                let high = decode_hex_digit(bytes[index + 1])?;
                let low = decode_hex_digit(bytes[index + 2])?;
                decoded.push((high << 4) | low);
                index += 3;
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }

    String::from_utf8(decoded).ok()
}

fn decode_hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(target_os = "windows")]
fn normalize_local_file_url_path(decoded: String) -> PathBuf {
    let normalized = decoded.replace('/', "\\");
    if let Some(without_leading_slash) = normalized.strip_prefix('\\') {
        if has_windows_drive_prefix(without_leading_slash) {
            return PathBuf::from(without_leading_slash);
        }
    }
    PathBuf::from(normalized)
}

#[cfg(target_os = "windows")]
fn has_windows_drive_prefix(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 2 && bytes[1] == b':'
}

#[cfg(not(target_os = "windows"))]
fn normalize_local_file_url_path(decoded: String) -> PathBuf {
    PathBuf::from(decoded)
}

pub async fn initialize_state_at(config_dir: &Path) -> Result<Arc<AppState>> {
    let identity =
        Identity::load_or_create(&config_dir.to_path_buf()).context("initialize identity")?;

    let db_conn = db::init_db_at(config_dir).context("initialize local database")?;
    let mut db_config = db::load_app_config(&db_conn).unwrap_or(None);
    let mut db_trusted = db::load_trusted_peers(&db_conn).unwrap_or_default();

    if db_config.is_none() && db_trusted.is_empty() {
        if let Ok(legacy) = persistence::load_state_at(config_dir) {
            if let Some(cfg) = legacy.app_config.clone() {
                let _ = db::save_app_config(&db_conn, &cfg);
                db_config = Some(cfg);
            }
            if !legacy.trusted_peers.is_empty() {
                let trusted_map: HashMap<String, TrustedPeer> = legacy
                    .trusted_peers
                    .iter()
                    .cloned()
                    .map(|peer| (peer.fingerprint.clone(), peer))
                    .collect();
                let _ = db::replace_trusted_peers(&db_conn, &trusted_map);
                db_trusted = legacy.trusted_peers;
            }
        }
    }

    let mut config = db_config.unwrap_or_else(|| AppConfig {
        device_name: identity.device_name.clone(),
        ..Default::default()
    });
    if config.device_name.trim().is_empty() {
        config.device_name = identity.device_name.clone();
    }

    let state = Arc::new(AppState::new(identity, config, db_conn));
    let mut trusted = state.trusted_peers.write().await;
    trusted.clear();
    for peer in db_trusted {
        trusted.insert(peer.fingerprint.clone(), peer);
    }
    drop(trusted);

    Ok(state)
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::ffi::OsString;
    use std::fs;
    use std::sync::{Mutex, OnceLock};

    use super::{
        collect_external_share_paths, collect_pairing_links, resolve_config_dir_from_base,
        resolve_headless_config_dir,
    };

    fn config_env_guard() -> &'static Mutex<()> {
        static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
        GUARD.get_or_init(|| Mutex::new(()))
    }

    fn cwd_guard() -> &'static Mutex<()> {
        static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
        GUARD.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn resolve_config_dir_appends_dashdrop_folder() {
        let _guard = config_env_guard().lock().expect("env guard");
        std::env::remove_var("DASHDROP_CONFIG_DIR");
        let base =
            std::env::temp_dir().join(format!("dashdrop-bootstrap-{}", uuid::Uuid::new_v4()));
        let resolved = resolve_config_dir_from_base(Some(base.clone())).expect("config dir");
        assert_eq!(resolved, base.join("dashdrop"));
    }

    #[test]
    fn resolve_headless_config_dir_prefers_env_override() {
        let _guard = config_env_guard().lock().expect("env guard");
        let override_dir =
            std::env::temp_dir().join(format!("dashdrop-headless-{}", uuid::Uuid::new_v4()));
        std::env::set_var("DASHDROP_CONFIG_DIR", &override_dir);
        let resolved = resolve_headless_config_dir().expect("headless config dir");
        assert_eq!(resolved, override_dir);
        std::env::remove_var("DASHDROP_CONFIG_DIR");
    }

    #[test]
    fn collect_pairing_links_from_args_filters_dashdrop_pair_links() {
        let links = vec![
            "dashdrop://pair?data=abc".to_string(),
            "/tmp/example.txt".to_string(),
            " dashdrop://pair?data=def".to_string(),
        ];
        assert_eq!(
            vec![
                "dashdrop://pair?data=abc".to_string(),
                " dashdrop://pair?data=def".to_string()
            ],
            collect_pairing_links(links)
        );
    }

    #[test]
    fn collect_external_share_paths_filters_invalid_and_deduplicates() {
        let temp_dir =
            std::env::temp_dir().join(format!("dashdrop-share-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&temp_dir).expect("create temp dir");
        let file_path = temp_dir.join("shared.txt");
        fs::write(&file_path, "payload").expect("write file");
        let normalized_file_path = fs::canonicalize(&file_path)
            .expect("canonicalize file")
            .to_string_lossy()
            .to_string();

        let paths = collect_external_share_paths([
            OsString::from(file_path.as_os_str()),
            OsString::from(file_path.as_os_str()),
            OsString::from(temp_dir.join("missing.txt").as_os_str()),
            OsString::from(""),
        ]);

        assert_eq!(paths, vec![normalized_file_path]);

        fs::remove_dir_all(&temp_dir).expect("remove temp dir");
    }

    #[test]
    fn collect_external_share_paths_accepts_local_file_urls_and_ignores_process_serial_number() {
        let temp_dir =
            std::env::temp_dir().join(format!("dashdrop-share-url-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&temp_dir).expect("create temp dir");
        let file_path = temp_dir.join("shared file.txt");
        fs::write(&file_path, "payload").expect("write file");
        let normalized_file_path = fs::canonicalize(&file_path)
            .expect("canonicalize file")
            .to_string_lossy()
            .to_string();

        let file_url = format!("file://{}", file_path.to_string_lossy().replace(' ', "%20"));
        let paths = collect_external_share_paths([
            OsString::from("-psn_0_12345"),
            OsString::from(file_url),
            OsString::from("file://example.com/tmp/not-local.txt"),
        ]);

        assert_eq!(paths, vec![normalized_file_path]);

        fs::remove_dir_all(&temp_dir).expect("remove temp dir");
    }

    #[test]
    fn collect_external_share_paths_accepts_localhost_file_urls_with_query_suffix() {
        let temp_dir = std::env::temp_dir().join(format!(
            "dashdrop-share-url-localhost-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&temp_dir).expect("create temp dir");
        let file_path = temp_dir.join("example.txt");
        fs::write(&file_path, "payload").expect("write file");
        let normalized_file_path = fs::canonicalize(&file_path)
            .expect("canonicalize file")
            .to_string_lossy()
            .to_string();

        let file_url = format!(
            "file://localhost{}?launch_source=finder#ignored",
            file_path.to_string_lossy()
        );
        let paths = collect_external_share_paths([OsString::from(file_url)]);

        assert_eq!(paths, vec![normalized_file_path]);

        fs::remove_dir_all(&temp_dir).expect("remove temp dir");
    }

    #[test]
    fn collect_external_share_paths_accepts_mixed_case_file_urls() {
        let temp_dir = std::env::temp_dir().join(format!(
            "dashdrop-share-url-mixed-case-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&temp_dir).expect("create temp dir");
        let file_path = temp_dir.join("mixed-case.txt");
        fs::write(&file_path, "payload").expect("write file");
        let normalized_file_path = fs::canonicalize(&file_path)
            .expect("canonicalize file")
            .to_string_lossy()
            .to_string();

        let file_url = format!(
            "FILE://LOCALHOST{}?launch_source=test",
            file_path.to_string_lossy()
        );
        let paths = collect_external_share_paths([OsString::from(file_url)]);

        assert_eq!(paths, vec![normalized_file_path]);

        fs::remove_dir_all(&temp_dir).expect("remove temp dir");
    }

    #[test]
    fn collect_external_share_paths_resolves_relative_existing_paths_from_cwd() {
        let _guard = cwd_guard().lock().expect("cwd guard");
        let original_cwd = env::current_dir().expect("current dir");
        let temp_dir =
            env::temp_dir().join(format!("dashdrop-share-relative-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&temp_dir).expect("create temp dir");
        let file_path = temp_dir.join("relative.txt");
        fs::write(&file_path, "payload").expect("write file");
        let normalized_file_path = fs::canonicalize(&file_path)
            .expect("canonicalize file")
            .to_string_lossy()
            .to_string();

        env::set_current_dir(&temp_dir).expect("set current dir");
        let paths = collect_external_share_paths([OsString::from("relative.txt")]);
        env::set_current_dir(original_cwd).expect("restore current dir");

        assert_eq!(paths, vec![normalized_file_path]);

        fs::remove_dir_all(&temp_dir).expect("remove temp dir");
    }

    #[test]
    fn collect_external_share_paths_rejects_malformed_file_urls() {
        let paths = collect_external_share_paths([
            OsString::from("file:///tmp/unterminated%"),
            OsString::from("file://"),
        ]);

        assert!(paths.is_empty());
    }
}
