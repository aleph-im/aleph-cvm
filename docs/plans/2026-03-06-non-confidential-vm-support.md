# Non-Confidential VM Support Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Allow aleph-cvm compute nodes to run regular (non-confidential) VMs alongside SEV-SNP confidential VMs, so nodes can accept standard Ubuntu/Debian workloads for additional revenue when confidential workload demand is low.

**Architecture:** Introduce a `TeeType::None` variant and `NoTeeBackend` that produces plain KVM QEMU arguments (no SEV, no hugepages, no OVMF). Make `kernel`/`initrd` optional in `VmConfig` to support disk-boot mode (guest boots from its own bootloader). The `VmManager` holds a map of `TeeType -> Arc<dyn TeeBackend>` to select the right backend per VM. dm-verity and kernel cmdline construction are skipped for non-confidential VMs. NUMA CPU pinning still works but hugepage capacity checks are skipped.

**Tech Stack:** Rust, tonic/prost (gRPC), QEMU/KVM, systemd transient units, nftables

---

## Design Decisions

| Decision | Rationale |
|----------|-----------|
| `TeeType::None` enum variant | Cleanest way to express "no TEE" through existing config structures |
| `NoTeeBackend` implements `TeeBackend` | Reuse existing trait; attestation methods return errors (no attestation available) |
| Optional `kernel`/`initrd` in VmConfig | When absent: disk-boot mode (`-boot order=c`). When present: direct kernel boot (current behavior). Backward-compatible via serde defaults. |
| CPU model moved to backend | SevSnp emits `-cpu EPYC-v4`, None emits `-cpu host`. Removes hardcoded CPU from base args. |
| NUMA: skip memory check for non-TEE | Non-confidential VMs use regular RAM, not hugepages. CPU pinning still valuable. |
| Per-VM backend selection | `VmManager` holds `HashMap<TeeType, Arc<dyn TeeBackend>>` instead of single backend |
| Networking unchanged | DHCP via dnsmasq works for Ubuntu/Debian (they ship dhclient). No cloud-init needed. |

## Out of Scope (follow-ups)

- Cloud-init ISO generation for richer guest config (hostname, SSH keys, etc.)
- GPU passthrough for non-confidential VMs
- Guest-initiated reboot support (currently `-no-reboot` kills VM; needs systemd restart policy change)
- Separate resource quotas (max confidential vs non-confidential VMs)
- qcow2 backing-file snapshots (aleph-vm style copy-on-write)

---

### Task 1: Add `TeeType::None` and make kernel/initrd optional

**Files:**
- Modify: `crates/aleph-tee/src/types.rs`

**Step 1: Write failing tests for TeeType::None**

Add to the existing `mod tests` block in `types.rs`:

```rust
#[test]
fn test_tee_type_none_serialization() {
    let json = serde_json::to_string(&TeeType::None).unwrap();
    assert_eq!(json, "\"none\"");
    let deserialized: TeeType = serde_json::from_str("\"none\"").unwrap();
    assert_eq!(deserialized, TeeType::None);
}

#[test]
fn test_vm_config_optional_kernel() {
    let json = r#"{
        "vm_id": "test-vm",
        "disks": [{"path": "/images/ubuntu.qcow2", "readonly": false, "format": "qcow2"}],
        "vcpus": 2,
        "memory_mb": 2048,
        "tee": {"backend": "none"}
    }"#;
    let config: VmConfig = serde_json::from_str(json).unwrap();
    assert!(config.kernel.is_none());
    assert!(config.initrd.is_none());
    assert_eq!(config.tee.backend, TeeType::None);
}

#[test]
fn test_vm_config_backward_compat_with_kernel() {
    // Old-style config with kernel/initrd still works
    let json = r#"{
        "vm_id": "test-vm",
        "kernel": "/boot/vmlinuz",
        "initrd": "/boot/initrd.img",
        "vcpus": 2,
        "memory_mb": 2048,
        "tee": {"backend": "sev-snp", "policy": "0x30000"}
    }"#;
    let config: VmConfig = serde_json::from_str(json).unwrap();
    assert_eq!(config.kernel.unwrap(), std::path::PathBuf::from("/boot/vmlinuz"));
    assert_eq!(config.initrd.unwrap(), std::path::PathBuf::from("/boot/initrd.img"));
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p aleph-tee -- test_tee_type_none`
Expected: FAIL (no `None` variant)

**Step 3: Implement changes**

