use std::path::PathBuf;

use aleph_tee::traits::TeeBackend;
use aleph_tee::types::VmConfig;

/// Paths to QEMU runtime files for a specific VM.
#[derive(Debug, Clone)]
pub struct QemuPaths {
    pub qmp_socket: PathBuf,
    pub serial_log: PathBuf,
    #[allow(dead_code)]
    pub pidfile: PathBuf,
}

impl QemuPaths {
    /// Create paths for a VM under the given run directory.
    pub fn for_vm(run_dir: &std::path::Path, vm_id: &str) -> Self {
        let vm_dir = run_dir.join(vm_id);
        Self {
            qmp_socket: vm_dir.join("qmp.sock"),
            serial_log: vm_dir.join("serial.log"),
            pidfile: vm_dir.join("qemu.pid"),
        }
    }
}

/// Build the full QEMU command-line argument list.
///
/// Combines base QEMU arguments (KVM, CPU, memory, serial, QMP, network, drives)
/// with TEE-specific arguments from the backend.
pub fn build_qemu_command(
    config: &VmConfig,
    paths: &QemuPaths,
    tap_name: &str,
    tee_backend: &dyn TeeBackend,
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

    // Kernel direct boot
    args.extend([
        "-kernel".into(),
        config.kernel.display().to_string(),
        "-initrd".into(),
        config.initrd.display().to_string(),
        "-append".into(),
        "console=ttyS0 root=/dev/vda ro".into(),
    ]);

    // Serial output
    args.extend([
        "-serial".into(),
        format!("file:{}", paths.serial_log.display()),
    ]);

    // QMP socket
    args.extend([
        "-qmp".into(),
        format!(
            "unix:{},server,nowait",
            paths.qmp_socket.display()
        ),
    ]);

    // Network (TAP)
    args.extend([
        "-netdev".into(),
        format!(
            "tap,id=net0,ifname={tap_name},script=no,downscript=no"
        ),
        "-device".into(),
        "virtio-net-pci,netdev=net0".into(),
    ]);

    // Rootfs drive (optional)
    if let Some(ref rootfs) = config.rootfs {
        args.extend([
            "-drive".into(),
            format!(
                "file={},format=raw,if=virtio,readonly=on",
                rootfs.display()
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
    use aleph_tee::types::{TeeConfig, TeeType};
    use std::path::PathBuf;

    fn make_config(rootfs: Option<&str>) -> VmConfig {
        VmConfig {
            vm_id: "test-vm-001".into(),
            kernel: PathBuf::from("/boot/vmlinuz"),
            initrd: PathBuf::from("/boot/initrd.img"),
            rootfs: rootfs.map(PathBuf::from),
            vcpus: 4,
            memory_mb: 2048,
            tee: TeeConfig {
                backend: TeeType::SevSnp,
                policy: Some("0x30000".into()),
            },
        }
    }

    #[test]
    fn test_build_command_includes_kernel() {
        let config = make_config(Some("/images/rootfs.ext4"));
        let paths = QemuPaths::for_vm("/run/aleph-cvm".as_ref(), "test-vm-001");
        let backend = SevSnpBackend::new("Genoa");
        let args = build_qemu_command(&config, &paths, "tap0", &backend);

        let kernel_idx = args.iter().position(|a| a == "-kernel").expect("-kernel flag missing");
        assert_eq!(args[kernel_idx + 1], "/boot/vmlinuz");
    }

    #[test]
    fn test_build_command_includes_sev_snp() {
        let config = make_config(None);
        let paths = QemuPaths::for_vm("/run/aleph-cvm".as_ref(), "test-vm-001");
        let backend = SevSnpBackend::new("Genoa");
        let args = build_qemu_command(&config, &paths, "tap0", &backend);

        assert!(
            args.iter().any(|a| a.contains("sev-snp-guest")),
            "expected sev-snp-guest in args: {args:?}"
        );
    }

    #[test]
    fn test_build_command_without_rootfs() {
        let config = make_config(None);
        let paths = QemuPaths::for_vm("/run/aleph-cvm".as_ref(), "test-vm-001");
        let backend = SevSnpBackend::new("Genoa");
        let args = build_qemu_command(&config, &paths, "tap0", &backend);

        assert!(
            !args.iter().any(|a| a.contains("-drive") || a.contains("rootfs")),
            "should not have -drive or rootfs when rootfs is None: {args:?}"
        );
    }

    #[test]
    fn test_qemu_paths() {
        let paths = QemuPaths::for_vm("/run/aleph-cvm".as_ref(), "my-vm");
        assert_eq!(paths.qmp_socket, PathBuf::from("/run/aleph-cvm/my-vm/qmp.sock"));
        assert_eq!(paths.serial_log, PathBuf::from("/run/aleph-cvm/my-vm/serial.log"));
        assert_eq!(paths.pidfile, PathBuf::from("/run/aleph-cvm/my-vm/qemu.pid"));
    }
}
