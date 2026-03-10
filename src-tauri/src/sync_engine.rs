use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
    time::Duration
};

use globset::{Glob, GlobSet, GlobSetBuilder};
use notify::{
    event::{ModifyKind, RenameMode},
    Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher
};
use reqwest::{multipart, Client};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, oneshot, RwLock};
use tokio_util::io::ReaderStream;

use crate::error::{OpenBoltError, OpenBoltResult};

pub type SyncLogBuffer = Arc<RwLock<VecDeque<SyncLogEntry>>>;
type RemoteOriginCache = Arc<RwLock<HashMap<String, u64>>>;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncDirection {
    Outbound,
    Bidirectional
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncLogEntry {
    pub ts: u64,
    pub level: String,
    pub action: String,
    pub path: String,
    pub message: String
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryQueueStatus {
    pub pending: usize,
    pub store_path: String
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryQueueItem {
    pub id: u64,
    pub attempts: u32,
    pub kind: String,
    pub target: String
}

const DEDUPE_WINDOW_SECS: u64 = 3;
const RETRY_TICK_SECS: u64 = 5;
const INBOUND_POLL_SECS: u64 = 30;
const ORIGIN_SUPPRESS_SECS: u64 = 10;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RetryTask {
    #[serde(default)]
    id: u64,
    attempts: u32,
    action: RetryAction
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum RetryAction {
    Upsert {
        local_path: String,
        remote_parent: String,
        file_name: String,
        peer_ip: String,
        api_token: Option<String>
    },
    Delete {
        remote_path: String,
        peer_ip: String,
        api_token: Option<String>
    },
    Move {
        from: String,
        to: String,
        peer_ip: String,
        api_token: Option<String>
    }
}

pub struct SyncRuntime {
    stop_tx: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>
}

impl SyncRuntime {
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        let _ = self.task.await;
    }
}

#[derive(Deserialize)]
struct RemoteStat {
    exists: bool,
    is_dir: bool,
    size: Option<u64>,
    mtime: Option<u64>
}

#[derive(Deserialize)]
struct RemoteFsEntry {
    name: String,
    is_dir: bool,
    #[allow(dead_code)]
    size: u64,
    mtime: u64
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SyncAction {
    Upsert,
    Delete
}

struct SyncEvent {
    path: PathBuf,
    action: SyncAction
}

struct RenameEvent {
    from: PathBuf,
    to: PathBuf
}

pub async fn start(
    local_dir: String,
    remote_base: String,
    peer_ip: String,
    log_buffer: SyncLogBuffer,
    api_token: Option<String>,
    direction: SyncDirection,
    ignore_patterns: Vec<String>,
    app_handle: Option<AppHandle>
) -> OpenBoltResult<SyncRuntime> {
    let local_root = PathBuf::from(local_dir);
    if !local_root.exists() {
        return Err(OpenBoltError::CommandFailed("local sync directory does not exist".to_string()));
    }

    append_log(
        &log_buffer,
        "info",
        "start",
        local_root.to_string_lossy().as_ref(),
        &format!(
            "sync started to {peer_ip}:{remote_base} direction={:?} ignore_patterns={}"
            , direction, ignore_patterns.len()
        ),
        app_handle.as_ref()
    )
    .await;

    if matches!(direction, SyncDirection::Bidirectional) {
        append_log(
            &log_buffer,
            "info",
            "mode",
            local_root.to_string_lossy().as_ref(),
            &format!("bidirectional mode: outbound watcher + inbound pull every {INBOUND_POLL_SECS}s"),
            app_handle.as_ref()
        )
        .await;
    }

    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<SyncEvent>();
    let (rename_tx, mut rename_rx) = mpsc::unbounded_channel::<RenameEvent>();
    let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
        if let Ok(event) = res {
            if let EventKind::Modify(ModifyKind::Name(RenameMode::Both)) = event.kind {
                if event.paths.len() >= 2 {
                    let _ = rename_tx.send(RenameEvent {
                        from: event.paths[0].clone(),
                        to: event.paths[1].clone()
                    });
                    return;
                }
            }

            let action = map_event_action(&event.kind);
            for path in event.paths {
                let _ = event_tx.send(SyncEvent { path, action });
            }
        }
    })
    .map_err(|e| OpenBoltError::CommandFailed(e.to_string()))?;

    watcher
        .watch(&local_root, RecursiveMode::Recursive)
        .map_err(|e| OpenBoltError::CommandFailed(e.to_string()))?;

    let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
    let client = Client::new();
    let retry_store_path = retry_store_path(&local_root);
    let initial_retry_queue = load_retry_queue(&retry_store_path).await;

    let task = tokio::spawn(async move {
        let _watcher_guard = watcher;
        let log_buffer = log_buffer;
        let retry_store_path = retry_store_path;
        let mut retry_queue = initial_retry_queue;
        let mut dedupe_cache: HashMap<String, (SyncAction, u64)> = HashMap::new();
        let mut retry_tick = tokio::time::interval(Duration::from_secs(RETRY_TICK_SECS));
        let mut inbound_tick = tokio::time::interval_at(
            tokio::time::Instant::now() + Duration::from_secs(INBOUND_POLL_SECS),
            Duration::from_secs(INBOUND_POLL_SECS)
        );
        let origin_cache: RemoteOriginCache = Arc::new(RwLock::new(HashMap::new()));
        let ignore_patterns = ignore_patterns;
        let ignore_glob = build_globset(&ignore_patterns);

        loop {
            tokio::select! {
                _ = &mut stop_rx => {
                    let _ = save_retry_queue(&retry_store_path, &retry_queue).await;
                    break;
                }
                _ = retry_tick.tick() => {
                    if !retry_queue.is_empty() {
                        process_retry_queue(
                            &client,
                            &mut retry_queue,
                            &retry_store_path,
                            &log_buffer,
                            app_handle.as_ref()
                        ).await;
                    }
                }
                _ = inbound_tick.tick() => {
                    if matches!(direction, SyncDirection::Bidirectional) {
                        inbound_pull_cycle(
                            &client,
                            &peer_ip,
                            api_token.as_deref(),
                            &remote_base,
                            &local_root,
                            &ignore_patterns,
                            ignore_glob.as_ref(),
                            &origin_cache,
                            &log_buffer,
                            app_handle.as_ref()
                        ).await;
                    }
                }
                first = event_rx.recv() => {
                    let Some(first_event) = first else { continue; };
                    let mut pending = HashMap::<PathBuf, SyncAction>::new();
                    let mut pending_renames = Vec::<RenameEvent>::new();
                    pending.insert(first_event.path, first_event.action);

                    loop {
                        let timed_event = tokio::time::timeout(Duration::from_millis(800), event_rx.recv());
                        let timed_rename = tokio::time::timeout(Duration::from_millis(800), rename_rx.recv());
                        tokio::select! {
                            next = timed_event => {
                                match next {
                                    Ok(Some(item)) => {
                                        pending
                                            .entry(item.path)
                                            .and_modify(|action| {
                                                if item.action == SyncAction::Upsert {
                                                    *action = SyncAction::Upsert;
                                                }
                                            })
                                            .or_insert(item.action);
                                    }
                                    Ok(None) => break,
                                    Err(_) => break
                                }
                            }
                            rename = timed_rename => {
                                if let Ok(Some(item)) = rename {
                                    pending_renames.push(item);
                                }
                            }
                        }
                    }

                    for rename in pending_renames {
                        if should_ignore_path(&local_root, &rename.to, &ignore_patterns, ignore_glob.as_ref())
                            || should_ignore_path(&local_root, &rename.from, &ignore_patterns, ignore_glob.as_ref())
                        {
                            continue;
                        }

                        if let Err(err) = sync_rename_path(&client, &local_root, &remote_base, &peer_ip, api_token.as_deref(), &rename).await {
                            tracing::warn!(target: "openbolt::sync", "rename sync failed from {:?} to {:?}: {}", rename.from, rename.to, err);
                            if let Some(task) = build_retry_rename_task(&local_root, &remote_base, &peer_ip, api_token.clone(), &rename) {
                                retry_queue.push_back(task);
                                let _ = save_retry_queue(&retry_store_path, &retry_queue).await;
                            }
                            append_log(
                                &log_buffer,
                                "warn",
                                "rename",
                                rename.to.to_string_lossy().as_ref(),
                                &format!("failed: {err}"),
                                app_handle.as_ref()
                            )
                            .await;
                        } else {
                            append_log(
                                &log_buffer,
                                "info",
                                "rename",
                                rename.to.to_string_lossy().as_ref(),
                                "ok",
                                app_handle.as_ref()
                            )
                            .await;
                        }
                    }

                    for (path, action) in pending {
                        if should_ignore_path(&local_root, &path, &ignore_patterns, ignore_glob.as_ref()) {
                            continue;
                        }

                        if is_remote_origin(&origin_cache, &path).await {
                            continue;
                        }

                        if should_skip_dedup(&mut dedupe_cache, &path, action) {
                            continue;
                        }

                        if let Err(err) = sync_one_path(&client, &local_root, &remote_base, &peer_ip, api_token.as_deref(), &path, action).await {
                            tracing::warn!(target: "openbolt::sync", "sync failed for {:?}: {}", path, err);
                            if let Some(task) = build_retry_task(&local_root, &remote_base, &peer_ip, api_token.clone(), &path, action) {
                                retry_queue.push_back(task);
                                let _ = save_retry_queue(&retry_store_path, &retry_queue).await;
                            }
                            append_log(
                                &log_buffer,
                                "warn",
                                if action == SyncAction::Delete { "delete" } else { "upsert" },
                                path.to_string_lossy().as_ref(),
                                &format!("failed: {err}"),
                                app_handle.as_ref()
                            )
                            .await;
                        } else {
                            append_log(
                                &log_buffer,
                                "info",
                                if action == SyncAction::Delete { "delete" } else { "upsert" },
                                path.to_string_lossy().as_ref(),
                                "ok",
                                app_handle.as_ref()
                            )
                            .await;
                        }
                    }
                }
            }
        }
    });

    Ok(SyncRuntime {
        stop_tx: Some(stop_tx),
        task
    })
}

async fn sync_one_path(
    client: &Client,
    local_root: &Path,
    remote_base: &str,
    peer_ip: &str,
    api_token: Option<&str>,
    path: &Path,
    action: SyncAction
) -> OpenBoltResult<()> {
    if !path.starts_with(local_root) {
        return Ok(());
    }

    let relative = path
        .strip_prefix(local_root)
        .map_err(|e| OpenBoltError::CommandFailed(e.to_string()))?;

    let remote_file_path = join_remote_path(remote_base, relative);
    let remote_parent = Path::new(&remote_file_path)
        .parent()
        .map(|v| v.to_string_lossy().to_string())
        .unwrap_or_else(|| remote_base.to_string());

    if action == SyncAction::Delete || !path.exists() {
        return delete_remote_path(client, peer_ip, api_token, &remote_file_path).await;
    }

    if !path.is_file() {
        return Ok(());
    }

    let local_meta = tokio::fs::metadata(path).await?;
    let local_size = local_meta.len();
    let local_mtime = local_meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or_default();

    let should_upload = match fetch_remote_stat(client, peer_ip, api_token, &remote_file_path).await {
        Ok(stat) => {
            if stat.exists && !stat.is_dir {
                let remote_size = stat.size.unwrap_or_default();
                let remote_mtime = stat.mtime.unwrap_or_default();
                if remote_mtime > local_mtime {
                    tracing::warn!(
                        target: "openbolt::sync",
                        "conflict keep remote: local={:?} remote_path={} local_mtime={} remote_mtime={}",
                        path,
                        remote_file_path,
                        local_mtime,
                        remote_mtime
                    );
                    false
                } else {
                    local_size != remote_size || local_mtime > remote_mtime
                }
            } else {
                true
            }
        }
        Err(_) => true
    };

    if !should_upload {
        return Ok(());
    }

    let file_name = path
        .file_name()
        .map(|v| v.to_string_lossy().to_string())
        .unwrap_or_else(|| "sync.bin".to_string());

    let url = format!(
        "http://{peer_ip}:7733/api/fs/upload?path={}",
        urlencoding::encode(&remote_parent)
    );

    send_upload_with_retry(client, &url, api_token, path, &file_name).await?;

    Ok(())
}

async fn sync_rename_path(
    client: &Client,
    local_root: &Path,
    remote_base: &str,
    peer_ip: &str,
    api_token: Option<&str>,
    rename: &RenameEvent
) -> OpenBoltResult<()> {
    if !rename.from.starts_with(local_root) || !rename.to.starts_with(local_root) {
        return Ok(());
    }

    let from_rel = rename
        .from
        .strip_prefix(local_root)
        .map_err(|e| OpenBoltError::CommandFailed(e.to_string()))?;
    let to_rel = rename
        .to
        .strip_prefix(local_root)
        .map_err(|e| OpenBoltError::CommandFailed(e.to_string()))?;

    let remote_from = join_remote_path(remote_base, from_rel);
    let remote_to = join_remote_path(remote_base, to_rel);
    move_remote_path(client, peer_ip, api_token, &remote_from, &remote_to).await
}

async fn fetch_remote_stat(
    client: &Client,
    peer_ip: &str,
    api_token: Option<&str>,
    remote_file_path: &str
) -> OpenBoltResult<RemoteStat> {
    let url = format!(
        "http://{peer_ip}:7733/api/fs/stat?path={}",
        urlencoding::encode(remote_file_path)
    );

    let mut last_err: Option<OpenBoltError> = None;
    for attempt in 1..=3 {
        match with_token(client.get(&url), api_token).send().await {
            Ok(response) if response.status().is_success() => {
                return response
                    .json::<RemoteStat>()
                    .await
                    .map_err(|e| OpenBoltError::CommandFailed(e.to_string()));
            }
            Ok(response) => {
                last_err = Some(OpenBoltError::CommandFailed(format!(
                    "stat failed (attempt {attempt}): {}",
                    response.status()
                )));
            }
            Err(err) => {
                last_err = Some(OpenBoltError::CommandFailed(format!(
                    "stat request failed (attempt {attempt}): {err}"
                )));
            }
        }

        tokio::time::sleep(Duration::from_millis(200 * attempt)).await;
    }

    Err(last_err.unwrap_or_else(|| OpenBoltError::CommandFailed("stat failed".to_string())))
}

async fn delete_remote_path(
    client: &Client,
    peer_ip: &str,
    api_token: Option<&str>,
    remote_file_path: &str
) -> OpenBoltResult<()> {
    let url = format!(
        "http://{peer_ip}:7733/api/fs/delete?path={}",
        urlencoding::encode(remote_file_path)
    );

    let mut last_err: Option<OpenBoltError> = None;
    for attempt in 1..=3 {
        match with_token(client.post(&url), api_token).send().await {
            Ok(response) if response.status().is_success() => return Ok(()),
            Ok(response) => {
                last_err = Some(OpenBoltError::CommandFailed(format!(
                    "delete failed (attempt {attempt}): {}",
                    response.status()
                )));
            }
            Err(err) => {
                last_err = Some(OpenBoltError::CommandFailed(format!(
                    "delete request failed (attempt {attempt}): {err}"
                )));
            }
        }

        tokio::time::sleep(Duration::from_millis(200 * attempt)).await;
    }

    Err(last_err.unwrap_or_else(|| OpenBoltError::CommandFailed("delete failed".to_string())))
}

async fn move_remote_path(
    client: &Client,
    peer_ip: &str,
    api_token: Option<&str>,
    from: &str,
    to: &str
) -> OpenBoltResult<()> {
    let url = format!(
        "http://{peer_ip}:7733/api/fs/move?from={}&to={}",
        urlencoding::encode(from),
        urlencoding::encode(to)
    );

    let mut last_err: Option<OpenBoltError> = None;
    for attempt in 1..=3 {
        match with_token(client.post(&url), api_token).send().await {
            Ok(response) if response.status().is_success() => return Ok(()),
            Ok(response) => {
                last_err = Some(OpenBoltError::CommandFailed(format!(
                    "move failed (attempt {attempt}): {}",
                    response.status()
                )));
            }
            Err(err) => {
                last_err = Some(OpenBoltError::CommandFailed(format!(
                    "move request failed (attempt {attempt}): {err}"
                )));
            }
        }

        tokio::time::sleep(Duration::from_millis(200 * attempt)).await;
    }

    Err(last_err.unwrap_or_else(|| OpenBoltError::CommandFailed("move failed".to_string())))
}

fn map_event_action(kind: &EventKind) -> SyncAction {
    match kind {
        EventKind::Remove(_) => SyncAction::Delete,
        _ => SyncAction::Upsert
    }
}

fn join_remote_path(remote_base: &str, relative: &Path) -> String {
    let rel = relative.to_string_lossy().replace('\\', "/");
    let base = remote_base.trim_end_matches('/');
    format!("{base}/{rel}")
}

fn should_skip_dedup(cache: &mut HashMap<String, (SyncAction, u64)>, path: &Path, action: SyncAction) -> bool {
    let key = path.to_string_lossy().to_string();
    let now = now_ts();

    cache.retain(|_, (_, ts)| now.saturating_sub(*ts) <= DEDUPE_WINDOW_SECS);

    if let Some((prev_action, ts)) = cache.get(&key) {
        if *prev_action == action && now.saturating_sub(*ts) <= DEDUPE_WINDOW_SECS {
            return true;
        }
    }

    cache.insert(key, (action, now));
    false
}

fn build_retry_task(
    local_root: &Path,
    remote_base: &str,
    peer_ip: &str,
    api_token: Option<String>,
    path: &Path,
    action: SyncAction
) -> Option<RetryTask> {
    let rel = path.strip_prefix(local_root).ok()?;
    let remote_path = join_remote_path(remote_base, rel);

    match action {
        SyncAction::Delete => Some(RetryTask {
            id: next_task_id(),
            attempts: 0,
            action: RetryAction::Delete {
                remote_path,
                peer_ip: peer_ip.to_string(),
                api_token
            }
        }),
        SyncAction::Upsert => {
            let remote_parent = Path::new(&remote_path)
                .parent()
                .map(|v| v.to_string_lossy().to_string())
                .unwrap_or_else(|| remote_base.to_string());
            let file_name = path.file_name()?.to_string_lossy().to_string();

            Some(RetryTask {
                id: next_task_id(),
                attempts: 0,
                action: RetryAction::Upsert {
                    local_path: path.to_string_lossy().to_string(),
                    remote_parent,
                    file_name,
                    peer_ip: peer_ip.to_string(),
                    api_token
                }
            })
        }
    }
}

fn build_retry_rename_task(
    local_root: &Path,
    remote_base: &str,
    peer_ip: &str,
    api_token: Option<String>,
    rename: &RenameEvent
) -> Option<RetryTask> {
    let from_rel = rename.from.strip_prefix(local_root).ok()?;
    let to_rel = rename.to.strip_prefix(local_root).ok()?;
    let from = join_remote_path(remote_base, from_rel);
    let to = join_remote_path(remote_base, to_rel);

    Some(RetryTask {
        id: next_task_id(),
        attempts: 0,
        action: RetryAction::Move {
            from,
            to,
            peer_ip: peer_ip.to_string(),
            api_token
        }
    })
}

fn retry_store_path(local_root: &Path) -> PathBuf {
    local_root.join(".openbolt").join("retry_queue.json")
}

async fn load_retry_queue(path: &Path) -> VecDeque<RetryTask> {
    let Ok(content) = tokio::fs::read_to_string(path).await else {
        return VecDeque::new();
    };

    let mut queue = serde_json::from_str::<Vec<RetryTask>>(&content)
        .map(VecDeque::from)
        .unwrap_or_default();

    for item in &mut queue {
        if item.id == 0 {
            item.id = next_task_id();
        }
    }

    queue
}

async fn save_retry_queue(path: &Path, queue: &VecDeque<RetryTask>) -> OpenBoltResult<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let data = serde_json::to_string_pretty(&queue.iter().cloned().collect::<Vec<_>>())
        .map_err(|e| OpenBoltError::CommandFailed(e.to_string()))?;
    tokio::fs::write(path, data).await?;
    Ok(())
}

async fn process_retry_queue(
    client: &Client,
    queue: &mut VecDeque<RetryTask>,
    store_path: &Path,
    logs: &SyncLogBuffer,
    app_handle: Option<&AppHandle>
) {
    let mut remaining = VecDeque::new();

    while let Some(mut task) = queue.pop_front() {
        let action_name = match &task.action {
            RetryAction::Upsert { .. } => "retry_upsert",
            RetryAction::Delete { .. } => "retry_delete",
            RetryAction::Move { .. } => "retry_move"
        };

        let run_result = execute_retry_action(client, &task.action).await;
        match run_result {
            Ok(()) => {
                append_log(logs, "info", action_name, "retry", "ok", app_handle).await;
            }
            Err(err) => {
                task.attempts += 1;
                append_log(
                    logs,
                    "warn",
                    action_name,
                    "retry",
                    &format!("failed attempt {}: {err}", task.attempts),
                    app_handle
                )
                .await;
                if task.attempts < 8 {
                    remaining.push_back(task);
                }
            }
        }
    }

    *queue = remaining;
    let _ = save_retry_queue(store_path, queue).await;
}

async fn execute_retry_action(client: &Client, action: &RetryAction) -> OpenBoltResult<()> {
    match action {
        RetryAction::Upsert {
            local_path,
            remote_parent,
            file_name,
            peer_ip,
            api_token
        } => {
            let url = format!(
                "http://{peer_ip}:7733/api/fs/upload?path={}",
                urlencoding::encode(remote_parent)
            );
            send_upload_with_retry(
                client,
                &url,
                api_token.as_deref(),
                Path::new(local_path),
                file_name
            )
            .await
        }
        RetryAction::Delete {
            remote_path,
            peer_ip,
            api_token
        } => delete_remote_path(client, peer_ip, api_token.as_deref(), remote_path).await,
        RetryAction::Move {
            from,
            to,
            peer_ip,
            api_token
        } => move_remote_path(client, peer_ip, api_token.as_deref(), from, to).await
    }
}

pub async fn get_retry_queue_status(local_dir: String) -> OpenBoltResult<RetryQueueStatus> {
    let root = PathBuf::from(local_dir);
    let store = retry_store_path(&root);
    let queue = load_retry_queue(&store).await;

    Ok(RetryQueueStatus {
        pending: queue.len(),
        store_path: store.to_string_lossy().to_string()
    })
}

pub async fn clear_retry_queue(local_dir: String) -> OpenBoltResult<()> {
    let root = PathBuf::from(local_dir);
    let store = retry_store_path(&root);
    save_retry_queue(&store, &VecDeque::new()).await
}

pub async fn retry_queue_item(local_dir: String, item_id: u64) -> OpenBoltResult<()> {
    let root = PathBuf::from(local_dir);
    let store = retry_store_path(&root);
    let mut queue = load_retry_queue(&store).await;

    if let Some(pos) = queue.iter().position(|item| item.id == item_id) {
        if let Some(task) = queue.remove(pos) {
            let client = Client::new();
            execute_retry_action(&client, &task.action).await?;
            save_retry_queue(&store, &queue).await?;
            return Ok(());
        }
    }

    Err(OpenBoltError::CommandFailed("retry item not found".to_string()))
}

pub async fn remove_retry_queue_item(local_dir: String, item_id: u64) -> OpenBoltResult<()> {
    let root = PathBuf::from(local_dir);
    let store = retry_store_path(&root);
    let mut queue = load_retry_queue(&store).await;
    queue.retain(|item| item.id != item_id);
    save_retry_queue(&store, &queue).await
}

pub async fn get_retry_queue_items(
    local_dir: String,
    limit: usize,
    kind_filter: Option<String>,
    min_attempts: Option<u32>
) -> OpenBoltResult<Vec<RetryQueueItem>> {
    let root = PathBuf::from(local_dir);
    let store = retry_store_path(&root);
    let queue = load_retry_queue(&store).await;

    let max = limit.min(200);
    let mut items = Vec::new();
    for task in queue.into_iter() {
        if items.len() >= max {
            break;
        }

        let (kind, target) = match task.action {
            RetryAction::Upsert { local_path, .. } => ("upsert".to_string(), local_path),
            RetryAction::Delete { remote_path, .. } => ("delete".to_string(), remote_path),
            RetryAction::Move { to, .. } => ("move".to_string(), to)
        };

        if let Some(ref kf) = kind_filter {
            if !kf.eq_ignore_ascii_case("all") && !kind.eq_ignore_ascii_case(kf) {
                continue;
            }
        }

        if let Some(ma) = min_attempts {
            if task.attempts < ma {
                continue;
            }
        }

        items.push(RetryQueueItem {
            id: task.id,
            attempts: task.attempts,
            kind,
            target
        });
    }

    Ok(items)
}

async fn append_log(
    logs: &SyncLogBuffer,
    level: &str,
    action: &str,
    path: &str,
    message: &str,
    handle: Option<&AppHandle>
) {
    let mut guard = logs.write().await;
    let entry = SyncLogEntry {
        ts: now_ts(),
        level: level.to_string(),
        action: action.to_string(),
        path: path.to_string(),
        message: message.to_string()
    };

    guard.push_back(entry.clone());

    if guard.len() > 300 {
        let _ = guard.pop_front();
    }

    if let Some(h) = handle {
        let _ = h.emit("sync-log", &entry);
    }
}

fn now_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

async fn send_upload_with_retry(
    client: &Client,
    url: &str,
    api_token: Option<&str>,
    path: &Path,
    file_name: &str
) -> OpenBoltResult<()> {
    let file_name = file_name.to_string();
    for attempts in 1..=3usize {
        let file = tokio::fs::File::open(path).await?;
        let stream = ReaderStream::new(file);
        let body = reqwest::Body::wrap_stream(stream);
        let part = multipart::Part::stream(body).file_name(file_name.clone());
        let form = multipart::Form::new().part("file", part);
        let response = with_token(client.post(url).multipart(form), api_token).send().await;

        match response {
            Ok(resp) if resp.status().is_success() => return Ok(()),
            Ok(resp) => {
                if attempts >= 3 {
                    return Err(OpenBoltError::CommandFailed(format!(
                        "upload failed (attempt {attempts}): {}",
                        resp.status()
                    )));
                }
            }
            Err(err) => {
                if attempts >= 3 {
                    return Err(OpenBoltError::CommandFailed(format!(
                        "upload request failed (attempt {attempts}): {err}"
                    )));
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(200 * attempts as u64)).await;
    }

    Err(OpenBoltError::CommandFailed("upload failed".to_string()))
}

fn with_token(builder: reqwest::RequestBuilder, api_token: Option<&str>) -> reqwest::RequestBuilder {
    if let Some(token) = api_token {
        builder.header("x-openbolt-token", token)
    } else {
        builder
    }
}

fn should_ignore_path(local_root: &Path, path: &Path, patterns: &[String], globset: Option<&GlobSet>) -> bool {
    if patterns.is_empty() {
        return false;
    }

    let rel = path
        .strip_prefix(local_root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();

    if let Some(set) = globset {
        if set.is_match(&rel) {
            return true;
        }
    }

    patterns
        .iter()
        .map(|p| p.trim().to_ascii_lowercase())
        .filter(|p| !p.is_empty())
        .any(|p| rel.contains(&p))
}

fn build_globset(patterns: &[String]) -> Option<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    let mut added = 0usize;
    for pattern in patterns {
        let pat = pattern.trim();
        if pat.is_empty() {
            continue;
        }
        if let Ok(glob) = Glob::new(&pat.to_ascii_lowercase()) {
            builder.add(glob);
            added += 1;
        }
    }

    if added == 0 {
        return None;
    }

    builder.build().ok()
}

fn next_task_id() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or_else(|_| now_ts())
}

async fn is_remote_origin(cache: &RemoteOriginCache, path: &Path) -> bool {
    let key = path.to_string_lossy().to_string();
    let now = now_ts();
    let mut guard = cache.write().await;
    guard.retain(|_, ts| now.saturating_sub(*ts) <= ORIGIN_SUPPRESS_SECS);
    guard.contains_key(&key)
}

async fn list_remote_recursive(
    client: &Client,
    peer_ip: &str,
    api_token: Option<&str>,
    remote_base: &str
) -> Vec<(String, RemoteFsEntry)> {
    let mut files = Vec::new();
    let mut dirs_to_visit = vec![(remote_base.to_string(), 0u32)];
    let base_prefix = remote_base.trim_end_matches('/').to_string();

    while let Some((dir_path, depth)) = dirs_to_visit.pop() {
        if depth > 8 {
            continue;
        }

        let url = format!(
            "http://{peer_ip}:7733/api/fs/list?path={}",
            urlencoding::encode(&dir_path)
        );

        let Ok(response) = with_token(client.get(&url), api_token).send().await else {
            continue;
        };
        if !response.status().is_success() {
            continue;
        }
        let Ok(entries) = response.json::<Vec<RemoteFsEntry>>().await else {
            continue;
        };

        for entry in entries {
            let child_path = format!("{}/{}", dir_path.trim_end_matches('/'), entry.name);
            if entry.is_dir {
                dirs_to_visit.push((child_path, depth + 1));
            } else {
                let rel = if child_path.starts_with(&base_prefix) {
                    child_path[base_prefix.len()..].trim_start_matches('/').to_string()
                } else {
                    entry.name.clone()
                };
                if !rel.is_empty() {
                    files.push((rel, entry));
                }
            }
        }
    }

    files
}

async fn inbound_pull_cycle(
    client: &Client,
    peer_ip: &str,
    api_token: Option<&str>,
    remote_base: &str,
    local_root: &Path,
    ignore_patterns: &[String],
    ignore_glob: Option<&GlobSet>,
    origin_cache: &RemoteOriginCache,
    logs: &SyncLogBuffer,
    app_handle: Option<&AppHandle>
) {
    let remote_files = list_remote_recursive(client, peer_ip, api_token, remote_base).await;
    for (rel_path, entry) in remote_files {
        let local_rel = rel_path.replace('/', std::path::MAIN_SEPARATOR_STR);
        let local_path = local_root.join(&local_rel);

        if should_ignore_path(local_root, &local_path, ignore_patterns, ignore_glob) {
            continue;
        }

        match tokio::fs::metadata(&local_path).await {
            Ok(meta) => {
                let local_mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or_default();
                if local_mtime >= entry.mtime {
                    continue;
                }
            }
            Err(_) => {} // file doesn't exist locally, proceed with download
        }

        let remote_full_path = format!("{}/{}", remote_base.trim_end_matches('/'), rel_path);
        let url = format!(
            "http://{peer_ip}:7733/api/fs/download?path={}",
            urlencoding::encode(&remote_full_path)
        );

        let response = match with_token(client.get(&url), api_token).send().await {
            Ok(r) if r.status().is_success() => r,
            Ok(r) => {
                append_log(
                    logs,
                    "warn",
                    "inbound",
                    &rel_path,
                    &format!("download failed: {}", r.status()),
                    app_handle
                ).await;
                continue;
            }
            Err(e) => {
                append_log(
                    logs,
                    "warn",
                    "inbound",
                    &rel_path,
                    &format!("download error: {e}"),
                    app_handle
                ).await;
                continue;
            }
        };

        let Ok(bytes) = response.bytes().await else {
            continue;
        };

        if let Some(parent) = local_path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }

        {
            let mut cache = origin_cache.write().await;
            cache.insert(local_path.to_string_lossy().to_string(), now_ts());
        }

        if let Err(e) = tokio::fs::write(&local_path, &bytes).await {
            append_log(logs, "warn", "inbound", &rel_path, &format!("write failed: {e}"), app_handle).await;
        } else {
            append_log(logs, "info", "inbound", &rel_path, "pulled from remote", app_handle).await;
        }
    }
}

pub async fn preview_ignore_patterns(
    local_dir: String,
    patterns: Vec<String>
) -> OpenBoltResult<Vec<String>> {
    let root = PathBuf::from(&local_dir);
    if !root.exists() {
        return Err(OpenBoltError::CommandFailed("directory does not exist".to_string()));
    }

    let globset = build_globset(&patterns);
    let mut matched = Vec::new();
    let mut dirs_to_visit = vec![root.clone()];

    while let Some(dir) = dirs_to_visit.pop() {
        let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if should_ignore_path(&root, &path, &patterns, globset.as_ref()) {
                let rel = path
                    .strip_prefix(&root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                matched.push(rel);
                if matched.len() >= 200 {
                    return Ok(matched);
                }
            } else if path.is_dir() {
                dirs_to_visit.push(path);
            }
        }
    }

    Ok(matched)
}
