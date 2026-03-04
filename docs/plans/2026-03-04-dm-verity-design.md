# dm-verity Rootfs Integrity Design

## Overview

The rootfs (ext4 image) is currently mounted without integrity verification.
A compromised or tampered rootfs would not be detected. dm-verity closes this
gap by creating a Merkle hash tree over the rootfs blocks and embedding the
root hash in the kernel command line, which is covered by the SEV-SNP launch
measurement.

**Chain of trust:**

```
AMD PSP (hardware root of trust)
  → SEV-SNP measurement (OVMF + kernel + initrd + cmdline)
    → cmdline contains roothash=<hex>
      → dm-verity verifies every block read from rootfs
```

## Design Decisions

**Hash tree storage:** Separate file, attached as a second virtio block device
(`/dev/vdb`). The raw rootfs file is not modified, preserving its SHA-256 /
IPFS CID for volume identification in Aleph Cloud.

**Hash tree computation:** Both client-side and host-side compute independently.
The root hash is deterministic for a given rootfs, so both sides arrive at the
same value. The client uses it to compute the expected measurement; the host
uses it to build the kernel cmdline.

**Kernel cmdline:** No longer a compile-time constant. Includes `roothash=<hex>`
which varies per rootfs. Different rootfs → different cmdline → different
SEV-SNP measurement. This is correct and desired.

**Backwards compatibility:** If no verity root hash is configured, the old
cmdline (`console=ttyS0 root=/dev/vda ro`) is used and the init script falls
back to direct mount.

## Changes by Layer

### 1. Kernel Config (`nix/kernel.nix`)

Add built-in support for device-mapper and dm-verity:

```nix
BLK_DEV_DM = lib.mkForce yes;
DM_VERITY = lib.mkForce yes;
```

SHA-256 crypto support is already present via the CCP/PSP modules, but we
should ensure the software fallback is also built-in:

```nix
CRYPTO_SHA256 = lib.mkForce yes;
```

### 2. Initrd (`nix/initrd.nix`)

Add `veritysetup` (from the `cryptsetup` package) to the initrd contents.
This is a statically-linked binary that sets up the dm-verity device.

### 3. Init Script (`nix/init.sh`)

Modified rootfs mount sequence:

1. Parse `roothash=` from `/proc/cmdline`
2. If present:
   - Wait for both `/dev/vda` (data) and `/dev/vdb` (hash tree)
   - Run `veritysetup open /dev/vda verity-root /dev/vdb --root-hash=$hash`
   - Mount `/dev/mapper/verity-root` read-only at `/mnt/root`
3. If absent: fall back to direct mount of `/dev/vda` (current behavior)

If `veritysetup open` fails (hash mismatch = tampered rootfs), the VM refuses
to mount the rootfs and logs an error. This is the correct behavior — a
tampered rootfs should cause a boot failure, not silent corruption.

### 4. Orchestrator (`aleph-compute-node`)

#### Verity hash tree computation

New module `src/verity.rs`:
- `ensure_verity(rootfs_path) -> Result<VerityInfo>` where
  `VerityInfo { root_hash: String, hashtree_path: PathBuf, data_size: u64 }`
- Checks for cached `{rootfs_path}.verity` and `{rootfs_path}.roothash`
- If missing, runs `veritysetup format {rootfs_path} {rootfs_path}.verity`,
  parses root hash from stdout, saves to `{rootfs_path}.roothash`
- Caches are invalidated by checking rootfs file mtime

#### Kernel cmdline

`KERNEL_CMDLINE` is no longer a constant. A new function builds the cmdline:

```rust
pub fn build_kernel_cmdline(roothash: Option<&str>) -> String {
    match roothash {
        Some(hash) => format!("console=ttyS0 root=/dev/mapper/verity-root ro roothash={hash}"),
        None => "console=ttyS0 root=/dev/vda ro".to_string(),
    }
}
```

#### VM creation flow

1. Identify the rootfs disk (first disk in the list, or marked explicitly)
2. Call `ensure_verity(rootfs_path)` → get root hash + hash tree path
3. Add hash tree as a second readonly virtio disk
4. Build cmdline with `roothash=<hash>`
5. Attach both disks and boot

#### QEMU args

`build_qemu_command` accepts the cmdline as a parameter instead of using a
constant. The root hash disk is just another entry in `config.disks`.

### 5. Nix Build (`nix/flake.nix`)

For the demo bundle (`vm-fib-demo`):
- Run `veritysetup format` on the rootfs at build time
- Output the hash tree file and root hash
- Update `kernelCmdline` to include the root hash
- Recompute `sev-snp-measure` with the updated cmdline

The `kernelCmdline` variable is no longer hardcoded — it's derived from the
rootfs verity root hash.

### 6. gRPC Proto

No proto changes needed. The orchestrator handles verity internally — it
computes the hash tree from the rootfs disk that's already in the
`CreateVmRequest`. The caller doesn't need to know about dm-verity; the
orchestrator applies it automatically to the first disk.

### 7. Scheduler Agent

No changes needed. It already sends rootfs as the first disk in the
`CreateVmRequest`. The orchestrator handles verity setup transparently.

## Client-Side Verification

The client verifies a VM is running the expected rootfs:

1. Obtain the rootfs (by SHA-256 hash or IPFS CID)
2. Run `veritysetup format <rootfs> /dev/null` → extract root hash
3. Build the expected cmdline: `console=ttyS0 root=/dev/mapper/verity-root ro roothash=<hash>`
4. Run `sev-snp-measure` with that cmdline → expected measurement
5. Connect to VM, verify attestation report measurement matches

## Security Properties

- **Integrity:** Every 4K block read from rootfs is verified against the
  Merkle tree. Any tampering causes an I/O error.
- **Binding to attestation:** The root hash is in the cmdline, which is part
  of the SEV-SNP measurement. A verifier can confirm the exact rootfs content
  from the attestation report.
- **No secrets involved:** dm-verity is purely integrity, not confidentiality.
  The rootfs and hash tree are public. This means lazy attestation verification
  works — no need to verify before boot.
- **Host cannot tamper:** The host provides the rootfs and hash tree, but if
  either is modified, dm-verity will detect it (hash mismatch against the
  measured root hash).

## Out of Scope

- **Encrypted disks (dm-crypt/LUKS):** Separate feature requiring a different
  trust model (secrets must be released after attestation verification).
- **Writable verity (dm-verity + dm-snapshot):** Not needed; rootfs is
  read-only.
- **Custom hash algorithms:** SHA-256 is the default and sufficient.
