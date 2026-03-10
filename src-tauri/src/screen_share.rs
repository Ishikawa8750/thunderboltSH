use std::path::PathBuf;

use tokio::process::Child;

use crate::error::{OpenBoltError, OpenBoltResult};

/// Resolves the Sunshine config file path on the current platform.
/// Returns `None` if Sunshine does not appear to be installed.
pub async fn sunshine_config_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let candidates: Vec<PathBuf> = vec![
            std::env::var("APPDATA")
                .ok()
                .map(|d| PathBuf::from(d).join("Sunshine").join("sunshine.conf"))
                .unwrap_or_default(),
            PathBuf::from(r"C:\Program Files\Sunshine\config\sunshine.conf"),
            PathBuf::from(r"C:\Program Files (x86)\Sunshine\config\sunshine.conf"),
        ];
        for c in candidates {
            if c.exists() {
                return Some(c);
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        let candidate = PathBuf::from(std::env::var("HOME").unwrap_or_default())
            .join(".config")
            .join("sunshine")
            .join("sunshine.conf");
        if candidate.exists() {
            return Some(candidate);
        }
        let brew = PathBuf::from("/opt/homebrew/etc/sunshine/sunshine.conf");
        if brew.exists() {
            return Some(brew);
        }
    }

    None
}

/// Patches key = value pairs in a Sunshine config string.
/// Existing keys are updated in-place; new keys are appended.
fn patch_config(content: &str, updates: &[(&str, &str)]) -> String {
    let mut lines: Vec<String> = content.lines().map(String::from).collect();

    'outer: for (key, value) in updates {
        let new_line = format!("{key} = {value}");
        let key_lower = key.to_ascii_lowercase();
        for line in &mut lines {
            let trimmed = line.trim_start().to_ascii_lowercase();
            if trimmed.starts_with(&format!("{key_lower} ="))
                || trimmed.starts_with(&format!("{key_lower}="))
            {
                *line = new_line.clone();
                continue 'outer;
            }
        }
        lines.push(new_line);
    }

    lines.join("\n")
}

/// Configures Sunshine to bind on `local_ip:47989` with minimal log level.
/// Returns an error if `sunshine.conf` cannot be located.
pub async fn configure_sunshine(local_ip: &str) -> OpenBoltResult<String> {
    let config_path = sunshine_config_path().await.ok_or_else(|| {
        OpenBoltError::CommandFailed(
            "sunshine.conf not found; verify Sunshine is installed".to_string(),
        )
    })?;

    let existing = tokio::fs::read_to_string(&config_path)
        .await
        .unwrap_or_default();

    let patched = patch_config(
        &existing,
        &[
            ("address", local_ip),
            ("port", "47989"),
            ("min_log_level", "1"),
        ],
    );

    tokio::fs::write(&config_path, patched).await?;

    Ok(config_path.to_string_lossy().to_string())
}

/// Spawns the Sunshine host process (Windows only).
pub async fn start_sunshine() -> OpenBoltResult<Child> {
    #[cfg(target_os = "windows")]
    {
        let candidates = [
            r"C:\Program Files\Sunshine\sunshine.exe",
            r"C:\Program Files (x86)\Sunshine\sunshine.exe",
        ];
        for exe in &candidates {
            if std::path::Path::new(exe).exists() {
                let child = tokio::process::Command::new(exe)
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .map_err(|e| OpenBoltError::CommandFailed(e.to_string()))?;
                return Ok(child);
            }
        }
        return Err(OpenBoltError::CommandFailed(
            "sunshine.exe not found; expected at C:\\Program Files\\Sunshine\\sunshine.exe"
                .to_string(),
        ));
    }

    #[allow(unreachable_code)]
    Err(OpenBoltError::UnsupportedPlatform)
}

/// Spawns the Moonlight stream client towards `peer_ip` (macOS only).
pub async fn launch_moonlight(#[allow(unused_variables)] peer_ip: &str) -> OpenBoltResult<Child> {
    #[cfg(target_os = "macos")]
    {
        let child = tokio::process::Command::new("moonlight")
            .args(["stream", peer_ip, "--audio-on-host", "--display-mode", "windowed"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                OpenBoltError::CommandFailed(format!(
                    "moonlight not found or failed to start: {e}"
                ))
            })?;
        return Ok(child);
    }

    #[allow(unreachable_code)]
    Err(OpenBoltError::UnsupportedPlatform)
}
