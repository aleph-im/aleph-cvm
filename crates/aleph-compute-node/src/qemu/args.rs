use std::path::PathBuf;

use aleph_tee::traits::TeeBackend;
use aleph_tee::types::VmConfig;

/// Fixed kernel command line used for all VMs.
///
/// This is deliberately kept constant (no per-VM parameters like IP addresses)
/// so that the SEV-SNP launch measurement is deterministic for a given VM image.
/// The VM obtains its IP via DHCP from the host's dnsmasq instead.
///
/// This constant must match what `sev-snp-measure --append` uses when
/// pre-computing the expected measurement.
pub const KERNEL_CMDLINE: &str = "console=ttyS0 root=/dev/vda ro";

/// Paths to QEMU runtime files for a specific VM.
#[derive(Debug, Clone)]
pub struct QemuPaths {
    pub qmp_socket: PathBuf,
}

impl QemuPaths {
    /// Create paths for a VM under the given run directory.
    pub fn for_vm(run_dir: &std::path::Path, vm_id: &str) -> Self {
        let vm_dir = run_dir.join(vm_id);
        Self {
            qmp_socket: vm_dir.join("qmp.sock"),
        }
    }
}

/// Build the full QEMU command-line argument list.
///
/// Combines base QEMU arguments (KVM, CPU, memory, serial, QMP, network, drives)
/// with TEE-specific arguments from the backend.
///
/// The `mac_addr` is assigned to the virtio-net device so that dnsmasq can
/// map it to a reserved IP via DHCP.
pub fn build_qemu_command(
    config: &VmConfig,
    paths: &QemuPaths,
    tap_name: &str,
    tee_backend: &dyn TeeBackend,
    mac_addr: &str,
) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();

    // Base args
    args.extend([
        "-enable-kvm".into(),
        "-cpu".into(),
        "EPYC-v4".into(),
        "-smp".into(),
        config.vcpus.to_string(),
        "-m".into(),
        format!("{}M", config.memory_mb),
        "-nographic".into(),
        "-no-reboot".into(),
    ]);

    // Kernel direct boot with fixed command line
    args.extend([
        "-kernel".into(),
        config.kernel.display().to_string(),
        "-initrd".into(),
        config.initrd.display().to_string(),
        "-append".into(),
        KERNEL_CMDLINE.into(),
    ]);

    // Serial output to stdout (captured by journald when running under systemd)
    args.extend(["-serial".into(), "stdio".into()]);

    // QMP socket
    args.extend([
        "-qmp".into(),
        format!(
            "unix:{},server,nowait",
            paths.qmp_socket.display()
        ),
    ]);

    // Network (TAP) with explicit MAC for DHCP reservation
    args.extend([
        "-netdev".into(),
        format!(
            "tap,id=net0,ifname={tap_name},script=no,downscript=no"
        ),
        "-device".into(),
        format!("virtio-net-pci,netdev=net0,mac={mac_addr}"),
    ]);

    // Disk drives
    for disk in &config.disks {
        let ro = if disk.readonly { "on" } else { "off" };
        args.extend([
            "-drive".into(),
            format!(
                "file={},format={},if=virtio,readonly={}",
                disk.path.display(),
                disk.format,
                ro,
            ),
        ]);
    }

    // TEE-specific args
    args.extend(tee_backend.qemu_args(config));

    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use aleph_tee::sev_snp::SevSnpBackend;
    use aleph_tee::types::{DiskConfig, TeeConfig, TeeType};
    use std::path::PathBuf;

    fn make_config(disks: Vec<DiskConfig>) -> VmConfig {
        VmConfig {
            vm_id: "test-vm-001".into(),
            kernel: PathBuf::from("/boot/vmlinuz"),
            initrd: PathBuf::from("/boot/initrd.img"),
            disks,
            vcpus: 4,
            memory_mb: 2048,
            tee: TeeConfig {
                backend: TeeType::SevSnp,
                policy: Some("0x30000".into()),
            },
        }
    }

    fn rootfs_disk(path: &str) -> DiskConfig {
        DiskConfig {
            path: PathBuf::from(path),
            readonly: true,
            format: "raw".to_string(),
        }
    }

    const TEST_MAC: &str = "52:54:00:00:64:02";

    #[test]
    fn test_build_command_includes_kernel() {
        let config = make_config(vec![rootfs_disk("/images/rootfs.ext4")]);
        let paths = QemuPaths::for_vm("/run/aleph-cvm".as_ref(), "test-vm-001");
        let backend = SevSnpBackend::new("Genoa");
        let args = build_qemu_command(&config, &paths, "tap0", &backend, TEST_MAC);

        let kernel_idx = args.iter().position(|a| a == "-kernel").expect("-kernel flag missing");
        assert_eq!(args[kernel_idx + 1], "/boot/vmlinuz");
    }

    #[test]
    fn test_build_command_fixed_cmdline() {
        let config = make_config(vec![]);
        let paths = QemuPaths::for_vm("/run/aleph-cvm".as_ref(), "test-vm-001");
        let backend = SevSnpBackend::new("Genoa");
        let args = build_qemu_command(&config, &paths, "tap0", &backend, TEST_MAC);

        let append_idx = args.iter().position(|a| a == "-append").expect("-append flag missing");
        assert_eq!(args[append_idx + 1], KERNEL_CMDLINE);
        assert!(
            !args[append_idx + 1].contains("ip="),
            "cmdline must not contain per-VM IP (breaks measurement determinism)"
        );
    }

    #[test]
    fn test_build_command_includes_mac() {
        let config = make_config(vec![]);
        let paths = QemuPaths::for_vm("/run/aleph-cvm".as_ref(), "test-vm-001");
        let backend = SevSnpBackend::new("Genoa");
        let args = build_qemu_command(&config, &paths, "tap0", &backend, TEST_MAC);

        let device_arg = args
            .iter()
            .find(|a| a.contains("virtio-net-pci"))
            .expect("should have virtio-net-pci arg");
        assert!(
            device_arg.contains(&format!("mac={TEST_MAC}")),
            "virtio-net should have MAC address, got: {device_arg}"
        );
    }

    #[test]
    fn test_build_command_includes_sev_snp() {
        let config = make_config(vec![]);
        let paths = QemuPaths::for_vm("/run/aleph-cvm".as_ref(), "test-vm-001");
        let backend = SevSnpBackend::new("Genoa");
        let args = build_qemu_command(&config, &paths, "tap0", &backend, TEST_MAC);

        assert!(
            args.iter().any(|a| a.contains("sev-snp-guest")),
            "expected sev-snp-guest in args: {args:?}"
        );
    }

    #[test]
    fn test_build_command_no_disks() {
        let config = make_config(vec![]);
        let paths = QemuPaths::for_vm("/run/aleph-cvm".as_ref(), "test-vm-001");
        let backend = SevSnpBackend::new("Genoa");
        let args = build_qemu_command(&config, &paths, "tap0", &backend, TEST_MAC);

        assert!(
            !args.iter().any(|a| a.contains("-drive")),
            "should not have -drive when disks is empty: {args:?}"
        );
    }

    #[test]
    fn test_build_command_multiple_disks() {
        let disks = vec![
            DiskConfig {
                path: PathBuf::from("/images/rootfs.ext4"),
                readonly: true,
                format: "raw".to_string(),
            },
            DiskConfig {
                path: PathBuf::from("/data/volume.qcow2"),
                readonly: false,
                format: "qcow2".to_string(),
            },
        ];
        let config = make_config(disks);
        let paths = QemuPaths::for_vm("/run/aleph-cvm".as_ref(), "test-vm-001");
        let backend = SevSnpBackend::new("Genoa");
        let args = build_qemu_command(&config, &paths, "tap0", &backend, TEST_MAC);

        let drive_args: Vec<&String> = args
            .iter()
            .enumerate()
            .filter_map(|(i, a)| if a == "-drive" { args.get(i + 1) } else { None })
            .collect();
        assert_eq!(drive_args.len(), 2, "should have 2 -drive args: {args:?}");
        assert!(drive_args[0].contains("rootfs.ext4"));
        assert!(drive_args[0].contains("format=raw"));
        assert!(drive_args[0].contains("readonly=on"));
        assert!(drive_args[1].contains("volume.qcow2"));
        assert!(drive_args[1].contains("format=qcow2"));
        assert!(drive_args[1].contains("readonly=off"));
    }

    #[test]
    fn test_qemu_paths() {
        let paths = QemuPaths::for_vm("/run/aleph-cvm".as_ref(), "my-vm");
        assert_eq!(paths.qmp_socket, PathBuf::from("/run/aleph-cvm/my-vm/qmp.sock"));
    }
}
