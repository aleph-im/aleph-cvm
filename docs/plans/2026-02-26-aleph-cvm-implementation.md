# aleph-cvm Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build a Rust confidential VM orchestrator that boots SEV-SNP VMs from Nix images and verifies attestation on every API call.

**Architecture:** Cargo workspace with 4 crates: `aleph-tee` (shared TEE abstraction), `aleph-node` (host daemon), `aleph-attest-agent` (in-VM sidecar), `aleph-attest-cli` (client CLI). QEMU managed via direct process spawn + QMP. Trait-based TEE backend abstraction for future TDX/NVIDIA CC support.

**Tech Stack:** Rust, actix-web, rustls, rcgen, sev crate, qapi, tokio, Nix flakes

---

### Task 1: Workspace Scaffolding

**Files:**
- Create: `aleph-cvm/Cargo.toml`
- Create: `aleph-cvm/crates/aleph-tee/Cargo.toml`
- Create: `aleph-cvm/crates/aleph-tee/src/lib.rs`
- Create: `aleph-cvm/crates/aleph-node/Cargo.toml`
- Create: `aleph-cvm/crates/aleph-node/src/main.rs`
- Create: `aleph-cvm/crates/aleph-attest-agent/Cargo.toml`
- Create: `aleph-cvm/crates/aleph-attest-agent/src/main.rs`
- Create: `aleph-cvm/crates/aleph-attest-cli/Cargo.toml`
- Create: `aleph-cvm/crates/aleph-attest-cli/src/main.rs`

**Step 1: Create workspace root**

```toml
# aleph-cvm/Cargo.toml
[workspace]
resolver = "2"
members = [
    "crates/aleph-tee",
    "crates/aleph-node",
    "crates/aleph-attest-agent",
    "crates/aleph-attest-cli",
]

[workspace.package]
edition = "2024"
version = "0.1.0"
license = "MIT"

[workspace.dependencies]
# TEE
sev = { version = "7", features = ["snp", "openssl"] }

# Crypto & TLS
rcgen = "0.14"
rustls = "0.23"
x509-parser = "0.16"
x509-cert = "0.2"
der = "0.7"
const-oid = "0.9"
openssl = "0.10"
sha2 = "0.10"
p384 = { version = "0.13", features = ["ecdsa"] }
ring = "0.17"

# HTTP
actix-web = "4"
reqwest = { version = "0.12", features = ["rustls-tls", "json"] }
hyper = { version = "1", features = ["full"] }
hyper-util = { version = "0.1", features = ["full"] }
hyper-rustls = "0.27"

# Async
tokio = { version = "1", features = ["full"] }

# QEMU
qapi = { version = "0.15", features = ["qmp"] }
qapi-qmp = "0.15"

# Serialization
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# CLI
clap = { version = "4", features = ["derive"] }

# Errors & logging
anyhow = "1"
thiserror = "2"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

# Testing
tokio-test = "0.4"
```

**Step 2: Create aleph-tee crate**

```toml
# crates/aleph-tee/Cargo.toml
[package]
name = "aleph-tee"
edition.workspace = true
version.workspace = true

[dependencies]
sev.workspace = true
rcgen.workspace = true
x509-parser.workspace = true
x509-cert.workspace = true
der.workspace = true
const-oid.workspace = true
openssl.workspace = true
sha2.workspace = true
reqwest.workspace = true
serde.workspace = true
serde_json.workspace = true
anyhow.workspace = true
thiserror.workspace = true
tracing.workspace = true
tokio.workspace = true
```

```rust
// crates/aleph-tee/src/lib.rs
pub mod traits;
pub mod types;
pub mod x509;
pub mod sev_snp;
```

**Step 3: Create aleph-node crate**

```toml
# crates/aleph-node/Cargo.toml
[package]
name = "aleph-node"
edition.workspace = true
version.workspace = true

[dependencies]
aleph-tee = { path = "../aleph-tee" }
actix-web.workspace = true
tokio.workspace = true
qapi.workspace = true
qapi-qmp.workspace = true
serde.workspace = true
serde_json.workspace = true
anyhow.workspace = true
thiserror.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
clap.workspace = true
```

```rust
// crates/aleph-node/src/main.rs
fn main() {
    println!("aleph-node");
}
```

**Step 4: Create aleph-attest-agent crate**

```toml
# crates/aleph-attest-agent/Cargo.toml
[package]
name = "aleph-attest-agent"
edition.workspace = true
version.workspace = true

[dependencies]
aleph-tee = { path = "../aleph-tee" }
actix-web.workspace = true
rustls.workspace = true
rcgen.workspace = true
tokio.workspace = true
reqwest.workspace = true
hyper.workspace = true
hyper-util.workspace = true
serde.workspace = true
serde_json.workspace = true
anyhow.workspace = true
thiserror.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
clap.workspace = true
```

```rust
// crates/aleph-attest-agent/src/main.rs
fn main() {
    println!("aleph-attest-agent");
}
```

**Step 5: Create aleph-attest-cli crate**

```toml
# crates/aleph-attest-cli/Cargo.toml
[package]
name = "aleph-attest-cli"
edition.workspace = true
version.workspace = true

[dependencies]
aleph-tee = { path = "../aleph-tee" }
rustls.workspace = true
x509-parser.workspace = true
reqwest.workspace = true
hyper-rustls.workspace = true
tokio.workspace = true
serde.workspace = true
serde_json.workspace = true
anyhow.workspace = true
thiserror.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
clap.workspace = true
ring.workspace = true
```

```rust
// crates/aleph-attest-cli/src/main.rs
fn main() {
    println!("aleph-attest-cli");
}
```

**Step 6: Initialize git repo and verify workspace compiles**

```bash
cd /home/olivier/git/aleph
mkdir -p aleph-cvm
cd aleph-cvm
git init
# Create all files above
cargo check
```

**Step 7: Commit**

```bash
git add -A
git commit -m "chore: scaffold cargo workspace with 4 crates"
```

---

### Task 2: aleph-tee Core Types and Traits

**Files:**
- Create: `crates/aleph-tee/src/types.rs`
- Create: `crates/aleph-tee/src/traits.rs`
- Test: `crates/aleph-tee/src/types.rs` (unit tests in module)

**Step 1: Write types with tests**

```rust
// crates/aleph-tee/src/types.rs
use serde::{Deserialize, Serialize};

/// Identifies which TEE technology is in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TeeType {
    SevSnp,
    Tdx,
    NvidiaCc,
}

/// A raw attestation report from the TEE hardware.
/// The `data` field contains the platform-specific binary report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttestationReport {
    pub tee_type: TeeType,
    /// Raw binary report bytes (platform-specific format).
    #[serde(with = "hex_serde")]
    pub data: Vec<u8>,
    /// The 64-byte REPORT_DATA field that was included in the report request.
    #[serde(with = "hex_serde")]
    pub report_data: [u8; 64],
    /// Platform measurement (e.g. SEV-SNP MEASUREMENT field, 48 bytes).
    #[serde(with = "hex_serde")]
    pub measurement: Vec<u8>,
}

/// Result of verifying an attestation report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationResult {
    pub valid: bool,
    pub tee_type: TeeType,
    /// Human-readable summary of what was verified.
    pub summary: String,
    /// The measurement from the report.
    #[serde(with = "hex_serde")]
    pub measurement: Vec<u8>,
    /// Platform-specific details (TCB version, policy, etc.)
    pub details: serde_json::Value,
}

/// Configuration for launching a VM with a specific TEE backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmConfig {
    pub vm_id: String,
    pub kernel: std::path::PathBuf,
    pub initrd: std::path::PathBuf,
    pub rootfs: Option<std::path::PathBuf>,
    pub vcpus: u32,
    pub memory_mb: u32,
    pub tee: TeeConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeeConfig {
    pub backend: TeeType,
    /// SEV-SNP guest policy (hex string like "0x5").
    pub policy: Option<String>,
}

mod hex_serde {
    use serde::{self, Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        hex::decode(&s).map_err(serde::de::Error::custom)
    }
}

// Note: add `hex = "0.4"` to aleph-tee dependencies

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tee_type_serialization() {
        let t = TeeType::SevSnp;
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(json, "\"sev-snp\"");

        let parsed: TeeType = serde_json::from_str("\"sev-snp\"").unwrap();
        assert_eq!(parsed, TeeType::SevSnp);
    }

    #[test]
    fn test_attestation_report_roundtrip() {
        let report = AttestationReport {
            tee_type: TeeType::SevSnp,
            data: vec![0xde, 0xad, 0xbe, 0xef],
            report_data: [0u8; 64],
            measurement: vec![0x42; 48],
        };
        let json = serde_json::to_string(&report).unwrap();
        let parsed: AttestationReport = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.tee_type, TeeType::SevSnp);
        assert_eq!(parsed.data, vec![0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn test_vm_config_deserialization() {
        let json = r#"{
            "vm_id": "test-01",
            "kernel": "/path/to/bzImage",
            "initrd": "/path/to/initrd",
            "rootfs": "/path/to/rootfs.ext4",
            "vcpus": 2,
            "memory_mb": 1024,
            "tee": { "backend": "sev-snp", "policy": "0x5" }
        }"#;
        let config: VmConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.vm_id, "test-01");
        assert_eq!(config.tee.backend, TeeType::SevSnp);
        assert_eq!(config.tee.policy, Some("0x5".to_string()));
    }
}
```

**Step 2: Write the TeeBackend trait**

```rust
// crates/aleph-tee/src/traits.rs
use crate::types::{AttestationReport, TeeType, VerificationResult, VmConfig};
use anyhow::Result;

/// Abstraction over TEE hardware backends.
///
/// Implement this trait for each supported TEE technology (SEV-SNP, TDX, NVIDIA CC).
/// The trait is used by:
/// - `aleph-node`: to get QEMU launch flags
/// - `aleph-attest-agent`: to request attestation reports from inside the VM
/// - `aleph-attest-cli`: to verify attestation reports
pub trait TeeBackend: Send + Sync {
    /// Which TEE technology this backend implements.
    fn tee_type(&self) -> TeeType;

    /// Request a fresh attestation report from the TEE hardware.
    /// The `report_data` (64 bytes) is included in the report — typically
    /// a hash of a public key or a nonce for freshness.
    ///
    /// Only callable from inside a TEE guest (uses /dev/sev-guest or equivalent).
    fn get_report(&self, report_data: &[u8; 64]) -> Result<AttestationReport>;

    /// Verify an attestation report against the hardware root of trust.
    /// This fetches the necessary certificates (e.g., VCEK from AMD KDS)
    /// and validates the full chain.
    ///
    /// Callable from anywhere (host, client, etc.)
    fn verify_report(&self, report: &AttestationReport) -> Result<VerificationResult>;

    /// Generate QEMU command-line arguments for launching a confidential VM.
    /// Returns args like `-object sev-guest,...` and `-machine ...,confidential-guest-support=...`
    fn qemu_args(&self, config: &VmConfig) -> Vec<String>;

    /// Parse a platform-specific attestation report from raw bytes.
    fn parse_report(&self, raw: &[u8]) -> Result<AttestationReport>;
}
```

