use std::io::Write;
use std::net::Ipv4Addr;
use std::path::Path;

use anyhow::{Context, Result};
use ipnet::Ipv6Net;
use serde::{Deserialize, Serialize};
use tracing::warn;

use aleph_network::types::PortForward;
use aleph_tee::types::VmConfig;

/// Persisted VM metadata — everything needed to reconstruct in-memory state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedVm {
    pub config: VmConfig,
    pub ip: Ipv4Addr,
    pub ipv6: Option<Ipv6Net>,
    pub tap_name: String,
    pub mac_addr: String,
    pub port_forwards: Vec<PortForward>,
    /// Seconds since UNIX epoch when the VM was created.
    pub created_at_epoch: u64,
}

/// Save a VM's metadata to `{state_dir}/{vm_id}.json`.
pub fn save_vm(state_dir: &Path, vm_id: &str, vm: &PersistedVm) -> Result<()> {
    std::fs::create_dir_all(state_dir)
        .with_context(|| format!("failed to create state dir: {}", state_dir.display()))?;
    let path = state_dir.join(format!("{vm_id}.json"));
    let json = serde_json::to_string_pretty(vm)?;
    // Atomic write: write to temp file, fsync, then rename
    let tmp = state_dir.join(format!("{vm_id}.json.tmp"));
    let mut file = std::fs::File::create(&tmp)
        .with_context(|| format!("failed to create {}", tmp.display()))?;
    file.write_all(json.as_bytes())
        .with_context(|| format!("failed to write {}", tmp.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to fsync {}", tmp.display()))?;
    drop(file);
    std::fs::rename(&tmp, &path)
        .with_context(|| format!("failed to rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Load all persisted VMs from `{state_dir}/*.json`.
pub fn load_all_vms(state_dir: &Path) -> Result<Vec<PersistedVm>> {
    let mut vms = Vec::new();
    let entries = match std::fs::read_dir(state_dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vms),
        Err(e) => return Err(e).context("failed to read state dir"),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "json") {
            match std::fs::read_to_string(&path) {
                Ok(json) => match serde_json::from_str::<PersistedVm>(&json) {
                    Ok(vm) => vms.push(vm),
                    Err(e) => warn!(path = %path.display(), error = %e, "skipping malformed VM state file"),
                },
                Err(e) => warn!(path = %path.display(), error = %e, "failed to read VM state file"),
            }
        }
    }
    Ok(vms)
}

/// Delete a VM's state file.
pub fn delete_vm(state_dir: &Path, vm_id: &str) -> Result<()> {
    let path = state_dir.join(format!("{vm_id}.json"));
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("failed to delete {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use aleph_tee::types::{TeeConfig, TeeType};

    fn test_vm(vm_id: &str) -> PersistedVm {
        PersistedVm {
            config: VmConfig {
                vm_id: vm_id.to_string(),
                kernel: PathBuf::from("/boot/vmlinuz"),
                initrd: PathBuf::from("/boot/initrd.img"),
                disks: vec![],
                vcpus: 2,
                memory_mb: 1024,
                tee: TeeConfig {
                    backend: TeeType::SevSnp,
                    policy: None,
                },
                encrypted: false,
            },
            ip: Ipv4Addr::new(10, 0, 100, 2),
            ipv6: None,
            tap_name: "tap-test".to_string(),
            mac_addr: "52:54:00:00:64:02".to_string(),
            port_forwards: vec![],
            created_at_epoch: 1709500000,
        }
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let vm = test_vm("vm-001");
        save_vm(dir.path(), "vm-001", &vm).unwrap();

        let loaded = load_all_vms(dir.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].config.vm_id, "vm-001");
        assert_eq!(loaded[0].ip, Ipv4Addr::new(10, 0, 100, 2));
    }

    #[test]
    fn test_delete_vm() {
        let dir = tempfile::tempdir().unwrap();
        let vm = test_vm("vm-002");
        save_vm(dir.path(), "vm-002", &vm).unwrap();
        assert!(dir.path().join("vm-002.json").exists());

        delete_vm(dir.path(), "vm-002").unwrap();
        assert!(!dir.path().join("vm-002.json").exists());
    }

    #[test]
    fn test_delete_nonexistent_ok() {
        let dir = tempfile::tempdir().unwrap();
        delete_vm(dir.path(), "nonexistent").unwrap();
    }

    #[test]
    fn test_load_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let vms = load_all_vms(dir.path()).unwrap();
        assert!(vms.is_empty());
    }

    #[test]
    fn test_load_nonexistent_dir() {
        let vms = load_all_vms(Path::new("/tmp/nonexistent-aleph-test")).unwrap();
        assert!(vms.is_empty());
    }

    #[test]
    fn test_load_skips_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let vm = test_vm("good");
        save_vm(dir.path(), "good", &vm).unwrap();
        std::fs::write(dir.path().join("bad.json"), "not valid json").unwrap();

        let loaded = load_all_vms(dir.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].config.vm_id, "good");
    }

    #[test]
    fn test_save_with_port_forwards() {
        let dir = tempfile::tempdir().unwrap();
        let mut vm = test_vm("vm-ports");
        vm.port_forwards = vec![PortForward {
            vm_id: "vm-ports".to_string(),
            host_port: 8080,
            vm_port: 80,
            protocol: aleph_network::types::Protocol::Tcp,
        }];
        save_vm(dir.path(), "vm-ports", &vm).unwrap();

        let loaded = load_all_vms(dir.path()).unwrap();
        assert_eq!(loaded[0].port_forwards.len(), 1);
        assert_eq!(loaded[0].port_forwards[0].host_port, 8080);
    }
}
