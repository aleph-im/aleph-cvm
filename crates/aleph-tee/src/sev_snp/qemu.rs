use crate::types::VmConfig;

/// Default SEV-SNP guest policy value.
///
/// 0x30000 enables SEV-SNP with SMT allowed and debug disabled.
const DEFAULT_POLICY: &str = "0x30000";

/// Default OVMF firmware path (AMD SEV-SNP variant, built by nix/ovmf.nix).
pub const DEFAULT_OVMF_PATH: &str = "/usr/local/share/ovmf-snp/OVMF.fd";

/// Generate QEMU command-line arguments for launching an SEV-SNP confidential VM.
///
/// Produces the following arguments:
/// - `-machine q35,confidential-guest-support=sev0,memory-backend=ram1`
/// - `-object memory-backend-memfd,id=ram1,size={memory_mb}M,share=true`
/// - `-object sev-snp-guest,id=sev0,cbitpos=51,reduced-phys-bits=1,kernel-hashes=on,policy={policy}`
/// - `-bios <ovmf_path>` (SEV-SNP requires OVMF firmware)
///
/// `kernel-hashes=on` tells QEMU to populate the OVMF kernel hashing area with
/// SHA-256 hashes of the kernel, initrd, and cmdline. The AmdSev OVMF variant's
/// QemuKernelLoaderFsDxe uses BlobVerifierLibSevHashes to verify these hashes
/// before loading the kernel — without this flag, the blob verifier fails and
/// OVMF falls back to the embedded GRUB bootloader.
pub fn sev_snp_qemu_args(config: &VmConfig, ovmf_path: &str) -> Vec<String> {
    let policy = config.tee.policy.as_deref().unwrap_or(DEFAULT_POLICY);

    vec![
        "-machine".to_string(),
        "q35,confidential-guest-support=sev0,memory-backend=ram1,vmport=off".to_string(),
        "-object".to_string(),
        format!(
            "memory-backend-memfd,id=ram1,size={}M,share=true,hugetlb=on,hugetlbsize=2M",
            config.memory_mb
        ),
        "-object".to_string(),
        format!(
            "sev-snp-guest,id=sev0,cbitpos=51,reduced-phys-bits=1,kernel-hashes=on,policy={policy}"
        ),
        // Strip QEMU's default devices (USB, floppy, etc.) to reduce
        // OVMF PCI enumeration time.
        "-nodefaults".to_string(),
        "-bios".to_string(),
        ovmf_path.to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{TeeConfig, TeeType, VmConfig};
    use std::path::PathBuf;

    fn make_config(memory_mb: u32, policy: Option<&str>) -> VmConfig {
        VmConfig {
            vm_id: "test-vm".to_string(),
            kernel: PathBuf::from("/boot/vmlinuz"),
            initrd: PathBuf::from("/boot/initrd.img"),
            disks: vec![],
            vcpus: 2,
            memory_mb,
            tee: TeeConfig {
                backend: TeeType::SevSnp,
                policy: policy.map(|s| s.to_string()),
            },
            encrypted: false,
        }
    }

    #[test]
    fn test_sev_snp_args_with_policy() {
        let config = make_config(2048, Some("0x50000"));
        let args = sev_snp_qemu_args(&config, DEFAULT_OVMF_PATH);

        // Find the sev-snp-guest object arg
        let sev_arg = args
            .iter()
            .find(|a| a.contains("sev-snp-guest"))
            .expect("should have sev-snp-guest arg");

        assert!(
            sev_arg.contains("policy=0x50000"),
            "policy should be 0x50000 but got: {sev_arg}"
        );
        assert!(
            sev_arg.contains("kernel-hashes=on"),
            "kernel-hashes should be on but got: {sev_arg}"
        );
    }

    #[test]
    fn test_sev_snp_args_default_policy() {
        let config = make_config(2048, None);
        let args = sev_snp_qemu_args(&config, DEFAULT_OVMF_PATH);

        let sev_arg = args
            .iter()
            .find(|a| a.contains("sev-snp-guest"))
            .expect("should have sev-snp-guest arg");

        assert!(
            sev_arg.contains("policy=0x30000"),
            "default policy should be 0x30000 but got: {sev_arg}"
        );
        assert!(
            sev_arg.contains("kernel-hashes=on"),
            "kernel-hashes should be on but got: {sev_arg}"
        );
    }

    #[test]
    fn test_memory_backend_matches_config() {
        let config = make_config(4096, None);
        let args = sev_snp_qemu_args(&config, DEFAULT_OVMF_PATH);

        let mem_arg = args
            .iter()
            .find(|a| a.contains("memory-backend-memfd"))
            .expect("should have memory-backend-memfd arg");

        assert!(
            mem_arg.contains("size=4096M"),
            "memory size should be 4096M but got: {mem_arg}"
        );
    }

    #[test]
    fn test_machine_arg_present() {
        let config = make_config(1024, None);
        let args = sev_snp_qemu_args(&config, DEFAULT_OVMF_PATH);

        // -machine <val>, -object <val>, -object <val>, -nodefaults, -bios <val>
        assert_eq!(args.len(), 9, "expected 9 args, got {}", args.len());
        assert_eq!(args[0], "-machine");
        assert!(args[1].contains("q35"));
        assert!(args[1].contains("confidential-guest-support=sev0"));
        assert!(args[1].contains("memory-backend=ram1"));
    }

    #[test]
    fn test_custom_ovmf_path() {
        let config = make_config(1024, None);
        let custom_path = "/opt/custom/OVMF.fd";
        let args = sev_snp_qemu_args(&config, custom_path);

        let bios_idx = args
            .iter()
            .position(|a| a == "-bios")
            .expect("-bios flag missing");
        assert_eq!(args[bios_idx + 1], custom_path);
    }
}
