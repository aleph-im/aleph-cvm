use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::Path;

use anyhow::{Context, Result};
use ipnet::Ipv6Net;
use tracing::info;

use aleph_network::bridge::run_ip;

/// Derive a deterministic MAC address from a VM's IP address.
///
/// Format: `52:54:00:{oct2:02x}:{oct3:02x}:{oct4:02x}`
/// where oct2/oct3/oct4 are the last three octets of the IP.
///
/// The `52:54:00` prefix is QEMU's standard locally-administered OUI.
/// This gives unique MACs for any IP in the 10.0.0.0/8 range.
pub fn mac_for_vm_ip(ip: Ipv4Addr) -> String {
    let o = ip.octets();
    format!("52:54:00:{:02x}:{:02x}:{:02x}", o[1], o[2], o[3])
}

/// Write a dnsmasq DHCP host reservation file for a VM.
///
/// Creates a file `{hostsdir}/{vm_id}` containing `{mac},{ip}` so that
/// dnsmasq assigns the expected IP to the VM via DHCP.
///
/// dnsmasq with `--dhcp-hostsdir` watches the directory via inotify
/// and picks up new files automatically (no SIGHUP needed).
pub fn write_dhcp_reservation(hostsdir: &Path, vm_id: &str, mac: &str, ip: Ipv4Addr) -> Result<()> {
    let path = hostsdir.join(vm_id);
    let content = format!("{mac},{ip}\n");
    std::fs::write(&path, &content)
        .with_context(|| format!("failed to write DHCP reservation to {}", path.display()))?;
    info!(vm_id = %vm_id, mac = %mac, ip = %ip, path = %path.display(), "wrote DHCP reservation");
    Ok(())
}

/// Remove a dnsmasq DHCP host reservation file for a VM.
pub fn remove_dhcp_reservation(hostsdir: &Path, vm_id: &str) {
    let path = hostsdir.join(vm_id);
    if let Err(e) = std::fs::remove_file(&path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), error = %e, "failed to remove DHCP reservation");
    }
}

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

/// Allocate a VM IP address by adding `offset` to the gateway IP.
///
/// For example, gateway `10.0.100.1` with offset `5` yields `10.0.100.6`.
pub fn allocate_vm_ip(gateway_ip: Ipv4Addr, offset: u8) -> Ipv4Addr {
    let octets = gateway_ip.octets();
    let ip_u32 = u32::from_be_bytes(octets);
    let vm_u32 = ip_u32.wrapping_add(offset as u32);
    Ipv4Addr::from(vm_u32)
}

/// Add an IPv6 address to a TAP interface.
pub async fn add_ipv6_to_tap(tap_name: &str, ipv6: Ipv6Net) -> Result<()> {
    let addr = format!("{}/{}", ipv6.addr(), ipv6.prefix_len());
    run_ip(&["addr", "add", &addr, "dev", tap_name])
        .await
        .with_context(|| format!("failed to add IPv6 {addr} to {tap_name}"))?;

    info!(tap = %tap_name, ipv6 = %addr, "added IPv6 to TAP");
    Ok(())
}

/// Write a dnsmasq DHCPv6 host reservation file for a VM.
///
/// Creates a file `{hostsdir}/{vm_id}.v6` containing the DHCPv6 reservation
/// in dnsmasq format: `{mac},id:*,[{ipv6}]`
pub fn write_dhcpv6_reservation(
    hostsdir: &Path,
    vm_id: &str,
    mac: &str,
    ipv6: Ipv6Addr,
) -> Result<()> {
    let path = hostsdir.join(format!("{vm_id}.v6"));
    let content = format!("{mac},id:*,[{ipv6}]\n");
    std::fs::write(&path, &content)
        .with_context(|| format!("failed to write DHCPv6 reservation to {}", path.display()))?;
    info!(vm_id = %vm_id, mac = %mac, ipv6 = %ipv6, path = %path.display(), "wrote DHCPv6 reservation");
    Ok(())
}

/// Remove a dnsmasq DHCPv6 host reservation file for a VM.
pub fn remove_dhcpv6_reservation(hostsdir: &Path, vm_id: &str) {
    let path = hostsdir.join(format!("{vm_id}.v6"));
    if let Err(e) = std::fs::remove_file(&path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), error = %e, "failed to remove DHCPv6 reservation");
    }
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

    #[test]
    fn test_mac_for_vm_ip() {
        assert_eq!(
            mac_for_vm_ip(Ipv4Addr::new(10, 0, 100, 2)),
            "52:54:00:00:64:02"
        );
        assert_eq!(
            mac_for_vm_ip(Ipv4Addr::new(10, 0, 100, 10)),
            "52:54:00:00:64:0a"
        );
        assert_eq!(
            mac_for_vm_ip(Ipv4Addr::new(192, 168, 1, 42)),
            "52:54:00:a8:01:2a"
        );
    }

    #[test]
    fn test_write_and_remove_dhcpv6_reservation() {
        let dir = tempfile::tempdir().unwrap();
        let ipv6: Ipv6Addr = "2001:db8::5".parse().unwrap();
        let mac = "52:54:00:00:64:05";

        // Write
        write_dhcpv6_reservation(dir.path(), "test-vm", mac, ipv6).unwrap();
        let content = std::fs::read_to_string(dir.path().join("test-vm.v6")).unwrap();
        assert_eq!(content, "52:54:00:00:64:05,id:*,[2001:db8::5]\n");

        // Remove
        remove_dhcpv6_reservation(dir.path(), "test-vm");
        assert!(!dir.path().join("test-vm.v6").exists());

        // Remove again (should not panic)
        remove_dhcpv6_reservation(dir.path(), "test-vm");
    }

    #[test]
    fn test_write_and_remove_dhcp_reservation() {
        let dir = tempfile::tempdir().unwrap();
        let ip = Ipv4Addr::new(10, 0, 100, 5);
        let mac = mac_for_vm_ip(ip);

        // Write
        write_dhcp_reservation(dir.path(), "test-vm", &mac, ip).unwrap();
        let content = std::fs::read_to_string(dir.path().join("test-vm")).unwrap();
        assert_eq!(content, "52:54:00:00:64:05,10.0.100.5\n");

        // Remove
        remove_dhcp_reservation(dir.path(), "test-vm");
        assert!(!dir.path().join("test-vm").exists());

        // Remove again (should not panic)
        remove_dhcp_reservation(dir.path(), "test-vm");
    }
}