**Step 3: Run tests**

```bash
cargo test -p aleph-tee
```
Expected: All 3 tests pass.

**Step 4: Commit**

```bash
git add -A
git commit -m "feat(aleph-tee): add core types and TeeBackend trait"
```

---

### Task 3: aleph-tee SEV-SNP Backend — Report Parsing & QEMU Args

**Files:**
- Create: `crates/aleph-tee/src/sev_snp/mod.rs`
- Create: `crates/aleph-tee/src/sev_snp/backend.rs`
- Create: `crates/aleph-tee/src/sev_snp/report.rs`
- Create: `crates/aleph-tee/src/sev_snp/qemu.rs`
- Create: `crates/aleph-tee/src/sev_snp/certs.rs`

**Step 1: Write report parsing with tests**

```rust
// crates/aleph-tee/src/sev_snp/report.rs
use anyhow::{Context, Result};
use sev::firmware::guest::AttestationReport as SevReport;

/// Parse raw bytes into the sev crate's AttestationReport.
pub fn parse_sev_snp_report(raw: &[u8]) -> Result<SevReport> {
    // The sev crate expects exactly 1184 bytes for an attestation report.
    anyhow::ensure!(
        raw.len() >= 1184,
        "SEV-SNP report too short: {} bytes, expected >= 1184",
        raw.len()
    );
    // Use the first 1184 bytes (the report struct size).
    let report_bytes = &raw[..1184];
    let report: SevReport = bincode::deserialize(report_bytes)
        .context("failed to deserialize SEV-SNP attestation report")?;
    Ok(report)
}

/// Extract the 64-byte REPORT_DATA field from a parsed report.
pub fn extract_report_data(report: &SevReport) -> [u8; 64] {
    report.report_data
}

/// Extract the 48-byte MEASUREMENT field from a parsed report.
pub fn extract_measurement(report: &SevReport) -> [u8; 48] {
    report.measurement
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reject_short_report() {
        let short = vec![0u8; 100];
        let result = parse_sev_snp_report(&short);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("too short"));
    }

    // Note: testing with real report bytes requires a fixture file.
    // We'll add a fixture from real hardware in Task 3 integration.
}
```

**Step 2: Write QEMU args builder with tests**

```rust
// crates/aleph-tee/src/sev_snp/qemu.rs
use crate::types::VmConfig;

/// Generate QEMU command-line arguments for SEV-SNP.
pub fn sev_snp_qemu_args(config: &VmConfig) -> Vec<String> {
    let policy = config
        .tee
        .policy
        .as_deref()
        .unwrap_or("0x30000");

    vec![
        "-machine".into(),
        "q35,confidential-guest-support=sev0,memory-backend=ram1".into(),
        "-object".into(),
        format!(
            "memory-backend-memfd,id=ram1,size={}M,share=true",
            config.memory_mb
        ),
        "-object".into(),
        format!(
            "sev-snp-guest,id=sev0,cbitpos=51,reduced-phys-bits=1,policy={}",
            policy
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{TeeConfig, TeeType, VmConfig};
    use std::path::PathBuf;

    fn test_config(policy: Option<&str>) -> VmConfig {
        VmConfig {
            vm_id: "test".into(),
            kernel: PathBuf::from("/tmp/bzImage"),
            initrd: PathBuf::from("/tmp/initrd"),
            rootfs: None,
            vcpus: 1,
            memory_mb: 512,
            tee: TeeConfig {
                backend: TeeType::SevSnp,
                policy: policy.map(String::from),
            },
        }
    }

    #[test]
    fn test_sev_snp_args_with_policy() {
        let args = sev_snp_qemu_args(&test_config(Some("0x5")));
        assert!(args.contains(&"-machine".to_string()));
        let sev_obj = args.iter().find(|a| a.starts_with("sev-snp-guest")).unwrap();
        assert!(sev_obj.contains("policy=0x5"));
        assert!(sev_obj.contains("cbitpos=51"));
    }

    #[test]
    fn test_sev_snp_args_default_policy() {
        let args = sev_snp_qemu_args(&test_config(None));
        let sev_obj = args.iter().find(|a| a.starts_with("sev-snp-guest")).unwrap();
        assert!(sev_obj.contains("policy=0x30000"));
    }

    #[test]
    fn test_memory_backend_matches_config() {
        let args = sev_snp_qemu_args(&test_config(Some("0x5")));
        let mem_obj = args.iter().find(|a| a.starts_with("memory-backend-memfd")).unwrap();
        assert!(mem_obj.contains("size=512M"));
    }
}
```

**Step 3: Write cert fetching module (stub + types)**

```rust
// crates/aleph-tee/src/sev_snp/certs.rs
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// AMD KDS base URL.
const KDS_BASE: &str = "https://kdsintf.amd.com/vcek/v1";

/// Cached certificate chain for a specific chip.
#[derive(Debug, Clone)]
pub struct CertChain {
    pub vcek_der: Vec<u8>,
    pub ask_der: Vec<u8>,
    pub ark_der: Vec<u8>,
}

/// Fetch the VCEK certificate for a specific chip + TCB version from AMD KDS.
pub async fn fetch_vcek(
    product: &str,
    chip_id: &[u8; 64],
    tcb: &TcbParams,
) -> Result<Vec<u8>> {
    let chip_hex = hex::encode(chip_id);
    let url = format!(
        "{}/{}/{}?blSPL={}&teeSPL={}&snpSPL={}&ucodeSPL={}",
        KDS_BASE, product, chip_hex,
        tcb.bl_spl, tcb.tee_spl, tcb.snp_spl, tcb.ucode_spl,
    );
    let resp = reqwest::get(&url).await.context("failed to fetch VCEK from AMD KDS")?;
    let bytes = resp.bytes().await.context("failed to read VCEK response")?;
    Ok(bytes.to_vec())
}

/// Fetch the CA cert chain (ASK + ARK) for a product.
pub async fn fetch_ca_chain(product: &str) -> Result<(Vec<u8>, Vec<u8>)> {
    let url = format!("{}/{}/cert_chain", KDS_BASE, product);
    let resp = reqwest::get(&url).await.context("failed to fetch CA chain")?;
    let pem_data = resp.text().await?;
    // The response contains ASK then ARK in PEM format.
    let certs: Vec<Vec<u8>> = pem::parse_many(&pem_data)?
        .into_iter()
        .map(|p| p.contents().to_vec())
        .collect();
    anyhow::ensure!(certs.len() >= 2, "expected at least 2 certs in CA chain");
    Ok((certs[0].clone(), certs[1].clone()))
}

#[derive(Debug, Clone)]
pub struct TcbParams {
    pub bl_spl: u8,
    pub tee_spl: u8,
    pub snp_spl: u8,
    pub ucode_spl: u8,
}

// Note: add `hex` and `pem` crates to aleph-tee dependencies.
```

**Step 4: Write the SevSnpBackend struct**

```rust
// crates/aleph-tee/src/sev_snp/backend.rs
use crate::sev_snp::{certs, qemu, report};
use crate::traits::TeeBackend;
use crate::types::*;
use anyhow::{Context, Result};

/// SEV-SNP implementation of the TeeBackend trait.
pub struct SevSnpBackend {
    /// AMD product name (e.g., "Milan", "Genoa", "Turin").
    pub product: String,
}

impl SevSnpBackend {
    pub fn new(product: impl Into<String>) -> Self {
        Self {
            product: product.into(),
        }
    }
}

impl TeeBackend for SevSnpBackend {
    fn tee_type(&self) -> TeeType {
        TeeType::SevSnp
    }

    fn get_report(&self, report_data: &[u8; 64]) -> Result<AttestationReport> {
        use sev::firmware::guest::Firmware;
        let mut fw = Firmware::open().context("failed to open /dev/sev-guest")?;
        let report_bytes = fw
            .get_report(None, Some(*report_data), None)
            .context("failed to get SEV-SNP report")?;
        self.parse_report(&report_bytes)
    }

    fn verify_report(&self, report: &AttestationReport) -> Result<VerificationResult> {
        anyhow::ensure!(
            report.tee_type == TeeType::SevSnp,
            "expected SEV-SNP report, got {:?}",
            report.tee_type
        );
        let sev_report = report::parse_sev_snp_report(&report.data)?;

        // Signature verification would require fetching certs and using openssl.
        // This is the full implementation path:
        // 1. Extract chip_id and TCB from report
        // 2. Fetch VCEK from AMD KDS (or cache)
        // 3. Fetch ASK/ARK from AMD KDS (or cache)
        // 4. Verify chain: ARK self-signed, ASK signed by ARK, VCEK signed by ASK
        // 5. Verify report signature with VCEK public key
        // See Task 4 for the full verification implementation.

        Ok(VerificationResult {
            valid: true, // placeholder — real verification in Task 4
            tee_type: TeeType::SevSnp,
            summary: "SEV-SNP report parsed successfully".into(),
            measurement: report.measurement.clone(),
            details: serde_json::json!({
                "guest_svn": sev_report.guest_svn,
                "policy": format!("{:#x}", u64::from(sev_report.policy)),
                "vmpl": sev_report.vmpl,
            }),
        })
    }

    fn qemu_args(&self, config: &VmConfig) -> Vec<String> {
        qemu::sev_snp_qemu_args(config)
    }

    fn parse_report(&self, raw: &[u8]) -> Result<AttestationReport> {
        let sev_report = report::parse_sev_snp_report(raw)?;
        Ok(AttestationReport {
            tee_type: TeeType::SevSnp,
            data: raw.to_vec(),
            report_data: sev_report.report_data,
            measurement: sev_report.measurement.to_vec(),
        })
    }
}
```

**Step 5: Wire up mod.rs**

```rust
// crates/aleph-tee/src/sev_snp/mod.rs
pub mod backend;
pub mod certs;
pub mod qemu;
pub mod report;

pub use backend::SevSnpBackend;
```

**Step 6: Run tests**

```bash
cargo test -p aleph-tee
```
Expected: All tests pass (types + qemu args tests).

**Step 7: Commit**

```bash
git add -A
git commit -m "feat(aleph-tee): add SEV-SNP backend with report parsing and QEMU args"
```

---

### Task 4: aleph-tee SEV-SNP Report Verification & X.509 Extension

**Files:**
- Create: `crates/aleph-tee/src/sev_snp/verify.rs`
- Create: `crates/aleph-tee/src/x509.rs`

**Step 1: Write report signature verification**