In `TeeType` enum, add `None` variant:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TeeType {
    SevSnp,
    Tdx,
    NvidiaCc,
    None,
}
```

In `VmConfig`, make `kernel` and `initrd` optional:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmConfig {
    pub vm_id: String,
    #[serde(default)]
    pub kernel: Option<std::path::PathBuf>,
    #[serde(default)]
    pub initrd: Option<std::path::PathBuf>,
    #[serde(default)]
    pub disks: Vec<DiskConfig>,
    pub vcpus: u32,
    pub memory_mb: u32,
    pub tee: TeeConfig,
    #[serde(default)]
    pub encrypted: bool,
    #[serde(default)]
    pub numa_node: Option<u32>,
}
```

**Step 4: Fix all compilation errors from VmConfig change**

Every place that accesses `config.kernel` or `config.initrd` directly now gets an `Option`. The main sites:
- `crates/aleph-compute-node/src/qemu/args.rs` — `build_qemu_command` and tests (Task 3 handles this)
- `crates/aleph-compute-node/src/grpc/service.rs` — VmConfig construction (Task 6 handles this)

For now, update the test helpers in `types.rs` to use `Some(...)`:

```rust
// In test_vm_config_deserialization, verify existing test still passes since
// the JSON has kernel/initrd fields — serde deserializes them as Some(PathBuf)
```

**Step 5: Run tests**

Run: `cargo test -p aleph-tee`
Expected: All pass

**Step 6: Commit**

```
feat(tee): add TeeType::None and make kernel/initrd optional in VmConfig
```

---

### Task 2: Create NoTeeBackend

**Files:**
- Create: `crates/aleph-tee/src/none.rs`
- Modify: `crates/aleph-tee/src/lib.rs`

**Step 1: Write the NoTeeBackend with tests**

Create `crates/aleph-tee/src/none.rs`:

```rust
use anyhow::Result;

use crate::traits::TeeBackend;
use crate::types::{AttestationReport, TeeType, VerificationResult, VmConfig};

/// Backend for non-confidential VMs (plain KVM, no TEE).
///
/// Produces minimal QEMU arguments: just `-cpu host`. No SEV, no OVMF,
/// no hugepages. Attestation methods return errors since there is no TEE
/// to attest.
pub struct NoTeeBackend;

impl TeeBackend for NoTeeBackend {
    fn tee_type(&self) -> TeeType {
        TeeType::None
    }

    fn get_report(&self, _report_data: &[u8; 64]) -> Result<AttestationReport> {
        anyhow::bail!("attestation not available: VM is not running in a TEE")
    }

    fn verify_report(&self, _report: &AttestationReport) -> Result<VerificationResult> {
        anyhow::bail!("attestation not available: VM is not running in a TEE")
    }

    fn qemu_args(&self, _config: &VmConfig) -> Vec<String> {
        vec!["-cpu".to_string(), "host".to_string()]
    }

    fn parse_report(&self, _raw: &[u8]) -> Result<AttestationReport> {
        anyhow::bail!("attestation not available: VM is not running in a TEE")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_tee_backend_type() {
        let backend = NoTeeBackend;
        assert_eq!(backend.tee_type(), TeeType::None);
    }

    #[test]
    fn test_no_tee_qemu_args() {
        let backend = NoTeeBackend;
        let config = VmConfig {
            vm_id: "test".to_string(),
            kernel: None,
            initrd: None,
            disks: vec![],
            vcpus: 2,
            memory_mb: 2048,
            tee: crate::types::TeeConfig {
                backend: TeeType::None,
                policy: None,
            },
            encrypted: false,
            numa_node: None,
        };
        let args = backend.qemu_args(&config);
        assert_eq!(args, vec!["-cpu", "host"]);
    }

    #[test]
    fn test_no_tee_attestation_fails() {
        let backend = NoTeeBackend;
        assert!(backend.get_report(&[0; 64]).is_err());
        assert!(backend.parse_report(&[0; 10]).is_err());
    }
}
```

**Step 2: Register module in lib.rs**

Add to `crates/aleph-tee/src/lib.rs`:

```rust
pub mod none;
pub mod sev_snp;
pub mod traits;
pub mod types;
pub mod x509;
```

**Step 3: Run tests**

Run: `cargo test -p aleph-tee`
Expected: All pass

**Step 4: Commit**

```
feat(tee): add NoTeeBackend for non-confidential VMs
```

---

### Task 3: Update build_qemu_command and SevSnpBackend for conditional kernel boot

**Files:**
- Modify: `crates/aleph-compute-node/src/qemu/args.rs`
- Modify: `crates/aleph-tee/src/sev_snp/qemu.rs`

