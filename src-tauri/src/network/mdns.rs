use std::{
    net::{IpAddr, Ipv4Addr},
    sync::Arc,
    time::Duration
};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::RwLock;

use crate::error::{OpenBoltError, OpenBoltResult};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredPeer {
    pub hostname: String,
    pub os: String,
    pub ip: String,
    pub api_port: u16,
    pub kvm_port: u16
}

pub struct DiscoveryRuntime {
    daemon: ServiceDaemon,
    receiver_task: tokio::task::JoinHandle<()>
}

impl DiscoveryRuntime {
    pub async fn shutdown(self) {
        self.receiver_task.abort();
        let _ = self.daemon.shutdown();
    }
}

pub async fn start_discovery(
    local_ip: String,
    peers: Arc<RwLock<Vec<DiscoveredPeer>>>,
    app_handle: Option<AppHandle>
) -> OpenBoltResult<DiscoveryRuntime> {
    let daemon = ServiceDaemon::new().map_err(|e| OpenBoltError::Mdns(e.to_string()))?;

    let host_name = hostname();
    let mdns_host = format!("{}.local.", host_name);
    let props = [
        ("os", std::env::consts::OS),
        ("hostname", host_name.as_str()),
        ("kvm_port", "4242"),
        ("api_port", "7733")
    ];

    let ip = local_ip
        .parse::<Ipv4Addr>()
        .map_err(|e| OpenBoltError::CommandFailed(e.to_string()))?;

    let service = ServiceInfo::new(
        "_openbolt._tcp.local.",
        "openbolt-node",
        &mdns_host,
        IpAddr::V4(ip),
        7733,
        &props[..]
    )
    .map_err(|e| OpenBoltError::Mdns(e.to_string()))?;

    daemon
        .register(service)
        .map_err(|e| OpenBoltError::Mdns(e.to_string()))?;

    let receiver = daemon
        .browse("_openbolt._tcp.local.")
        .map_err(|e| OpenBoltError::Mdns(e.to_string()))?;

    let receiver_task = tokio::spawn(async move {
        loop {
            if let Ok(event) = receiver.recv_timeout(Duration::from_secs(1)) {
                if let ServiceEvent::ServiceResolved(info) = event {
                    let ip = info
                        .get_addresses_v4()
                        .iter()
                        .next()
                        .map(|v| v.to_string())
                        .unwrap_or_default();

                    if !ip.starts_with("10.99.99.") {
                        continue;
                    }

                    let api_port = info
                        .get_properties()
                        .get("api_port")
                        .map(|v| v.val_str())
                        .and_then(|v| v.parse::<u16>().ok())
                        .unwrap_or(7733);

                    let kvm_port = info
                        .get_properties()
                        .get("kvm_port")
                        .map(|v| v.val_str())
                        .and_then(|v| v.parse::<u16>().ok())
                        .unwrap_or(4242);

                    let os = info
                        .get_properties()
                        .get("os")
                        .map(|v| v.val_str())
                        .unwrap_or("unknown")
                        .to_string();

                    let hostname = info
                        .get_properties()
                        .get("hostname")
                        .map(|v| v.val_str())
                        .unwrap_or("openbolt-peer")
                        .to_string();

                    let mut guard = peers.write().await;
                    if !guard.iter().any(|p| p.ip == ip) {
                        let new_peer = DiscoveredPeer {
                            hostname,
                            os,
                            ip,
                            api_port,
                            kvm_port
                        };
                        guard.push(new_peer.clone());
                        if let Some(ref handle) = app_handle {
                            let _ = handle.emit("peer-discovered", &new_peer);
                        }
                    }
                }
            }
        }
    });

    Ok(DiscoveryRuntime {
        daemon,
        receiver_task
    })
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "openbolt-device".to_string())
}
