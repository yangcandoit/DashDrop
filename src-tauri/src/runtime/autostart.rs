use std::path::{Path, PathBuf};

use tauri::{AppHandle, Runtime};

pub fn sync_launch_at_login<R: Runtime>(app: &AppHandle<R>, enabled: bool) -> Result<(), String> {
    let identifier = app.config().identifier.clone();
    let executable = std::env::current_exe()
        .map_err(|err| format!("failed to resolve current executable: {err}"))?;
    sync_launch_at_login_for_executable(&identifier, &executable, enabled)
}

fn sync_launch_at_login_for_executable(
    identifier: &str,
    executable: &Path,
    enabled: bool,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        sync_macos_launch_agent(identifier, executable, enabled)
    }
    #[cfg(target_os = "linux")]
    {
        sync_linux_autostart_entry(identifier, executable, enabled)
    }
    #[cfg(target_os = "windows")]
    {
        sync_windows_run_key(identifier, executable, enabled)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        let _ = (identifier, executable, enabled);
        Err("launch at login is not supported on this platform".to_string())
    }
}

fn ensure_parent_dir(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            format!(
                "failed to create startup directory {}: {err}",
                parent.display()
            )
        })?;
    }
    Ok(())
}

fn user_home_dir() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set".to_string())
}

fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(any(target_os = "linux", test))]
fn desktop_exec_escape(path: &Path) -> String {
    let value = path.to_string_lossy();
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

#[cfg(any(target_os = "windows", test))]
fn windows_command_escape(path: &Path) -> String {
    format!("\"{}\"", path.to_string_lossy())
}

#[cfg(target_os = "macos")]
fn launch_agent_path(identifier: &str) -> Result<PathBuf, String> {
    Ok(user_home_dir()?
        .join("Library/LaunchAgents")
        .join(format!("{identifier}.plist")))
}

#[cfg(target_os = "macos")]
fn render_launch_agent_plist(identifier: &str, executable: &Path) -> String {
    let exec = xml_escape(&executable.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
  <dict>
    <key>Label</key>
    <string>{identifier}</string>
    <key>ProgramArguments</key>
    <array>
      <string>{exec}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <false/>
  </dict>
</plist>
"#
    )
}

#[cfg(target_os = "macos")]
fn sync_macos_launch_agent(
    identifier: &str,
    executable: &Path,
    enabled: bool,
) -> Result<(), String> {
    let path = launch_agent_path(identifier)?;
    if enabled {
        ensure_parent_dir(&path)?;
        std::fs::write(&path, render_launch_agent_plist(identifier, executable))
            .map_err(|err| format!("failed to write launch agent {}: {err}", path.display()))?;
    } else if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|err| format!("failed to remove launch agent {}: {err}", path.display()))?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn xdg_autostart_path(identifier: &str) -> Result<PathBuf, String> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or(user_home_dir()?.join(".config"));
    Ok(base.join("autostart").join(format!("{identifier}.desktop")))
}

#[cfg(target_os = "linux")]
fn render_desktop_entry(identifier: &str, executable: &Path) -> String {
    let exec = desktop_exec_escape(executable);
    format!(
        "[Desktop Entry]\nType=Application\nVersion=1.0\nName=DashDrop\nComment=Start DashDrop automatically when you sign in\nExec={exec}\nTerminal=false\nHidden=false\nX-GNOME-Autostart-enabled=true\nStartupNotify=false\nX-DashDrop-Identifier={identifier}\n"
    )
}

#[cfg(target_os = "linux")]
fn sync_linux_autostart_entry(
    identifier: &str,
    executable: &Path,
    enabled: bool,
) -> Result<(), String> {
    let path = xdg_autostart_path(identifier)?;
    if enabled {
        ensure_parent_dir(&path)?;
        std::fs::write(&path, render_desktop_entry(identifier, executable)).map_err(|err| {
            format!(
                "failed to write autostart desktop entry {}: {err}",
                path.display()
            )
        })?;
    } else if path.exists() {
        std::fs::remove_file(&path).map_err(|err| {
            format!(
                "failed to remove autostart desktop entry {}: {err}",
                path.display()
            )
        })?;
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn sync_windows_run_key(identifier: &str, executable: &Path, enabled: bool) -> Result<(), String> {
    use std::ptr;
    use windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
        KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
    };

    fn to_wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    unsafe {
        let mut key: HKEY = ptr::null_mut();
        let subkey = to_wide("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
        let status = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            ptr::null_mut(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            ptr::null(),
            &mut key,
            ptr::null_mut(),
        );
        if status != 0 {
            return Err(format!("failed to open startup registry key: {status}"));
        }

        let value_name = to_wide(identifier);
        let result = if enabled {
            let command = to_wide(&windows_command_escape(executable));
            RegSetValueExW(
                key,
                value_name.as_ptr(),
                0,
                REG_SZ,
                command.as_ptr() as *const u8,
                (command.len() * std::mem::size_of::<u16>()) as u32,
            )
        } else {
            let delete_status = RegDeleteValueW(key, value_name.as_ptr());
            if delete_status == ERROR_FILE_NOT_FOUND {
                0
            } else {
                delete_status
            }
        };
        RegCloseKey(key);

        if result != 0 {
            return Err(if enabled {
                format!("failed to update startup registry value: {result}")
            } else {
                format!("failed to remove startup registry value: {result}")
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{desktop_exec_escape, windows_command_escape, xml_escape};
    use std::path::Path;

    #[test]
    fn xml_escape_handles_common_entities() {
        assert_eq!(xml_escape("A&B\"<'"), "A&amp;B&quot;&lt;&apos;");
    }

    #[test]
    fn desktop_exec_escape_wraps_and_escapes_path() {
        let escaped = desktop_exec_escape(Path::new("/tmp/Dash Drop/app\\bin"));
        assert_eq!(escaped, "\"/tmp/Dash Drop/app\\\\bin\"");
    }

    #[test]
    fn windows_command_escape_wraps_path() {
        let escaped = windows_command_escape(Path::new(r"C:\Program Files\DashDrop\dashdrop.exe"));
        assert_eq!(escaped, r#""C:\Program Files\DashDrop\dashdrop.exe""#);
    }
}