**Step 1: Write failing tests for disk-boot mode**

Add to tests in `args.rs`:

```rust
#[test]
fn test_build_command_disk_boot_no_kernel() {
    use aleph_tee::none::NoTeeBackend;

    let config = VmConfig {
        vm_id: "test-vm-001".into(),
        kernel: None,
        initrd: None,
        disks: vec![DiskConfig {
            path: PathBuf::from("/images/ubuntu.qcow2"),
            readonly: false,
            format: "qcow2".to_string(),
        }],
        vcpus: 4,
        memory_mb: 2048,
        tee: TeeConfig {
            backend: TeeType::None,
            policy: None,
        },
        encrypted: false,
        numa_node: None,
    };
    let paths = QemuPaths::for_vm("/run/aleph-cvm".as_ref(), "test-vm-001");
    let backend = NoTeeBackend;
    let args = build_qemu_command(&config, &paths, "tap0", &backend, TEST_MAC, None);

    // Should NOT have -kernel, -initrd, -append
    assert!(!args.iter().any(|a| a == "-kernel"), "should not have -kernel: {args:?}");
    assert!(!args.iter().any(|a| a == "-initrd"), "should not have -initrd: {args:?}");
    assert!(!args.iter().any(|a| a == "-append"), "should not have -append: {args:?}");

    // Should have -boot order=c
    let boot_idx = args.iter().position(|a| a == "-boot").expect("-boot flag missing");
    assert_eq!(args[boot_idx + 1], "order=c");

    // Should have -cpu host (from NoTeeBackend)
    let cpu_idx = args.iter().position(|a| a == "-cpu").expect("-cpu flag missing");
    assert_eq!(args[cpu_idx + 1], "host");

    // Should NOT have sev-snp-guest
    assert!(!args.iter().any(|a| a.contains("sev-snp-guest")));
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p aleph-compute-node -- test_build_command_disk_boot`
Expected: FAIL

**Step 3: Update build_qemu_command**

Change signature — `kernel_cmdline` becomes `Option<&str>`:

```rust
pub fn build_qemu_command(
    config: &VmConfig,
    paths: &QemuPaths,
    tap_name: &str,
    tee_backend: &dyn TeeBackend,
    mac_addr: &str,
    kernel_cmdline: Option<&str>,
) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();

    // Base args (no CPU — backend provides it)
    args.extend([
        "-enable-kvm".into(),
        "-smp".into(),
        config.vcpus.to_string(),
        "-m".into(),
        format!("{}M", config.memory_mb),
        "-nographic".into(),
        "-no-reboot".into(),
    ]);

    // Boot mode: direct kernel boot or disk boot
    if let (Some(kernel), Some(initrd)) = (&config.kernel, &config.initrd) {
        args.extend([
            "-kernel".into(),
            kernel.display().to_string(),
            "-initrd".into(),
            initrd.display().to_string(),
        ]);
        if let Some(cmdline) = kernel_cmdline {
            args.extend(["-append".into(), cmdline.into()]);
        }
    } else {
        // Disk boot: BIOS/UEFI boots from first disk
        args.extend(["-boot".into(), "order=c".into()]);
    }

    // Serial output to stdout (captured by journald when running under systemd)
    args.extend(["-serial".into(), "stdio".into()]);

    // QMP socket
    args.extend([
        "-qmp".into(),
        format!("unix:{},server,nowait", paths.qmp_socket.display()),
    ]);

    // Network (TAP) with explicit MAC for DHCP reservation
    args.extend([
        "-netdev".into(),
        format!("tap,id=net0,ifname={tap_name},script=no,downscript=no"),
        "-device".into(),
        format!("virtio-net-pci,netdev=net0,mac={mac_addr}"),
    ]);

    // Disk drives
    for disk in &config.disks {
        let format = match disk.format.as_str() {
            "raw" | "qcow2" => &disk.format,
            other => panic!("unsupported disk format: {other} (allowed: raw, qcow2)"),
        };
        let path_str = disk.path.display().to_string();
        assert!(
            !path_str.contains(','),
            "disk path must not contain commas: {path_str}"
        );
        let ro = if disk.readonly { "on" } else { "off" };
        args.extend([
            "-drive".into(),
            format!("file={path_str},format={format},if=virtio,readonly={ro}"),
        ]);
    }

    // TEE-specific args (includes CPU model)
    args.extend(tee_backend.qemu_args(config));

    args
}
```

