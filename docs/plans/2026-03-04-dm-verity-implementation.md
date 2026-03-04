# dm-verity Rootfs Integrity Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add dm-verity integrity verification to rootfs, binding rootfs content to the SEV-SNP measurement via the kernel command line.

**Architecture:** The Nix build produces a verity hash tree + root hash alongside the rootfs. The orchestrator computes verity artifacts at deploy time for arbitrary rootfs images. The kernel cmdline includes `roothash=<hex>`, which is measured by SEV-SNP. The init script sets up dm-verity before mounting.

**Tech Stack:** Nix, dm-verity (kernel), veritysetup (cryptsetup), QEMU virtio-blk, shell scripting

---

### Task 1: Kernel Config — Enable dm-verity

**Files:**
- Modify: `nix/kernel.nix:4-28`

**Context:** The kernel needs `BLK_DEV_DM` (device-mapper core) and `DM_VERITY` (verity target) built-in. Without these, `veritysetup` in the init script has nothing to talk to. `CRYPTO_SHA256` ensures the software hash fallback is available even without the CCP accelerator.

**Step 1: Add dm-verity kernel config options**

Add these lines after the `EXT4_FS` block (line 17) in `nix/kernel.nix`:

```nix
    # Device-mapper + dm-verity (built-in for rootfs integrity verification)
    BLK_DEV_DM = lib.mkForce yes;
    DM_VERITY = lib.mkForce yes;
    CRYPTO_SHA256 = lib.mkForce yes;
```

The full file should look like:

```nix
{ pkgs, lib, ... }:

pkgs.linuxPackages_6_6.kernel.override {
  structuredExtraConfig = with lib.kernel; {
    # SEV-SNP guest support (mkForce to override base config "m" → "y")
    AMD_MEM_ENCRYPT = lib.mkForce yes;
    SEV_GUEST = lib.mkForce yes;
    CRYPTO_DEV_CCP = lib.mkForce yes;
    CRYPTO_DEV_CCP_DD = lib.mkForce yes;
    CRYPTO_DEV_SP_PSP = lib.mkForce yes;

    # Networking (built-in, since we boot from initrd without module loading)
    PACKET = lib.mkForce yes;         # AF_PACKET sockets (required by udhcpc/DHCP)
    UNIX = lib.mkForce yes;           # AF_UNIX sockets

    # Filesystems (built-in for initrd boot)
    EXT4_FS = lib.mkForce yes;        # ext4 rootfs support

    # Device-mapper + dm-verity (built-in for rootfs integrity verification)
    BLK_DEV_DM = lib.mkForce yes;
    DM_VERITY = lib.mkForce yes;
    CRYPTO_SHA256 = lib.mkForce yes;

    # Virtio (for disk and network)
    VIRTIO = lib.mkForce yes;
    VIRTIO_PCI = lib.mkForce yes;
    VIRTIO_BLK = lib.mkForce yes;
    VIRTIO_NET = lib.mkForce yes;
    VIRTIO_CONSOLE = lib.mkForce yes;

    # Note: MODULES left as default (yes) to avoid interactive config questions.
    # For a minimal production kernel, use a fully custom .config instead.
  };
}
```

**Step 2: Verify Nix evaluates the kernel**

Run: `cd /home/olivier/git/aleph/aleph-cvm && nix eval .#packages.x86_64-linux.kernel.name 2>&1 | head -5`

Expected: No evaluation errors. The actual build is slow (kernel compile) so don't build it now — just verify the Nix expression evaluates.

**Step 3: Commit**

```bash
git add nix/kernel.nix
git commit -m "feat: enable dm-verity in kernel config"
```

---

### Task 2: Init Script — dm-verity Mount Logic

**Files:**
- Modify: `nix/init.sh:44-73`

**Context:** The init script runs as PID 1 inside the VM. It currently mounts `/dev/vda` directly. We need to:
1. Parse `roothash=` from `/proc/cmdline`
2. If present, use `veritysetup open` with `/dev/vda` (data) + `/dev/vdb` (hash tree), then mount `/dev/mapper/verity-root`
3. If absent, fall back to the current direct mount (backwards compatible)