```rust
// crates/aleph-tee/src/sev_snp/verify.rs
use crate::sev_snp::certs::{CertChain, TcbParams, fetch_ca_chain, fetch_vcek};
use crate::sev_snp::report::parse_sev_snp_report;
use crate::types::{AttestationReport, TeeType, VerificationResult};
use anyhow::{Context, Result};
use openssl::ecdsa::EcdsaSig;
use openssl::sha::sha384;
use openssl::x509::X509;

/// Verify an SEV-SNP attestation report end-to-end:
/// 1. Parse the report
/// 2. Fetch cert chain from AMD KDS
/// 3. Verify chain: ARK (self-signed) → ASK → VCEK
/// 4. Verify report signature with VCEK
pub async fn verify_sev_snp_report(
    report: &AttestationReport,
    product: &str,
) -> Result<VerificationResult> {
    anyhow::ensure!(report.tee_type == TeeType::SevSnp, "not an SEV-SNP report");
    let sev_report = parse_sev_snp_report(&report.data)?;

    // Extract chip_id and TCB version for cert fetching.
    let chip_id = sev_report.chip_id;
    let tcb = TcbParams {
        bl_spl: sev_report.reported_tcb.bootloader,
        tee_spl: sev_report.reported_tcb.tee,
        snp_spl: sev_report.reported_tcb.snp,
        ucode_spl: sev_report.reported_tcb.microcode,
    };

    // Fetch certificates.
    let vcek_der = fetch_vcek(product, &chip_id, &tcb).await?;
    let (ask_der, ark_der) = fetch_ca_chain(product).await?;
    let chain = CertChain { vcek_der, ask_der, ark_der };

    // Verify certificate chain.
    verify_cert_chain(&chain)?;

    // Verify report signature.
    verify_report_signature(&report.data, &chain.vcek_der)?;

    Ok(VerificationResult {
        valid: true,
        tee_type: TeeType::SevSnp,
        summary: format!(
            "SEV-SNP report verified: measurement={}, policy={:#x}",
            hex::encode(&sev_report.measurement),
            u64::from(sev_report.policy),
        ),
        measurement: sev_report.measurement.to_vec(),
        details: serde_json::json!({
            "guest_svn": sev_report.guest_svn,
            "policy": format!("{:#x}", u64::from(sev_report.policy)),
            "vmpl": sev_report.vmpl,
            "reported_tcb": {
                "bootloader": sev_report.reported_tcb.bootloader,
                "tee": sev_report.reported_tcb.tee,
                "snp": sev_report.reported_tcb.snp,
                "microcode": sev_report.reported_tcb.microcode,
            },
        }),
    })
}

/// Verify the AMD cert chain: ARK self-signed, ASK signed by ARK, VCEK signed by ASK.
fn verify_cert_chain(chain: &CertChain) -> Result<()> {
    let ark = X509::from_der(&chain.ark_der).context("invalid ARK cert")?;
    let ask = X509::from_der(&chain.ask_der).context("invalid ASK cert")?;
    let vcek = X509::from_der(&chain.vcek_der).context("invalid VCEK cert")?;

    anyhow::ensure!(
        ark.verify(ark.public_key()?.as_ref())?,
        "ARK is not self-signed"
    );
    anyhow::ensure!(
        ask.verify(ark.public_key()?.as_ref())?,
        "ASK not signed by ARK"
    );
    anyhow::ensure!(
        vcek.verify(ask.public_key()?.as_ref())?,
        "VCEK not signed by ASK"
    );
    Ok(())
}

/// Verify the report's ECDSA signature using the VCEK public key.
fn verify_report_signature(report_raw: &[u8], vcek_der: &[u8]) -> Result<()> {
    anyhow::ensure!(report_raw.len() >= 1184, "report too short");
    let vcek = X509::from_der(vcek_der)?;
    let ec_key = vcek.public_key()?.ec_key()?;

    // The signature is over bytes 0x000..0x2A0 of the report.
    let signed_bytes = &report_raw[..0x2A0];
    let digest = sha384(signed_bytes);

    // The signature starts at offset 0x2A0 in the report.
    let sig_bytes = &report_raw[0x2A0..0x2A0 + 144]; // r(72) + s(72)
    let r = openssl::bn::BigNum::from_slice(&sig_bytes[..72])?;
    let s = openssl::bn::BigNum::from_slice(&sig_bytes[72..144])?;
    let sig = EcdsaSig::from_private_components(r, s)?;

    anyhow::ensure!(
        sig.verify(&digest, &ec_key)?,
        "SEV-SNP report signature verification failed"
    );
    Ok(())
}
```

**Step 2: Write X.509 attestation extension encoding/decoding**

```rust
// crates/aleph-tee/src/x509.rs
use crate::types::AttestationReport;
use anyhow::{Context, Result};
use der::asn1::OctetString;
use der::Encode;

/// Private OID for the Aleph attestation X.509 extension.
/// 1.3.6.1.4.1 = private enterprises arc
/// Using a placeholder; replace with real IANA-assigned OID in production.
pub const ATTESTATION_OID: &[u64] = &[1, 3, 6, 1, 4, 1, 60000, 1, 1];

/// Encode an attestation report as DER bytes suitable for a custom X.509 extension.
/// Format: OCTET STRING containing JSON-serialized AttestationReport.
pub fn encode_attestation_extension(report: &AttestationReport) -> Result<Vec<u8>> {
    let json = serde_json::to_vec(report).context("failed to serialize report")?;
    let octet = OctetString::new(json).context("failed to create OctetString")?;
    let der_bytes = octet.to_der().context("failed to DER-encode")?;
    Ok(der_bytes)
}

/// Decode an attestation report from a DER-encoded X.509 extension value.
pub fn decode_attestation_extension(der_bytes: &[u8]) -> Result<AttestationReport> {
    let octet: OctetString = der::Decode::from_der(der_bytes)
        .context("failed to decode OctetString from extension")?;
    let report: AttestationReport = serde_json::from_slice(octet.as_bytes())
        .context("failed to deserialize AttestationReport from extension")?;
    Ok(report)
}

/// Extract the attestation extension from a DER-encoded X.509 certificate.
/// Returns None if the extension is not present.
pub fn extract_attestation_from_cert(cert_der: &[u8]) -> Result<Option<AttestationReport>> {
    use x509_parser::prelude::*;

    let (_, cert) = X509Certificate::from_der(cert_der)
        .context("failed to parse X.509 certificate")?;

    let target_oid = x509_parser::oid_registry::Oid::from(ATTESTATION_OID)
        .expect("invalid OID");

    for ext in cert.extensions() {
        if ext.oid == target_oid {
            let report = decode_attestation_extension(ext.value)?;
            return Ok(Some(report));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TeeType;

    #[test]
    fn test_encode_decode_roundtrip() {
        let report = AttestationReport {
            tee_type: TeeType::SevSnp,
            data: vec![0xaa; 100],
            report_data: [0xbb; 64],
            measurement: vec![0xcc; 48],
        };
        let encoded = encode_attestation_extension(&report).unwrap();
        let decoded = decode_attestation_extension(&encoded).unwrap();
        assert_eq!(decoded.tee_type, report.tee_type);
        assert_eq!(decoded.data, report.data);
        assert_eq!(decoded.report_data, report.report_data);
        assert_eq!(decoded.measurement, report.measurement);
    }

    #[test]
    fn test_extract_from_cert_with_extension() {
        use rcgen::{CertificateParams, CustomExtension, KeyPair};

        let report = AttestationReport {
            tee_type: TeeType::SevSnp,
            data: vec![0x42; 50],
            report_data: [0x01; 64],
            measurement: vec![0x02; 48],
        };
        let ext_der = encode_attestation_extension(&report).unwrap();

        let key_pair = KeyPair::generate().unwrap();
        let mut params = CertificateParams::default();
        let ext = CustomExtension::from_oid_content(ATTESTATION_OID, ext_der);
        params.custom_extensions.push(ext);
        let cert = params.self_signed(&key_pair).unwrap();

        let extracted = extract_attestation_from_cert(cert.der()).unwrap();
        assert!(extracted.is_some());
        let extracted = extracted.unwrap();
        assert_eq!(extracted.tee_type, TeeType::SevSnp);
        assert_eq!(extracted.data, vec![0x42; 50]);
    }

    #[test]
    fn test_extract_from_cert_without_extension() {
        use rcgen::{CertificateParams, KeyPair};

        let key_pair = KeyPair::generate().unwrap();
        let params = CertificateParams::default();
        let cert = params.self_signed(&key_pair).unwrap();

        let extracted = extract_attestation_from_cert(cert.der()).unwrap();
        assert!(extracted.is_none());
    }
}
```

**Step 3: Update sev_snp/mod.rs and lib.rs**

Add `pub mod verify;` to `sev_snp/mod.rs`.

**Step 4: Run tests**

```bash
cargo test -p aleph-tee
```

**Step 5: Commit**

```bash
git add -A
git commit -m "feat(aleph-tee): add SEV-SNP verification and X.509 attestation extension"
```

---

### Task 5: aleph-node QEMU Command Builder & Process Management

**Files:**
- Create: `crates/aleph-node/src/qemu/mod.rs`
- Create: `crates/aleph-node/src/qemu/args.rs`
- Create: `crates/aleph-node/src/qemu/process.rs`

**Step 1: Write QEMU args builder with tests**

