use serde::Serialize;

use crate::{
    app_state::SharedAppState,
    clipboard,
    error::OpenBoltError,
    file_api,
    kvm::{self, KvmMode},
    network,
    screen_share,
    sync_engine::{RetryQueueItem, RetryQueueStatus, SyncDirection, SyncLogEntry}
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemOverview {
    pub app_version: String,
    pub platform: String,
    pub local_ip: Option<String>,
    pub discovered: Vec<network::mdns::DiscoveredPeer>,
    pub sync_running: bool,
    pub api_auth_enabled: bool
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStats {
    pub total: usize,
    pub success: usize,
    pub failed: usize,
    pub conflicts: usize,
    pub retries: usize
}

#[tauri::command]
pub async fn get_system_overview(
    state: tauri::State<'_, SharedAppState>
) -> Result<SystemOverview, String> {
    let local_ip = state.0.local_ip.read().await.clone();
    let discovered = state.0.discovered.read().await.clone();
    let sync_running = state.0.sync_runtime.lock().await.is_some();
    let api_auth_enabled = state.0.api_token.read().await.is_some();

    Ok(SystemOverview {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        platform: std::env::consts::OS.to_string(),
        local_ip,
        discovered,
        sync_running,
        api_auth_enabled
    })
}

#[tauri::command]
pub async fn start_discovery(state: tauri::State<'_, SharedAppState>) -> Result<(), String> {
    let local_ip = network::ip_config::ensure_local_link_ip().await.map_err(to_err)?;
    *state.0.local_ip.write().await = Some(local_ip.clone());

    let mut discovery_guard = state.0.discovery.lock().await;
    if discovery_guard.is_some() {
        return Err(OpenBoltError::AlreadyRunning("mdns").to_string());
    }

    let app_handle = state.0.app_handle.read().await.clone();
    let runtime = network::mdns::start_discovery(local_ip, state.0.discovered.clone(), app_handle)
        .await
        .map_err(to_err)?;
    *discovery_guard = Some(runtime);
    Ok(())
}

#[tauri::command]
pub async fn stop_discovery(state: tauri::State<'_, SharedAppState>) -> Result<(), String> {
    let mut discovery_guard = state.0.discovery.lock().await;
    if let Some(runtime) = discovery_guard.take() {
        runtime.shutdown().await;
    }
    // Clear discovered peers so stale entries don't linger.
    state.0.discovered.write().await.clear();
    Ok(())
}

#[tauri::command]
pub async fn start_kvm(
    mode: String,
    peer_ip: String,
    state: tauri::State<'_, SharedAppState>
) -> Result<(), String> {
    let local_ip = state
        .0
        .local_ip
        .read()
        .await
        .clone()
        .unwrap_or_else(|| "10.99.99.2".to_string());

    let kvm_mode = match mode.as_str() {
        "server" => KvmMode::Server,
        "client" => KvmMode::Client,
        _ => return Err("mode must be server|client".to_string())
    };

    let child = kvm::start(kvm_mode, &local_ip, &peer_ip).await.map_err(to_err)?;
    let mut guard = state.0.kvm_child.lock().await;
    if let Some(mut old) = guard.take() {
        let _ = old.kill().await;
    }
    *guard = Some(child);
    Ok(())
}

#[tauri::command]
pub async fn start_file_api(
    bind_ip: String,
    state: tauri::State<'_, SharedAppState>
) -> Result<(), String> {
    let mut guard = state.0.api_server.lock().await;
    if guard.is_some() {
        return Err(OpenBoltError::AlreadyRunning("file_api").to_string());
    }

    let token = state.0.api_token.read().await.clone();
    let handle = file_api::spawn_file_api_server(bind_ip, 7733, token)
        .await
        .map_err(to_err)?;
    *guard = Some(handle);
    Ok(())
}

#[tauri::command]
pub async fn start_folder_sync(
    local_dir: String,
    remote_dir: String,
    peer_ip: String,
    direction: Option<String>,
    ignore_patterns: Option<Vec<String>>,
    state: tauri::State<'_, SharedAppState>
) -> Result<(), String> {
    let mut guard = state.0.sync_runtime.lock().await;
    if guard.is_some() {
        return Err(OpenBoltError::AlreadyRunning("file_sync").to_string());
    }

    state.0.sync_logs.write().await.clear();

    let token = state.0.api_token.read().await.clone();
    let sync_direction = match direction.as_deref() {
        Some("bidirectional") => SyncDirection::Bidirectional,
        _ => SyncDirection::Outbound
    };

    let app_handle = state.0.app_handle.read().await.clone();
    let runtime = crate::sync_engine::start(
        local_dir,
        remote_dir,
        peer_ip,
        state.0.sync_logs.clone(),
        token,
        sync_direction,
        ignore_patterns.unwrap_or_default(),
        app_handle
    )
        .await
        .map_err(to_err)?;
    *guard = Some(runtime);
    Ok(())
}

#[tauri::command]
pub async fn stop_folder_sync(state: tauri::State<'_, SharedAppState>) -> Result<(), String> {
    let mut guard = state.0.sync_runtime.lock().await;
    if let Some(runtime) = guard.take() {
        runtime.shutdown().await;
    }
    Ok(())
}

#[tauri::command]
pub async fn get_sync_logs(
    state: tauri::State<'_, SharedAppState>,
    limit: Option<usize>
) -> Result<Vec<SyncLogEntry>, String> {
    let limit = limit.unwrap_or(100).min(500);
    let logs = state.0.sync_logs.read().await;
    let start = logs.len().saturating_sub(limit);
    Ok(logs.iter().skip(start).cloned().collect())
}

#[tauri::command]
pub async fn clear_sync_logs(state: tauri::State<'_, SharedAppState>) -> Result<(), String> {
    state.0.sync_logs.write().await.clear();
    Ok(())
}

#[tauri::command]
pub async fn get_sync_stats(state: tauri::State<'_, SharedAppState>) -> Result<SyncStats, String> {
    let logs = state.0.sync_logs.read().await;

    let mut success = 0usize;
    let mut failed = 0usize;
    let mut conflicts = 0usize;
    let mut retries = 0usize;

    for item in logs.iter() {
        if item.level.eq_ignore_ascii_case("warn") {
            failed += 1;
        }
        if item.level.eq_ignore_ascii_case("info") {
            success += 1;
        }

        let msg = item.message.to_ascii_lowercase();
        if msg.contains("conflict keep remote") {
            conflicts += 1;
        }
        if msg.contains("attempt") {
            retries += 1;
        }
    }

    Ok(SyncStats {
        total: logs.len(),
        success,
        failed,
        conflicts,
        retries
    })
}

#[tauri::command]
pub async fn get_retry_queue_status(local_dir: String) -> Result<RetryQueueStatus, String> {
    crate::sync_engine::get_retry_queue_status(local_dir)
        .await
        .map_err(to_err)
}

#[tauri::command]
pub async fn clear_retry_queue(local_dir: String) -> Result<(), String> {
    crate::sync_engine::clear_retry_queue(local_dir)
        .await
        .map_err(to_err)
}

#[tauri::command]
pub async fn get_retry_queue_items(
    local_dir: String,
    limit: Option<usize>,
    kind_filter: Option<String>,
    min_attempts: Option<u32>
) -> Result<Vec<RetryQueueItem>, String> {
    crate::sync_engine::get_retry_queue_items(local_dir, limit.unwrap_or(50), kind_filter, min_attempts)
        .await
        .map_err(to_err)
}

#[tauri::command]
pub async fn retry_queue_item(local_dir: String, item_id: u64) -> Result<(), String> {
    crate::sync_engine::retry_queue_item(local_dir, item_id)
        .await
        .map_err(to_err)
}

#[tauri::command]
pub async fn remove_retry_queue_item(local_dir: String, item_id: u64) -> Result<(), String> {
    crate::sync_engine::remove_retry_queue_item(local_dir, item_id)
        .await
        .map_err(to_err)
}

#[tauri::command]
pub async fn preview_ignore_patterns(
    local_dir: String,
    patterns: Vec<String>
) -> Result<Vec<String>, String> {
    crate::sync_engine::preview_ignore_patterns(local_dir, patterns)
        .await
        .map_err(to_err)
}

// ─── Screen Sharing ───────────────────────────────────────────────────────────

#[tauri::command]
pub async fn get_sunshine_config_path() -> Result<Option<String>, String> {
    Ok(screen_share::sunshine_config_path()
        .await
        .map(|p| p.to_string_lossy().to_string()))
}

#[tauri::command]
pub async fn configure_sunshine(
    local_ip: String,
    state: tauri::State<'_, SharedAppState>
) -> Result<String, String> {
    let ip = if local_ip.is_empty() {
        state
            .0
            .local_ip
            .read()
            .await
            .clone()
            .unwrap_or_else(|| "10.99.99.2".to_string())
    } else {
        local_ip
    };
    screen_share::configure_sunshine(&ip).await.map_err(to_err)
}

#[tauri::command]
pub async fn start_sunshine(state: tauri::State<'_, SharedAppState>) -> Result<(), String> {
    let mut guard = state.0.sunshine_child.lock().await;
    if guard.is_some() {
        return Err(OpenBoltError::AlreadyRunning("sunshine").to_string());
    }
    let child = screen_share::start_sunshine().await.map_err(to_err)?;
    *guard = Some(child);
    Ok(())
}

#[tauri::command]
pub async fn stop_sunshine(state: tauri::State<'_, SharedAppState>) -> Result<(), String> {
    if let Some(mut child) = state.0.sunshine_child.lock().await.take() {
        let _ = child.kill().await;
    }
    Ok(())
}

#[tauri::command]
pub async fn launch_moonlight(
    peer_ip: String,
    state: tauri::State<'_, SharedAppState>
) -> Result<(), String> {
    let mut guard = state.0.moonlight_child.lock().await;
    if guard.is_some() {
        return Err(OpenBoltError::AlreadyRunning("moonlight").to_string());
    }
    let child = screen_share::launch_moonlight(&peer_ip).await.map_err(to_err)?;
    *guard = Some(child);
    Ok(())
}

#[tauri::command]
pub async fn stop_moonlight(state: tauri::State<'_, SharedAppState>) -> Result<(), String> {
    if let Some(mut child) = state.0.moonlight_child.lock().await.take() {
        let _ = child.kill().await;
    }
    Ok(())
}

// ─── Clipboard ────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn get_clipboard_files() -> Result<Vec<String>, String> {
    clipboard::read_clipboard_file_paths().await.map_err(to_err)
}

#[tauri::command]
pub async fn send_clipboard_files(
    peer_ip: String,
    dest_dir: Option<String>,
    state: tauri::State<'_, SharedAppState>
) -> Result<usize, String> {
    let token = state.0.api_token.read().await.clone();
    let files = clipboard::read_clipboard_file_paths().await.map_err(to_err)?;
    if files.is_empty() {
        return Ok(0);
    }
    let dir = dest_dir
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(clipboard::default_clipboard_temp_dir);
    clipboard::upload_clipboard_files(&peer_ip, &dir, token.as_deref(), &files)
        .await
        .map_err(to_err)
}

#[tauri::command]
pub async fn write_file_to_clipboard(path: String) -> Result<(), String> {
    clipboard::write_file_to_local_clipboard(&path)
        .await
        .map_err(to_err)
}

#[tauri::command]
pub async fn start_clipboard_watcher(
    peer_ip: String,
    dest_dir: Option<String>,
    state: tauri::State<'_, SharedAppState>
) -> Result<(), String> {
    let mut guard = state.0.clipboard_watcher.lock().await;
    if guard.is_some() {
        return Err(OpenBoltError::AlreadyRunning("clipboard_watcher").to_string());
    }

    let token = state.0.api_token.read().await.clone();
    let app_handle = state.0.app_handle.read().await.clone();
    let peer = if peer_ip.trim().is_empty() {
        None
    } else {
        Some(peer_ip)
    };
    let runtime = clipboard::start_clipboard_watcher(
        peer,
        dest_dir,
        token,
        app_handle
    );
    *guard = Some(runtime);
    Ok(())
}

#[tauri::command]
pub async fn stop_clipboard_watcher(state: tauri::State<'_, SharedAppState>) -> Result<(), String> {
    let mut guard = state.0.clipboard_watcher.lock().await;
    if let Some(runtime) = guard.take() {
        runtime.shutdown().await;
    }
    Ok(())
}

#[tauri::command]
pub async fn stop_all_services(state: tauri::State<'_, SharedAppState>) -> Result<(), String> {
    shutdown_all_services(&state).await;
    Ok(())
}

pub async fn shutdown_all_services(state: &SharedAppState) {
    if let Some(runtime) = state.0.discovery.lock().await.take() {
        runtime.shutdown().await;
    }

    if let Some(runtime) = state.0.sync_runtime.lock().await.take() {
        runtime.shutdown().await;
    }

    if let Some(mut child) = state.0.kvm_child.lock().await.take() {
        let _ = child.kill().await;
    }

    if let Some(mut child) = state.0.sunshine_child.lock().await.take() {
        let _ = child.kill().await;
    }

    if let Some(mut child) = state.0.moonlight_child.lock().await.take() {
        let _ = child.kill().await;
    }

    if let Some(handle) = state.0.api_server.lock().await.take() {
        handle.abort();
    }

    if let Some(runtime) = state.0.clipboard_watcher.lock().await.take() {
        runtime.shutdown().await;
    }
}

fn to_err(err: OpenBoltError) -> String {
    err.to_string()
}