The `veritysetup` binary will be added to the initrd in Task 3.

**Step 1: Replace the rootfs mount section**

Replace lines 44-73 of `nix/init.sh` (from `# Wait for block device` to the end of the rootfs mount block) with:

```bash
# Parse dm-verity root hash from kernel command line (if present).
roothash=$(/bin/busybox sed -n 's/.*roothash=\([0-9a-fA-F]*\).*/\1/p' /proc/cmdline)

# Wait for block device to appear.
blkdev=""
n=0
while [ "$n" -lt 30 ]; do
    for dev in /dev/vda /dev/sda; do
        if [ -b "$dev" ]; then
            blkdev="$dev"
            break 2
        fi
    done
    /bin/busybox sleep 0.1
    n=$((n + 1))
done

# Mount rootfs and start user application.
if [ -n "$blkdev" ]; then
    /bin/busybox mkdir -p /mnt/root

    if [ -n "$roothash" ]; then
        # dm-verity: wait for hash tree device (/dev/vdb)
        hashdev=""
        n=0
        while [ "$n" -lt 30 ]; do
            if [ -b /dev/vdb ]; then
                hashdev="/dev/vdb"
                break
            fi
            /bin/busybox sleep 0.1
            n=$((n + 1))
        done

        if [ -z "$hashdev" ]; then
            echo "init: FATAL: roothash set but /dev/vdb (hash tree) not found"
        else
            echo "init: setting up dm-verity on ${blkdev} with hash tree ${hashdev}"
            if /bin/veritysetup open "$blkdev" verity-root "$hashdev" --root-hash="$roothash"; then
                echo "init: mounting /dev/mapper/verity-root"
                /bin/busybox mount -o ro /dev/mapper/verity-root /mnt/root || echo "init: verity mount failed"
            else
                echo "init: FATAL: dm-verity verification failed — rootfs may be tampered"
            fi
        fi
    else
        # No dm-verity: direct mount (backwards compatible)
        echo "init: mounting ${blkdev} (no dm-verity)"
        if ! /bin/busybox mount -o ro "$blkdev" /mnt/root; then
            echo "init: mount failed, trying without readonly"
            /bin/busybox mount "$blkdev" /mnt/root || echo "init: mount failed completely"
        fi
    fi

    if [ -x /mnt/root/bin/fib-service ]; then
        /mnt/root/bin/fib-service &
    fi
else
    echo "init: no block device found, skipping rootfs mount"
fi
```

**Step 2: Verify the script is syntactically valid**

Run: `bash -n /home/olivier/git/aleph/aleph-cvm/nix/init.sh`

Expected: No output (no syntax errors). Note: this checks bash syntax, but the script runs under busybox sh — the constructs used are POSIX-compatible.

**Step 3: Commit**

```bash
git add nix/init.sh
git commit -m "feat: init script supports dm-verity rootfs mount"
```

---

### Task 3: Initrd — Add veritysetup Binary

**Files:**
- Modify: `nix/initrd.nix`

**Context:** `veritysetup` is part of the `cryptsetup` package. We need it in the initrd so the init script can call it. We need a statically-linked build so it works in the minimal initrd environment. The binary also needs `libdevmapper` to create dm devices.

**Step 1: Add veritysetup to initrd contents**

Replace the contents of `nix/initrd.nix` with:

```nix
{ pkgs, attest-agent, init-script, ... }:

let
  # veritysetup needs to be statically linked for the initrd environment.
  # pkgsStatic provides fully static builds.
  staticCryptsetup = pkgs.pkgsStatic.cryptsetup;
in
pkgs.makeInitrd {
  contents = [
    { object = "${pkgs.busybox}/bin/busybox"; symlink = "/bin/busybox"; }
    { object = init-script; symlink = "/init"; }
    { object = "${attest-agent}/bin/aleph-attest-agent"; symlink = "/bin/aleph-attest-agent"; }
    { object = "${staticCryptsetup}/bin/veritysetup"; symlink = "/bin/veritysetup"; }
  ];
}
```

