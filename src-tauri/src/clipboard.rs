use reqwest::multipart;
use tauri::{AppHandle, Emitter};
use tokio::{
    sync::oneshot,
    task::JoinHandle,
    time::Duration
};

#[cfg(target_os = "windows")]
use windows::Win32::{
    Foundation::{HGLOBAL, HWND},
    System::{
        DataExchange::{CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard},
        Memory::{GlobalLock, GlobalUnlock}
    },
    UI::Shell::DROPFILES
};

use crate::error::{OpenBoltError, OpenBoltResult};

pub fn default_clipboard_temp_dir() -> String {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());

    std::path::Path::new(&home)
        .join(".openbolt")
        .join("temp")
        .to_string_lossy()
        .to_string()
}

pub struct ClipboardWatcherRuntime {
    stop_tx: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>
}

impl ClipboardWatcherRuntime {
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        let _ = self.task.await;
    }
}

pub fn start_clipboard_watcher(
    peer_ip: Option<String>,
    dest_dir: Option<String>,
    api_token: Option<String>,
    app_handle: Option<AppHandle>
) -> ClipboardWatcherRuntime {
    let (stop_tx, mut stop_rx) = oneshot::channel::<()>();

    let task = tokio::spawn(async move {
        let mut last_files: Vec<String> = Vec::new();
        let mut tick = tokio::time::interval(Duration::from_secs(1));

        loop {
            tokio::select! {
                _ = &mut stop_rx => break,
                _ = tick.tick() => {
                    let current = read_clipboard_file_paths().await.unwrap_or_default();
                    if current != last_files {
                        if let Some(ref handle) = app_handle {
                            let _ = handle.emit("clipboard-change", &current);
                        }

                        if !current.is_empty() {
                            if let Some(ref ip) = peer_ip {
                                let default_dir = default_clipboard_temp_dir();
                                let target_dir = dest_dir
                                    .as_deref()
                                    .filter(|v| !v.trim().is_empty())
                                    .unwrap_or(default_dir.as_str());
                                let _ = upload_clipboard_files(
                                    ip,
                                    target_dir,
                                    api_token.as_deref(),
                                    &current
                                ).await;
                            }
                        }

                        last_files = current;
                    }
                }
            }
        }
    });

    ClipboardWatcherRuntime {
        stop_tx: Some(stop_tx),
        task
    }
}

/// Returns file paths currently in the system clipboard.
///
/// * Windows – reads `CF_HDROP` via `System.Windows.Forms.Clipboard.GetFileDropList()`.
/// * macOS – reads file URLs via `osascript`.
/// * Other platforms – returns an empty list without error.
pub async fn read_clipboard_file_paths() -> OpenBoltResult<Vec<String>> {
    #[cfg(target_os = "windows")]
    return read_clipboard_windows().await;

    #[cfg(target_os = "macos")]
    return read_clipboard_macos().await;

    #[allow(unreachable_code)]
    Ok(vec![])
}

#[cfg(target_os = "windows")]
async fn read_clipboard_windows() -> OpenBoltResult<Vec<String>> {
    tokio::task::spawn_blocking(read_clipboard_windows_native)
    .await
    .map_err(|e| OpenBoltError::CommandFailed(e.to_string()))?
}

#[cfg(target_os = "windows")]
fn read_clipboard_windows_native() -> OpenBoltResult<Vec<String>> {
    const CF_HDROP_FORMAT: u32 = 15;

    unsafe {
        if IsClipboardFormatAvailable(CF_HDROP_FORMAT).is_err() {
            return Ok(Vec::new());
        }

        if OpenClipboard(HWND(std::ptr::null_mut())).is_err() {
            return Err(OpenBoltError::CommandFailed("OpenClipboard failed".to_string()));
        }

        let mut output = Vec::new();
        if let Ok(handle) = GetClipboardData(CF_HDROP_FORMAT) {
            let mem = HGLOBAL(handle.0);
            let locked = GlobalLock(mem);
            if !locked.is_null() {
                let dropfiles = &*(locked as *const DROPFILES);
                let data_ptr = (locked as *const u8).add(dropfiles.pFiles as usize);

                if dropfiles.fWide.as_bool() {
                    let mut cur = data_ptr as *const u16;
                    loop {
                        let mut len = 0usize;
                        while *cur.add(len) != 0 {
                            len += 1;
                        }
                        if len == 0 {
                            break;
                        }
                        let slice = std::slice::from_raw_parts(cur, len);
                        output.push(String::from_utf16_lossy(slice));
                        cur = cur.add(len + 1);
                    }
                }

                let _ = GlobalUnlock(mem);
            }
        }

        let _ = CloseClipboard();
        Ok(output)
    }
}

#[cfg(target_os = "macos")]
async fn read_clipboard_macos() -> OpenBoltResult<Vec<String>> {
    tokio::task::spawn_blocking(|| {
        // Try to get a single file alias first; then try file list.
        let output = std::process::Command::new("osascript")
            .args([
                "-e", "try",
                "-e", "    POSIX path of (the clipboard as alias)",
                "-e", "on error",
                "-e", "    \"\"",
                "-e", "end try",
            ])
            .output()
            .map_err(|e| OpenBoltError::CommandFailed(e.to_string()))?;

        let text = String::from_utf8_lossy(&output.stdout);
        let paths: Vec<String> = text
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| l.starts_with('/'))
            .collect();

        Ok(paths)
    })
    .await
    .map_err(|e| OpenBoltError::CommandFailed(e.to_string()))?
}

/// Uploads a list of local file paths to the peer's `dest_dir` via the file API.
/// Returns the number of files successfully transferred.
pub async fn upload_clipboard_files(
    peer_ip: &str,
    dest_dir: &str,
    api_token: Option<&str>,
    paths: &[String],
) -> OpenBoltResult<usize> {
    let client = reqwest::Client::new();
    let mut sent = 0usize;

    for path_str in paths {
        let path = std::path::Path::new(path_str);
        if !path.is_file() {
            continue;
        }

        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "clipboard_file".to_string());

        let bytes = match tokio::fs::read(path).await {
            Ok(b) => b,
            Err(_) => continue,
        };

        let url = format!(
            "http://{peer_ip}:7733/api/fs/upload?path={}",
            urlencoding::encode(dest_dir)
        );

        let part = multipart::Part::bytes(bytes).file_name(file_name);
        let form = multipart::Form::new().part("file", part);

        let mut req = client.post(&url).multipart(form);
        if let Some(token) = api_token {
            req = req.header("x-openbolt-token", token);
        }

        if let Ok(resp) = req.send().await {
            if resp.status().is_success() {
                sent += 1;
            }
        }
    }

    Ok(sent)
}

/// macOS only: Places a local file on the system clipboard as a file alias,
/// so the peer's file can be pasted in Finder and other apps.
pub async fn write_file_to_local_clipboard(#[allow(unused_variables)] path: &str) -> OpenBoltResult<()> {
    #[cfg(target_os = "macos")]
    {
        let path = path.to_string();
        tokio::task::spawn_blocking(move || {
            std::process::Command::new("osascript")
                .args([
                    "-e",
                    &format!(r#"set the clipboard to (POSIX file "{path}" as alias)"#),
                ])
                .status()
                .map_err(|e| OpenBoltError::CommandFailed(e.to_string()))
        })
        .await
        .map_err(|e| OpenBoltError::CommandFailed(e.to_string()))??;
        return Ok(());
    }

    #[allow(unreachable_code)]
    Err(OpenBoltError::UnsupportedPlatform)
}
