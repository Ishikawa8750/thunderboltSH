import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useMemo, useState } from "react";
import type {
  DeviceInfo,
  KvmMode,
  RetryQueueItem,
  RetryQueueStatus,
  SyncDirection,
  SyncLogEntry,
  SyncStats,
  SystemOverview
} from "./types";

const fallbackOverview: SystemOverview = {
  appVersion: "0.1.0",
  platform: "unknown",
  discovered: [],
  syncRunning: false,
  apiAuthEnabled: false
};

function App() {
  const [overview, setOverview] = useState<SystemOverview>(fallbackOverview);
  const [selectedPeerIp, setSelectedPeerIp] = useState("10.99.99.2");
  const [localSyncDir, setLocalSyncDir] = useState("C:/Users/Public/OpenBoltSync");
  const [remoteSyncDir, setRemoteSyncDir] = useState("/tmp/openbolt-sync");
  const [syncDirection, setSyncDirection] = useState<SyncDirection>("outbound");
  const [ignorePatterns, setIgnorePatterns] = useState(".tmp\n.DS_Store\nThumbs.db\n.openbolt");
  const [status, setStatus] = useState("idle");
  const [busy, setBusy] = useState(false);
  const [syncLogs, setSyncLogs] = useState<SyncLogEntry[]>([]);
  const [syncStats, setSyncStats] = useState<SyncStats>({
    total: 0,
    success: 0,
    failed: 0,
    conflicts: 0,
    retries: 0
  });
  const [retryStatus, setRetryStatus] = useState<RetryQueueStatus>({ pending: 0, storePath: "" });
  const [retryItems, setRetryItems] = useState<RetryQueueItem[]>([]);
  const [ignorePreview, setIgnorePreview] = useState<string[] | null>(null);
  const [retryKindFilter, setRetryKindFilter] = useState("all");
  const [retryMinAttempts, setRetryMinAttempts] = useState(0);
  const [sunshineConfigPath, setSunshineConfigPath] = useState<string | null>(null);
  const [clipboardFiles, setClipboardFiles] = useState<string[]>([]);
  const [clipboardDestDir, setClipboardDestDir] = useState("");

  const peers = useMemo<DeviceInfo[]>(() => overview.discovered ?? [], [overview.discovered]);

  const filteredRetryItems = useMemo(() => {
    return retryItems.filter((item) => {
      if (retryKindFilter !== "all" && item.kind !== retryKindFilter) return false;
      if (item.attempts < retryMinAttempts) return false;
      return true;
    });
  }, [retryItems, retryKindFilter, retryMinAttempts]);

  const refreshOverview = async () => {
    try {
      const data = await invoke<SystemOverview>("get_system_overview");
      setOverview(data);
      if (data.discovered.length > 0) {
        setSelectedPeerIp(data.discovered[0].ip);
      }
    } catch (err) {
      setStatus(`overview failed: ${String(err)}`);
    }
  };

  useEffect(() => {
    void refreshOverview();
  }, []);

  useEffect(() => {
    let disposed = false;
    const setup = async () => {
      const unlistenSyncLog = await listen<SyncLogEntry>("sync-log", (event) => {
        setSyncLogs((prev) => [...prev.slice(-99), event.payload]);
      });
      const unlistenPeer = await listen<DeviceInfo>("peer-discovered", (event) => {
        const peer = event.payload;
        setOverview((prev) => {
          if (prev.discovered.some((p) => p.ip === peer.ip)) {
            return prev;
          }
          return { ...prev, discovered: [...prev.discovered, peer] };
        });
      });
      const unlistenClipboard = await listen<string[]>("clipboard-change", (event) => {
        setClipboardFiles(event.payload);
      });

      if (disposed) {
        unlistenSyncLog();
        unlistenPeer();
        unlistenClipboard();
      }

      return () => {
        unlistenSyncLog();
        unlistenPeer();
        unlistenClipboard();
      };
    };

    let teardown: (() => void) | undefined;
    void setup().then((fn) => {
      teardown = fn;
    });

    return () => {
      disposed = true;
      teardown?.();
    };
  }, []);

  const refreshSyncLogs = async () => {
    try {
      const logs = await invoke<SyncLogEntry[]>("get_sync_logs", { limit: 80 });
      setSyncLogs(logs);
    } catch {
      // Keep UI usable even if log API is temporarily unavailable.
    }
  };

  const refreshRetryStatus = async () => {
    try {
      const status = await invoke<RetryQueueStatus>("get_retry_queue_status", {
        localDir: localSyncDir
      });
      setRetryStatus(status);
    } catch {
      // Ignore temporary command failures.
    }
  };

  const refreshRetryItems = async () => {
    try {
      const items = await invoke<RetryQueueItem[]>("get_retry_queue_items", {
        localDir: localSyncDir,
        limit: 30
      });
      setRetryItems(items);
    } catch {
      // Ignore temporary command failures.
    }
  };

  useEffect(() => {
    void refreshOverview();
    void refreshRetryStatus();
    void refreshRetryItems();
    const timer = window.setInterval(() => {
      void refreshOverview();
      void refreshRetryStatus();
      void refreshRetryItems();
    }, 2000);
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    let success = 0;
    let failed = 0;
    let conflicts = 0;
    let retries = 0;

    for (const item of syncLogs) {
      if (item.level.toLowerCase() === "warn") failed += 1;
      if (item.level.toLowerCase() === "info") success += 1;
      const msg = item.message.toLowerCase();
      if (msg.includes("conflict keep remote")) conflicts += 1;
      if (msg.includes("attempt")) retries += 1;
    }

    setSyncStats({
      total: syncLogs.length,
      success,
      failed,
      conflicts,
      retries
    });
  }, [syncLogs]);

  const runAction = async (fn: () => Promise<unknown>, doneMsg: string) => {
    setBusy(true);
    setStatus("processing...");
    try {
      await fn();
      setStatus(doneMsg);
      await refreshOverview();
      await refreshSyncLogs();
      await refreshRetryStatus();
      await refreshRetryItems();
    } catch (err) {
      setStatus(String(err));
    } finally {
      setBusy(false);
    }
  };

  const handlePreviewIgnore = async () => {
    try {
      const patterns = ignorePatterns
        .split("\n")
        .map((v) => v.trim())
        .filter((v) => v.length > 0);
      const result = await invoke<string[]>("preview_ignore_patterns", {
        localDir: localSyncDir,
        patterns
      });
      setIgnorePreview(result);
    } catch (err) {
      setStatus(String(err));
    }
  };

  const handleGetClipboardFiles = async () => {
    try {
      const files = await invoke<string[]>("get_clipboard_files");
      setClipboardFiles(files);
      if (files.length === 0) setStatus("clipboard: no file paths detected");
    } catch (err) {
      setStatus(String(err));
    }
  };

  const startKvm = async (mode: KvmMode) => {
    await runAction(
      () => invoke("start_kvm", { mode, peerIp: selectedPeerIp }),
      `kvm ${mode} started`
    );
  };

  return (
    <main className="min-h-screen bg-shell p-6 text-slate-100">
      <div className="mx-auto grid max-w-6xl gap-6 lg:grid-cols-[1.3fr_1fr]">
        <section className="glass-panel p-6">
          <header className="mb-5 flex items-start justify-between gap-4">
            <div>
              <p className="text-xs uppercase tracking-[0.18em] text-cyan-200/70">OpenBolt</p>
              <h1 className="text-4xl font-bold leading-tight">Thunderbolt Share, Open Source</h1>
              <p className="mt-2 text-sm text-slate-300">
                Native control plane with Rust + Tauri, optimized for 10.99.99.0/24 high-speed links.
              </p>
            </div>
            <button className="chip" disabled={busy} onClick={() => void refreshOverview()}>
              Refresh
            </button>
          </header>

          <div className="grid gap-4 md:grid-cols-3">
            <article className="stat-card">
              <p>Version</p>
              <h2>{overview.appVersion}</h2>
            </article>
            <article className="stat-card">
              <p>Platform</p>
              <h2>{overview.platform}</h2>
            </article>
            <article className="stat-card">
              <p>Local Link IP</p>
              <h2>{overview.localIp ?? "unconfigured"}</h2>
            </article>
            <article className="stat-card">
              <p>API Auth</p>
              <h2>{overview.apiAuthEnabled ? "enabled" : "disabled"}</h2>
            </article>
          </div>

          <div className="mt-6 grid gap-4 md:grid-cols-2">
            <button
              className="action-btn"
              disabled={busy}
              onClick={() => void runAction(() => invoke("start_discovery"), "discovery started")}
            >
              Start Discovery
            </button>
            <button
              className="action-btn"
              disabled={busy}
              onClick={() => void runAction(() => invoke("stop_discovery"), "discovery stopped")}
            >
              Stop Discovery
            </button>
            <button className="action-btn" disabled={busy} onClick={() => void startKvm("server")}>
              Start KVM Host
            </button>
            <button className="action-btn" disabled={busy} onClick={() => void startKvm("client")}>
              Start KVM Client
            </button>
            <button
              className="action-btn"
              disabled={busy}
              onClick={() =>
                void runAction(
                  () => invoke("start_file_api", { bindIp: overview.localIp ?? "10.99.99.2" }),
                  "file api started"
                )
              }
            >
              Start File API
            </button>
            <button
              className="action-btn"
              disabled={busy}
              onClick={() => void runAction(() => invoke("stop_all_services"), "all services stopped")}
            >
              Stop All
            </button>
          </div>

          {/* ―― Screen Sharing ―― */}
          <div className="mt-6 grid gap-3 rounded-2xl border border-white/10 bg-slate-950/30 p-4">
            <p className="text-xs uppercase tracking-[0.12em] text-cyan-200/80">Screen Sharing</p>
            <p className="text-[11px] text-slate-400">
              Windows (host): configure &amp; start Sunshine. macOS (client): stream via Moonlight.
            </p>
            <div className="grid gap-3 md:grid-cols-2">
              <button
                className="action-btn"
                disabled={busy}
                onClick={() =>
                  void runAction(
                    async () => {
                      const path = await invoke<string>("configure_sunshine", { localIp: overview.localIp ?? "" });
                      setSunshineConfigPath(path);
                    },
                    "sunshine configured"
                  )
                }
              >
                Configure Sunshine
              </button>
              <button
                className="action-btn"
                disabled={busy}
                onClick={() => void runAction(() => invoke("start_sunshine"), "sunshine started")}
              >
                Start Sunshine
              </button>
              <button
                className="action-btn"
                disabled={busy}
                onClick={() => void runAction(() => invoke("stop_sunshine"), "sunshine stopped")}
              >
                Stop Sunshine
              </button>
              <button
                className="action-btn"
                disabled={busy}
                onClick={() =>
                  void runAction(
                    () => invoke("launch_moonlight", { peerIp: selectedPeerIp }),
                    "moonlight launched"
                  )
                }
              >
                Launch Moonlight
              </button>
            </div>
            {sunshineConfigPath && (
              <p className="text-[11px] text-slate-400 truncate">Config: {sunshineConfigPath}</p>
            )}
          </div>

          {/* ―― Clipboard Transfer ―― */}
          <div className="mt-6 grid gap-3 rounded-2xl border border-white/10 bg-slate-950/30 p-4">
            <p className="text-xs uppercase tracking-[0.12em] text-cyan-200/80">Clipboard File Transfer</p>
            <p className="text-[11px] text-slate-400">
              Windows: reads CF_HDROP. macOS: reads file alias via osascript.
            </p>
            <input
              className="input"
              value={clipboardDestDir}
              onChange={(e) => setClipboardDestDir(e.target.value)}
              placeholder="Destination dir on peer (留空则使用 ~/.openbolt/temp)"
            />
            <div className="grid gap-3 md:grid-cols-2">
              <button
                className="action-btn"
                disabled={busy}
                onClick={() => void handleGetClipboardFiles()}
              >
                Detect Clipboard Files
              </button>
              <button
                className="action-btn"
                disabled={busy || clipboardFiles.length === 0}
                onClick={() =>
                  void runAction(
                    () =>
                      invoke("send_clipboard_files", {
                        peerIp: selectedPeerIp,
                        destDir: clipboardDestDir || null
                      }),
                    `clipboard files sent (${clipboardFiles.length})`
                  )
                }
              >
                Send to Peer ({clipboardFiles.length})
              </button>
              <button
                className="action-btn"
                disabled={busy}
                onClick={() =>
                  void runAction(
                    () =>
                      invoke("start_clipboard_watcher", {
                        peerIp: selectedPeerIp,
                        destDir: clipboardDestDir || null
                      }),
                    "clipboard watcher started"
                  )
                }
              >
                Start Clipboard Watcher
              </button>
              <button
                className="action-btn"
                disabled={busy}
                onClick={() =>
                  void runAction(() => invoke("stop_clipboard_watcher"), "clipboard watcher stopped")
                }
              >
                Stop Clipboard Watcher
              </button>
            </div>
            {clipboardFiles.length > 0 && (
              <div className="rounded-xl border border-white/10 bg-slate-900/30 p-2">
                {clipboardFiles.map((f, i) => (
                  <p key={i} className="font-mono text-[11px] text-slate-300 truncate">{f}</p>
                ))}
              </div>
            )}
          </div>

          <div className="mt-6 grid gap-3 rounded-2xl border border-white/10 bg-slate-950/30 p-4">
            <p className="text-xs uppercase tracking-[0.12em] text-cyan-200/80">Folder Sync</p>
            <input
              className="input"
              value={localSyncDir}
              onChange={(e) => setLocalSyncDir(e.target.value)}
              placeholder="Local directory"
            />
            <input
              className="input"
              value={remoteSyncDir}
              onChange={(e) => setRemoteSyncDir(e.target.value)}
              placeholder="Remote directory"
            />
            <div className="grid gap-3 md:grid-cols-2">
              <button
                className="action-btn"
                disabled={busy}
                onClick={() =>
                  void runAction(
                    () =>
                      invoke("start_folder_sync", {
                        localDir: localSyncDir,
                        remoteDir: remoteSyncDir,
                        peerIp: selectedPeerIp,
                        direction: syncDirection,
                        ignorePatterns: ignorePatterns
                          .split("\n")
                          .map((v) => v.trim())
                          .filter((v) => v.length > 0)
                      }),
                    "folder sync started"
                  )
                }
              >
                Start Folder Sync
              </button>
              <button
                className="action-btn"
                disabled={busy}
                onClick={() => void runAction(() => invoke("stop_folder_sync"), "folder sync stopped")}
              >
                Stop Folder Sync
              </button>
            </div>

            <div className="grid gap-3 md:grid-cols-2">
              <label className="text-xs text-slate-300">
                Direction
                <select
                  className="input mt-1"
                  value={syncDirection}
                  onChange={(e) => setSyncDirection(e.target.value as SyncDirection)}
                >
                  <option value="outbound">Outbound</option>
                  <option value="bidirectional">Bidirectional (beta)</option>
                </select>
              </label>
              <label className="text-xs text-slate-300">
                Ignore Patterns (one per line)
                <textarea
                  className="input mt-1 min-h-[92px]"
                  value={ignorePatterns}
                  onChange={(e) => { setIgnorePatterns(e.target.value); setIgnorePreview(null); }}
                />
                <button
                  className="chip mt-1"
                  disabled={busy}
                  onClick={() => void handlePreviewIgnore()}
                >
                  Preview Matches
                </button>
              </label>
            </div>
            {ignorePreview !== null && (
              <div className="rounded-xl border border-white/10 bg-slate-900/30 p-3">
                <p className="mb-1 text-xs text-cyan-100/80">
                  Preview: {ignorePreview.length} path(s) would be ignored
                </p>
                <div className="log-box">
                  {ignorePreview.length === 0 ? (
                    <p className="text-xs text-slate-400">No paths matched current patterns.</p>
                  ) : (
                    ignorePreview.map((p, i) => (
                      <p key={i} className="font-mono text-xs text-slate-300">{p}</p>
                    ))
                  )}
                </div>
              </div>
            )}
            <p className="text-sm text-cyan-100/80">
              Sync Engine: {overview.syncRunning ? "running" : "stopped"} | Direction: {syncDirection}
            </p>

            <div className="mt-2 grid gap-2 md:grid-cols-5">
              <article className="mini-stat">
                <p>Total</p>
                <h4>{syncStats.total}</h4>
              </article>
              <article className="mini-stat">
                <p>Success</p>
                <h4>{syncStats.success}</h4>
              </article>
              <article className="mini-stat">
                <p>Failed</p>
                <h4>{syncStats.failed}</h4>
              </article>
              <article className="mini-stat">
                <p>Conflicts</p>
                <h4>{syncStats.conflicts}</h4>
              </article>
              <article className="mini-stat">
                <p>Retries</p>
                <h4>{syncStats.retries}</h4>
              </article>
            </div>

            <div className="mt-2 flex items-center justify-between rounded-xl border border-white/10 bg-slate-900/35 p-3">
              <div>
                <p className="text-xs uppercase tracking-[0.12em] text-cyan-100/80">Retry Queue</p>
                <p className="text-sm text-slate-200">Pending: {retryStatus.pending}</p>
                <p className="text-[11px] text-slate-400 truncate max-w-[420px]">{retryStatus.storePath}</p>
              </div>
              <button
                className="chip"
                disabled={busy}
                onClick={() =>
                  void runAction(
                    () => invoke("clear_retry_queue", { localDir: localSyncDir }),
                    "retry queue cleared"
                  )
                }
              >
                Clear Retry Queue
              </button>
            </div>

            <div className="mt-2 rounded-xl border border-white/10 bg-slate-900/30 p-3">
              <div className="mb-3 flex flex-wrap items-center gap-3">
                <p className="text-xs uppercase tracking-[0.12em] text-cyan-100/80">Retry Queue Items</p>
                <select
                  className="input py-0.5 px-2 text-xs"
                  value={retryKindFilter}
                  onChange={(e) => setRetryKindFilter(e.target.value)}
                >
                  <option value="all">All kinds</option>
                  <option value="upsert">Upsert</option>
                  <option value="delete">Delete</option>
                  <option value="move">Move</option>
                </select>
                <label className="flex items-center gap-1 text-xs text-slate-300">
                  Min attempts:
                  <input
                    type="number"
                    className="input w-14 py-0.5 px-1 text-xs"
                    value={retryMinAttempts}
                    min={0}
                    onChange={(e) => setRetryMinAttempts(Number(e.target.value))}
                  />
                </label>
              </div>
              <div className="log-box space-y-2">
                {filteredRetryItems.length === 0 ? (
                  <p className="text-xs text-slate-400">No pending retry item.</p>
                ) : (
                  filteredRetryItems.map((item, idx) => (
                    <div key={`${item.kind}-${item.target}-${idx}`} className="log-item">
                      <p className="text-[11px] text-cyan-200/75">{item.kind.toUpperCase()} · attempts: {item.attempts}</p>
                      <p className="text-xs text-slate-300 truncate">{item.target}</p>
                      <div className="mt-2 flex gap-2">
                        <button
                          className="chip"
                          disabled={busy}
                          onClick={() =>
                            void runAction(
                              () => invoke("retry_queue_item", { localDir: localSyncDir, itemId: item.id }),
                              "retry item executed"
                            )
                          }
                        >
                          Retry Now
                        </button>
                        <button
                          className="chip"
                          disabled={busy}
                          onClick={() =>
                            void runAction(
                              () => invoke("remove_retry_queue_item", { localDir: localSyncDir, itemId: item.id }),
                              "retry item removed"
                            )
                          }
                        >
                          Remove
                        </button>
                      </div>
                    </div>
                  ))
                )}
              </div>
            </div>
          </div>

          <p className="mt-5 rounded-xl border border-cyan-300/20 bg-cyan-500/5 px-4 py-3 text-sm text-cyan-50">
            Status: {status}
          </p>
        </section>

        <section className="glass-panel p-6">
          <h2 className="text-lg font-semibold">Discovered Peers</h2>
          <p className="mt-1 text-sm text-slate-300">mDNS service: _openbolt._tcp.local.</p>
          <div className="mt-4 space-y-3">
            {peers.length === 0 ? (
              <p className="rounded-xl border border-white/10 p-4 text-sm text-slate-300">No peer detected yet.</p>
            ) : (
              peers.map((peer) => (
                <button
                  key={`${peer.hostname}-${peer.ip}`}
                  className={`peer-item ${selectedPeerIp === peer.ip ? "peer-active" : ""}`}
                  onClick={() => setSelectedPeerIp(peer.ip)}
                >
                  <div>
                    <p className="font-medium">{peer.hostname}</p>
                    <p className="text-xs text-slate-300">{peer.os}</p>
                  </div>
                  <p className="text-sm">{peer.ip}</p>
                </button>
              ))
            )}
          </div>

          <div className="mt-6 rounded-2xl border border-white/10 bg-slate-950/30 p-4">
            <div className="mb-3 flex items-center justify-between">
              <h3 className="text-sm font-semibold uppercase tracking-[0.12em] text-cyan-100/90">Sync Logs</h3>
              <div className="flex gap-2">
                <button className="chip" disabled={busy} onClick={() => void refreshSyncLogs()}>
                  Refresh Logs
                </button>
                <button
                  className="chip"
                  disabled={busy}
                  onClick={() =>
                    void runAction(async () => {
                      await invoke("clear_sync_logs");
                    }, "sync logs cleared")
                  }
                >
                  Clear Logs
                </button>
              </div>
            </div>
            <div className="log-box space-y-2">
              {syncLogs.length === 0 ? (
                <p className="text-xs text-slate-400">No sync logs yet.</p>
              ) : (
                syncLogs
                  .slice()
                  .reverse()
                  .map((item, idx) => (
                    <div key={`${item.ts}-${idx}`} className="log-item">
                      <p className="text-[11px] text-cyan-200/75">[{new Date(item.ts * 1000).toLocaleTimeString()}] {item.level.toUpperCase()} {item.action}</p>
                      <p className="truncate text-xs text-slate-200">{item.path}</p>
                      <p className="text-xs text-slate-300">{item.message}</p>
                    </div>
                  ))
              )}
            </div>
          </div>
        </section>
      </div>
    </main>
  );
}

export default App;