**Step 4: Add `-cpu EPYC-v4` to SevSnpBackend's qemu_args**

In `crates/aleph-tee/src/sev_snp/qemu.rs`, add CPU to the returned vec:

```rust
pub fn sev_snp_qemu_args(config: &VmConfig, ovmf_path: &str) -> Vec<String> {
    let policy = config.tee.policy.as_deref().unwrap_or(DEFAULT_POLICY);

    let memfd_opts = if let Some(node) = config.numa_node {
        format!(
            "memory-backend-memfd,id=ram1,size={}M,share=true,hugetlb=on,hugetlbsize=2M,host-nodes={},policy=bind",
            config.memory_mb, node
        )
    } else {
        format!(
            "memory-backend-memfd,id=ram1,size={}M,share=true,hugetlb=on,hugetlbsize=2M",
            config.memory_mb
        )
    };

    vec![
        // CPU model for SEV-SNP
        "-cpu".to_string(),
        "EPYC-v4".to_string(),
        "-machine".to_string(),
        "q35,confidential-guest-support=sev0,memory-backend=ram1,vmport=off".to_string(),
        "-object".to_string(),
        memfd_opts,
        "-object".to_string(),
        format!(
            "sev-snp-guest,id=sev0,cbitpos=51,reduced-phys-bits=1,kernel-hashes=on,policy={policy}"
        ),
        "-nodefaults".to_string(),
        "-bios".to_string(),
        ovmf_path.to_string(),
    ]
}
```

**Step 5: Fix existing tests**

Update all `build_qemu_command` call sites in `args.rs` tests:
- Change `TEST_CMDLINE` usage to `Some(TEST_CMDLINE)`
- Update `make_config` to use `Some(PathBuf::from(...))` for kernel/initrd

Update `sev_snp/qemu.rs` tests:
- Update `make_config` to use `Some(...)` for kernel/initrd
- Add assertion for `-cpu EPYC-v4` in existing tests

**Step 6: Run all tests**

Run: `cargo test -p aleph-compute-node -p aleph-tee`
Expected: All pass

**Step 7: Commit**

```
feat(qemu): conditional kernel boot and per-backend CPU model
```

---

### Task 4: Update NUMA allocator to skip hugepage check for non-TEE VMs

**Files:**
- Modify: `crates/aleph-compute-node/src/numa.rs`

**Step 1: Write failing test**

```rust
#[test]
fn test_allocator_no_hugepage_check() {
    let topo = two_node_topology(
        BTreeSet::from([0, 1, 2, 3]),
        0, // zero hugepages
        BTreeSet::from([4, 5, 6, 7]),
        0,
    );
    let mut alloc = NumaAllocator::new(topo);

    // With uses_hugepages=true, should fail (0 hugepages available)
    assert!(alloc.allocate(1, 1024, None, true).is_err());

    // With uses_hugepages=false, should succeed (memory check skipped)
    let p = alloc.allocate(1, 1024, None, false).unwrap();
    assert_eq!(p.node, 0);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p aleph-compute-node -- test_allocator_no_hugepage`
Expected: FAIL (wrong number of args)

**Step 3: Add `uses_hugepages` parameter to `allocate()`**

```rust
/// Allocate vCPUs and memory on a NUMA node using pack-first strategy.
///
/// When `uses_hugepages` is true, memory capacity is checked against the
/// node's hugepage pool (2 MiB pages). When false (non-confidential VMs),
/// only CPU capacity is checked — the OS manages regular memory.
pub fn allocate(
    &mut self,
    vcpus: u32,
    memory_mb: u32,
    hint: Option<u32>,
    uses_hugepages: bool,
) -> Result<NumaPlacement> {
    let candidates: Vec<usize> = if let Some(node_id) = hint {
        self.topology
            .nodes
            .iter()
            .position(|n| n.id == node_id)
            .map(|i| vec![i])
            .unwrap_or_default()
    } else {
        (0..self.topology.nodes.len()).collect()
    };

    for idx in candidates {
        let node = &self.topology.nodes[idx];
        let available_cpus = node.cpus.len() as u32 - self.allocated_vcpus[idx];

        if vcpus > available_cpus {
            continue;
        }

        if uses_hugepages {
            let capacity_mb = node.total_hugepages * 2;
            let available_mb = capacity_mb.saturating_sub(self.allocated_memory_mb[idx]);
            if memory_mb > available_mb {
                continue;
            }
        }

        self.allocated_vcpus[idx] += vcpus;
        if uses_hugepages {
            self.allocated_memory_mb[idx] += memory_mb;
        }

        return Ok(NumaPlacement {
            node: node.id,
            cpuset: format_cpuset(&node.cpus),
        });
    }

    anyhow::bail!("no NUMA node has enough resources for {vcpus} vCPUs and {memory_mb} MB")
}
```