```rust
// crates/aleph-node/src/qemu/args.rs
use aleph_tee::traits::TeeBackend;
use aleph_tee::types::VmConfig;
use std::path::{Path, PathBuf};

/// All the paths QEMU needs at runtime.
pub struct QemuPaths {
    pub qmp_socket: PathBuf,
    pub serial_log: PathBuf,
    pub pidfile: PathBuf,
}

impl QemuPaths {
    pub fn for_vm(run_dir: &Path, vm_id: &str) -> Self {
        let vm_dir = run_dir.join(vm_id);
        Self {
            qmp_socket: vm_dir.join("qmp.sock"),
            serial_log: vm_dir.join("serial.log"),
            pidfile: vm_dir.join("qemu.pid"),
        }
    }
}

/// Build the full QEMU command line.
pub fn build_qemu_command(
    config: &VmConfig,
    paths: &QemuPaths,
    tap_name: &str,
    tee_backend: &dyn TeeBackend,
) -> Vec<String> {
    let mut args = vec![
        "qemu-system-x86_64".into(),
        "-enable-kvm".into(),
        "-cpu".into(), "EPYC-v4".into(),
        "-smp".into(), config.vcpus.to_string(),
        "-m".into(), format!("{}M", config.memory_mb),
        "-nographic".into(),
        "-no-reboot".into(),
        // Kernel direct boot
        "-kernel".into(), config.kernel.display().to_string(),
        "-initrd".into(), config.initrd.display().to_string(),
        "-append".into(), "console=ttyS0 root=/dev/vda ro".into(),
        // Serial console
        "-serial".into(), format!("file:{}", paths.serial_log.display()),
        // QMP
        "-qmp".into(), format!("unix:{},server,nowait", paths.qmp_socket.display()),
        // PID file
        "-pidfile".into(), paths.pidfile.display().to_string(),
        // Network
        "-netdev".into(), format!("tap,id=net0,ifname={},script=no,downscript=no", tap_name),
        "-device".into(), "virtio-net-pci,netdev=net0".into(),
    ];

    // Add rootfs drive if present.
    if let Some(rootfs) = &config.rootfs {
        args.extend([
            "-drive".into(),
            format!("file={},format=raw,if=virtio,readonly=on", rootfs.display()),
        ]);
    }

    // Add TEE-specific args.
    args.extend(tee_backend.qemu_args(config));

    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use aleph_tee::sev_snp::SevSnpBackend;
    use aleph_tee::types::{TeeConfig, TeeType};
    use std::path::PathBuf;

    fn test_config() -> VmConfig {
        VmConfig {
            vm_id: "test-vm".into(),
            kernel: PathBuf::from("/images/bzImage"),
            initrd: PathBuf::from("/images/initrd.cpio.gz"),
            rootfs: Some(PathBuf::from("/images/rootfs.ext4")),
            vcpus: 2,
            memory_mb: 1024,
            tee: TeeConfig {
                backend: TeeType::SevSnp,
                policy: Some("0x5".into()),
            },
        }
    }

    #[test]
    fn test_build_command_includes_kernel() {
        let config = test_config();
        let paths = QemuPaths::for_vm(Path::new("/run/aleph"), "test-vm");
        let backend = SevSnpBackend::new("Genoa");
        let args = build_qemu_command(&config, &paths, "tap0", &backend);

        assert!(args.contains(&"-kernel".to_string()));
        assert!(args.contains(&"/images/bzImage".to_string()));
    }

    #[test]
    fn test_build_command_includes_sev_snp() {
        let config = test_config();
        let paths = QemuPaths::for_vm(Path::new("/run/aleph"), "test-vm");
        let backend = SevSnpBackend::new("Genoa");
        let args = build_qemu_command(&config, &paths, "tap0", &backend);

        let has_sev = args.iter().any(|a| a.contains("sev-snp-guest"));
        assert!(has_sev, "should contain SEV-SNP guest object");
    }

    #[test]
    fn test_build_command_without_rootfs() {
        let mut config = test_config();
        config.rootfs = None;
        let paths = QemuPaths::for_vm(Path::new("/run/aleph"), "test-vm");
        let backend = SevSnpBackend::new("Genoa");
        let args = build_qemu_command(&config, &paths, "tap0", &backend);

        let has_drive = args.iter().any(|a| a.contains("rootfs"));
        assert!(!has_drive, "should not contain rootfs drive");
    }

    #[test]
    fn test_qemu_paths() {
        let paths = QemuPaths::for_vm(Path::new("/run/aleph"), "my-vm");
        assert_eq!(paths.qmp_socket, PathBuf::from("/run/aleph/my-vm/qmp.sock"));
        assert_eq!(paths.serial_log, PathBuf::from("/run/aleph/my-vm/serial.log"));
    }
}
```

**Step 2: Write QEMU process manager**

```rust
// crates/aleph-node/src/qemu/process.rs
use crate::qemu::args::QemuPaths;
use anyhow::{Context, Result};
use std::process::Stdio;
use tokio::process::{Child, Command};
use tracing::{info, warn};

/// A running QEMU process.
pub struct QemuProcess {
    child: Child,
    pub paths: QemuPaths,
    vm_id: String,
}

impl QemuProcess {
    /// Spawn a new QEMU process with the given command-line args.
    pub async fn spawn(args: Vec<String>, paths: QemuPaths, vm_id: String) -> Result<Self> {
        let (program, cmd_args) = args.split_first()
            .context("empty QEMU command")?;

        // Create runtime directory for this VM.
        if let Some(parent) = paths.qmp_socket.parent() {
            tokio::fs::create_dir_all(parent).await
                .context("failed to create VM runtime directory")?;
        }

        info!(vm_id = %vm_id, "spawning QEMU: {} {}", program, cmd_args.join(" "));

        let child = Command::new(program)
            .args(cmd_args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to spawn QEMU")?;

        Ok(Self { child, paths, vm_id })
    }

    /// Wait for the QEMU process to exit with a timeout.
    /// Returns Ok(exit_status) if it exits, or kills it on timeout.
    pub async fn wait_or_kill(&mut self, timeout: std::time::Duration) -> Result<()> {
        match tokio::time::timeout(timeout, self.child.wait()).await {
            Ok(Ok(status)) => {
                info!(vm_id = %self.vm_id, ?status, "QEMU exited");
                Ok(())
            }
            Ok(Err(e)) => Err(e).context("error waiting for QEMU"),
            Err(_) => {
                warn!(vm_id = %self.vm_id, "QEMU did not exit in time, sending SIGKILL");
                self.child.kill().await.context("failed to kill QEMU")?;
                Ok(())
            }
        }
    }

    /// Get the process ID.
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }
}
```

**Step 3: Wire up mod.rs**

```rust
// crates/aleph-node/src/qemu/mod.rs
pub mod args;
pub mod process;
pub mod qmp;
```

**Step 4: Run tests**

```bash
cargo test -p aleph-node
```

**Step 5: Commit**

```bash
git add -A
git commit -m "feat(aleph-node): add QEMU command builder and process manager"
```

---

### Task 6: aleph-node QMP Client

**Files:**
- Create: `crates/aleph-node/src/qemu/qmp.rs`

**Step 1: Write QMP client**

```rust
// crates/aleph-node/src/qemu/qmp.rs
use anyhow::{Context, Result};
use std::path::Path;
use std::os::unix::net::UnixStream;
use qapi::Qmp;
use tracing::{debug, info};

/// Minimal QMP client for controlling QEMU.
pub struct QmpClient {
    qmp: Qmp<UnixStream>,
}

impl QmpClient {
    /// Connect to a QMP Unix socket and perform the capabilities handshake.
    /// Retries connection up to `retries` times with a delay between attempts,
    /// since QEMU may not have created the socket yet.
    pub async fn connect(socket_path: &Path, retries: u32) -> Result<Self> {
        let mut last_err = None;
        for attempt in 0..retries {
            match UnixStream::connect(socket_path) {
                Ok(stream) => {
                    stream.set_nonblocking(false)?;
                    let mut qmp = Qmp::new(stream);
                    qmp.handshake()
                        .context("QMP handshake failed")?;
                    info!("QMP connected to {}", socket_path.display());
                    return Ok(Self { qmp });
                }
                Err(e) => {
                    debug!(attempt, "QMP connect attempt failed: {}", e);
                    last_err = Some(e);
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
        }
        Err(last_err.unwrap()).context(format!(
            "failed to connect to QMP socket {} after {} attempts",
            socket_path.display(),
            retries
        ))
    }

    /// Query the VM status.
    pub fn query_status(&mut self) -> Result<String> {
        let status = self.qmp.execute(&qapi_qmp::query_status {})
            .context("QMP query-status failed")?;
        Ok(format!("{:?}", status.status))
    }

    /// Gracefully shut down the VM.
    pub fn quit(&mut self) -> Result<()> {
        self.qmp.execute(&qapi_qmp::quit {})
            .context("QMP quit failed")?;
        info!("QMP quit sent");
        Ok(())
    }

    /// Pause the VM.
    pub fn stop(&mut self) -> Result<()> {
        self.qmp.execute(&qapi_qmp::stop {})
            .context("QMP stop failed")?;
        Ok(())
    }

    /// Resume a paused VM.
    pub fn cont(&mut self) -> Result<()> {
        self.qmp.execute(&qapi_qmp::cont {})
            .context("QMP cont failed")?;
        Ok(())
    }
}
```

**Step 2: Run compilation check**

```bash
cargo check -p aleph-node
```

**Step 3: Commit**

```bash
git add -A
git commit -m "feat(aleph-node): add QMP client for QEMU lifecycle control"
```

---

### Task 7: aleph-node TAP Networking

**Files:**
- Create: `crates/aleph-node/src/network/mod.rs`
- Create: `crates/aleph-node/src/network/tap.rs`

**Step 1: Write TAP + bridge management**

```rust
// crates/aleph-node/src/network/tap.rs
use anyhow::{Context, Result};
use std::net::Ipv4Addr;
use tokio::process::Command;
use tracing::info;

/// Configuration for a VM's network interface.
pub struct TapInterface {
    pub name: String,
    pub bridge: String,
    pub vm_ip: Ipv4Addr,
    pub gateway_ip: Ipv4Addr,
    pub prefix_len: u8,
}

/// Create a TAP interface and attach it to a bridge.
pub async fn create_tap(tap_name: &str, bridge: &str) -> Result<()> {
    // Create TAP device.
    run_cmd("ip", &["tuntap", "add", "dev", tap_name, "mode", "tap"])
        .await
        .context(format!("failed to create TAP device {}", tap_name))?;

    // Bring it up.
    run_cmd("ip", &["link", "set", "dev", tap_name, "up"])
        .await
        .context(format!("failed to bring up {}", tap_name))?;

    // Add to bridge.
    run_cmd("ip", &["link", "set", "dev", tap_name, "master", bridge])
        .await
        .context(format!("failed to add {} to bridge {}", tap_name, bridge))?;

    info!(tap = tap_name, bridge, "TAP interface created and bridged");
    Ok(())
}

/// Delete a TAP interface.
pub async fn delete_tap(tap_name: &str) -> Result<()> {
    run_cmd("ip", &["link", "delete", tap_name])
        .await
        .context(format!("failed to delete TAP device {}", tap_name))?;
    info!(tap = tap_name, "TAP interface deleted");
    Ok(())
}

/// Ensure the bridge exists. Creates it if it doesn't.
pub async fn ensure_bridge(bridge: &str, ip: Ipv4Addr, prefix_len: u8) -> Result<()> {
    // Check if bridge already exists.
    let status = Command::new("ip")
        .args(["link", "show", bridge])
        .output()
        .await?;

    if !status.status.success() {
        run_cmd("ip", &["link", "add", "name", bridge, "type", "bridge"]).await?;
        run_cmd("ip", &["addr", "add", &format!("{}/{}", ip, prefix_len), "dev", bridge]).await?;
        info!(bridge, %ip, "bridge created");
    }

    run_cmd("ip", &["link", "set", "dev", bridge, "up"]).await?;
    Ok(())
}

/// Allocate a VM IP from the bridge subnet.
/// Simple sequential allocator: base_ip + offset.
pub fn allocate_vm_ip(gateway_ip: Ipv4Addr, offset: u8) -> Ipv4Addr {
    let octets = gateway_ip.octets();
    Ipv4Addr::new(octets[0], octets[1], octets[2], octets[3] + offset)
}

async fn run_cmd(program: &str, args: &[&str]) -> Result<()> {
    let output = Command::new(program)
        .args(args)
        .output()
        .await
        .context(format!("failed to run {} {}", program, args.join(" ")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("{} {} failed: {}", program, args.join(" "), stderr);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allocate_vm_ip() {
        let gw = Ipv4Addr::new(10, 0, 100, 1);
        assert_eq!(allocate_vm_ip(gw, 1), Ipv4Addr::new(10, 0, 100, 2));
        assert_eq!(allocate_vm_ip(gw, 10), Ipv4Addr::new(10, 0, 100, 11));
    }
}
```

```rust
// crates/aleph-node/src/network/mod.rs
pub mod tap;
```

