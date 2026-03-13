# aleph-cvm: Confidential VM Orchestrator — MVP Design

## Overview

A Rust rewrite of the aleph-vm node daemon, focused exclusively on confidential VMs.
SEV-SNP first, designed for TDX + NVIDIA CC compatibility. Compatible with current CRN model.

**MVP deliverable:** Boot a Nix-built Fibonacci API inside an SEV-SNP VM, verify
attestation on every API call via a CLI tool.

## Workspace Structure

```
aleph-cvm/
├── Cargo.toml                      # workspace root
├── crates/
│   ├── aleph-tee/                  # shared TEE abstraction library
│   │   ├── traits.rs               # TeeBackend trait
│   │   ├── types.rs                # AttestationReport, VerificationResult, Measurement
│   │   ├── x509.rs                 # X.509 extension encoding/decoding
│   │   └── sev_snp/               # SEV-SNP backend implementation
│   │       ├── backend.rs          # SevSnpBackend impl
│   │       ├── report.rs           # report parsing (ATTESTATION_REPORT struct)
│   │       ├── certs.rs            # VCEK/ASK/ARK cert chain fetching + caching
│   │       └── qemu.rs             # SEV-SNP-specific QEMU flags
│   │
│   ├── aleph-node/                 # host-side node daemon
│   │   ├── api/                    # actix-web handlers
│   │   │   ├── vms.rs              # POST/GET/DELETE /vms
│   │   │   └── health.rs           # GET /health
│   │   ├── vm/
│   │   │   ├── manager.rs          # VmManager: HashMap<VmId, VmHandle>
│   │   │   ├── config.rs           # VmConfig: image paths, resources, TEE params
│   │   │   └── lifecycle.rs        # Defined→Booting→Running→Stopping→Stopped→Failed
│   │   ├── qemu/
│   │   │   ├── process.rs          # spawn QEMU, manage child process
│   │   │   ├── qmp.rs              # QMP client (Unix socket, JSON)
│   │   │   └── args.rs             # command-line builder
│   │   └── network/
│   │       └── tap.rs              # TAP interface + bridge setup
│   │
│   ├── aleph-attest-agent/         # in-VM sidecar (baked into initrd)
│   │   ├── attestation.rs          # /dev/sev-guest ioctl, report request
│   │   ├── tls.rs                  # self-signed cert with attestation X.509 ext
│   │   └── proxy.rs                # reverse proxy to user app + attestation endpoint
│   │
│   └── aleph-attest-cli/           # client verification CLI
│       ├── verify.rs               # extract report from TLS cert, verify chain
│       └── client.rs               # HTTPS client with attestation validation
│
├── nix/                            # Nix expressions for VM images
│   ├── flake.nix
│   ├── kernel.nix                  # Linux kernel with SEV-SNP guest support
│   ├── initrd.nix                  # initrd with attest-agent + init
│   ├── rootfs.nix                  # rootfs with Fibonacci service
│   └── fib-service/                # demo Fibonacci HTTP service
│
└── tests/
    ├── integration/                # integration tests
    └── fixtures/                   # test certificates, sample reports
```

## TEE Abstraction

Trait-based abstraction in `aleph-tee`. SEV-SNP implemented first, TDX and NVIDIA CC
added later as new trait implementations.

```rust
pub trait TeeBackend: Send + Sync {
    /// Request a fresh attestation report
    fn get_report(&self, nonce: &[u8; 64]) -> Result<AttestationReport>;

    /// Verify a report against hardware root of trust
    fn verify_report(&self, report: &AttestationReport) -> Result<VerificationResult>;

    /// QEMU flags needed to launch a confidential VM
    fn qemu_args(&self, config: &VmConfig) -> Vec<String>;

    /// Parse platform-specific report from raw bytes
    fn parse_report(raw: &[u8]) -> Result<AttestationReport>;
}
```

## VM Lifecycle

**State machine:**

```
Defined ──→ Booting ──→ Running ──→ Stopping ──→ Stopped
               │                       │
               └───→ Failed ←──────────┘
```

**Boot sequence (on `POST /vms`):**

1. Validate request — image paths exist, resources sane, TEE type supported
2. Create TAP interface — `ip tuntap add`, attach to bridge
3. Build QEMU command — base args + `TeeBackend::qemu_args()` for SEV-SNP flags
4. Spawn QEMU — `tokio::process::Command`, capture stderr, connect QMP socket
5. Wait for VM ready — poll attest agent's health endpoint via TAP network with timeout
6. Transition to Running — register in VmManager, return VM ID + IP

**Shutdown sequence:**

1. Send `quit` via QMP
2. Wait for process exit with timeout (10s)
3. SIGKILL if timeout
4. Clean up TAP interface
5. Transition to Stopped

**QMP client** — minimal:
- `qmp_capabilities` handshake
- `query-status`
- `quit`
- `stop` / `cont` (for future migration)
- Event stream parsing

**VmManager** — `HashMap<VmId, VmHandle>` behind `Arc<RwLock<>>`. No database,
no persistence. A dead VM is gone.

## Attestation Flow

### In-VM (aleph-attest-agent)

The agent runs inside the guest as the first service after init:

1. Generates ephemeral ECDSA P-384 key pair (in memory, never touches disk)
2. Requests SEV-SNP attestation report via `ioctl` on `/dev/sev-guest` —
   `REPORT_DATA` = SHA-384(public_key), binding key to hardware attestation
3. Builds self-signed X.509 cert:
   - Key: the ephemeral ECDSA key
   - Custom X.509 extension (private OID) containing raw SEV-SNP attestation report
   - Short-lived validity (24h), regenerated on restart
