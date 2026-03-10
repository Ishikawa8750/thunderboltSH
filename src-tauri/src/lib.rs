mod app_state;
mod clipboard;
mod commands;
mod error;
mod file_api;
mod kvm;
mod network;
mod screen_share;
mod sync_engine;

use app_state::SharedAppState;
use network::mdns::DiscoveredPeer;

fn cleanup_before_exit(state: &SharedAppState) {
    if let Some(handle) = state.0.api_server.blocking_lock().take() {
        handle.abort();
    }

    if let Some(mut child) = state.0.kvm_child.blocking_lock().take() {
        let _ = child.start_kill();
    }

    if let Some(mut child) = state.0.sunshine_child.blocking_lock().take() {
        let _ = child.start_kill();
    }

    if let Some(mut child) = state.0.moonlight_child.blocking_lock().take() {
        let _ = child.start_kill();
    }

    if let Some(runtime) = state.0.discovery.blocking_lock().take() {
        tauri::async_runtime::spawn(async move {
            runtime.shutdown().await;
        });
    }

    if let Some(runtime) = state.0.sync_runtime.blocking_lock().take() {
        tauri::async_runtime::spawn(async move {
            runtime.shutdown().await;
        });
    }

    if let Some(runtime) = state.0.clipboard_watcher.blocking_lock().take() {
        tauri::async_runtime::spawn(async move {
            runtime.shutdown().await;
        });
    }
}

async fn bootstrap_from_env(state: SharedAppState) {
    let local_ip = std::env::var("OPENBOLT_LOCAL_IP").ok();
    let peer_ip = std::env::var("OPENBOLT_PEER_IP").ok();
    let api_token = std::env::var("OPENBOLT_API_TOKEN").ok();
    let autostart_file_api = std::env::var("OPENBOLT_AUTOSTART_FILE_API")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    if let Some(local) = local_ip.clone() {
        *state.0.local_ip.write().await = Some(local);
    }

    if let Some(token) = api_token {
        *state.0.api_token.write().await = Some(token);
    }

    if let Some(peer) = peer_ip {
        let mut discovered = state.0.discovered.write().await;
        if !discovered.iter().any(|d| d.ip == peer) {
            discovered.push(DiscoveredPeer {
                hostname: "peer-manual".to_string(),
                os: "unknown".to_string(),
                ip: peer,
                api_port: 7733,
                kvm_port: 4242
            });
        }
    }

    if autostart_file_api {
        let bind_ip = state
            .0
            .local_ip
            .read()
            .await
            .clone()
            .unwrap_or_else(|| "10.99.99.2".to_string());

        let token = state.0.api_token.read().await.clone();
        match file_api::spawn_file_api_server(bind_ip.clone(), 7733, token).await {
            Ok(handle) => {
                *state.0.api_server.lock().await = Some(handle);
                tracing::info!("file api auto-started on {bind_ip}:7733");
            }
            Err(err) => {
                tracing::error!("file api auto-start failed: {err}");
            }
        }
    }
}

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter("openbolt=info")
        .with_target(false)
        .compact()
        .init();

    let state = SharedAppState::new();
    let setup_state = state.clone();

    let app = tauri::Builder::default()
        .manage(state.clone())
        .setup(move |app| {
            let boot_state = setup_state.clone();
            *boot_state.0.app_handle.blocking_write() = Some(app.handle().clone());
            tauri::async_runtime::spawn(async move {
                bootstrap_from_env(boot_state).await;
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_system_overview,
            commands::start_discovery,
            commands::stop_discovery,
            commands::start_kvm,
            commands::start_file_api,
            commands::start_folder_sync,
            commands::stop_folder_sync,
            commands::get_sync_logs,
            commands::clear_sync_logs,
            commands::get_sync_stats,
            commands::get_retry_queue_status,
            commands::clear_retry_queue,
            commands::get_retry_queue_items,
            commands::retry_queue_item,
            commands::remove_retry_queue_item,
            commands::preview_ignore_patterns,
            commands::get_sunshine_config_path,
            commands::configure_sunshine,
            commands::start_sunshine,
            commands::stop_sunshine,
            commands::launch_moonlight,
            commands::stop_moonlight,
            commands::get_clipboard_files,
            commands::send_clipboard_files,
            commands::write_file_to_clipboard,
            commands::start_clipboard_watcher,
            commands::stop_clipboard_watcher,
            commands::stop_all_services
        ])
        .build(tauri::generate_context!())
        .expect("error while building OpenBolt");

    let exit_state = state.clone();
    app.run(move |_app_handle, event| {
        match event {
            tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit => {
                cleanup_before_exit(&exit_state);
            }
            _ => {}
        }
    });
}
