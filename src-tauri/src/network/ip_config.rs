use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{OpenBoltError, OpenBoltResult};

use super::nic;

pub async fn ensure_local_link_ip() -> OpenBoltResult<String> {
    let nic = nic::detect_thunderbolt_nic()
        .await
        .ok_or(OpenBoltError::CommandFailed("No Thunderbolt NIC found".to_string()))?;

    let host_octet = pick_host_octet();
    let target_ip = format!("10.99.99.{host_octet}");

    #[cfg(target_os = "windows")]
    set_windows_static_ip(&nic.name, &target_ip).await?;

    #[cfg(target_os = "macos")]
    set_macos_static_ip(&nic.friendly_name, &target_ip).await?;

    Ok(target_ip)
}

fn pick_host_octet() -> u8 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(128);

    2 + (nanos % 252) as u8
}

#[cfg(target_os = "windows")]
async fn set_windows_static_ip(adapter_name: &str, ip: &str) -> OpenBoltResult<()> {
    let adapter_name = adapter_name.to_string();
    let ip = ip.to_string();

    let result = tokio::task::spawn_blocking(move || {
        let alias = format!("name=\"{}\"", adapter_name.replace('"', ""));

        // Prefer the newer ipv4 syntax first.
        let first = std::process::Command::new("netsh")
            .args([
                "interface",
                "ipv4",
                "set",
                "address",
                &alias,
                "source=static",
                &format!("address={ip}"),
                "mask=255.255.255.0",
                "gateway=none"
            ])
            .output();

        if let Ok(output) = first {
            if output.status.success() {
                return Ok::<(), OpenBoltError>(());
            }

            // Fallback for older netsh variants.
            let second = std::process::Command::new("netsh")
                .args([
                    "interface",
                    "ip",
                    "set",
                    "address",
                    &alias,
                    "static",
                    &ip,
                    "255.255.255.0"
                ])
                .output();

            if let Ok(legacy) = second {
                if legacy.status.success() {
                    return Ok(());
                }

                let stderr = String::from_utf8_lossy(&legacy.stderr);
                let stdout = String::from_utf8_lossy(&legacy.stdout);
                return Err(OpenBoltError::CommandFailed(format!(
                    "failed to set static ip via netsh (legacy syntax). adapter={adapter_name}, ip={ip}, stderr={stderr}, stdout={stdout}. Ensure OpenBolt is running as Administrator"
                )));
            }

            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            return Err(OpenBoltError::CommandFailed(format!(
                "failed to set static ip via netsh. adapter={adapter_name}, ip={ip}, stderr={stderr}, stdout={stdout}. Ensure OpenBolt is running as Administrator"
            )));
        }

        Err(OpenBoltError::CommandFailed(
            "failed to launch netsh command".to_string()
        ))
    })
    .await
    .map_err(|e| OpenBoltError::CommandFailed(e.to_string()))?;

    result
}

#[cfg(target_os = "macos")]
async fn set_macos_static_ip(interface_name: &str, ip: &str) -> OpenBoltResult<()> {
    let interface_name = interface_name.to_string();
    let ip = ip.to_string();

    let output = tokio::task::spawn_blocking(move || {
        std::process::Command::new("networksetup")
            .args(["-setmanual", &interface_name, &ip, "255.255.255.0"])
            .output()
    })
    .await
    .map_err(|e| OpenBoltError::CommandFailed(e.to_string()))??;

    if output.status.success() {
        Ok(())
    } else {
        Err(OpenBoltError::CommandFailed(
            String::from_utf8_lossy(&output.stderr).to_string()
        ))
    }
}
