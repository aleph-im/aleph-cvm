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
pub fn start_vm_unit(
    vm_id: &str,
    qemu_args: &[String],
    run_dir: &std::path::Path,
    rw_dirs: &[&std::path::Path],
) -> Result<()> {
    let unit = unit_name(vm_id);
    let (program, args) = qemu_args
        .split_first()
        .context("empty qemu args")?;

    // ReadWritePaths: VM runtime dir (QMP socket) + any writable disk directories.
    let mut rw_path_list = vec![run_dir.display().to_string()];
    for dir in rw_dirs {
        rw_path_list.push(dir.display().to_string());
    }
    let rw_paths = format!("ReadWritePaths={}", rw_path_list.join(" "));

    let mut cmd = std::process::Command::new("systemd-run");
    cmd.args([
        "--unit", &unit,
        // Lifecycle
        "--property", "Type=simple",
        "--property", "Restart=on-failure",
        "--property", "RestartSec=5s",
        "--property", "KillMode=mixed",
        "--property", "TimeoutStopSec=30",
        // Logging
        "--property", &format!("SyslogIdentifier={unit}"),
        "--property", "StandardOutput=journal",
        "--property", "StandardError=journal",
        // Sandboxing — restrict QEMU's capabilities and filesystem access.
        "--property", "NoNewPrivileges=true",
        "--property", "ProtectSystem=strict",
        "--property", &rw_paths,
        "--property", "ProtectHome=true",
        "--property", "ProtectKernelTunables=true",
        "--property", "ProtectKernelModules=true",
        "--property", "ProtectControlGroups=true",
        // Device access — only allow what QEMU needs
        "--property", "DevicePolicy=closed",
        "--property", "DeviceAllow=/dev/kvm rw",
        "--property", "DeviceAllow=/dev/sev-guest rw",
        "--property", "DeviceAllow=/dev/sev rw",
        "--property", "DeviceAllow=/dev/null rw",
        "--property", "DeviceAllow=/dev/urandom r",
        "--property", "DeviceAllow=/dev/net/tun rw",
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