**Important:** If `pkgs.pkgsStatic.cryptsetup` doesn't build cleanly, try `pkgs.pkgsStatic.cryptsetup.overrideAttrs (old: { configureFlags = old.configureFlags ++ ["--disable-blkid"]; })` or use `pkgs.cryptsetup.overrideAttrs` with `enableStatic = true`. The exact approach depends on the nixpkgs version — try the simple version first.

**Step 2: Verify Nix evaluates the initrd**

Run: `cd /home/olivier/git/aleph/aleph-cvm && nix eval .#packages.x86_64-linux.initrd.name 2>&1 | head -5`

Expected: No evaluation errors. If `pkgsStatic.cryptsetup` causes issues, adjust per the note above.

**Step 3: Commit**

```bash
git add nix/initrd.nix
git commit -m "feat: add veritysetup to initrd for dm-verity support"
```

---

### Task 4: Nix Build — Verity Hash Tree + Root Hash for Demo Rootfs

**Files:**
- Modify: `nix/flake.nix:68-69,96-132`

**Context:** The demo bundle (`vm-fib-demo`) needs verity artifacts computed at Nix build time:
1. `veritysetup format <rootfs> <hashtree>` → produces hash tree file + prints root hash
2. The root hash goes into `kernelCmdline`, which changes the SEV-SNP measurement
3. The hash tree file is included in the demo bundle

The `kernelCmdline` is no longer a hardcoded constant — it's derived from the rootfs.

**Step 1: Add verity derivation and update cmdline/measurement**

Replace lines 68-132 of `nix/flake.nix` (from `kernelCmdline` through end of `packages`) with:

```nix
      # OVMF firmware built with AmdSev variant (kernel hashing support).
      ovmf = import ./ovmf.nix { inherit pkgs; };
      ovmfFd = "${ovmf}/OVMF.fd";

      # nixpkgs 24.11 ships sev-snp-measure 0.0.11 which has a measurement
      # calculation bug.  Override to 0.0.12 which produces correct results.
      sev-snp-measure = pkgs.python3Packages.sev-snp-measure.overridePythonAttrs (old: rec {
        version = "0.0.12";
        src = pkgs.fetchFromGitHub {
          owner = "virtee";
          repo = "sev-snp-measure";
          rev = "v${version}";
          hash = "sha256-UcXU6rNjcRN1T+iWUNrqeJCkSa02WU1/pBwLqHVPRyw=";
        };
      });

      # Compute dm-verity hash tree and root hash for the demo rootfs.
      # The root hash is embedded in the kernel cmdline, binding rootfs
      # integrity to the SEV-SNP measurement.
      verity = pkgs.runCommand "rootfs-verity" {
        nativeBuildInputs = [ pkgs.cryptsetup ];
      } ''
        mkdir -p $out
        veritysetup format \
          ${self.packages.${system}.rootfs} \
          $out/hashtree \
          | tee /dev/stderr \
          | grep "Root hash:" \
          | awk '{print $NF}' \
          | tr -d '\n' > $out/roothash
      '';

      # Kernel command line with dm-verity root hash.
      # This must match what the orchestrator builds at runtime.
      # The roothash binds rootfs integrity to the SEV-SNP measurement.
      kernelCmdline = "console=ttyS0 root=/dev/mapper/verity-root ro roothash=${builtins.readFile "${verity}/roothash"}";

    in {
      packages.${system} = {
        inherit fib-service attest-agent ovmf verity;

        kernel = pkgs.callPackage ./kernel.nix {};
        initrd = pkgs.callPackage ./initrd.nix {
          inherit attest-agent;
          init-script = ./init.sh;
        };
        rootfs = pkgs.callPackage ./rootfs.nix {
          inherit fib-service;
        };

        # Pre-computed SEV-SNP launch measurement for the demo config (2 vCPUs).
        # Now includes dm-verity root hash in the kernel cmdline.
        measurement = pkgs.runCommand "sev-snp-measurement" {
          nativeBuildInputs = [ sev-snp-measure ];
        } ''
          sev-snp-measure \
            --mode snp \
            --vcpus 2 \
            --vcpu-type EPYC-v4 \
            --ovmf ${ovmfFd} \
            --kernel ${self.packages.${system}.kernel}/bzImage \
            --initrd ${self.packages.${system}.initrd}/initrd \
            --append "${kernelCmdline}" \
            | tr -d '\n' > $out
        '';

        # Convenience: build all artifacts into one directory.
        # Now includes verity hash tree and root hash.
        vm-fib-demo = pkgs.runCommand "vm-fib-demo" {} ''
          mkdir -p $out
          ln -s ${self.packages.${system}.kernel}/bzImage $out/bzImage
          ln -s ${self.packages.${system}.initrd}/initrd $out/initrd
          ln -s ${self.packages.${system}.rootfs} $out/rootfs.ext4
          cp ${ovmfFd} $out/OVMF.fd
          cp ${self.packages.${system}.measurement} $out/measurement.hex
          cp ${verity}/hashtree $out/rootfs.verity
          cp ${verity}/roothash $out/rootfs.roothash
        '';
      };
    };
```

