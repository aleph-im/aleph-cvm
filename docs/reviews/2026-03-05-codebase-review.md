# Aleph CVM Codebase Review — 2026-03-05

Brutally honest review of security, code quality, build system, and architecture.

---

## CRITICAL — Fix Before Any Production Use

### 1. `verify_report()` trait stub always returns `valid: true`
`crates/aleph-tee/src/sev_snp/backend.rs:82` — The `TeeBackend::verify_report()` implementation is a stub that returns `valid: true` for any structurally valid report. The *real* crypto verification exists in `verify.rs`, but the trait interface — the one any future code would naturally reach for — is a landmine. Anyone calling `backend.verify_report()` gets zero security.

### 2. AMD Root Key (ARK) not pinned
`crates/aleph-tee/src/sev_snp/certs.rs` — The cert chain verification checks that ARK is self-signed, ASK is signed by ARK, and VCEK is signed by ASK. But it never verifies the ARK is actually *AMD's* root key. An attacker who poisons the `~/.cache/aleph-tee/kds/` cache with a self-consistent fake chain passes all checks. **The entire attestation model rests on an unpinned root of trust.**

### 3. `init.sh` continues running after fatal errors
`nix/init.sh` — After dm-verity failure ("rootfs may be tampered"), LUKS timeout, or missing block device, the script prints a message and **keeps going**. The attestation agent stays alive. The VM responds to attestation requests despite having no integrity. This is the worst kind of failure: silent degradation of a security guarantee. Should `exec poweroff -f` on any fatal path.

### 4. LUKS passphrase not zeroized
The passphrase file is `rm -f`'d (data stays on tmpfs), the shell passes it through pipes, and on the Rust side (`secrets.rs`) secret values are plain `String`/`Vec<u8>` with no zeroize-on-drop. In an SEV-SNP VM the host can't read guest memory, but a compromised in-guest process can recover the passphrase.

---

## HIGH — Serious Issues

### 5. No input validation on VM IDs
`crates/aleph-compute-node/src/grpc/service.rs:156` — `vm_id` is used directly in systemd unit names, TAP interface names, file paths, nftables chains, and DHCP files with **zero validation**. Path traversal (`../../../etc/shadow`), shell metacharacters, and names exceeding the 15-char Linux interface name limit are all accepted. Same for `kernel`/`initrd` paths — arbitrary filesystem paths accepted from gRPC.

### 6. QEMU disk format string injection
`crates/aleph-compute-node/src/qemu/args.rs:86-95` — `disk.format` and `disk.path` are interpolated into a QEMU `-drive` argument. A format string like `raw,snapshot=on` injects extra QEMU parameters via commas.

### 7. dm-verity failure silently falls back to unverified boot
`crates/aleph-compute-node/src/vm/manager.rs:240-243` — If `ensure_verity` fails, the VM boots **without** dm-verity and only logs a `warn!`. If the caller doesn't pin a measurement, the tampering is invisible.

### 8. Secret injection: incomplete path validation, no size limits, TOCTOU race
`crates/aleph-attest-agent/src/secrets.rs` — Keys are checked for `/` and `..` but not null bytes, control chars, or length. No limit on number of secrets or value sizes (DoS via tmpfs exhaustion). The one-shot `AtomicBool` flag is reset on validation failure, creating a race window where a concurrent attacker request can win.

### 9. No VMPL enforcement in attestation verification
`crates/aleph-tee/src/sev_snp/verify.rs:70-89` — The VMPL level is recorded in output but never checked. A report from VMPL > 0 (less privileged firmware component) would be accepted as valid.

### 10. Recovery doesn't restore nftables or NDP proxy
`crates/aleph-compute-node/src/vm/manager.rs:517-586` — After orchestrator restart, `recover_vms()` restores in-memory state but does NOT recreate nftables rules or NDP proxy entries. Recovered VMs have no port forwarding and no IPv6 connectivity.

### 11. No systemd sandboxing for QEMU
`crates/aleph-compute-node/src/systemd.rs:22-36` — The transient units have no `NoNewPrivileges`, `PrivateTmp`, `ProtectSystem`, `DevicePolicy`, or seccomp filters. QEMU runs with full privileges of the parent process.

---

## MEDIUM — Should Fix

### 12. IP allocator wraps at 255, never reclaims
`vm/manager.rs:157-162` — `next_ip_offset` is a `u8` with `wrapping_add`. After 255 VMs (including deleted ones), offset 0 = gateway IP collision. The proper `Ipv4Allocator` in `aleph-network` exists but isn't wired in.

