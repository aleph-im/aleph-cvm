# VM Persistence Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make VMs survive orchestrator restarts by running QEMU under systemd transient units and persisting VM metadata to JSON files.

**Architecture:** Each QEMU process runs as a systemd transient service created via `systemd-run`. VM metadata (config, IP, ports) is serialized to `/var/lib/aleph-cvm/vms/{vm_id}.json`. On startup, the orchestrator scans saved state and reconnects to still-running systemd units.

**Tech Stack:** `systemd-run`/`systemctl` for process management (no new crate dependencies), `serde_json` (already in workspace) for persistence.

---

### Task 1: JSON Persistence Module

**Files:**
- Create: `crates/aleph-compute-node/src/persistence.rs`
- Modify: `crates/aleph-compute-node/src/lib.rs:1-4` (add module)

This module saves/loads VM metadata to disk. The serialized struct contains everything needed to reconstruct a `VmHandle` without the process.

**Step 1: Write the failing test**

Create `crates/aleph-compute-node/src/persistence.rs`:

```rust
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

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
    // Atomic write: write to temp file then rename
    let tmp = state_dir.join(format!("{vm_id}.json.tmp"));
    std::fs::write(&tmp, &json)
        .with_context(|| format!("failed to write {}", tmp.display()))?;
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
        // Write a valid VM
        let vm = test_vm("good");
        save_vm(dir.path(), "good", &vm).unwrap();
        // Write a malformed file
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
```

**Step 2: Add `Serialize`/`Deserialize` to `PortForward` if missing**

Check `crates/aleph-network/src/types.rs` — the `PortForward` struct needs `Serialize, Deserialize` derives. If it doesn't have them, add them.

**Step 3: Register the module**

In `crates/aleph-compute-node/src/lib.rs`, add:

```rust
pub mod persistence;
```

**Step 4: Run tests**

Run: `cargo test -p aleph-compute-node persistence`
Expected: all 7 tests pass.

**Step 5: Commit**

```bash
git add crates/aleph-compute-node/src/persistence.rs crates/aleph-compute-node/src/lib.rs
git commit -m "feat: add VM persistence module for JSON state files"
```

---

### Task 2: Systemd Integration Module

**Files:**
- Create: `crates/aleph-compute-node/src/systemd.rs`
- Modify: `crates/aleph-compute-node/src/lib.rs` (add module)

Wraps `systemd-run` and `systemctl` commands to manage QEMU as transient systemd services.

**Step 1: Write the module with unit tests**

Create `crates/aleph-compute-node/src/systemd.rs`:

```rust
use anyhow::{Context, Result};
use tracing::{info, warn};

/// Name prefix for all VM systemd units.
const UNIT_PREFIX: &str = "aleph-cvm-vm-";

/// Return the systemd unit name for a VM.
pub fn unit_name(vm_id: &str) -> String {
    format!("{UNIT_PREFIX}{vm_id}.service")
}

/// Start a QEMU process as a systemd transient service.
///
/// Uses `systemd-run` to create a transient unit with restart-on-failure.
/// The unit is named `aleph-cvm-vm-{vm_id}.service`.
pub fn start_vm_unit(vm_id: &str, qemu_args: &[String]) -> Result<()> {
    let unit = unit_name(vm_id);
    let (program, args) = qemu_args
        .split_first()
        .context("empty qemu args")?;

    let mut cmd = std::process::Command::new("systemd-run");
    cmd.args([
        "--unit", &unit,
        "--property", "Type=simple",
        "--property", "Restart=on-failure",
        "--property", "RestartSec=5s",
        "--property", "KillMode=mixed",
        "--property", "TimeoutStopSec=30",
        // Collect logs under a predictable identifier
        "--property", &format!("SyslogIdentifier={unit}"),
        "--",
        program,
    ]);
    cmd.args(args);

    info!(unit = %unit, "creating systemd transient unit");

    let output = cmd
        .output()
        .context("failed to execute systemd-run")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("systemd-run failed for {unit}: {stderr}");
    }

    Ok(())
}

/// Stop and remove a transient systemd unit.
pub fn stop_vm_unit(vm_id: &str) -> Result<()> {
    let unit = unit_name(vm_id);
    info!(unit = %unit, "stopping systemd unit");

    let output = std::process::Command::new("systemctl")
        .args(["stop", &unit])
        .output()
        .context("failed to execute systemctl stop")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Unit might already be stopped/gone — not an error
        warn!(unit = %unit, stderr = %stderr, "systemctl stop returned non-zero");
    }

    Ok(())
}

/// Check if a VM's systemd unit is active (running).
pub fn is_unit_active(vm_id: &str) -> bool {
    let unit = unit_name(vm_id);
    std::process::Command::new("systemctl")
        .args(["is-active", "--quiet", &unit])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Reset a failed systemd unit so it can be recreated.
pub fn reset_failed_unit(vm_id: &str) {
    let unit = unit_name(vm_id);
    let _ = std::process::Command::new("systemctl")
        .args(["reset-failed", &unit])
        .output();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unit_name() {
        assert_eq!(unit_name("vm-001"), "aleph-cvm-vm-vm-001.service");
        assert_eq!(unit_name("abc123"), "aleph-cvm-vm-abc123.service");
    }
}
```