**Step 2: Verify Nix evaluates**

Run: `cd /home/olivier/git/aleph/aleph-cvm && nix eval .#packages.x86_64-linux.vm-fib-demo.name 2>&1 | head -5`

Expected: No evaluation errors. Note: the actual build requires `veritysetup` and the rootfs to exist, but evaluation should succeed.

**Step 3: Commit**

```bash
git add nix/flake.nix
git commit -m "feat: compute dm-verity hash tree at Nix build time"
```

---

### Task 5: Orchestrator — Verity Module

**Files:**
- Create: `crates/aleph-compute-node/src/verity.rs`
- Modify: `crates/aleph-compute-node/src/lib.rs:1-6`

**Context:** The orchestrator needs to compute verity artifacts at runtime for arbitrary rootfs images (not just Nix-built ones). This module:
1. Runs `veritysetup format` on a rootfs file
2. Caches the hash tree + root hash alongside the rootfs
3. Returns a `VerityInfo` struct with the root hash and hash tree path

**Step 1: Write the test**

Create `crates/aleph-compute-node/src/verity.rs` with tests first:

```rust
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::info;

/// Information about a rootfs's dm-verity hash tree.
#[derive(Debug, Clone)]
pub struct VerityInfo {
    /// The dm-verity root hash (hex string, lowercase).
    pub root_hash: String,
    /// Path to the hash tree file.
    pub hashtree_path: PathBuf,
}

/// Ensure dm-verity artifacts exist for the given rootfs image.
///
/// If `{rootfs_path}.verity` and `{rootfs_path}.roothash` exist and are
/// newer than the rootfs, returns the cached values. Otherwise, runs
/// `veritysetup format` to compute them.
pub fn ensure_verity(rootfs_path: &Path) -> Result<VerityInfo> {
    let hashtree_path = PathBuf::from(format!("{}.verity", rootfs_path.display()));
    let roothash_path = PathBuf::from(format!("{}.roothash", rootfs_path.display()));

    // Check if cached artifacts are still valid
    if hashtree_path.exists() && roothash_path.exists() {
        let rootfs_mtime = rootfs_path
            .metadata()
            .and_then(|m| m.modified())
            .ok();
        let cache_mtime = roothash_path
            .metadata()
            .and_then(|m| m.modified())
            .ok();

        if let (Some(rootfs_t), Some(cache_t)) = (rootfs_mtime, cache_mtime) {
            if cache_t >= rootfs_t {
                let root_hash = std::fs::read_to_string(&roothash_path)
                    .context("failed to read cached roothash")?
                    .trim()
                    .to_string();
                info!(rootfs = %rootfs_path.display(), root_hash = %root_hash, "using cached verity artifacts");
                return Ok(VerityInfo {
                    root_hash,
                    hashtree_path,
                });
            }
        }
    }

    // Compute verity hash tree
    info!(rootfs = %rootfs_path.display(), "computing dm-verity hash tree");
    let output = std::process::Command::new("veritysetup")
        .args([
            "format",
            &rootfs_path.display().to_string(),
            &hashtree_path.display().to_string(),
        ])
        .output()
        .context("failed to execute veritysetup format")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("veritysetup format failed: {stderr}");
    }

    // Parse root hash from stdout
    let stdout = String::from_utf8_lossy(&output.stdout);
    let root_hash = stdout
        .lines()
        .find(|line| line.starts_with("Root hash:"))
        .and_then(|line| line.split_whitespace().last())
        .map(|s| s.trim().to_lowercase())
        .context("failed to parse root hash from veritysetup output")?;

    // Cache the root hash
    std::fs::write(&roothash_path, &root_hash)
        .with_context(|| format!("failed to write {}", roothash_path.display()))?;

    info!(rootfs = %rootfs_path.display(), root_hash = %root_hash, "computed verity hash tree");

    Ok(VerityInfo {
        root_hash,
        hashtree_path,
    })
}

/// Build the kernel command line, optionally including a dm-verity root hash.
///
/// When `roothash` is `Some`, the cmdline tells the init script to set up
/// dm-verity and mount `/dev/mapper/verity-root` instead of `/dev/vda` directly.
pub fn build_kernel_cmdline(roothash: Option<&str>) -> String {
    match roothash {
        Some(hash) => format!("console=ttyS0 root=/dev/mapper/verity-root ro roothash={hash}"),
        None => "console=ttyS0 root=/dev/vda ro".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_kernel_cmdline_no_verity() {
        let cmdline = build_kernel_cmdline(None);
        assert_eq!(cmdline, "console=ttyS0 root=/dev/vda ro");
        assert!(!cmdline.contains("roothash"));
        assert!(!cmdline.contains("verity-root"));
    }

    #[test]
    fn test_build_kernel_cmdline_with_verity() {
        let hash = "abc123def456";
        let cmdline = build_kernel_cmdline(Some(hash));
        assert_eq!(
            cmdline,
            "console=ttyS0 root=/dev/mapper/verity-root ro roothash=abc123def456"
        );
        assert!(cmdline.contains("verity-root"));
        assert!(cmdline.contains("roothash=abc123def456"));
        assert!(!cmdline.contains("/dev/vda"));
    }

    #[test]
    fn test_build_kernel_cmdline_no_ip() {
        // Verify neither variant leaks per-VM IP addresses (breaks measurement determinism)
        let none = build_kernel_cmdline(None);
        let some = build_kernel_cmdline(Some("aabbccdd"));
        assert!(!none.contains("ip="));
        assert!(!some.contains("ip="));
    }
}
```

