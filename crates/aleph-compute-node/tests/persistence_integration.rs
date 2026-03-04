//! Integration test for VM persistence round-trip.
//!
//! Tests that VmManager can save state and a fresh manager can load it.
//! Does NOT require systemd (recovered VMs will be in Stopped state).

use std::net::Ipv4Addr;
use std::path::PathBuf;

use aleph_compute_node::persistence::{self, PersistedVm};
use aleph_tee::types::{TeeConfig, TeeType, VmConfig};

#[test]
fn test_persistence_roundtrip_multiple_vms() {
    let dir = tempfile::tempdir().unwrap();

    // Save 3 VMs
    for i in 1..=3u8 {
        let vm = PersistedVm {
            config: VmConfig {
                vm_id: format!("vm-{i:03}"),
                kernel: PathBuf::from("/boot/vmlinuz"),
                initrd: PathBuf::from("/boot/initrd.img"),
                disks: vec![],
                vcpus: 2,
                memory_mb: 1024,
                tee: TeeConfig {
                    backend: TeeType::SevSnp,
                    policy: Some("0x30000".to_string()),
                },
            },
            ip: Ipv4Addr::new(10, 0, 100, i + 1),
            ipv6: None,
            tap_name: format!("tap-vm-{i:03}"),
            mac_addr: format!("52:54:00:00:64:{i:02x}"),
            port_forwards: vec![],
            created_at_epoch: 1709500000 + i as u64,
        };
        persistence::save_vm(dir.path(), &format!("vm-{i:03}"), &vm).unwrap();
    }

    // Load all
    let loaded = persistence::load_all_vms(dir.path()).unwrap();
    assert_eq!(loaded.len(), 3);

    // Delete one
    persistence::delete_vm(dir.path(), "vm-002").unwrap();
    let loaded = persistence::load_all_vms(dir.path()).unwrap();
    assert_eq!(loaded.len(), 2);

    // Verify the right one was deleted
    let ids: Vec<&str> = loaded.iter().map(|v| v.config.vm_id.as_str()).collect();
    assert!(ids.contains(&"vm-001"));
    assert!(!ids.contains(&"vm-002"));
    assert!(ids.contains(&"vm-003"));
}