4. Starts HTTPS listener on port 8443 using that cert
5. Reverse-proxies requests to user app on `localhost:8080`
6. Serves `GET /.well-known/attestation?nonce=<hex>` for on-demand fresh reports

### Layer 2 — TLS-bound verification (CLI)

```
CLI connects to VM:8443
  → TLS handshake
  → extract server cert
  → find custom X.509 extension → parse SEV-SNP attestation report
  → check: REPORT_DATA == SHA-384(server_public_key)
  → fetch VCEK cert from AMD KDS (cached)
  → verify report signature: VCEK → ASK → ARK (AMD root)
  → check report fields: guest policy, measurement, TCB version
  → connection is attested — make the API call
```

### Layer 3 — On-demand verification (CLI)

```
CLI sends GET /.well-known/attestation?nonce=<random_hex>
  → agent does ioctl(/dev/sev-guest, nonce) → fresh report
  → returns JSON: { report: <hex>, certs: { vcek: <pem>, ask: <pem> } }
  → CLI verifies report signature + checks nonce is in REPORT_DATA
  → proves liveness (not a replay)
```

### Key dependencies

- `sev` crate for report structures and verification
- `rcgen` or `x509-cert` for certificate generation
- `rustls` for TLS (agent and CLI)

## Nix Image Build

Three artifacts produced by the Nix flake:

**Kernel** (`bzImage`):
- Linux 6.x LTS, SEV-SNP guest support enabled
- `CONFIG_AMD_MEM_ENCRYPT=y`, `CONFIG_SEV_GUEST=y`, `CONFIG_CRYPTO_DEV_CCP=y`
- Minimal — no unnecessary drivers, everything built-in

**Initrd** (`initrd.cpio.gz`):
- `init` script: mounts procfs/sysfs/devtmpfs, brings up eth0 via DHCP,
  mounts rootfs (virtio block), pivot-roots, starts attest agent + user app
- `aleph-attest-agent` binary (statically linked, musl)
- This is the measured component for SEV-SNP launch measurement

**Rootfs** (`rootfs.ext4` or `rootfs.erofs`):
- Minimal NixOS-based filesystem
- Fibonacci HTTP service (Rust, listening on localhost:8080)
- No SSH, no login, no unnecessary services

## Node API

Listens on localhost only. No auth for MVP.

| Method | Path | Description |
|--------|------|-------------|
| `POST /vms` | Boot a new VM | Image paths, resources, TEE config → VM ID + IP |
| `GET /vms/{id}` | Get VM status | State, IP, uptime, TEE type |
| `DELETE /vms/{id}` | Stop and destroy | QMP quit, cleanup → 204 |
| `GET /health` | Node health | Status, available resources, TEE capabilities |

### `POST /vms` request

```json
{
  "vm_id": "fib-demo-01",
  "image": {
    "kernel": "/path/to/bzImage",
    "initrd": "/path/to/initrd.cpio.gz",
    "rootfs": "/path/to/rootfs.ext4"
  },
  "resources": {
    "vcpus": 1,
    "memory_mb": 512
  },
  "tee": {
    "backend": "sev-snp",
    "policy": "0x5"
  }
}
```

### `POST /vms` response

```json
{
  "vm_id": "fib-demo-01",
  "status": "running",
  "ip": "10.0.100.2",
  "tee": {
    "backend": "sev-snp",
    "attested_url": "https://10.0.100.2:8443"
  }
}
```

## Networking (MVP)

TAP interface bridged to a host-only network. VM gets an IP on a private subnet.
No WireGuard, no gateway, no IPv6 overlay. Host talks to VM directly via TAP IP.

## Test Strategy

### Tier 1 — Local (any machine with QEMU/KVM)

- VM image boots to a shell (kernel + initrd + rootfs valid)
- Init script brings up networking (DHCP lease acquired)
- Fibonacci service responds on localhost:8080 inside guest
- Attest agent starts, listens on 8443, TLS handshake works
  (attestation extension absent since no `/dev/sev-guest`, `/.well-known/attestation`
  returns error)
- Node API: POST creates VM, GET returns status, DELETE tears it down
- QMP: handshake succeeds, graceful shutdown works
- TAP + bridge: host can reach guest IP
- QEMU command builder produces correct flags per TEE backend

### Tier 2 — SEV-SNP hardware only

- Attestation report is real and verifiable against AMD cert chain
- TLS cert contains valid attestation extension with correct REPORT_DATA binding
- On-demand endpoint returns fresh report with caller's nonce
- CLI end-to-end: connect, verify attestation, call API, get result
- Launch measurement matches expected value from Nix build

No mock/fake attestation reports. The agent either talks to real `/dev/sev-guest`
or returns an error.

## End-to-End Demo

```bash
# 1. Build the image
nix build .#vm-fib-demo

# 2. Start the node daemon
aleph-node --listen 127.0.0.1:4020 --bridge br0

# 3. Boot a confidential VM
curl -X POST http://localhost:4020/vms -d '{
  "vm_id": "fib-01",
  "image": {
    "kernel": "./result/bzImage",
    "initrd": "./result/initrd.cpio.gz",
    "rootfs": "./result/rootfs.ext4"
  },
  "resources": { "vcpus": 1, "memory_mb": 512 },
  "tee": { "backend": "sev-snp", "policy": "0x5" }
}'

# 4. Call the API with attestation verification
aleph-attest-cli --url https://10.0.100.2:8443/fib/10

# 5. Request fresh attestation (Layer 3)
aleph-attest-cli --url https://10.0.100.2:8443 --fresh-attest

# 6. Tear down
curl -X DELETE http://localhost:4020/vms/fib-01
```
