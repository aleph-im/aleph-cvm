use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use serde::Serialize;
use tokio::sync::RwLock;
use tracing::{error, info};

use aleph_tee::traits::TeeBackend;
use aleph_tee::types::VmConfig;

use crate::network;
use crate::qemu::args::{build_qemu_command, QemuPaths};
use crate::qemu::process::QemuProcess;
use crate::vm::lifecycle::VmState;

/// Internal handle for a managed VM.
struct VmHandle {
    config: VmConfig,
    state: VmState,
    ip: Ipv4Addr,
    process: Option<QemuProcess>,
    tap_name: String,
    created_at: Instant,
}

/// JSON-serializable VM information returned by the API.
#[derive(Debug, Clone, Serialize)]
pub struct VmInfo {
    pub vm_id: String,
    pub status: String,
    pub ip: String,
    pub tee: String,
    pub uptime_secs: u64,
}

impl VmInfo {
    fn from_handle(handle: &VmHandle) -> Self {
        Self {
            vm_id: handle.config.vm_id.clone(),
            status: handle.state.to_string(),
            ip: handle.ip.to_string(),
            tee: format!("{:?}", handle.config.tee.backend),
            uptime_secs: handle.created_at.elapsed().as_secs(),
        }
    }
}

/// Manages the lifecycle of confidential VMs.
pub struct VmManager {
    vms: RwLock<HashMap<String, VmHandle>>,
    run_dir: PathBuf,
    bridge: String,
    gateway_ip: Ipv4Addr,
    next_ip_offset: RwLock<u8>,
    tee_backend: Arc<dyn TeeBackend>,
}

impl VmManager {
    /// Create a new VM manager.
    pub fn new(
        run_dir: PathBuf,
        bridge: String,
        gateway_ip: Ipv4Addr,
        tee_backend: Arc<dyn TeeBackend>,
    ) -> Self {
        Self {
            vms: RwLock::new(HashMap::new()),
            run_dir,
            bridge,
            gateway_ip,
            next_ip_offset: RwLock::new(1),
            tee_backend,
        }
    }

    /// Create and start a new VM.
    pub async fn create_vm(&self, config: VmConfig) -> Result<VmInfo> {
        let vm_id = config.vm_id.clone();

        // Check for duplicate
        {
            let vms = self.vms.read().await;
            if vms.contains_key(&vm_id) {
                anyhow::bail!("VM {vm_id} already exists");
            }
        }

        // Allocate IP
        let offset = {
            let mut off = self.next_ip_offset.write().await;
            let current = *off;
            *off = off.wrapping_add(1);
            current
        };
        let vm_ip = network::allocate_vm_ip(self.gateway_ip, offset);

        // Create TAP interface
        let tap_name = format!("tap-{}", &vm_id);
        if let Err(e) = network::create_tap(&tap_name, &self.bridge).await {
            error!(vm_id = %vm_id, error = %e, "failed to create TAP interface");
            return Err(e);
        }

        // Build QEMU command
        let paths = QemuPaths::for_vm(&self.run_dir, &vm_id);
        let mut args = vec!["qemu-system-x86_64".to_string()];
        args.extend(build_qemu_command(
            &config,
            &paths,
            &tap_name,
            self.tee_backend.as_ref(),
        ));

        // Spawn QEMU
        let process = match QemuProcess::spawn(&args, paths, vm_id.clone()) {
            Ok(p) => p,
            Err(e) => {
                error!(vm_id = %vm_id, error = %e, "failed to spawn QEMU");
                // Clean up TAP on failure
                let _ = network::delete_tap(&tap_name).await;
                return Err(e);
            }
        };

        // Mark as Running immediately (no health check polling yet)
        let handle = VmHandle {
            config,
            state: VmState::Running,
            ip: vm_ip,
            process: Some(process),
            tap_name,
            created_at: Instant::now(),
        };

        let info = VmInfo::from_handle(&handle);

        self.vms.write().await.insert(vm_id.clone(), handle);
        info!(vm_id = %vm_id, ip = %vm_ip, "VM created");

        Ok(info)
    }

    /// Get information about a specific VM.
    pub async fn get_vm(&self, id: &str) -> Result<VmInfo> {
        let vms = self.vms.read().await;
        let handle = vms
            .get(id)
            .with_context(|| format!("VM {id} not found"))?;
        Ok(VmInfo::from_handle(handle))
    }

    /// Delete a VM, stopping it if running.
    pub async fn delete_vm(&self, id: &str) -> Result<()> {
        let mut vms = self.vms.write().await;
        let mut handle = vms
            .remove(id)
            .with_context(|| format!("VM {id} not found"))?;

        // Kill the QEMU process if still running
        if let Some(ref mut process) = handle.process {
            let _ = process.wait_or_kill(std::time::Duration::from_secs(5));
        }

        // Clean up TAP interface
        let _ = network::delete_tap(&handle.tap_name).await;

        info!(vm_id = %id, "VM deleted");
        Ok(())
    }

    /// List all VMs.
    #[allow(dead_code)]
    pub async fn list_vms(&self) -> Vec<VmInfo> {
        let vms = self.vms.read().await;
        vms.values().map(VmInfo::from_handle).collect()
    }
}