**Step 4: Fix existing test call sites**

All existing calls to `allocate()` gain `true` as the last arg (preserving current behavior):

```rust
// Example: alloc.allocate(2, 256, None) -> alloc.allocate(2, 256, None, true)
```

**Step 5: Run tests**

Run: `cargo test -p aleph-compute-node -- numa`
Expected: All pass

**Step 6: Commit**

```
feat(numa): skip hugepage capacity check for non-confidential VMs
```

---

### Task 5: Update systemd unit for non-confidential VMs

**Files:**
- Modify: `crates/aleph-compute-node/src/systemd.rs`

**Step 1: Add `needs_sev_devices` parameter**

The `start_vm_unit` function currently always allows `/dev/sev-guest` and `/dev/sev`. Non-confidential VMs don't need these.

```rust
pub fn start_vm_unit(
    vm_id: &str,
    qemu_args: &[String],
    run_dir: &std::path::Path,
    rw_dirs: &[&std::path::Path],
    numa_cpuset: Option<&str>,
    needs_sev_devices: bool,
) -> Result<()> {
    // ... existing code up to DevicePolicy ...

    cmd.args([
        "--property", "DevicePolicy=closed",
        "--property", "DeviceAllow=/dev/kvm rw",
        "--property", "DeviceAllow=/dev/null rw",
        "--property", "DeviceAllow=/dev/urandom r",
        "--property", "DeviceAllow=/dev/net/tun rw",
    ]);

    if needs_sev_devices {
        cmd.args([
            "--property", "DeviceAllow=/dev/sev-guest rw",
            "--property", "DeviceAllow=/dev/sev rw",
        ]);
    }

    // ... rest unchanged ...
}
```

**Step 2: Run tests (unit_name test still passes, no integration tests here)**

Run: `cargo test -p aleph-compute-node -- systemd`
Expected: Pass

**Step 3: Commit**

```
feat(systemd): conditional SEV device access for non-confidential VMs
```

---

### Task 6: Update proto and gRPC service

**Files:**
- Modify: `proto/compute.proto`
- Modify: `crates/aleph-compute-node/src/grpc/service.rs`

**Step 1: Update proto**

Make `kernel` and `initrd` optional-by-convention (protobuf3 strings are already optional — empty string means absent):

```protobuf
message CreateVmRequest {
  string vm_id = 1;
  string kernel = 2;       // empty = disk boot (no direct kernel boot)
  string initrd = 3;       // empty = disk boot
  repeated DiskConfig disks = 4;
  uint32 vcpus = 5;
  uint32 memory_mb = 6;
  TeeConfig tee = 7;       // backend "none" = non-confidential
  string ipv6_address = 8;
  uint32 ipv6_prefix_len = 9;
  bool encrypted = 10;
  uint32 numa_node = 11;
}
```

No structural change needed in proto (the fields already exist as strings, empty = not set). Just update the comments.

**Step 2: Rebuild proto**

Run: `cargo build -p aleph-compute-proto`

**Step 3: Update `parse_tee_config` in service.rs**

```rust
fn parse_tee_config(
    proto: Option<aleph_compute_proto::compute::TeeConfig>,
) -> Result<TeeConfig, Status> {
    let proto = proto.unwrap_or_default();
    let backend = match proto.backend.as_str() {
        "sev-snp" | "" => TeeType::SevSnp,  // default remains sev-snp for backward compat
        "tdx" => TeeType::Tdx,
        "nvidia-cc" => TeeType::NvidiaCc,
        "none" => TeeType::None,
        other => {
            return Err(Status::invalid_argument(format!(
                "unknown TEE backend: {other}"
            )));
        }
    };
    let policy = if proto.policy.is_empty() {
        None
    } else {
        Some(proto.policy)
    };
    Ok(TeeConfig { backend, policy })
}
```

**Step 4: Update `create_vm` validation in service.rs**

Kernel/initrd validation is now conditional on TEE type:

```rust
async fn create_vm(
    &self,
    request: Request<CreateVmRequest>,
) -> Result<Response<VmInfo>, Status> {
    let req = request.into_inner();
    validate_vm_id(&req.vm_id)?;
    validate_vm_resources(req.vcpus, req.memory_mb)?;

    let tee = parse_tee_config(req.tee)?;

    // Kernel/initrd: required for confidential VMs, optional for TeeType::None
    let kernel = if req.kernel.is_empty() {
        if tee.backend != TeeType::None {
            return Err(Status::invalid_argument(
                "kernel is required for confidential VMs",
            ));
        }
        None
    } else {
        validate_file_path(&req.kernel, "kernel")?;
        Some(req.kernel.into())
    };

    let initrd = if req.initrd.is_empty() {
        if tee.backend != TeeType::None {
            return Err(Status::invalid_argument(
                "initrd is required for confidential VMs",
            ));
        }
        None
    } else {
        validate_file_path(&req.initrd, "initrd")?;
        Some(req.initrd.into())
    };

    // Disk validation (unchanged)
    for d in &req.disks {
        validate_file_path(&d.path, "disk path")?;
        let fmt = if d.format.is_empty() { "raw" } else { &d.format };
        if fmt != "raw" && fmt != "qcow2" {
            return Err(Status::invalid_argument(format!(
                "unsupported disk format: {fmt} (allowed: raw, qcow2)"
            )));
        }
        if d.path.contains(',') {
            return Err(Status::invalid_argument(
                "disk path must not contain commas",
            ));
        }
    }

    // Non-confidential disk-boot VMs must have at least one disk
    if kernel.is_none() && req.disks.is_empty() {
        return Err(Status::invalid_argument(
            "at least one disk is required for disk-boot VMs (no kernel specified)",
        ));
    }

    let disks = req
        .disks
        .into_iter()
        .map(|d| DiskConfig {
            path: d.path.into(),
            readonly: d.readonly,
            format: if d.format.is_empty() { "raw".to_string() } else { d.format },
        })
        .collect();

    // IPv6, NUMA hint parsing (unchanged)
    let requested_ipv6 = if req.ipv6_address.is_empty() {
        None
    } else {
        let addr: std::net::Ipv6Addr = req.ipv6_address.parse()
            .map_err(|e| Status::invalid_argument(format!("invalid ipv6_address: {e}")))?;
        let prefix = if req.ipv6_prefix_len == 0 { 128 } else { req.ipv6_prefix_len as u8 };
        let net = Ipv6Net::new(addr, prefix)
            .map_err(|e| Status::invalid_argument(format!("invalid IPv6 prefix: {e}")))?;
        Some(net)
    };

    let numa_hint = if req.numa_node == 0 { None } else { Some(req.numa_node - 1) };

    let config = VmConfig {
        vm_id: req.vm_id,
        kernel,
        initrd,
        disks,
        vcpus: req.vcpus,
        memory_mb: req.memory_mb,
        tee,
        encrypted: req.encrypted,
        numa_node: None,
    };

    let info = self.manager
        .create_vm(config, requested_ipv6, numa_hint)
        .await
        .map_err(|e| Status::internal(e.to_string()))?;

    Ok(Response::new(vm_info_to_proto(&info)))
}
```

**Step 5: Run tests**

Run: `cargo build -p aleph-compute-node`
Expected: Compiles (remaining call-site fixes come in Task 7)

**Step 6: Commit**

```
feat(grpc): support TeeType::None and optional kernel/initrd in CreateVm
```

---

### Task 7: Update VmManager for per-VM backend selection

**Files:**
- Modify: `crates/aleph-compute-node/src/vm/manager.rs`

This is the largest task. The VmManager switches from a single `tee_backend` to a per-VM backend lookup.

**Step 1: Change VmManager to hold multiple backends**

```rust
use std::collections::HashMap;
use aleph_tee::types::TeeType;

pub struct VmManager {
    vms: RwLock<HashMap<String, VmHandle>>,
    run_dir: PathBuf,
    state_dir: PathBuf,
    bridge: String,
    gateway_ip: Ipv4Addr,
    used_ip_offsets: RwLock<BTreeSet<u8>>,
    backends: HashMap<TeeType, Arc<dyn TeeBackend>>,  // changed
    dhcp_hostsdir: Option<PathBuf>,
    nftables: NftablesManager,
    port_forwards: Mutex<PortForwardState>,
    ipv6_allocator: Option<Mutex<Ipv6RangeAllocator>>,
    ndp_proxy: Option<Arc<NdpProxy>>,
    numa: Mutex<NumaAllocator>,
}
```

**Step 2: Update constructor**

