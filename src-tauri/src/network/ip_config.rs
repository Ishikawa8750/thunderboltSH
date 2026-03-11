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
    set_windows_static_ip(&nic, &target_ip).await?;

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
async fn set_windows_static_ip(nic: &nic::ThunderboltNic, ip: &str) -> OpenBoltResult<()> {
    let adapter_name = nic.name.clone();
    let interface_index = nic.interface_index;
    let ip = ip.to_string();

    let result = tokio::task::spawn_blocking(move || {
        let safe_name = adapter_name.replace('"', "");
        let mut last_err = String::new();

        // Strategy 1: netsh via cmd /c — lets the Windows shell handle quoting correctly.
        let cmd1 = format!(
            r#"netsh interface ipv4 set address "{safe_name}" static {ip} 255.255.255.0 none"#
        );
        if let Ok(out) = std::process::Command::new("cmd").args(["/c", &cmd1]).output() {
            if out.status.success() {
                return Ok::<(), OpenBoltError>(());
            }
            last_err = format!(
                "[netsh ipv4] stderr={}, stdout={}",
                String::from_utf8_lossy(&out.stderr).trim(),
                String::from_utf8_lossy(&out.stdout).trim()
            );
        }

        // Strategy 2: legacy netsh syntax via cmd /c.
        let cmd2 = format!(
            r#"netsh interface ip set address "{safe_name}" static {ip} 255.255.255.0"#
        );
        if let Ok(out) = std::process::Command::new("cmd").args(["/c", &cmd2]).output() {
            if out.status.success() {
                return Ok(());
            }
            last_err = format!(
                "[netsh ip] stderr={}, stdout={}",
                String::from_utf8_lossy(&out.stderr).trim(),
                String::from_utf8_lossy(&out.stdout).trim()
            );
        }

        // Strategy 3: PowerShell New-NetIPAddress by InterfaceIndex.
        if let Some(index) = interface_index {
            let ps_script = format!(
                concat!(
                    "Remove-NetIPAddress -InterfaceIndex {index} -AddressFamily IPv4 -Confirm:$false -ErrorAction SilentlyContinue;",
                    "New-NetIPAddress -InterfaceIndex {index} -IPAddress '{ip}' -PrefixLength 24 -ErrorAction Stop | Out-Null"
                ),
                index = index,
                ip = ip
            );
            if let Ok(out) = std::process::Command::new("powershell")
                .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &ps_script])
                .output()
            {
                if out.status.success() {
                    return Ok(());
                }
                last_err = format!(
                    "[PowerShell] stderr={}, stdout={}",
                    String::from_utf8_lossy(&out.stderr).trim(),
                    String::from_utf8_lossy(&out.stdout).trim()
                );
            }
        }

        Err(OpenBoltError::CommandFailed(format!(
            "all strategies failed to set static ip. adapter={safe_name}, ip={ip}, last_error={last_err}. Ensure OpenBolt is running as Administrator"
        )))
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