**Step 2: Register the module**

Add `pub mod verity;` to `crates/aleph-compute-node/src/lib.rs`:

```rust
pub mod grpc;
pub mod network;
pub mod persistence;
pub mod qemu;
pub mod systemd;
pub mod verity;
pub mod vm;
```

**Step 3: Run tests**

Run: `cd /home/olivier/git/aleph/aleph-cvm && cargo test -p aleph-compute-node verity`

Expected: 3 tests pass (the `build_kernel_cmdline` tests). The `ensure_verity` function is not tested here because it requires `veritysetup` to be installed.

**Step 4: Commit**

```bash
git add crates/aleph-compute-node/src/verity.rs crates/aleph-compute-node/src/lib.rs
git commit -m "feat: add verity module with cmdline builder and ensure_verity"
```

---

### Task 6: QEMU Args — Dynamic Kernel Cmdline

**Files:**
- Modify: `crates/aleph-compute-node/src/qemu/args.rs:6-14,39-69,113-169`

**Context:** `build_qemu_command` currently uses the hardcoded `KERNEL_CMDLINE` constant. We need to:
1. Remove the `KERNEL_CMDLINE` constant (replaced by `verity::build_kernel_cmdline`)
2. Add a `kernel_cmdline: &str` parameter to `build_qemu_command`
3. Update all callers and tests

**Step 1: Update `build_qemu_command` signature and remove constant**

Remove the `KERNEL_CMDLINE` constant (lines 6-14) and add `kernel_cmdline` parameter:

```rust
use std::path::PathBuf;

use aleph_tee::traits::TeeBackend;
use aleph_tee::types::VmConfig;

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
/// The `kernel_cmdline` is passed as `-append` to QEMU. It may include
/// dm-verity parameters (e.g. `roothash=<hex>`).
///
/// The `mac_addr` is assigned to the virtio-net device so that dnsmasq can
/// map it to a reserved IP via DHCP.
pub fn build_qemu_command(
    config: &VmConfig,
    paths: &QemuPaths,
    tap_name: &str,
    tee_backend: &dyn TeeBackend,
    mac_addr: &str,
    kernel_cmdline: &str,
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
        kernel_cmdline.into(),
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
```