```rust
pub fn new(
    run_dir: PathBuf,
    state_dir: PathBuf,
    bridge: String,
    gateway_ip: Ipv4Addr,
    backends: HashMap<TeeType, Arc<dyn TeeBackend>>,  // changed
    dhcp_hostsdir: Option<PathBuf>,
    external_interface: String,
    ipv6_pool: Option<Ipv6Net>,
    use_ndp_proxy: bool,
    numa_topology: NumaTopology,
) -> Self {
    // ... (replace tee_backend with backends in struct init)
}
```

**Step 3: Add backend lookup helper**

```rust
impl VmManager {
    fn get_backend(&self, tee_type: TeeType) -> Result<&dyn TeeBackend> {
        self.backends
            .get(&tee_type)
            .map(|b| b.as_ref())
            .with_context(|| format!("no backend registered for TEE type: {tee_type:?}"))
    }
}
```

**Step 4: Update create_vm**

Key changes in `create_vm`:

```rust
pub async fn create_vm(
    &self,
    mut config: VmConfig,
    requested_ipv6: Option<Ipv6Net>,
    numa_hint: Option<u32>,
) -> Result<VmInfo> {
    // ... (duplicate check, IP allocation, DHCP, TAP, nftables — unchanged) ...

    // Look up the backend for this VM's TEE type
    let tee_backend = self.get_backend(config.tee.backend)?;
    let is_confidential = config.tee.backend != TeeType::None;

    // dm-verity: only for confidential VMs with a rootfs disk
    let kernel_cmdline = if !is_confidential {
        // Non-confidential: no kernel cmdline (disk boot)
        None
    } else if config.encrypted {
        Some(verity::build_kernel_cmdline(None, true))
    } else if let Some(rootfs_disk) = config.disks.first() {
        let vinfo = verity::ensure_verity(&rootfs_disk.path).context(
            "dm-verity setup failed — refusing to boot without integrity verification",
        )?;
        config.disks.insert(
            1,
            aleph_tee::types::DiskConfig {
                path: vinfo.hashtree_path,
                readonly: true,
                format: "raw".to_string(),
            },
        );
        Some(verity::build_kernel_cmdline(Some(&vinfo.root_hash), false))
    } else {
        Some(verity::build_kernel_cmdline(None, false))
    };

    // NUMA allocation: skip hugepage check for non-confidential VMs
    let placement = {
        let mut numa = self.numa.lock().await;
        numa.allocate(config.vcpus, config.memory_mb, numa_hint, is_confidential)?
    };
    config.numa_node = Some(placement.node);

    // Build QEMU command
    let paths = QemuPaths::for_vm(&self.run_dir, &vm_id);
    let mut args = vec!["qemu-system-x86_64".to_string()];
    args.extend(build_qemu_command(
        &config,
        &paths,
        &tap_name,
        tee_backend,
        &mac_addr,
        kernel_cmdline.as_deref(),
    ));

    // Writable disk dirs (unchanged)
    let rw_dirs: Vec<&std::path::Path> = config
        .disks
        .iter()
        .filter(|d| !d.readonly)
        .filter_map(|d| d.path.parent())
        .collect();

    // Spawn QEMU with conditional SEV device access
    let process = match QemuProcess::spawn(
        &args,
        paths,
        vm_id.clone(),
        &rw_dirs,
        Some(placement.cpuset.as_str()),
        is_confidential,  // needs_sev_devices
    ) {
        // ... (error handling unchanged, but pass is_confidential to release) ...
    };

    // ... (persist, insert handle — unchanged) ...
}
```

**Step 5: Update recover_vms**

In `recover_vms`, when restoring NUMA allocations, pass `uses_hugepages` based on TEE type:

```rust
if let Some(node) = pvm.numa_node {
    let mut numa = self.numa.lock().await;
    let is_confidential = pvm.config.tee.backend != TeeType::None;
    let _ = numa
        .allocate(pvm.config.vcpus, pvm.config.memory_mb, Some(node), is_confidential)
        .map_err(|e| warn!(...));
}
```

**Step 6: Run tests**

Run: `cargo build -p aleph-compute-node`
Expected: Compiles

**Step 7: Commit**

```
feat(vm): per-VM backend selection and non-confidential VM flow
```

---

### Task 8: Update QemuProcess::spawn to pass needs_sev_devices

**Files:**
- Modify: `crates/aleph-compute-node/src/qemu/process.rs`

**Step 1: Add `needs_sev_devices` parameter to spawn**

The `spawn` method calls `systemd::start_vm_unit`. Thread the new parameter through:

```rust
pub fn spawn(
    args: &[String],
    paths: QemuPaths,
    vm_id: String,
    rw_dirs: &[&std::path::Path],
    numa_cpuset: Option<&str>,
    needs_sev_devices: bool,
) -> Result<Self> {
    // Ensure runtime directory exists
    if let Some(parent) = paths.qmp_socket.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }

    crate::systemd::start_vm_unit(
        &vm_id,
        args,
        paths.qmp_socket.parent().unwrap_or(std::path::Path::new("/run/aleph-cvm")),
        rw_dirs,
        numa_cpuset,
        needs_sev_devices,
    )?;

    Ok(Self { paths, vm_id })
}
```

**Step 2: Run tests**

Run: `cargo build -p aleph-compute-node`
Expected: Compiles

**Step 3: Commit**

```
feat(qemu): thread needs_sev_devices through process spawn
```

---

### Task 9: Update main.rs to register both backends

**Files:**
- Modify: `crates/aleph-compute-node/src/main.rs`

**Step 1: Register NoTeeBackend alongside SevSnpBackend**

```rust
use std::collections::HashMap;
use aleph_tee::none::NoTeeBackend;
use aleph_tee::sev_snp::SevSnpBackend;
use aleph_tee::traits::TeeBackend;
use aleph_tee::types::TeeType;

// In main():

// Create TEE backends
let mut backend = SevSnpBackend::new(&cli.amd_product);
if let Some(ref path) = cli.ovmf_path {
    backend = backend.with_ovmf_path(path);
}

let mut backends: HashMap<TeeType, Arc<dyn TeeBackend>> = HashMap::new();
backends.insert(TeeType::SevSnp, Arc::new(backend));
backends.insert(TeeType::None, Arc::new(NoTeeBackend));

// Create the VM manager
let manager = Arc::new(VmManager::new(
    cli.run_dir.clone(),
    cli.state_dir.clone(),
    cli.bridge,
    cli.gateway_ip,
    backends,
    cli.dhcp_hostsdir,
    external_interface,
    cli.ipv6_pool,
    use_ndp_proxy,
    numa_topology,
));
```

**Step 2: Run full build**

Run: `cargo build`
Expected: Clean compile

**Step 3: Commit**

```
feat: register NoTeeBackend in compute-node startup
```

---

### Task 10: End-to-end verification and cleanup

**Step 1: Run full test suite**

Run: `cargo test --workspace`
Expected: All pass

**Step 2: Run clippy**

Run: `cargo clippy --workspace -- -D warnings`
Expected: No warnings

**Step 3: Manual smoke test (if SEV hardware available)**

Create a non-confidential VM via gRPC CLI:

```bash
aleph-cvm-cli create-vm \
  --vm-id test-plain \
  --vcpus 2 \
  --memory-mb 2048 \
  --disk /images/ubuntu-24.04.qcow2,rw,qcow2 \
  --tee none

aleph-cvm-cli get-vm --vm-id test-plain
# Expected: status=running, tee_backend=None

aleph-cvm-cli delete-vm --vm-id test-plain
```

**Step 4: Final commit**

```
chore: clippy fixes and test cleanup for non-confidential VM support
```

---

## Summary of Changes by File

| File | Change |
|------|--------|
| `crates/aleph-tee/src/types.rs` | Add `TeeType::None`, make `kernel`/`initrd` `Option<PathBuf>` |
| `crates/aleph-tee/src/none.rs` | New: `NoTeeBackend` implementing `TeeBackend` |
| `crates/aleph-tee/src/lib.rs` | Register `none` module |
| `crates/aleph-tee/src/sev_snp/qemu.rs` | Add `-cpu EPYC-v4` to SEV-SNP args |
| `crates/aleph-compute-node/src/qemu/args.rs` | Remove hardcoded CPU, conditional kernel boot, `-boot order=c` |
| `crates/aleph-compute-node/src/qemu/process.rs` | Thread `needs_sev_devices` parameter |
| `crates/aleph-compute-node/src/numa.rs` | `uses_hugepages` parameter on `allocate()` |
| `crates/aleph-compute-node/src/systemd.rs` | Conditional SEV device allows |
| `crates/aleph-compute-node/src/grpc/service.rs` | Parse `"none"` backend, conditional kernel/initrd validation |
| `crates/aleph-compute-node/src/vm/manager.rs` | `HashMap<TeeType, Arc<dyn TeeBackend>>`, skip verity for None |
| `crates/aleph-compute-node/src/main.rs` | Register both backends |
| `proto/compute.proto` | Updated comments (no structural change) |