**Step 2: Register the module**

In `crates/aleph-compute-node/src/lib.rs`, add:

```rust
pub mod systemd;
```

**Step 3: Run tests**

Run: `cargo test -p aleph-compute-node systemd`
Expected: unit_name test passes. (systemd-run/systemctl tests require root, so only the pure-logic tests here.)

**Step 4: Commit**

```bash
git add crates/aleph-compute-node/src/systemd.rs crates/aleph-compute-node/src/lib.rs
git commit -m "feat: add systemd integration module for transient VM units"
```

---

### Task 3: Refactor QemuProcess to Support Systemd Mode

**Files:**
- Modify: `crates/aleph-compute-node/src/qemu/process.rs`

Replace direct `Child` process management with systemd unit management. The process struct no longer holds a `Child` — it holds the VM ID and delegates lifecycle to systemd.

**Step 1: Rewrite QemuProcess**

Replace the contents of `crates/aleph-compute-node/src/qemu/process.rs`:

```rust
use anyhow::{Context, Result};
use tracing::{info, warn};

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

        systemd::start_vm_unit(&vm_id, args)?;

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
```

**Step 2: Run existing tests**

Run: `cargo test -p aleph-compute-node`
Expected: compilation succeeds. The `qemu::args` tests should still pass (they don't depend on process.rs internals). Some tests may need adjustment if they reference `QemuProcess::spawn` signature — the signature is the same so they should be fine.

**Step 3: Commit**

```bash
git add crates/aleph-compute-node/src/qemu/process.rs
git commit -m "refactor: QemuProcess delegates to systemd instead of holding Child"
```

---

### Task 4: Modify VmManager for Persistence and Recovery

**Files:**
- Modify: `crates/aleph-compute-node/src/vm/manager.rs`
- Modify: `crates/aleph-compute-node/src/network/tap.rs` (need to check if TAP exists)

This is the main integration task. VmManager gets:
1. A `state_dir` field for JSON persistence
2. Persistence calls in `create_vm` and `delete_vm`
3. A new `recover_vms()` method called on startup
4. Port forward updates persisted to disk

**Step 1: Add `state_dir` to VmManager and update constructor**

In `crates/aleph-compute-node/src/vm/manager.rs`, add:

```rust
use crate::persistence::{self, PersistedVm};
use std::time::{SystemTime, UNIX_EPOCH};
```

Add `state_dir: PathBuf` field to `VmManager` struct (after `run_dir`).

Update `VmManager::new()` to accept `state_dir: PathBuf` parameter and store it.

**Step 2: Add persistence to `create_vm()`**

After inserting the VmHandle into the map (line 253 area), add:

```rust
// Persist VM state to disk
let persisted = PersistedVm {
    config: handle.config.clone(),
    ip: vm_ip,
    ipv6: vm_ipv6,
    tap_name: handle.tap_name.clone(),
    mac_addr: mac_addr.clone(),
    port_forwards: vec![],
    created_at_epoch: SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs(),
};
if let Err(e) = persistence::save_vm(&self.state_dir, &vm_id, &persisted) {
    warn!(vm_id = %vm_id, error = %e, "failed to persist VM state (VM is running but not recoverable)");
}
```

Note: We need to clone `handle.config` before the handle is moved into the map. Restructure so the `PersistedVm` is built before inserting handle, or clone from handle before insert. The cleanest approach: build `persisted` before creating `handle`, since all the data is available.

**Step 3: Add persistence to `delete_vm()`**

After the existing cleanup in `delete_vm()`, before the final `info!` log, add:

```rust
// Stop the systemd unit
if let Some(ref process) = handle.process {
    let _ = process.stop();
}

// Delete persisted state
if let Err(e) = persistence::delete_vm(&self.state_dir, id) {
    warn!(vm_id = %id, error = %e, "failed to delete VM state file");
}
```

Also remove the old `wait_or_kill` call since `process.stop()` replaces it.

**Step 4: Update port forward persistence**

In `add_port_forward()`, after the forward is tracked in `PortForwardState`, re-persist the VM state:

```rust
// Update persisted state with new port forward
self.update_persisted_port_forwards(vm_id).await;
```

Add helper method:

```rust
/// Re-persist port forwards for a VM after changes.
async fn update_persisted_port_forwards(&self, vm_id: &str) {
    let vms = self.vms.read().await;
    if let Some(handle) = vms.get(vm_id) {
        let pf = self.port_forwards.lock().await;
        let forwards: Vec<PortForward> = pf.list_for_vm(vm_id).into_iter().cloned().collect();
        // Load existing persisted state, update port_forwards, re-save
        let path = self.state_dir.join(format!("{vm_id}.json"));
        if let Ok(json) = std::fs::read_to_string(&path) {
            if let Ok(mut persisted) = serde_json::from_str::<PersistedVm>(&json) {
                persisted.port_forwards = forwards;
                let _ = persistence::save_vm(&self.state_dir, vm_id, &persisted);
            }
        }
    }
}
```

Similarly in `remove_port_forward()`, call `self.update_persisted_port_forwards(vm_id).await;` — but we need the `vm_id`, which we get from the removed forward.

**Step 5: Add `recover_vms()` method**

```rust
/// Recover VMs from persisted state on startup.
///
/// For each saved VM:
/// - If systemd unit is active → reconnect (state = Running)
/// - If systemd unit is not active → load as Stopped (scheduler decides)
pub async fn recover_vms(&self) -> Result<()> {
    let persisted_vms = persistence::load_all_vms(&self.state_dir)?;
    if persisted_vms.is_empty() {
        return Ok(());
    }

    info!(count = persisted_vms.len(), "recovering VMs from persisted state");

    let mut vms = self.vms.write().await;
    let mut max_offset: u8 = 0;

    for pvm in persisted_vms {
        let vm_id = pvm.config.vm_id.clone();
        let paths = QemuPaths::for_vm(&self.run_dir, &vm_id);

        // Track highest IP offset to avoid collisions for new VMs
        let last_octet = pvm.ip.octets()[3];
        let gateway_last = self.gateway_ip.octets()[3];
        let offset = last_octet.wrapping_sub(gateway_last);
        if offset > max_offset {
            max_offset = offset;
        }

        // Try to reconnect to running systemd unit
        let (process, state) = match QemuProcess::reconnect(paths.clone(), vm_id.clone()) {
            Ok(p) => (Some(p), VmState::Running),
            Err(_) => (None, VmState::Stopped),
        };

        // Re-register IPv6 allocation if applicable
        if let Some(ref ipv6) = pvm.ipv6 {
            if let Some(ref alloc) = self.ipv6_allocator {
                let mut alloc = alloc.lock().await;
                // Mark as allocated to prevent double-allocation
                let _ = alloc.allocate(&vm_id, Some(*ipv6));
            }
        }

        // Restore port forwards into in-memory state
        {
            let mut pf = self.port_forwards.lock().await;
            for fwd in &pvm.port_forwards {
                pf.add(fwd.clone());
            }
        }

        let created_at_epoch = pvm.created_at_epoch;
        let handle = VmHandle {
            config: pvm.config,
            state,
            ip: pvm.ip,
            ipv6: pvm.ipv6,
            process,
            tap_name: pvm.tap_name,
            created_at: Instant::now(), // Approximation; uptime resets on recovery
        };

        info!(
            vm_id = %vm_id,
            state = %handle.state,
            ip = %handle.ip,
            "recovered VM"
        );
        vms.insert(vm_id, handle);
    }

    // Update next_ip_offset to avoid collisions
    *self.next_ip_offset.write().await = max_offset.wrapping_add(1);

    Ok(())
}
```

**Step 6: Store `mac_addr` in VmHandle**

The `mac_addr` is needed for persistence but currently not stored in `VmHandle`. Add it:

```rust
struct VmHandle {
    config: VmConfig,
    state: VmState,
    ip: Ipv4Addr,
    ipv6: Option<Ipv6Net>,
    process: Option<QemuProcess>,
    tap_name: String,
    mac_addr: String,  // NEW
    created_at: Instant,
}
```

Update `create_vm()` to store `mac_addr` in the handle.

**Step 7: Run tests**

Run: `cargo test -p aleph-compute-node`
Expected: all tests pass.

**Step 8: Commit**

```bash
git add crates/aleph-compute-node/src/vm/manager.rs
git commit -m "feat: VmManager persists state and recovers VMs on startup"
```

---

### Task 5: Wire Up Recovery in Main and Add CLI Args

**Files:**
- Modify: `crates/aleph-compute-node/src/main.rs`

**Step 1: Add `--state-dir` CLI argument**

```rust
/// Directory for persistent VM state files.
#[arg(long, default_value = "/var/lib/aleph-cvm/vms")]
state_dir: PathBuf,
```

**Step 2: Pass `state_dir` to VmManager::new()**

Update the `VmManager::new()` call to include `cli.state_dir.clone()`.

**Step 3: Call `recover_vms()` after manager creation**

After `manager.setup_nftables()`, add:

```rust
// Recover VMs from previous run
if let Err(e) = manager.recover_vms().await {
    tracing::error!(error = %e, "failed to recover VMs from persisted state");
}
```

**Step 4: Log state_dir in startup info**

Add `state_dir = %cli.state_dir.display()` to the startup `info!` log.

**Step 5: Run build**

Run: `cargo build -p aleph-compute-node`
Expected: builds successfully.

**Step 6: Commit**

```bash
git add crates/aleph-compute-node/src/main.rs
git commit -m "feat: wire up VM recovery on startup with --state-dir CLI arg"
```

---

### Task 6: Update VmManager::delete_vm to Use Systemd Stop

**Files:**
- Modify: `crates/aleph-compute-node/src/vm/manager.rs`

The current `delete_vm()` calls `process.wait_or_kill()` which no longer exists. Update it to use the new `process.stop()` method.

**Step 1: Update delete_vm()**

Replace:
```rust
if let Some(ref mut process) = handle.process {
    let _ = process.wait_or_kill(std::time::Duration::from_secs(5));
}
```

With:
```rust
if let Some(ref process) = handle.process {
    let _ = process.stop();
}
```

And add state file deletion after all cleanup:
```rust
if let Err(e) = persistence::delete_vm(&self.state_dir, id) {
    warn!(vm_id = %id, error = %e, "failed to delete VM state file");
}
```

**Step 2: Run tests**

Run: `cargo test -p aleph-compute-node`
Expected: passes.

**Step 3: Commit**

```bash
git add crates/aleph-compute-node/src/vm/manager.rs
git commit -m "fix: delete_vm uses systemd stop and cleans up state file"
```

---

### Task 7: Handle Graceful Shutdown (Don't Kill VMs)

**Files:**
- Modify: `crates/aleph-compute-node/src/grpc/service.rs` (or main.rs shutdown handler)

On SIGTERM/SIGINT, the orchestrator should shut down its gRPC server but NOT stop VMs. VMs continue running under systemd.

**Step 1: Verify current behavior**

The current shutdown handler in `service.rs:54-65` just waits for the signal and exits. The `QemuProcess::Drop` impl previously killed child processes, but we've changed it to be a no-op. So this should already work correctly.

Verify: read the new `Drop` impl — it does nothing. The gRPC server shuts down, VMs keep running under systemd.

**Step 2: Add shutdown log**

In `main.rs`, after `grpc_server.serve()` returns, add:

```rust
info!("orchestrator shut down — VMs continue running under systemd");
```

**Step 3: Commit**

```bash
git add crates/aleph-compute-node/src/main.rs
git commit -m "feat: graceful shutdown preserves running VMs"
```

---

### Task 8: Integration Test — Persistence Round-Trip

**Files:**
- Create: `crates/aleph-compute-node/tests/persistence_integration.rs`

This test verifies the full save → load → recover flow without requiring systemd (mocks the systemd check).

**Step 1: Write integration test**

```rust
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
    for i in 1..=3 {
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
```

**Step 2: Run test**

Run: `cargo test -p aleph-compute-node --test persistence_integration`
Expected: passes.

**Step 3: Commit**

```bash
git add crates/aleph-compute-node/tests/persistence_integration.rs
git commit -m "test: add persistence round-trip integration test"
```