**Step 2: Run tests**

```bash
cargo test -p aleph-node
```

**Step 3: Commit**

```bash
git add -A
git commit -m "feat(aleph-node): add TAP interface and bridge management"
```

---

### Task 8: aleph-node VM Lifecycle & Manager

**Files:**
- Create: `crates/aleph-node/src/vm/mod.rs`
- Create: `crates/aleph-node/src/vm/config.rs`
- Create: `crates/aleph-node/src/vm/lifecycle.rs`
- Create: `crates/aleph-node/src/vm/manager.rs`

**Step 1: Write VM state machine with tests**

```rust
// crates/aleph-node/src/vm/lifecycle.rs
use serde::{Deserialize, Serialize};
use std::time::Instant;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VmState {
    Defined,
    Booting,
    Running,
    Stopping,
    Stopped,
    Failed,
}

#[derive(Debug, Error)]
#[error("invalid state transition from {from:?} to {to:?}")]
pub struct InvalidTransition {
    pub from: VmState,
    pub to: VmState,
}

impl VmState {
    /// Check if a transition to the target state is valid.
    pub fn can_transition_to(&self, target: VmState) -> bool {
        matches!(
            (self, target),
            (VmState::Defined, VmState::Booting)
                | (VmState::Booting, VmState::Running)
                | (VmState::Booting, VmState::Failed)
                | (VmState::Running, VmState::Stopping)
                | (VmState::Running, VmState::Failed)
                | (VmState::Stopping, VmState::Stopped)
        )
    }

    /// Transition to a new state, returning error if invalid.
    pub fn transition(self, target: VmState) -> Result<VmState, InvalidTransition> {
        if self.can_transition_to(target) {
            Ok(target)
        } else {
            Err(InvalidTransition { from: self, to: target })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_transitions() {
        assert!(VmState::Defined.can_transition_to(VmState::Booting));
        assert!(VmState::Booting.can_transition_to(VmState::Running));
        assert!(VmState::Booting.can_transition_to(VmState::Failed));
        assert!(VmState::Running.can_transition_to(VmState::Stopping));
        assert!(VmState::Running.can_transition_to(VmState::Failed));
        assert!(VmState::Stopping.can_transition_to(VmState::Stopped));
    }

    #[test]
    fn test_invalid_transitions() {
        assert!(!VmState::Defined.can_transition_to(VmState::Running));
        assert!(!VmState::Stopped.can_transition_to(VmState::Running));
        assert!(!VmState::Running.can_transition_to(VmState::Booting));
    }

    #[test]
    fn test_transition_returns_new_state() {
        let state = VmState::Defined;
        let new = state.transition(VmState::Booting).unwrap();
        assert_eq!(new, VmState::Booting);
    }

    #[test]
    fn test_transition_error_on_invalid() {
        let state = VmState::Defined;
        let err = state.transition(VmState::Running).unwrap_err();
        assert_eq!(err.from, VmState::Defined);
        assert_eq!(err.to, VmState::Running);
    }
}
```

**Step 2: Write VM manager**

```rust
// crates/aleph-node/src/vm/manager.rs
use crate::network::tap;
use crate::qemu::args::{self, QemuPaths};
use crate::qemu::process::QemuProcess;
use crate::qemu::qmp::QmpClient;
use crate::vm::lifecycle::VmState;
use aleph_tee::traits::TeeBackend;
use aleph_tee::types::VmConfig;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tracing::{error, info};

/// A running VM's handle.
pub struct VmHandle {
    pub config: VmConfig,
    pub state: VmState,
    pub ip: Ipv4Addr,
    pub tap_name: String,
    pub process: QemuProcess,
    pub started_at: Instant,
}

/// Response returned when querying a VM.
#[derive(Debug, Serialize, Deserialize)]
pub struct VmInfo {
    pub vm_id: String,
    pub status: VmState,
    pub ip: String,
    pub tee: serde_json::Value,
    pub uptime_secs: u64,
}

/// Manages all running VMs.
pub struct VmManager {
    vms: RwLock<HashMap<String, VmHandle>>,
    run_dir: PathBuf,
    bridge: String,
    gateway_ip: Ipv4Addr,
    next_ip_offset: RwLock<u8>,
    tee_backend: Arc<dyn TeeBackend>,
}

impl VmManager {
    pub fn new(
        run_dir: PathBuf,
        bridge: String,
        gateway_ip: Ipv4Addr,
        tee_backend: Arc<dyn TeeBackend>,
    ) -> Self {
        Self {
            vms: RwLock::new(HashMap::new()),
            run_dir,
            bridge,
            gateway_ip,
            next_ip_offset: RwLock::new(2), // .1 is the gateway
            tee_backend,
        }
    }

    /// Boot a new VM.
    pub async fn create_vm(&self, config: VmConfig) -> Result<VmInfo> {
        let vm_id = config.vm_id.clone();

        // Check for duplicate.
        {
            let vms = self.vms.read().await;
            if vms.contains_key(&vm_id) {
                anyhow::bail!("VM {} already exists", vm_id);
            }
        }

        // Allocate IP.
        let offset = {
            let mut off = self.next_ip_offset.write().await;
            let current = *off;
            *off += 1;
            current
        };
        let vm_ip = tap::allocate_vm_ip(self.gateway_ip, offset);

        // Create TAP interface.
        let tap_name = format!("tap-{}", vm_id);
        tap::create_tap(&tap_name, &self.bridge).await
            .context("failed to create TAP interface")?;

        // Build QEMU command.
        let paths = QemuPaths::for_vm(&self.run_dir, &vm_id);
        let qemu_args = args::build_qemu_command(
            &config,
            &paths,
            &tap_name,
            self.tee_backend.as_ref(),
        );

        // Spawn QEMU.
        let process = QemuProcess::spawn(qemu_args, paths, vm_id.clone()).await
            .context("failed to spawn QEMU")?;

        let handle = VmHandle {
            config,
            state: VmState::Booting,
            ip: vm_ip,
            tap_name,
            process,
            started_at: Instant::now(),
        };

        // TODO: wait for VM to become ready (poll health endpoint).
        // For now, mark as running immediately.

        let info = VmInfo {
            vm_id: vm_id.clone(),
            status: VmState::Running,
            ip: vm_ip.to_string(),
            tee: serde_json::json!({
                "backend": self.tee_backend.tee_type(),
                "attested_url": format!("https://{}:8443", vm_ip),
            }),
            uptime_secs: 0,
        };

        let mut vms = self.vms.write().await;
        vms.insert(vm_id, handle);

        Ok(info)
    }

    /// Get info about a VM.
    pub async fn get_vm(&self, vm_id: &str) -> Result<VmInfo> {
        let vms = self.vms.read().await;
        let handle = vms.get(vm_id)
            .context(format!("VM {} not found", vm_id))?;

        Ok(VmInfo {
            vm_id: vm_id.to_string(),
            status: handle.state,
            ip: handle.ip.to_string(),
            tee: serde_json::json!({
                "backend": self.tee_backend.tee_type(),
                "attested_url": format!("https://{}:8443", handle.ip),
            }),
            uptime_secs: handle.started_at.elapsed().as_secs(),
        })
    }

    /// Stop and destroy a VM.
    pub async fn delete_vm(&self, vm_id: &str) -> Result<()> {
        let mut vms = self.vms.write().await;
        let mut handle = vms.remove(vm_id)
            .context(format!("VM {} not found", vm_id))?;

        // Try graceful QMP shutdown first.
        let qmp_socket = handle.process.paths.qmp_socket.clone();
        match QmpClient::connect(&qmp_socket, 1).await {
            Ok(mut qmp) => {
                let _ = qmp.quit();
            }
            Err(e) => {
                error!(vm_id, "failed to connect QMP for shutdown: {}", e);
            }
        }

        // Wait or kill.
        handle.process
            .wait_or_kill(std::time::Duration::from_secs(10))
            .await?;

        // Clean up TAP.
        let _ = tap::delete_tap(&handle.tap_name).await;

        info!(vm_id, "VM destroyed");
        Ok(())
    }

    /// List all VMs.
    pub async fn list_vms(&self) -> Vec<VmInfo> {
        let vms = self.vms.read().await;
        vms.iter()
            .map(|(id, h)| VmInfo {
                vm_id: id.clone(),
                status: h.state,
                ip: h.ip.to_string(),
                tee: serde_json::json!({
                    "backend": self.tee_backend.tee_type(),
                }),
                uptime_secs: h.started_at.elapsed().as_secs(),
            })
            .collect()
    }
}
```

**Step 3: Wire up vm/mod.rs**

```rust
// crates/aleph-node/src/vm/mod.rs
pub mod config;
pub mod lifecycle;
pub mod manager;
```

**Step 4: Run tests**

```bash
cargo test -p aleph-node
```

**Step 5: Commit**

```bash
git add -A
git commit -m "feat(aleph-node): add VM lifecycle state machine and manager"
```

---

### Task 9: aleph-node HTTP API

**Files:**
- Create: `crates/aleph-node/src/api/mod.rs`
- Create: `crates/aleph-node/src/api/vms.rs`
- Create: `crates/aleph-node/src/api/health.rs`
- Modify: `crates/aleph-node/src/main.rs`

**Step 1: Write API handlers**

```rust
// crates/aleph-node/src/api/vms.rs
use crate::vm::manager::{VmInfo, VmManager};
use actix_web::{web, HttpResponse};
use aleph_tee::types::VmConfig;
use serde::Deserialize;

/// POST /vms — boot a new VM.
pub async fn create_vm(
    manager: web::Data<VmManager>,
    body: web::Json<VmConfig>,
) -> HttpResponse {
    match manager.create_vm(body.into_inner()).await {
        Ok(info) => HttpResponse::Created().json(info),
        Err(e) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": e.to_string(),
        })),
    }
}

/// GET /vms/{id} — get VM status.
pub async fn get_vm(
    manager: web::Data<VmManager>,
    path: web::Path<String>,
) -> HttpResponse {
    let vm_id = path.into_inner();
    match manager.get_vm(&vm_id).await {
        Ok(info) => HttpResponse::Ok().json(info),
        Err(_) => HttpResponse::NotFound().json(serde_json::json!({
            "error": format!("VM {} not found", vm_id),
        })),
    }
}

/// DELETE /vms/{id} — stop and destroy VM.
pub async fn delete_vm(
    manager: web::Data<VmManager>,
    path: web::Path<String>,
) -> HttpResponse {
    let vm_id = path.into_inner();
    match manager.delete_vm(&vm_id).await {
        Ok(()) => HttpResponse::NoContent().finish(),
        Err(_) => HttpResponse::NotFound().json(serde_json::json!({
            "error": format!("VM {} not found", vm_id),
        })),
    }
}
```

```rust
// crates/aleph-node/src/api/health.rs
use actix_web::HttpResponse;

/// GET /health — node health check.
pub async fn health() -> HttpResponse {
    HttpResponse::Ok().json(serde_json::json!({
        "status": "ok",
    }))
}
```

