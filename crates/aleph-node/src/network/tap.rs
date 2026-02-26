use std::net::Ipv4Addr;

use anyhow::{Context, Result};
use tracing::info;

/// Create a TAP interface and attach it to a bridge.
///
/// Runs the following commands:
/// 1. `ip tuntap add dev {tap_name} mode tap`
/// 2. `ip link set {tap_name} up`
/// 3. `ip link set {tap_name} master {bridge}`
pub async fn create_tap(tap_name: &str, bridge: &str) -> Result<()> {
    run_ip(&["tuntap", "add", "dev", tap_name, "mode", "tap"])
        .await
        .with_context(|| format!("failed to create TAP device {tap_name}"))?;

    run_ip(&["link", "set", tap_name, "up"])
        .await
        .with_context(|| format!("failed to bring up TAP device {tap_name}"))?;

    run_ip(&["link", "set", tap_name, "master", bridge])
        .await
        .with_context(|| format!("failed to attach {tap_name} to bridge {bridge}"))?;

    info!(tap = %tap_name, bridge = %bridge, "TAP interface created");
    Ok(())
}

/// Delete a TAP interface.
pub async fn delete_tap(tap_name: &str) -> Result<()> {
    run_ip(&["link", "delete", tap_name])
        .await
        .with_context(|| format!("failed to delete TAP device {tap_name}"))?;

    info!(tap = %tap_name, "TAP interface deleted");
    Ok(())
}

/// Ensure a bridge interface exists with the given IP address.
///
/// If the bridge already exists, this is a no-op (the `ip link add` will
/// fail with "File exists", which we ignore). We always attempt to assign
/// the address and bring the link up.
pub async fn ensure_bridge(bridge: &str, ip: Ipv4Addr, prefix_len: u8) -> Result<()> {
    // Create bridge (ignore "already exists" errors)
    let _ = run_ip(&["link", "add", bridge, "type", "bridge"]).await;

    // Assign address (ignore "already assigned" errors)
    let addr = format!("{ip}/{prefix_len}");
    let _ = run_ip(&["addr", "add", &addr, "dev", bridge]).await;

    // Bring it up
    run_ip(&["link", "set", bridge, "up"])
        .await
        .with_context(|| format!("failed to bring up bridge {bridge}"))?;

    info!(bridge = %bridge, addr = %addr, "bridge ensured");
    Ok(())
}

/// Allocate a VM IP address by adding `offset` to the gateway IP.
///
/// For example, gateway `10.0.100.1` with offset `5` yields `10.0.100.6`.
pub fn allocate_vm_ip(gateway_ip: Ipv4Addr, offset: u8) -> Ipv4Addr {
    let octets = gateway_ip.octets();
    let ip_u32 = u32::from_be_bytes(octets);
    let vm_u32 = ip_u32.wrapping_add(offset as u32);
    Ipv4Addr::from(vm_u32)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allocate_vm_ip() {
        let gw = Ipv4Addr::new(10, 0, 100, 1);
        assert_eq!(allocate_vm_ip(gw, 1), Ipv4Addr::new(10, 0, 100, 2));
        assert_eq!(allocate_vm_ip(gw, 5), Ipv4Addr::new(10, 0, 100, 6));
        assert_eq!(allocate_vm_ip(gw, 0), Ipv4Addr::new(10, 0, 100, 1));
    }
}
