import { jsx as _jsx, jsxs as _jsxs } from "react/jsx-runtime";
import { invoke } from "@tauri-apps/api/core";
import { useEffect, useMemo, useState } from "react";
const fallbackOverview = {
    appVersion: "0.1.0",
    platform: "unknown",
    discovered: [],
    syncRunning: false
};
function App() {
    const [overview, setOverview] = useState(fallbackOverview);
    const [selectedPeerIp, setSelectedPeerIp] = useState("10.99.99.2");
    const [localSyncDir, setLocalSyncDir] = useState("C:/Users/Public/OpenBoltSync");
    const [remoteSyncDir, setRemoteSyncDir] = useState("/tmp/openbolt-sync");
    const [status, setStatus] = useState("idle");
    const [busy, setBusy] = useState(false);
    const peers = useMemo(() => overview.discovered ?? [], [overview.discovered]);
    const refreshOverview = async () => {
        try {
            const data = await invoke("get_system_overview");
            setOverview(data);
            if (data.discovered.length > 0) {
                setSelectedPeerIp(data.discovered[0].ip);
            }
        }
        catch (err) {
            setStatus(`overview failed: ${String(err)}`);
        }
    };
    useEffect(() => {
        void refreshOverview();
    }, []);
    const runAction = async (fn, doneMsg) => {
        setBusy(true);
        setStatus("processing...");
        try {
            await fn();
            setStatus(doneMsg);
            await refreshOverview();
        }
        catch (err) {
            setStatus(String(err));
        }
        finally {
            setBusy(false);
        }
    };
    const startKvm = async (mode) => {
        await runAction(() => invoke("start_kvm", { mode, peerIp: selectedPeerIp }), `kvm ${mode} started`);
    };
    return (_jsx("main", { className: "min-h-screen bg-shell p-6 text-slate-100", children: _jsxs("div", { className: "mx-auto grid max-w-6xl gap-6 lg:grid-cols-[1.3fr_1fr]", children: [_jsxs("section", { className: "glass-panel p-6", children: [_jsxs("header", { className: "mb-5 flex items-start justify-between gap-4", children: [_jsxs("div", { children: [_jsx("p", { className: "text-xs uppercase tracking-[0.18em] text-cyan-200/70", children: "OpenBolt" }), _jsx("h1", { className: "text-4xl font-bold leading-tight", children: "Thunderbolt Share, Open Source" }), _jsx("p", { className: "mt-2 text-sm text-slate-300", children: "Native control plane with Rust + Tauri, optimized for 10.99.99.0/24 high-speed links." })] }), _jsx("button", { className: "chip", disabled: busy, onClick: () => void refreshOverview(), children: "Refresh" })] }), _jsxs("div", { className: "grid gap-4 md:grid-cols-3", children: [_jsxs("article", { className: "stat-card", children: [_jsx("p", { children: "Version" }), _jsx("h2", { children: overview.appVersion })] }), _jsxs("article", { className: "stat-card", children: [_jsx("p", { children: "Platform" }), _jsx("h2", { children: overview.platform })] }), _jsxs("article", { className: "stat-card", children: [_jsx("p", { children: "Local Link IP" }), _jsx("h2", { children: overview.localIp ?? "unconfigured" })] })] }), _jsxs("div", { className: "mt-6 grid gap-4 md:grid-cols-2", children: [_jsx("button", { className: "action-btn", disabled: busy, onClick: () => void runAction(() => invoke("start_discovery"), "discovery started"), children: "Start Discovery" }), _jsx("button", { className: "action-btn", disabled: busy, onClick: () => void runAction(() => invoke("stop_discovery"), "discovery stopped"), children: "Stop Discovery" }), _jsx("button", { className: "action-btn", disabled: busy, onClick: () => void startKvm("server"), children: "Start KVM Host" }), _jsx("button", { className: "action-btn", disabled: busy, onClick: () => void startKvm("client"), children: "Start KVM Client" }), _jsx("button", { className: "action-btn", disabled: busy, onClick: () => void runAction(() => invoke("start_file_api", { bindIp: overview.localIp ?? "10.99.99.2" }), "file api started"), children: "Start File API" }), _jsx("button", { className: "action-btn", disabled: busy, onClick: () => void runAction(() => invoke("stop_all_services"), "all services stopped"), children: "Stop All" })] }), _jsxs("div", { className: "mt-6 grid gap-3 rounded-2xl border border-white/10 bg-slate-950/30 p-4", children: [_jsx("p", { className: "text-xs uppercase tracking-[0.12em] text-cyan-200/80", children: "Folder Sync" }), _jsx("input", { className: "input", value: localSyncDir, onChange: (e) => setLocalSyncDir(e.target.value), placeholder: "Local directory" }), _jsx("input", { className: "input", value: remoteSyncDir, onChange: (e) => setRemoteSyncDir(e.target.value), placeholder: "Remote directory" }), _jsxs("div", { className: "grid gap-3 md:grid-cols-2", children: [_jsx("button", { className: "action-btn", disabled: busy, onClick: () => void runAction(() => invoke("start_folder_sync", {
                                                localDir: localSyncDir,
                                                remoteDir: remoteSyncDir,
                                                peerIp: selectedPeerIp
                                            }), "folder sync started"), children: "Start Folder Sync" }), _jsx("button", { className: "action-btn", disabled: busy, onClick: () => void runAction(() => invoke("stop_folder_sync"), "folder sync stopped"), children: "Stop Folder Sync" })] }), _jsxs("p", { className: "text-sm text-cyan-100/80", children: ["Sync Engine: ", overview.syncRunning ? "running" : "stopped"] })] }), _jsxs("p", { className: "mt-5 rounded-xl border border-cyan-300/20 bg-cyan-500/5 px-4 py-3 text-sm text-cyan-50", children: ["Status: ", status] })] }), _jsxs("section", { className: "glass-panel p-6", children: [_jsx("h2", { className: "text-lg font-semibold", children: "Discovered Peers" }), _jsx("p", { className: "mt-1 text-sm text-slate-300", children: "mDNS service: _openbolt._tcp.local." }), _jsx("div", { className: "mt-4 space-y-3", children: peers.length === 0 ? (_jsx("p", { className: "rounded-xl border border-white/10 p-4 text-sm text-slate-300", children: "No peer detected yet." })) : (peers.map((peer) => (_jsxs("button", { className: `peer-item ${selectedPeerIp === peer.ip ? "peer-active" : ""}`, onClick: () => setSelectedPeerIp(peer.ip), children: [_jsxs("div", { children: [_jsx("p", { className: "font-medium", children: peer.hostname }), _jsx("p", { className: "text-xs text-slate-300", children: peer.os })] }), _jsx("p", { className: "text-sm", children: peer.ip })] }, `${peer.hostname}-${peer.ip}`)))) })] })] }) }));
}
export default App;