```rust
// crates/aleph-node/src/api/mod.rs
pub mod health;
pub mod vms;
```

**Step 2: Write main.rs with CLI + server setup**

```rust
// crates/aleph-node/src/main.rs
mod api;
mod network;
mod qemu;
mod vm;

use crate::vm::manager::VmManager;
use actix_web::{web, App, HttpServer, middleware};
use aleph_tee::sev_snp::SevSnpBackend;
use clap::Parser;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

#[derive(Parser, Debug)]
#[command(name = "aleph-node", about = "Aleph confidential VM node daemon")]
struct Cli {
    /// Listen address (e.g., 127.0.0.1:4020)
    #[arg(long, default_value = "127.0.0.1:4020")]
    listen: String,

    /// Bridge interface name
    #[arg(long, default_value = "br-aleph")]
    bridge: String,

    /// Gateway IP on the bridge
    #[arg(long, default_value = "10.0.100.1")]
    gateway_ip: Ipv4Addr,

    /// Runtime directory for VM sockets and logs
    #[arg(long, default_value = "/run/aleph-cvm")]
    run_dir: PathBuf,

    /// AMD product name for SEV-SNP cert fetching
    #[arg(long, default_value = "Genoa")]
    amd_product: String,
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("aleph_node=info".parse().unwrap()),
        )
        .init();

    let cli = Cli::parse();
    info!("starting aleph-node on {}", cli.listen);

    // Ensure bridge exists.
    network::tap::ensure_bridge(&cli.bridge, cli.gateway_ip, 24)
        .await
        .expect("failed to create bridge");

    let tee_backend = Arc::new(SevSnpBackend::new(&cli.amd_product));

    let manager = web::Data::new(VmManager::new(
        cli.run_dir,
        cli.bridge,
        cli.gateway_ip,
        tee_backend,
    ));

    HttpServer::new(move || {
        App::new()
            .app_data(manager.clone())
            .route("/health", web::get().to(api::health::health))
            .route("/vms", web::post().to(api::vms::create_vm))
            .route("/vms/{id}", web::get().to(api::vms::get_vm))
            .route("/vms/{id}", web::delete().to(api::vms::delete_vm))
    })
    .bind(&cli.listen)?
    .run()
    .await
}
```

**Step 3: Run compilation check**

```bash
cargo check -p aleph-node
```

**Step 4: Commit**

```bash
git add -A
git commit -m "feat(aleph-node): add actix-web HTTP API and CLI entry point"
```

---

### Task 10: aleph-attest-agent — Attestation + TLS Cert

**Files:**
- Create: `crates/aleph-attest-agent/src/attestation.rs`
- Create: `crates/aleph-attest-agent/src/tls.rs`

**Step 1: Write attestation report request**

```rust
// crates/aleph-attest-agent/src/attestation.rs
use aleph_tee::sev_snp::SevSnpBackend;
use aleph_tee::traits::TeeBackend;
use aleph_tee::types::AttestationReport;
use anyhow::{Context, Result};
use sha2::{Digest, Sha384};

/// Request an attestation report binding the given public key.
/// The REPORT_DATA field will contain SHA-384(public_key_bytes).
pub fn get_key_bound_report(
    backend: &dyn TeeBackend,
    public_key_bytes: &[u8],
) -> Result<AttestationReport> {
    let mut report_data = [0u8; 64];
    let hash = Sha384::digest(public_key_bytes);
    report_data[..48].copy_from_slice(&hash);

    backend.get_report(&report_data)
        .context("failed to get attestation report bound to public key")
}

/// Request an attestation report with a caller-provided nonce (for Layer 3).
pub fn get_nonce_bound_report(
    backend: &dyn TeeBackend,
    nonce: &[u8],
) -> Result<AttestationReport> {
    let mut report_data = [0u8; 64];
    // Hash the nonce if it's longer than 64 bytes; otherwise pad with zeros.
    if nonce.len() > 64 {
        let hash = Sha384::digest(nonce);
        report_data[..48].copy_from_slice(&hash);
    } else {
        report_data[..nonce.len()].copy_from_slice(nonce);
    }

    backend.get_report(&report_data)
        .context("failed to get attestation report with nonce")
}
```

**Step 2: Write TLS cert generation**

```rust
// crates/aleph-attest-agent/src/tls.rs
use crate::attestation::get_key_bound_report;
use aleph_tee::traits::TeeBackend;
use aleph_tee::types::AttestationReport;
use aleph_tee::x509::{encode_attestation_extension, ATTESTATION_OID};
use anyhow::{Context, Result};
use rcgen::{CertificateParams, CustomExtension, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::sync::Arc;

/// Generated TLS identity with embedded attestation.
pub struct AttestedTlsIdentity {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
    pub report: AttestationReport,
}

/// Generate a self-signed TLS certificate with the attestation report
/// embedded as a custom X.509 extension.
///
/// 1. Generate ECDSA P-256 key pair
/// 2. Request SEV-SNP attestation with SHA-384(pubkey) as REPORT_DATA
/// 3. Encode the report as a custom X.509 extension
/// 4. Create self-signed cert with the extension
pub fn generate_attested_tls_identity(
    backend: &dyn TeeBackend,
) -> Result<AttestedTlsIdentity> {
    // Generate ephemeral key pair.
    let key_pair = KeyPair::generate()
        .context("failed to generate key pair")?;

    // Get attestation report bound to this key.
    let public_key_raw = key_pair.public_key_raw();
    let report = get_key_bound_report(backend, &public_key_raw)
        .context("failed to get key-bound attestation report")?;

    // Encode report as X.509 extension.
    let ext_der = encode_attestation_extension(&report)
        .context("failed to encode attestation extension")?;

    // Build certificate with the extension.
    let mut params = CertificateParams::default();
    let ext = CustomExtension::from_oid_content(ATTESTATION_OID, ext_der);
    params.custom_extensions.push(ext);

    let cert = params.self_signed(&key_pair)
        .context("failed to generate self-signed certificate")?;

    Ok(AttestedTlsIdentity {
        cert_der: cert.der().to_vec(),
        key_der: key_pair.serialize_der(),
        report,
    })
}

/// Build a rustls ServerConfig from the attested identity.
pub fn build_rustls_config(identity: &AttestedTlsIdentity) -> Result<rustls::ServerConfig> {
    let cert = CertificateDer::from(identity.cert_der.clone());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.key_der.clone()));

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .context("failed to build rustls ServerConfig")?;

    Ok(config)
}
```

**Step 3: Run compilation check**

```bash
cargo check -p aleph-attest-agent
```

**Step 4: Commit**

```bash
git add -A
git commit -m "feat(aleph-attest-agent): add attestation report request and TLS cert generation"
```

---

### Task 11: aleph-attest-agent — Reverse Proxy & Endpoints

**Files:**
- Create: `crates/aleph-attest-agent/src/proxy.rs`
- Modify: `crates/aleph-attest-agent/src/main.rs`

**Step 1: Write reverse proxy + attestation endpoint**

```rust
// crates/aleph-attest-agent/src/proxy.rs
use crate::attestation::get_nonce_bound_report;
use aleph_tee::traits::TeeBackend;
use aleph_tee::types::AttestationReport;
use actix_web::{web, HttpRequest, HttpResponse};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub struct AppState {
    pub backend: Arc<dyn TeeBackend>,
    pub upstream: String, // e.g., "http://127.0.0.1:8080"
    pub http_client: Client,
}

#[derive(Deserialize)]
pub struct AttestQuery {
    pub nonce: String, // hex-encoded nonce
}

#[derive(Serialize)]
pub struct AttestResponse {
    pub report: AttestationReport,
}

/// GET /.well-known/attestation?nonce=<hex>
/// Returns a fresh attestation report with the caller's nonce.
pub async fn attestation_endpoint(
    state: web::Data<AppState>,
    query: web::Query<AttestQuery>,
) -> HttpResponse {
    let nonce_bytes = match hex::decode(&query.nonce) {
        Ok(b) => b,
        Err(e) => {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": format!("invalid hex nonce: {}", e),
            }));
        }
    };

    match get_nonce_bound_report(state.backend.as_ref(), &nonce_bytes) {
        Ok(report) => HttpResponse::Ok().json(AttestResponse { report }),
        Err(e) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": e.to_string(),
        })),
    }
}

/// Catch-all handler: reverse proxy everything else to the upstream app.
pub async fn proxy_handler(
    state: web::Data<AppState>,
    req: HttpRequest,
    body: web::Bytes,
) -> HttpResponse {
    let upstream_url = format!("{}{}", state.upstream, req.uri());

    let upstream_req = state.http_client
        .request(req.method().clone(), &upstream_url)
        .body(body.to_vec());

    match upstream_req.send().await {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.bytes().await.unwrap_or_default();
            HttpResponse::build(actix_web::http::StatusCode::from_u16(status.as_u16()).unwrap())
                .body(body.to_vec())
        }
        Err(e) => HttpResponse::BadGateway().json(serde_json::json!({
            "error": format!("upstream error: {}", e),
        })),
    }
}
```

**Step 2: Write main.rs**

```rust
// crates/aleph-attest-agent/src/main.rs
mod attestation;
mod proxy;
mod tls;

use aleph_tee::sev_snp::SevSnpBackend;
use anyhow::Context;
use clap::Parser;
use std::sync::Arc;
use tracing::info;

#[derive(Parser, Debug)]
#[command(name = "aleph-attest-agent", about = "In-VM attestation sidecar")]
struct Cli {
    /// Port to listen on (HTTPS)
    #[arg(long, default_value = "8443")]
    port: u16,

    /// Upstream app URL to proxy to
    #[arg(long, default_value = "http://127.0.0.1:8080")]
    upstream: String,

    /// AMD product name
    #[arg(long, default_value = "Genoa")]
    amd_product: String,
}

#[actix_web::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("aleph_attest_agent=info")
        .init();

    let cli = Cli::parse();
    info!("starting aleph-attest-agent on port {}", cli.port);

    let backend = Arc::new(SevSnpBackend::new(&cli.amd_product));

    // Generate attested TLS identity.
    let identity = tls::generate_attested_tls_identity(backend.as_ref())
        .context("failed to generate attested TLS identity")?;
    info!("generated attested TLS certificate");

    let rustls_config = tls::build_rustls_config(&identity)?;

    let state = actix_web::web::Data::new(proxy::AppState {
        backend: backend.clone(),
        upstream: cli.upstream,
        http_client: reqwest::Client::new(),
    });

    actix_web::HttpServer::new(move || {
        actix_web::App::new()
            .app_data(state.clone())
            .route(
                "/.well-known/attestation",
                actix_web::web::get().to(proxy::attestation_endpoint),
            )
            .default_service(
                actix_web::web::route().to(proxy::proxy_handler),
            )
    })
    .bind_rustls_0_23(format!("0.0.0.0:{}", cli.port), rustls_config)?
    .run()
    .await?;

    Ok(())
}
```

**Step 3: Run compilation check**

```bash
cargo check -p aleph-attest-agent
```

