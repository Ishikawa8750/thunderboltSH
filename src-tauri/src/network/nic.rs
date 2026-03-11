use serde::Serialize;

#[cfg(target_os = "windows")]
use windows::Win32::NetworkManagement::IpHelper::{
    GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH, IF_TYPE_ETHERNET_CSMACD,
    GET_ADAPTERS_ADDRESSES_FLAGS
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThunderboltNic {
    pub name: String,
    pub friendly_name: String
}

pub async fn detect_thunderbolt_nic() -> Option<ThunderboltNic> {
    #[cfg(target_os = "windows")]
    {
        return detect_windows().await;
    }

    #[cfg(target_os = "macos")]
    {
        return detect_macos().await;
    }

    #[allow(unreachable_code)]
    None
}

#[cfg(target_os = "windows")]
async fn detect_windows() -> Option<ThunderboltNic> {
    if let Some(native) = detect_windows_native().await {
        return Some(native);
    }

    tokio::task::spawn_blocking(|| {
        let output = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                r#"Get-NetAdapter | ForEach-Object { "$($_.Name)|$($_.InterfaceDescription)" }"#
            ])
            .output()
            .ok()?;

        let text = String::from_utf8_lossy(&output.stdout);
        text.lines().find_map(|line| {
            let parts: Vec<&str> = line.splitn(2, '|').collect();
            let name = parts.first().map(|s| s.trim()).unwrap_or("").to_string();
            let desc = parts.get(1).map(|s| s.trim()).unwrap_or("").to_string();
            let name_lower = name.to_ascii_lowercase();
            let desc_lower = desc.to_ascii_lowercase();
            if name_lower.contains("thunderbolt")
                || desc_lower.contains("thunderbolt")
                || desc_lower.contains("usb4")
                || name_lower.contains("usb4")
                || desc_lower.contains("p2p network")
                || name.contains("雷电")
                || desc.contains("雷电")
            {
                Some(ThunderboltNic {
                    name: name.clone(),
                    friendly_name: name
                })
            } else {
                None
            }
        })
    })
    .await
    .ok()
    .flatten()
}

#[cfg(target_os = "windows")]
async fn detect_windows_native() -> Option<ThunderboltNic> {
    tokio::task::spawn_blocking(|| {
        let mut size: u32 = 0;

        unsafe {
            let _ = GetAdaptersAddresses(0, GET_ADAPTERS_ADDRESSES_FLAGS(0), None, None, &mut size);
        }

        if size == 0 {
            return None;
        }

        let mut buffer = vec![0u8; size as usize];
        let head = buffer.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH;
        let status = unsafe {
            GetAdaptersAddresses(0, GET_ADAPTERS_ADDRESSES_FLAGS(0), None, Some(head), &mut size)
        };
        if status != 0 {
            return None;
        }

        let mut current = head;
        while !current.is_null() {
            let adapter = unsafe { &*current };
            if adapter.IfType == IF_TYPE_ETHERNET_CSMACD {
                let friendly = unsafe { adapter.FriendlyName.to_string().unwrap_or_default() };
                let description = unsafe { adapter.Description.to_string().unwrap_or_default() };
                let friendly_lower = friendly.to_ascii_lowercase();
                let description_lower = description.to_ascii_lowercase();
                if friendly_lower.contains("thunderbolt")
                    || description_lower.contains("thunderbolt")
                    || description_lower.contains("intel(r) thunderbolt")
                    || friendly.contains("雷电")
                    || description.contains("雷电")
                    || description_lower.contains("usb4")
                    || friendly_lower.contains("usb4")
                    || description_lower.contains("p2p network")
                {
                    let name = if !friendly.is_empty() {
                        friendly.clone()
                    } else {
                        description.clone()
                    };
                    return Some(ThunderboltNic {
                        name,
                        friendly_name: friendly
                    });
                }
            }

            current = adapter.Next;
        }

        None
    })
    .await
    .ok()
    .flatten()
}

#[cfg(target_os = "macos")]
async fn detect_macos() -> Option<ThunderboltNic> {
    tokio::task::spawn_blocking(|| {
        let output = std::process::Command::new("networksetup")
            .arg("-listallhardwareports")
            .output()
            .ok()?;

        let text = String::from_utf8_lossy(&output.stdout);
        for block in text.split("\n\n") {
            let mut hardware_port: Option<String> = None;
            let mut device: Option<String> = None;

            for line in block.lines() {
                let trimmed = line.trim();
                if let Some(value) = trimmed.strip_prefix("Hardware Port: ") {
                    hardware_port = Some(value.trim().to_string());
                } else if let Some(value) = trimmed.strip_prefix("Device: ") {
                    device = Some(value.trim().to_string());
                }
            }

            if let (Some(port), Some(dev)) = (hardware_port, device) {
                if port.contains("Thunderbolt Bridge") && dev == "bridge0" {
                    return Some(ThunderboltNic {
                        name: dev,
                        friendly_name: port
                    });
                }
            }
        }

        None
    })
    .await
    .ok()
    .flatten()
}
