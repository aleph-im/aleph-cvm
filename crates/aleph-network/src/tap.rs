use anyhow::{Context, Result};
use tracing::info;

use crate::types::TapInterface;

/// Create a TAP interface with dual-stack addressing and attach it to a bridge.
///
/// Port of aleph-vm's `TapInterface.create()`:
/// 1. `ip tuntap add dev {name} mode tap`
/// 2. `ip addr add {host_ipv4} dev {name}`
/// 3. `ip addr add {host_ipv6} dev {name}`
/// 4. `ip link set {name} up`
/// 5. `ip link set {name} master {bridge}`
pub async fn create_tap(interface: &TapInterface, bridge: &str) -> Result<()> {
    // Create TAP device
    run_ip(&["tuntap", "add", "dev", &interface.device_name, "mode", "tap"])
        .await
        .with_context(|| format!("failed to create TAP device {}", interface.device_name))?;

    // Assign IPv4 address (host side)
    let ipv4_addr = format!(
        "{}/{}",
        interface.host_ipv4(),
        interface.ipv4_network.prefix_len()
    );
    run_ip(&["addr", "add", &ipv4_addr, "dev", &interface.device_name])
        .await
        .with_context(|| format!("failed to add IPv4 address to {}", interface.device_name))?;

    // Assign IPv6 address (host side)
    let ipv6_addr = format!(
        "{}/{}",
        interface.host_ipv6(),
        interface.ipv6_network.prefix_len()
    );
    run_ip(&["addr", "add", &ipv6_addr, "dev", &interface.device_name])
        .await
        .with_context(|| format!("failed to add IPv6 address to {}", interface.device_name))?;

    // Bring it up
    run_ip(&["link", "set", &interface.device_name, "up"])
        .await
        .with_context(|| format!("failed to bring up TAP device {}", interface.device_name))?;

    // Attach to bridge
    run_ip(&[
        "link",
        "set",
        &interface.device_name,
        "master",
        bridge,
    ])
    .await
    .with_context(|| {
        format!(
            "failed to attach {} to bridge {}",
            interface.device_name, bridge
        )
    })?;

    info!(
        tap = %interface.device_name,
        ipv4 = %ipv4_addr,
        ipv6 = %ipv6_addr,
        bridge = %bridge,
        "TAP interface created"
    );
    Ok(())
}

/// Delete a TAP interface.
///
/// Port of aleph-vm's `TapInterface.delete()`:
/// Sleeps briefly to avoid EBUSY, then deletes.
pub async fn delete_tap(interface: &TapInterface) -> Result<()> {
    // Brief sleep to avoid "Device or resource busy"
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    run_ip(&["link", "delete", &interface.device_name])
        .await
        .with_context(|| format!("failed to delete TAP device {}", interface.device_name))?;

    info!(tap = %interface.device_name, "TAP interface deleted");
    Ok(())
}

/// Run an `ip` command and return an error if it fails.
async fn run_ip(args: &[&str]) -> Result<()> {
    let output = tokio::process::Command::new("ip")
        .args(args)
        .output()
        .await
        .context("failed to execute `ip` command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("`ip {}` failed: {}", args.join(" "), stderr.trim());
    }

    Ok(())
}