**Step 2: Update tests**

Replace the test module with:

```rust
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
    const TEST_CMDLINE: &str = "console=ttyS0 root=/dev/vda ro";
    const TEST_CMDLINE_VERITY: &str =
        "console=ttyS0 root=/dev/mapper/verity-root ro roothash=abcdef1234567890";

    #[test]
    fn test_build_command_includes_kernel() {
        let config = make_config(vec![rootfs_disk("/images/rootfs.ext4")]);
        let paths = QemuPaths::for_vm("/run/aleph-cvm".as_ref(), "test-vm-001");
        let backend = SevSnpBackend::new("Genoa");
        let args =
            build_qemu_command(&config, &paths, "tap0", &backend, TEST_MAC, TEST_CMDLINE);

        let kernel_idx = args
            .iter()
            .position(|a| a == "-kernel")
            .expect("-kernel flag missing");
        assert_eq!(args[kernel_idx + 1], "/boot/vmlinuz");
    }

    #[test]
    fn test_build_command_uses_provided_cmdline() {
        let config = make_config(vec![]);
        let paths = QemuPaths::for_vm("/run/aleph-cvm".as_ref(), "test-vm-001");
        let backend = SevSnpBackend::new("Genoa");
        let args =
            build_qemu_command(&config, &paths, "tap0", &backend, TEST_MAC, TEST_CMDLINE);

        let append_idx = args
            .iter()
            .position(|a| a == "-append")
            .expect("-append flag missing");
        assert_eq!(args[append_idx + 1], TEST_CMDLINE);
    }

    #[test]
    fn test_build_command_verity_cmdline() {
        let config = make_config(vec![]);
        let paths = QemuPaths::for_vm("/run/aleph-cvm".as_ref(), "test-vm-001");
        let backend = SevSnpBackend::new("Genoa");
        let args = build_qemu_command(
            &config,
            &paths,
            "tap0",
            &backend,
            TEST_MAC,
            TEST_CMDLINE_VERITY,
        );

        let append_idx = args
            .iter()
            .position(|a| a == "-append")
            .expect("-append flag missing");
        assert!(args[append_idx + 1].contains("roothash="));
        assert!(args[append_idx + 1].contains("verity-root"));
    }

    #[test]
    fn test_build_command_includes_mac() {
        let config = make_config(vec![]);
        let paths = QemuPaths::for_vm("/run/aleph-cvm".as_ref(), "test-vm-001");
        let backend = SevSnpBackend::new("Genoa");
        let args =
            build_qemu_command(&config, &paths, "tap0", &backend, TEST_MAC, TEST_CMDLINE);

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
        let args =
            build_qemu_command(&config, &paths, "tap0", &backend, TEST_MAC, TEST_CMDLINE);

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
        let args =
            build_qemu_command(&config, &paths, "tap0", &backend, TEST_MAC, TEST_CMDLINE);

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
        let args =
            build_qemu_command(&config, &paths, "tap0", &backend, TEST_MAC, TEST_CMDLINE);

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
        assert_eq!(
            paths.qmp_socket,
            PathBuf::from("/run/aleph-cvm/my-vm/qmp.sock")
        );
    }
}
```

**Step 3: Run tests**

Run: `cd /home/olivier/git/aleph/aleph-cvm && cargo test -p aleph-compute-node args`

Expected: Compilation will fail because `manager.rs` still calls `build_qemu_command` without the new `kernel_cmdline` parameter. That's expected — Task 7 fixes the caller.

**Step 4: Commit**

```bash
git add crates/aleph-compute-node/src/qemu/args.rs
git commit -m "feat: build_qemu_command accepts dynamic kernel cmdline"
```

---

### Task 7: VM Manager — Wire Up Verity in create_vm

**Files:**
- Modify: `crates/aleph-compute-node/src/vm/manager.rs:21-26,222-232`

