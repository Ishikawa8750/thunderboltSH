use std::{collections::VecDeque, sync::Arc};

use tauri::AppHandle;
use tokio::{
    process::Child,
    sync::{Mutex, RwLock}
};

use crate::network::mdns::DiscoveredPeer;

#[derive(Default)]
pub struct AppState {
    pub local_ip: RwLock<Option<String>>,
    pub api_token: RwLock<Option<String>>,
    pub discovered: Arc<RwLock<Vec<DiscoveredPeer>>>,
    pub discovery: Mutex<Option<crate::network::mdns::DiscoveryRuntime>>,
    pub kvm_child: Mutex<Option<Child>>,
    pub sunshine_child: Mutex<Option<Child>>,
    pub moonlight_child: Mutex<Option<Child>>,
    pub api_server: Mutex<Option<tokio::task::JoinHandle<()>>>,
    pub sync_runtime: Mutex<Option<crate::sync_engine::SyncRuntime>>,
    pub sync_logs: Arc<RwLock<VecDeque<crate::sync_engine::SyncLogEntry>>>,
    pub app_handle: RwLock<Option<AppHandle>>,
    pub clipboard_watcher: Mutex<Option<crate::clipboard::ClipboardWatcherRuntime>>
}

#[derive(Clone)]
pub struct SharedAppState(pub Arc<AppState>);

impl SharedAppState {
    pub fn new() -> Self {
        Self(Arc::new(AppState::default()))
    }
}
