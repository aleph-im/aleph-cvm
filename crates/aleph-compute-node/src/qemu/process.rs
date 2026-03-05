use anyhow::{Context, Result};
use tracing::info;

use super::args::QemuPaths;
use crate::systemd;

/// A QEMU process managed by systemd.
///
/// Instead of holding a `Child` process directly, this delegates to a
/// systemd transient unit. The QEMU process survives orchestrator restarts.
pub struct QemuProcess {
    pub paths: QemuPaths,
    pub vm_id: String,
}

impl QemuProcess {
    /// Start a QEMU process as a systemd transient unit.
    ///
    /// Creates the VM runtime directory, then delegates to `systemd-run`.
    pub fn spawn(args: &[String], paths: QemuPaths, vm_id: String) -> Result<Self> {
        // Create the runtime directory for QMP socket, serial log, etc.
        let vm_dir = paths
            .qmp_socket
            .parent()
            .context("qmp_socket path has no parent")?;
        std::fs::create_dir_all(vm_dir)
            .with_context(|| format!("failed to create VM runtime dir: {}", vm_dir.display()))?;

        // Clean up any leftover failed unit from a previous run
        systemd::reset_failed_unit(&vm_id);

        systemd::start_vm_unit(&vm_id, args, vm_dir)?;

        info!(vm_id = %vm_id, "QEMU started via systemd");

        Ok(Self { paths, vm_id })
    }

    /// Reconnect to an existing systemd-managed QEMU process.
    ///
    /// Used during recovery: the unit is already running, we just need
    /// to recreate the in-memory handle.
    pub fn reconnect(paths: QemuPaths, vm_id: String) -> Result<Self> {
        if !systemd::is_unit_active(&vm_id) {
            anyhow::bail!("systemd unit for VM {vm_id} is not active");
        }
        info!(vm_id = %vm_id, "reconnected to running QEMU systemd unit");
        Ok(Self { paths, vm_id })
    }

    /// Check if the underlying systemd unit is still active.
    pub fn is_running(&self) -> bool {
        systemd::is_unit_active(&self.vm_id)
    }

    /// Stop the QEMU process via systemd.
    pub fn stop(&self) -> Result<()> {
        systemd::stop_vm_unit(&self.vm_id)
    }
}

impl Drop for QemuProcess {
    fn drop(&mut self) {
        // Do NOT stop the unit on drop — the whole point is that
        // QEMU survives orchestrator restarts. Only explicit
        // delete_vm() calls should stop the unit.
    }
}