**Context:** The manager's `create_vm` method needs to:
1. Identify the rootfs (first disk in the config)
2. Call `ensure_verity` to get the root hash + hash tree path
3. Insert the hash tree as a second disk (readonly, raw)
4. Build the kernel cmdline with the root hash
5. Pass the cmdline to `build_qemu_command`

**Step 1: Update imports**

Change the import on line 23 from:

```rust
use crate::qemu::args::{build_qemu_command, QemuPaths};
```

to:

```rust
use crate::qemu::args::{build_qemu_command, QemuPaths};
use crate::verity;
```

**Step 2: Update the QEMU command building section in `create_vm`**

Replace lines 222-232 (the "Build QEMU command" section) with:

```rust
        // Compute dm-verity for the rootfs (first disk)
        let kernel_cmdline = if let Some(rootfs_disk) = config.disks.first() {
            match verity::ensure_verity(&rootfs_disk.path) {
                Ok(vinfo) => {
                    // Insert hash tree as second disk (right after rootfs)
                    config.disks.insert(1, aleph_tee::types::DiskConfig {
                        path: vinfo.hashtree_path,
                        readonly: true,
                        format: "raw".to_string(),
                    });
                    verity::build_kernel_cmdline(Some(&vinfo.root_hash))
                }
                Err(e) => {
                    warn!(vm_id = %vm_id, error = %e, "dm-verity setup failed, falling back to direct mount");
                    verity::build_kernel_cmdline(None)
                }
            }
        } else {
            verity::build_kernel_cmdline(None)
        };

        // Build QEMU command
        let paths = QemuPaths::for_vm(&self.run_dir, &vm_id);
        let mut args = vec!["qemu-system-x86_64".to_string()];
        args.extend(build_qemu_command(
            &config,
            &paths,
            &tap_name,
            self.tee_backend.as_ref(),
            &mac_addr,
            &kernel_cmdline,
        ));
```

**Important:** The `config` parameter in `create_vm` needs to be `mut` for this to work. Change the function signature from `config: VmConfig` to `mut config: VmConfig`.

**Step 3: Run tests**

Run: `cd /home/olivier/git/aleph/aleph-cvm && cargo test -p aleph-compute-node`

Expected: All tests pass. The `create_vm` flow now computes verity, but unit tests that don't go through `create_vm` won't be affected.

**Step 4: Commit**

```bash
git add crates/aleph-compute-node/src/vm/manager.rs
git commit -m "feat: wire dm-verity into VM creation flow"
```

---

### Task 8: Fix Any Remaining Callers + Integration Test

**Files:**
- Check: `crates/aleph-compute-node/tests/tier1_api.rs`
- Check: `crates/aleph-compute-node/tests/persistence_integration.rs`

**Context:** Any test files that call `build_qemu_command` directly need the new `kernel_cmdline` parameter. Also verify the full crate compiles and all tests pass.

**Step 1: Fix test callers**

Search for `build_qemu_command` in test files. If any exist, add the `kernel_cmdline` parameter (use `"console=ttyS0 root=/dev/vda ro"` for tests that don't need verity).

**Step 2: Run the full test suite**

Run: `cd /home/olivier/git/aleph/aleph-cvm && cargo test --workspace`

Expected: All tests pass. Fix any compilation errors from the changed `build_qemu_command` signature.

**Step 3: Commit any fixes**

```bash
git add -A
git commit -m "fix: update remaining callers for dynamic kernel cmdline"
```

---

### Summary

| Task | What | Files |
|------|------|-------|
| 1 | Kernel config: enable dm-verity | `nix/kernel.nix` |
| 2 | Init script: dm-verity mount logic | `nix/init.sh` |
| 3 | Initrd: add veritysetup binary | `nix/initrd.nix` |
| 4 | Nix: verity hash tree + updated measurement | `nix/flake.nix` |
| 5 | Orchestrator: verity module + cmdline builder | `src/verity.rs`, `src/lib.rs` |
| 6 | QEMU args: dynamic kernel cmdline | `src/qemu/args.rs` |
| 7 | VM manager: wire verity into create_vm | `src/vm/manager.rs` |
| 8 | Fix remaining callers + verify | test files |