### 13. Port forward TOCTOU race
`vm/manager.rs:428-467` — The port forward lock is acquired and released **three times** during allocation. Between releases, another concurrent request can claim the same port.

### 14. TLS private key not zeroized; QMP socket world-readable; gRPC socket default perms
Multiple file permission and memory hygiene issues across the codebase.

### 15. Hardcoded `/24` in init.sh ignores parsed subnet mask
`nix/init.sh:35` — Parses the mask from cmdline but then hardcodes `/24` in `ip addr add`.

### 16. Kernel has no explicit hardening options
`nix/kernel.nix` — No explicit `KASLR`, `HARDENED_USERCOPY`, `FORTIFY_SOURCE`, `STACKPROTECTOR_STRONG`. May be nixpkgs defaults, but for a CVM kernel they should be explicitly verified.

### 17. `build-ovmf.sh` builds wrong OVMF variant
`scripts/build-ovmf.sh:92` — Builds `OvmfPkgX64.dsc` (standard) instead of `AmdSevX64.dsc` (needed for kernel hash measurement). The Nix build is correct, but this script will produce an OVMF that silently breaks attestation.

### 18. Measurement hardcodes 2 vCPUs and EPYC-v4
`nix/flake.nix:125-126` — If someone changes VM size, the measurement won't match. Should be parameterized.

### 19. `expect()` in production paths
`main.rs:117` (`ensure_bridge`), `scheduler-agent/main.rs:292-296` (token parsing) — These should propagate errors with `?`, not panic.

### 20. Unregistered OID `1.3.6.1.4.1.60000.1.1`
`crates/aleph-tee/src/x509.rs:11` — PEN 60000 is likely not registered to Aleph. Could collide.

---

## LOW / Technical Debt

### 21. Duplicate code everywhere
- `run_ip()` exists in 3 places
- `ensure_bridge()` exists in 2 places
- `NetworkManager` and `VmManager` have parallel, overlapping network logic
- UDS gRPC connection code duplicated between CLI and scheduler-agent

### 22. Dead code
- `QmpClient` is fully built but never used (`#[allow(dead_code)]`)
- `VmState` machine is defined with tests but bypassed — VMs jump straight to `Running`
- `qapi`/`qapi-qmp` dependencies are unused in production

### 23. `rand = "0.8"` in attest-cli vs workspace `rand = "0.9"`
Two versions of rand compiled and linked.

### 24. No /proc, /sys, /dev, or DNS in chroot
Any real workload beyond fib-service will fail. No `/dev/urandom`, no DNS resolution.

### 25. No resource limits, health monitoring, metrics, or network isolation between VMs
No quota enforcement, no VM health checks post-boot, no Prometheus endpoint, no inter-VM firewall rules.

---

## What's Actually Good

- **Architecture is clean.** The crate separation, host/guest boundary, and TEE trait abstraction are well-designed.
- **The attestation *flow* is correct.** TLS-bound (Layer 2) + nonce-bound (Layer 3) dual verification is sound. The transitive measurement chain (AMD PSP → OVMF+kernel+initrd+cmdline → roothash → dm-verity) is cryptographically solid *in design*.
- **Nix build is excellent.** Reproducible, well-pinned, correct verity hash tree computation, proper sev-snp-measure override.
- **Test coverage is good where it exists.** Lifecycle, persistence, QEMU args, verity cmdline, X.509 roundtrip, allocators, nftables rules — all well-tested.
- **Demo scripts are thorough.** Cleanup traps, preflight checks, colored output, timeout polling.
- **Systemd integration is thoughtful.** VMs surviving orchestrator restarts is the right design.

---

## Bottom Line

The *design* is strong. The *implementation* is early-stage with several security gaps that would be unacceptable in production. The three most urgent fixes are:

1. **Pin the AMD ARK** — without this, attestation verification is theater
2. **Halt on fatal init errors** — a VM with failed integrity running an attestation agent is actively dangerous
3. **Validate all user inputs** — VM IDs, paths, disk formats flow unsanitized into shell commands and file paths

The codebase reads like a fast-moving prototype that got the hard crypto and attestation architecture right but skipped input validation and defense-in-depth. That's a reasonable place to be at this stage — but these gaps need to close before this goes anywhere near real workloads.