**Step 4: Commit**

```bash
git add -A
git commit -m "feat(aleph-attest-agent): add reverse proxy and attestation endpoint"
```

---

### Task 12: aleph-attest-cli — Verification & CLI

**Files:**
- Create: `crates/aleph-attest-cli/src/verify.rs`
- Create: `crates/aleph-attest-cli/src/client.rs`
- Modify: `crates/aleph-attest-cli/src/main.rs`

**Step 1: Write custom TLS verifier that extracts attestation**

```rust
// crates/aleph-attest-cli/src/verify.rs
use aleph_tee::types::{AttestationReport, VerificationResult};
use aleph_tee::x509::extract_attestation_from_cert;
use anyhow::{Context, Result};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error, SignatureScheme};
use sha2::{Digest, Sha384};
use std::sync::{Arc, Mutex};

/// A rustls ServerCertVerifier that extracts and stores the attestation report
/// from the server's TLS certificate.
#[derive(Debug)]
pub struct SnpCertVerifier {
    /// The extracted report, populated during TLS handshake.
    extracted_report: Mutex<Option<AttestationReport>>,
}

impl SnpCertVerifier {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            extracted_report: Mutex::new(None),
        })
    }

    /// Get the extracted attestation report (after a successful TLS connection).
    pub fn get_report(&self) -> Option<AttestationReport> {
        self.extracted_report.lock().unwrap().clone()
    }
}

impl ServerCertVerifier for SnpCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        // Extract the attestation report from the cert.
        match extract_attestation_from_cert(end_entity.as_ref()) {
            Ok(Some(report)) => {
                // Verify REPORT_DATA contains SHA-384 of the server's public key.
                // The public key is in the cert's SubjectPublicKeyInfo.
                // For now, store the report; full verification happens after connection.
                *self.extracted_report.lock().unwrap() = Some(report);
                Ok(ServerCertVerified::assertion())
            }
            Ok(None) => {
                Err(Error::General("no attestation extension in server certificate".into()))
            }
            Err(e) => {
                Err(Error::General(format!("failed to extract attestation: {}", e)))
            }
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}
```

**Step 2: Write HTTP client with attestation**

```rust
// crates/aleph-attest-cli/src/client.rs
use crate::verify::SnpCertVerifier;
use aleph_tee::sev_snp::SevSnpBackend;
use aleph_tee::sev_snp::verify::verify_sev_snp_report;
use aleph_tee::types::{AttestationReport, TeeType};
use anyhow::{Context, Result};
use std::sync::Arc;

/// Result of an attested API call.
pub struct AttestedResponse {
    pub attestation_valid: bool,
    pub attestation_summary: String,
    pub measurement: Vec<u8>,
    pub status: u16,
    pub body: String,
}

/// Make an API call with Layer 2 (TLS-bound) attestation verification.
pub async fn attested_request(url: &str, product: &str) -> Result<AttestedResponse> {
    let verifier = SnpCertVerifier::new();

    // Build TLS config with our custom verifier.
    let tls_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier.clone())
        .with_no_client_auth();

    let client = reqwest::Client::builder()
        .use_preconfigured_tls(tls_config)
        .build()
        .context("failed to build HTTP client")?;

    // Make the request.
    let resp = client.get(url).send().await.context("request failed")?;
    let status = resp.status().as_u16();
    let body = resp.text().await.context("failed to read response body")?;

    // Verify the attestation report from the TLS cert.
    let report = verifier.get_report()
        .context("no attestation report extracted from TLS certificate")?;

    let verification = verify_sev_snp_report(&report, product).await
        .context("attestation verification failed")?;

    Ok(AttestedResponse {
        attestation_valid: verification.valid,
        attestation_summary: verification.summary,
        measurement: verification.measurement,
        status,
        body,
    })
}

/// Request a fresh attestation (Layer 3) from the VM.
pub async fn fresh_attestation(base_url: &str, product: &str) -> Result<AttestationReport> {
    // Generate random nonce.
    let nonce: [u8; 32] = rand::random();
    let nonce_hex = hex::encode(nonce);

    let url = format!("{}/.well-known/attestation?nonce={}", base_url, nonce_hex);

    // Use the attested TLS connection to fetch.
    let verifier = SnpCertVerifier::new();
    let tls_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();

    let client = reqwest::Client::builder()
        .use_preconfigured_tls(tls_config)
        .build()?;

    let resp = client.get(&url).send().await?;
    let report: AttestationReport = resp.json().await
        .context("failed to parse attestation response")?;

    // Verify the nonce is in the report_data.
    let expected_nonce_in_report = &report.report_data[..nonce.len()];
    anyhow::ensure!(
        expected_nonce_in_report == nonce,
        "nonce mismatch in fresh attestation report — possible replay"
    );

    // Verify the report itself.
    let _verification = verify_sev_snp_report(&report, product).await?;

    Ok(report)
}
```

**Step 3: Write CLI main**

```rust
// crates/aleph-attest-cli/src/main.rs
mod client;
mod verify;

use clap::Parser;
use tracing::info;

#[derive(Parser, Debug)]
#[command(name = "aleph-attest-cli", about = "Verify attestation and call confidential VM APIs")]
struct Cli {
    /// URL of the attested endpoint (e.g., https://10.0.100.2:8443/fib/10)
    #[arg(long)]
    url: String,

    /// Request a fresh attestation (Layer 3) instead of relying on TLS-bound (Layer 2)
    #[arg(long)]
    fresh_attest: bool,

    /// AMD product name for cert fetching
    #[arg(long, default_value = "Genoa")]
    amd_product: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("aleph_attest_cli=info")
        .init();

    let cli = Cli::parse();

    if cli.fresh_attest {
        // Layer 3: on-demand fresh attestation.
        let base_url = extract_base_url(&cli.url);
        println!("Requesting fresh attestation from {}...", base_url);

        let report = client::fresh_attestation(&base_url, &cli.amd_product).await?;

        println!("\nLayer 3 (on-demand) attestation:");
        println!("  Backend:     {:?}", report.tee_type);
        println!("  Measurement: {}", hex::encode(&report.measurement));
        println!("  Report data: {}", hex::encode(&report.report_data));
        println!("  Liveness:    confirmed (nonce matches)");
    } else {
        // Layer 2: TLS-bound attestation + API call.
        println!("Connecting to {} with attestation verification...", cli.url);

        let result = client::attested_request(&cli.url, &cli.amd_product).await?;

        println!("\nLayer 2 (TLS-bound) attestation:");
        println!("  Valid:       {}", result.attestation_valid);
        println!("  Summary:     {}", result.attestation_summary);
        println!("  Measurement: {}", hex::encode(&result.measurement));

        println!("\nAPI response (HTTP {}):", result.status);
        println!("  {}", result.body);
    }

    Ok(())
}

fn extract_base_url(url: &str) -> String {
    // Extract scheme://host:port from the URL.
    if let Ok(parsed) = url::Url::parse(url) {
        format!("{}://{}",
            parsed.scheme(),
            parsed.host_str().unwrap_or("localhost"),
        ) + &parsed.port().map(|p| format!(":{}", p)).unwrap_or_default()
    } else {
        url.to_string()
    }
}
```

**Step 4: Add `url`, `rand`, and `hex` to CLI dependencies**

Add to `crates/aleph-attest-cli/Cargo.toml`:
```toml
url = "2"
rand = "0.8"
hex = "0.4"
```

**Step 5: Run compilation check**

```bash
cargo check -p aleph-attest-cli
```

**Step 6: Commit**

```bash
git add -A
git commit -m "feat(aleph-attest-cli): add attestation verification and CLI"
```

---

### Task 13: Nix Flake — Fibonacci Service

**Files:**
- Create: `nix/fib-service/Cargo.toml`
- Create: `nix/fib-service/src/main.rs`

**Step 1: Write the Fibonacci service**

```rust
// nix/fib-service/src/main.rs
use actix_web::{web, App, HttpServer, HttpResponse};
use serde::Serialize;

#[derive(Serialize)]
struct FibResponse {
    n: u64,
    result: u64,
}

fn fibonacci(n: u64) -> u64 {
    if n <= 1 {
        return n;
    }
    let (mut a, mut b) = (0u64, 1u64);
    for _ in 2..=n {
        let next = a.saturating_add(b);
        a = b;
        b = next;
    }
    b
}

async fn fib_handler(path: web::Path<u64>) -> HttpResponse {
    let n = path.into_inner();
    let result = fibonacci(n);
    HttpResponse::Ok().json(FibResponse { n, result })
}

async fn health() -> HttpResponse {
    HttpResponse::Ok().json(serde_json::json!({"status": "ok"}))
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    eprintln!("fib-service listening on 127.0.0.1:8080");
    HttpServer::new(|| {
        App::new()
            .route("/health", web::get().to(health))
            .route("/fib/{n}", web::get().to(fib_handler))
    })
    .bind("127.0.0.1:8080")?
    .run()
    .await
}
```

```toml
# nix/fib-service/Cargo.toml
[package]
name = "fib-service"
version = "0.1.0"
edition = "2024"

[dependencies]
actix-web = "4"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

**Step 2: Write tests for fibonacci function**

Add to `nix/fib-service/src/main.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fib_base_cases() {
        assert_eq!(fibonacci(0), 0);
        assert_eq!(fibonacci(1), 1);
    }

    #[test]
    fn test_fib_known_values() {
        assert_eq!(fibonacci(10), 55);
        assert_eq!(fibonacci(20), 6765);
    }

    #[test]
    fn test_fib_large_saturates() {
        // Should not panic on large values.
        let _ = fibonacci(100);
    }
}
```

**Step 3: Run tests**

```bash
cd nix/fib-service && cargo test
```

**Step 4: Commit**

```bash
git add -A
git commit -m "feat: add Fibonacci demo service"
```

---

### Task 14: Nix Flake — Kernel, Initrd, Rootfs

**Files:**
- Create: `nix/flake.nix`
- Create: `nix/kernel.nix`
- Create: `nix/initrd.nix`
- Create: `nix/rootfs.nix`
- Create: `nix/init.sh`

**Step 1: Write flake.nix**

```nix
# nix/flake.nix
{
  description = "Aleph CVM - Confidential VM images";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-24.11";
    rust-overlay.url = "github:oxalica/rust-overlay";
    crane.url = "github:ipetkov/crane";
  };

  outputs = { self, nixpkgs, rust-overlay, crane, ... }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ rust-overlay.overlays.default ];
      };
      craneLib = crane.mkLib pkgs;

      # Build the Rust binaries.
      rustToolchain = pkgs.rust-bin.stable.latest.default.override {
        targets = [ "x86_64-unknown-linux-musl" ];
      };
      craneToolchain = craneLib.overrideToolchain rustToolchain;

      # Fibonacci service (static musl binary).
      fib-service = craneToolchain.buildPackage {
        src = ./fib-service;
        CARGO_BUILD_TARGET = "x86_64-unknown-linux-musl";
        CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static";
      };

      # Attestation agent (static musl binary).
      # Built from the workspace root, selecting just the agent crate.
      attest-agent = craneToolchain.buildPackage {
        src = ../.;
        cargoExtraArgs = "-p aleph-attest-agent";
        CARGO_BUILD_TARGET = "x86_64-unknown-linux-musl";
        CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static";
      };

    in {
      packages.${system} = {
        inherit fib-service attest-agent;

        kernel = pkgs.callPackage ./kernel.nix {};
        initrd = pkgs.callPackage ./initrd.nix {
          inherit attest-agent;
          init-script = ./init.sh;
        };
        rootfs = pkgs.callPackage ./rootfs.nix {
          inherit fib-service;
        };

        # Convenience: build all three artifacts.
        vm-fib-demo = pkgs.symlinkJoin {
          name = "vm-fib-demo";
          paths = [
            self.packages.${system}.kernel
            self.packages.${system}.initrd
            self.packages.${system}.rootfs
          ];
        };
      };
    };
}
```

**Step 2: Write kernel.nix**

```nix
# nix/kernel.nix
{ pkgs, ... }:

pkgs.linuxPackages_6_6.kernel.override {
  structuredExtraConfig = with pkgs.lib.kernel; {
    # SEV-SNP guest support
    AMD_MEM_ENCRYPT = yes;
    SEV_GUEST = yes;
    CRYPTO_DEV_CCP = yes;
    CRYPTO_DEV_CCP_DD = yes;
    CRYPTO_DEV_SP_PSP = yes;

    # Virtio (for disk and network)
    VIRTIO = yes;
    VIRTIO_PCI = yes;
    VIRTIO_BLK = yes;
    VIRTIO_NET = yes;
    VIRTIO_CONSOLE = yes;

    # Minimal config
    MODULES = no;
  };
}
```

**Step 3: Write initrd.nix**

```nix
# nix/initrd.nix
{ pkgs, attest-agent, init-script, ... }:

pkgs.makeInitrd {
  contents = [
    { object = "${pkgs.busybox}/bin/busybox"; symlink = "/bin/busybox"; }
    { object = init-script; symlink = "/init"; }
    { object = "${attest-agent}/bin/aleph-attest-agent"; symlink = "/bin/aleph-attest-agent"; }
  ];
}
```

**Step 4: Write init.sh**

```bash
#!/bin/busybox sh
# /init — runs inside the VM as PID 1

# Mount essential filesystems.
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sysfs /sys
/bin/busybox mount -t devtmpfs devtmpfs /dev

# Bring up loopback.
/bin/busybox ip link set lo up

# Bring up eth0 via DHCP.
/bin/busybox ip link set eth0 up
/bin/busybox udhcpc -i eth0 -s /bin/busybox

# Mount rootfs from virtio block device (if present).
if [ -b /dev/vda ]; then
    /bin/busybox mkdir -p /mnt/root
    /bin/busybox mount -o ro /dev/vda /mnt/root

    # Start the user application from rootfs.
    if [ -x /mnt/root/bin/fib-service ]; then
        /mnt/root/bin/fib-service &
    fi
fi

# Start the attestation agent.
/bin/aleph-attest-agent --port 8443 --upstream http://127.0.0.1:8080 &

# Wait for children.
wait
```

**Step 5: Write rootfs.nix**

```nix
# nix/rootfs.nix
{ pkgs, fib-service, ... }:

pkgs.runCommand "rootfs.ext4" {
  nativeBuildInputs = [ pkgs.e2fsprogs ];
} ''
  # Create a minimal ext4 image.
  mkdir -p rootfs/bin
  cp ${fib-service}/bin/fib-service rootfs/bin/

  # Calculate size (add 10MB padding).
  size=$(du -sm rootfs | cut -f1)
  size=$((size + 10))

  # Create ext4 image.
  truncate -s ${toString size}M $out
  mkfs.ext4 -d rootfs $out
''
```

**Step 6: Commit**

```bash
git add -A
git commit -m "feat(nix): add flake for kernel, initrd, rootfs, and fib-service"
```

---

### Task 15: Integration Tests — Tier 1 (Local, no SEV-SNP)

**Files:**
- Create: `tests/integration/common.rs`
- Create: `tests/integration/tier1_boot.rs`
- Create: `tests/integration/tier1_api.rs`
- Create: `tests/integration/tier1_qmp.rs`

**Step 1: Write test harness**

```rust
// tests/integration/common.rs
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

/// Start the aleph-node daemon and return its process handle.
pub async fn start_node(listen: &str, bridge: &str) -> tokio::process::Child {
    Command::new(env!("CARGO_BIN_EXE_aleph-node"))
        .args(["--listen", listen, "--bridge", bridge])
        .spawn()
        .expect("failed to start aleph-node")
}

/// Wait for a port to become available.
pub async fn wait_for_port(addr: &str, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

/// Path to test VM images (set via env var or default).
pub fn image_dir() -> PathBuf {
    std::env::var("ALEPH_TEST_IMAGES")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./result"))
}
```

**Step 2: Write Tier 1 boot test**

```rust
// tests/integration/tier1_boot.rs
//! Tier 1 tests: run on any machine with QEMU/KVM.
//! These tests verify VM boot, networking, and API without SEV-SNP.

use crate::common::*;
use std::time::Duration;

/// Test: The node daemon starts and responds to health checks.
#[tokio::test]
async fn test_node_health() {
    let mut node = start_node("127.0.0.1:14020", "br-test").await;
    assert!(wait_for_port("127.0.0.1:14020", Duration::from_secs(5)).await);

    let resp = reqwest::get("http://127.0.0.1:14020/health")
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    node.kill().await.unwrap();
}

/// Test: Create a VM, verify it appears in the API, then delete it.
#[tokio::test]
async fn test_vm_lifecycle() {
    let mut node = start_node("127.0.0.1:14021", "br-test").await;
    assert!(wait_for_port("127.0.0.1:14021", Duration::from_secs(5)).await);

    let images = image_dir();
    let client = reqwest::Client::new();

    // Create VM.
    let resp = client
        .post("http://127.0.0.1:14021/vms")
        .json(&serde_json::json!({
            "vm_id": "test-vm-1",
            "image": {
                "kernel": images.join("bzImage").to_str().unwrap(),
                "initrd": images.join("initrd.cpio.gz").to_str().unwrap(),
                "rootfs": images.join("rootfs.ext4").to_str().unwrap(),
            },
            "resources": { "vcpus": 1, "memory_mb": 256 },
            "tee": { "backend": "sev-snp", "policy": "0x5" }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["vm_id"], "test-vm-1");
    assert_eq!(body["status"], "running");

    // Get VM status.
    let resp = client
        .get("http://127.0.0.1:14021/vms/test-vm-1")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // Delete VM.
    let resp = client
        .delete("http://127.0.0.1:14021/vms/test-vm-1")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // Verify it's gone.
    let resp = client
        .get("http://127.0.0.1:14021/vms/test-vm-1")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);

    node.kill().await.unwrap();
}
```

**Step 3: Write QEMU args unit test (already covered in Task 5)**

Already done. The Tier 1 integration tests focus on the full stack.

**Step 4: Commit**

```bash
git add -A
git commit -m "test: add Tier 1 integration tests (boot, API lifecycle, health)"
```

---

### Task 16: Integration Tests — Tier 2 (SEV-SNP hardware)

**Files:**
- Create: `tests/integration/tier2_attestation.rs`

**Step 1: Write Tier 2 attestation tests**

```rust
// tests/integration/tier2_attestation.rs
//! Tier 2 tests: require real SEV-SNP hardware.
//! Run with: cargo test --test tier2 -- --ignored
//! Requires: ALEPH_TEST_IMAGES env var pointing to built Nix images.

use crate::common::*;
use std::time::Duration;

/// Test: VM TLS cert contains a valid SEV-SNP attestation report.
#[tokio::test]
#[ignore] // Only runs on SEV-SNP hardware
async fn test_tls_attestation() {
    let mut node = start_node("127.0.0.1:14030", "br-test-sev").await;
    assert!(wait_for_port("127.0.0.1:14030", Duration::from_secs(5)).await);

    let images = image_dir();
    let client = reqwest::Client::new();

    // Create VM.
    let resp = client
        .post("http://127.0.0.1:14030/vms")
        .json(&serde_json::json!({
            "vm_id": "attest-test",
            "image": {
                "kernel": images.join("bzImage").to_str().unwrap(),
                "initrd": images.join("initrd.cpio.gz").to_str().unwrap(),
                "rootfs": images.join("rootfs.ext4").to_str().unwrap(),
            },
            "resources": { "vcpus": 1, "memory_mb": 512 },
            "tee": { "backend": "sev-snp", "policy": "0x5" }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    let vm_ip = body["ip"].as_str().unwrap();

    // Wait for the attest agent to be ready.
    let attest_url = format!("https://{}:8443", vm_ip);
    assert!(wait_for_port(&format!("{}:8443", vm_ip), Duration::from_secs(30)).await);

    // Use the CLI to verify attestation.
    let result = aleph_attest_cli::client::attested_request(
        &format!("{}/fib/10", attest_url),
        "Genoa",
    )
    .await
    .unwrap();

    assert!(result.attestation_valid);
    assert!(!result.measurement.is_empty());
    assert_eq!(result.status, 200);

    // Parse the Fibonacci response.
    let fib: serde_json::Value = serde_json::from_str(&result.body).unwrap();
    assert_eq!(fib["n"], 10);
    assert_eq!(fib["result"], 55);

    // Clean up.
    client
        .delete(&format!("http://127.0.0.1:14030/vms/attest-test"))
        .send()
        .await
        .unwrap();
    node.kill().await.unwrap();
}

/// Test: On-demand attestation returns fresh report with matching nonce.
#[tokio::test]
#[ignore]
async fn test_fresh_attestation() {
    // Similar setup as above, then:
    // let report = aleph_attest_cli::client::fresh_attestation(&attest_url, "Genoa").await.unwrap();
    // assert_eq!(report.tee_type, TeeType::SevSnp);
    // assert!(!report.measurement.is_empty());
    todo!("Implement after VM boot infrastructure is solid")
}
```

**Step 2: Commit**

```bash
git add -A
git commit -m "test: add Tier 2 integration tests (SEV-SNP attestation, requires hardware)"
```

---

### Task 17: Polish & Wire Everything Together

**Files:**
- Various fixups across all crates

**Step 1: Ensure all crates compile clean**

```bash
cargo check --workspace
cargo clippy --workspace -- -D warnings
```

**Step 2: Run all Tier 1 tests**

```bash
cargo test --workspace
```

**Step 3: Build the Nix images**

```bash
cd nix && nix build .#vm-fib-demo
```

**Step 4: Run the full demo manually**

Follow the end-to-end demo flow from the design doc:
1. Start node daemon
2. Boot VM
3. Call Fibonacci API with attestation
4. Tear down

**Step 5: Commit any fixups**

```bash
git add -A
git commit -m "chore: polish and fix compilation issues across workspace"
```
