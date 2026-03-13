# HOSTDATA Owner Authentication — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Implement the HOSTDATA owner authentication protocol from `docs/plans/2026-03-08-hostdata-owner-auth-design.md` — bind CVMs to owner pubkeys and gate secret injection on challenge-response signatures.

**Architecture:** Bottom-up: data model (aleph-tee) -> host-side plumbing (QEMU args, proto, gRPC) -> in-VM enforcement (attest-agent challenge + auth) -> client-side (CLI signing) -> init.sh (SSH keys).

**Tech Stack:** Rust, k256 (secp256k1 ECDSA), SHA-256, actix-web, clap, protobuf, QEMU `host-data=`.

---

### Task 1: Add `host_data` to AttestationReport and report parsing

**Files:**
- Modify: `crates/aleph-tee/src/types.rs`
- Modify: `crates/aleph-tee/src/sev_snp/report.rs`
- Modify: `crates/aleph-tee/src/sev_snp/backend.rs`

**Step 1: Add `hex_serde_array_32` serde helper and `host_data` field to `AttestationReport`**

In `crates/aleph-tee/src/types.rs`, add a new serde helper module for `[u8; 32]` (the existing `hex_serde_array` handles `[u8; 64]`), then add the field:

```rust
/// Serde helper for hex-encoding `[u8; 32]` fields.
mod hex_serde_array_32 {
    use serde::{self, Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<[u8; 32], D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let bytes = hex::decode(&s).map_err(serde::de::Error::custom)?;
        let array: [u8; 32] = bytes
            .try_into()
            .map_err(|_| serde::de::Error::custom("expected exactly 32 bytes"))?;
        Ok(array)
    }
}
```

Then add to `AttestationReport`:

```rust
#[serde(with = "hex_serde_array_32")]
pub host_data: [u8; 32],
```

**Step 2: Add `extract_host_data` to `report.rs`**

In `crates/aleph-tee/src/sev_snp/report.rs`, add:

```rust
/// Extract the 32-byte host_data field from a parsed report.
pub fn extract_host_data(report: &SevReport) -> [u8; 32] {
    report.inner.host_data
}
```

**Step 3: Wire `host_data` into `backend.rs::parse_report`**

In `crates/aleph-tee/src/sev_snp/backend.rs`, update `parse_report()` to include `host_data`:

```rust
fn parse_report(&self, raw: &[u8]) -> Result<AttestationReport> {
    let parsed = parse_sev_snp_report(raw)?;

    Ok(AttestationReport {
        tee_type: TeeType::SevSnp,
        data: raw.to_vec(),
        report_data: extract_report_data(&parsed),
        measurement: extract_measurement(&parsed).to_vec(),
        host_data: extract_host_data(&parsed),
    })
}
```

Import `extract_host_data` in the use statement at the top.

**Step 4: Fix all compiler errors from the new field**

Every place that constructs an `AttestationReport` needs updating. Search for `AttestationReport {` across the codebase:

- `crates/aleph-tee/src/sev_snp/backend.rs` — already fixed above
- `crates/aleph-attest-agent/src/attestation.rs` — MockBackend in tests: add `host_data: [0u8; 32]`
- `crates/aleph-attest-agent/src/tls.rs` — MockBackend in tests: add `host_data: [0u8; 32]`
- `crates/aleph-tee/src/types.rs` — test `test_attestation_report_roundtrip`: add `host_data: [0x99; 32]`
- Any other test mocks: add `host_data: [0u8; 32]`

**Step 5: Update test for roundtrip serialization**

In `crates/aleph-tee/src/types.rs`, update `test_attestation_report_roundtrip`:

```rust
let report = AttestationReport {
    tee_type: TeeType::SevSnp,
    data: vec![0xde, 0xad, 0xbe, 0xef],
    report_data: [0x42; 64],
    measurement: vec![0x01, 0x02, 0x03],
    host_data: [0x99; 32],
};
// ... existing assertions ...
assert_eq!(deserialized.host_data, report.host_data);
assert!(json.contains(&"99".repeat(32)));
```

Add a test for `extract_host_data` in `report.rs`:

```rust
#[test]
fn test_host_data_extraction() {
    use sev::firmware::guest::AttestationReport as SevAR;
    use sev::parser::Encoder;

    let mut report = SevAR {
        version: 3,
        report_data: [0x42; 64],
        measurement: [0xAB; 48],
        host_data: [0xCD; 32],
        cpuid_fam_id: Some(0x19),
        cpuid_mod_id: Some(0x01),
        cpuid_step: Some(0x00),
        ..Default::default()
    };
    report.chip_id[0] = 1;

    let mut buf = Vec::new();
    report.encode(&mut buf, ()).expect("encode should succeed");
    let parsed = parse_sev_snp_report(&buf).expect("parse should succeed");
    assert_eq!(extract_host_data(&parsed), [0xCD; 32]);
}
```

**Step 6: Run tests and verify**

Run: `cargo test -p aleph-tee -p aleph-attest-agent`
Expected: All pass.

**Step 7: Commit**

```
feat(tee): add host_data field to AttestationReport
```

---

### Task 2: Add `host_data` to TeeConfig and QEMU args

**Files:**
- Modify: `crates/aleph-tee/src/types.rs`
- Modify: `crates/aleph-tee/src/sev_snp/qemu.rs`

**Step 1: Add `host_data` to `TeeConfig`**

In `crates/aleph-tee/src/types.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeeConfig {
    pub backend: TeeType,
    pub policy: Option<String>,
    /// SHA-256(owner_pubkey) — binds the VM to a specific owner.
    #[serde(default, with = "option_hex_32")]
    pub host_data: Option<[u8; 32]>,
}
```

Add the serde helper for `Option<[u8; 32]>`:

```rust
mod option_hex_32 {
    use serde::{self, Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(val: &Option<[u8; 32]>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match val {
            Some(bytes) => serializer.serialize_str(&hex::encode(bytes)),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<[u8; 32]>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let opt: Option<String> = Option::deserialize(deserializer)?;
        match opt {
            None => Ok(None),
            Some(s) if s.is_empty() => Ok(None),
            Some(s) => {
                let bytes = hex::decode(&s).map_err(serde::de::Error::custom)?;
                let array: [u8; 32] = bytes
                    .try_into()
                    .map_err(|_| serde::de::Error::custom("expected exactly 32 bytes"))?;
                Ok(Some(array))
            }
        }
    }
}
```

**Step 2: Append `host-data=` to QEMU args when set**

In `crates/aleph-tee/src/sev_snp/qemu.rs`, modify the `sev-snp-guest` object string:

```rust
let host_data_opt = config
    .tee
    .host_data
    .map(|hd| format!(",host-data={}", hex::encode(hd)))
    .unwrap_or_default();

// In the vec!, change the sev-snp-guest format string:
format!(
    "sev-snp-guest,id=sev0,cbitpos=51,reduced-phys-bits=1,kernel-hashes=on,policy={policy}{host_data_opt}"
),
```

**Step 3: Fix all `TeeConfig` construction sites**

Search for `TeeConfig {` across the codebase. Every constructor needs `host_data: None` (or a specific value in tests). Key locations:

- `crates/aleph-tee/src/sev_snp/qemu.rs` — `make_config()` test helper
- `crates/aleph-tee/src/sev_snp/backend.rs` — tests
- `crates/aleph-compute-node/src/grpc/service.rs` — `parse_tee_config()`
- `crates/aleph-compute-node/tests/tier1_api.rs` — MockTeeBackend
- Any other test files

**Step 4: Add test for host-data in QEMU args**

In `crates/aleph-tee/src/sev_snp/qemu.rs`, add:

```rust
#[test]
fn test_sev_snp_args_with_host_data() {
    let mut config = make_config(1024, None);
    config.tee.host_data = Some([0xAB; 32]);
    let args = sev_snp_qemu_args(&config, DEFAULT_OVMF_PATH);

    let sev_arg = args
        .iter()
        .find(|a| a.contains("sev-snp-guest"))
        .expect("should have sev-snp-guest arg");

    let expected_hex = "ab".repeat(32);
    assert!(
        sev_arg.contains(&format!("host-data={expected_hex}")),
        "should contain host-data but got: {sev_arg}"
    );
}

#[test]
fn test_sev_snp_args_without_host_data() {
    let config = make_config(1024, None);
    let args = sev_snp_qemu_args(&config, DEFAULT_OVMF_PATH);

    let sev_arg = args
        .iter()
        .find(|a| a.contains("sev-snp-guest"))
        .expect("should have sev-snp-guest arg");

    assert!(
        !sev_arg.contains("host-data"),
        "should NOT contain host-data but got: {sev_arg}"
    );
}
```

**Step 5: Run tests and verify**

Run: `cargo test -p aleph-tee`
Expected: All pass, including the two new tests.

**Step 6: Commit**

```
feat(tee): pass host_data through TeeConfig to QEMU args
```

---

### Task 3: Add `host_data` to gRPC proto and service

**Files:**
- Modify: `proto/compute.proto`
- Modify: `crates/aleph-compute-node/src/grpc/service.rs`

**Step 1: Add `host_data` to proto `TeeConfig`**

In `proto/compute.proto`:

```protobuf
message TeeConfig {
  string backend = 1; // "sev-snp", "tdx", "nvidia-cc"
  string policy = 2;  // empty string means default
  bytes host_data = 3; // 32 bytes: SHA-256(owner_pubkey), set by orchestrator
}
```

**Step 2: Regenerate proto code**

Run: `cargo build -p aleph-compute-proto`

This triggers `tonic-build` via the crate's `build.rs`.

**Step 3: Wire `host_data` through `parse_tee_config`**

In `crates/aleph-compute-node/src/grpc/service.rs`, update `parse_tee_config`:

```rust
fn parse_tee_config(
    proto: Option<aleph_compute_proto::compute::TeeConfig>,
) -> Result<TeeConfig, Status> {
    let proto = proto.unwrap_or_default();
    let backend = match proto.backend.as_str() {
        "sev-snp" | "" => TeeType::SevSnp,
        "tdx" => TeeType::Tdx,
        "nvidia-cc" => TeeType::NvidiaCc,
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
    let host_data = if proto.host_data.is_empty() {
        None
    } else {
        let bytes: [u8; 32] = proto.host_data.try_into().map_err(|v: Vec<u8>| {
            Status::invalid_argument(format!(
                "host_data must be exactly 32 bytes, got {}",
                v.len()
            ))
        })?;
        Some(bytes)
    };
    Ok(TeeConfig {
        backend,
        policy,
        host_data,
    })
}
```

**Step 4: Run tests and verify**

Run: `cargo test -p aleph-compute-node`
Expected: All pass.

**Step 5: Commit**

```
feat(proto): add host_data to TeeConfig for owner binding
```

---

### Task 4: Add challenge endpoint and authenticated injection to attest-agent

This is the core task. The attest-agent gets a new challenge endpoint and the inject-secret endpoint requires owner authentication.

**Files:**
- Modify: `Cargo.toml` (workspace deps)
- Modify: `crates/aleph-attest-agent/Cargo.toml`
- Create: `crates/aleph-attest-agent/src/challenge.rs`
- Modify: `crates/aleph-attest-agent/src/secrets.rs`
- Modify: `crates/aleph-attest-agent/src/proxy.rs`
- Modify: `crates/aleph-attest-agent/src/main.rs`

**Step 1: Add `k256` and `rand` to workspace dependencies**

In root `Cargo.toml`, add to `[workspace.dependencies]`:

```toml
k256 = { version = "0.13", features = ["ecdsa"] }
```

(`rand` is already a workspace dep.)

In `crates/aleph-attest-agent/Cargo.toml`, add:

```toml
k256 = { workspace = true }
rand.workspace = true
```

**Step 2: Create `challenge.rs` — challenge nonce management**

Create `crates/aleph-attest-agent/src/challenge.rs`:

```rust
use std::sync::Mutex;
use std::time::{Duration, Instant};

use actix_web::{HttpResponse, web};
use rand::Rng;
use serde::Serialize;

/// How long a challenge nonce remains valid.
const CHALLENGE_TTL: Duration = Duration::from_secs(300); // 5 minutes

/// A challenge nonce with an expiry time.
struct Challenge {
    nonce: [u8; 32],
    expires_at: Instant,
}

/// Single-slot challenge store. Only one active challenge at a time.
pub struct ChallengeStore {
    inner: Mutex<Option<Challenge>>,
}

impl ChallengeStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    /// Generate a new challenge, replacing any existing one.
    pub fn issue(&self) -> [u8; 32] {
        let mut nonce = [0u8; 32];
        rand::rng().fill(&mut nonce);
        let challenge = Challenge {
            nonce,
            expires_at: Instant::now() + CHALLENGE_TTL,
        };
        *self.inner.lock().unwrap() = Some(challenge);
        nonce
    }

    /// Consume the current challenge if it matches and hasn't expired.
    /// Returns the nonce on success, None if no challenge, expired, or mismatch.
    pub fn verify_and_consume(&self, nonce: &[u8; 32]) -> bool {
        let mut guard = self.inner.lock().unwrap();
        match guard.as_ref() {
            Some(c) if &c.nonce == nonce && Instant::now() < c.expires_at => {
                *guard = None; // consume
                true
            }
            _ => false,
        }
    }
}

#[derive(Serialize)]
struct ChallengeResponse {
    nonce: String,
}

/// GET /confidential/challenge
///
/// Issues a fresh 32-byte random nonce. The caller must sign this nonce
/// with their secp256k1 private key and include the signature in the
/// inject-secret request.
pub async fn challenge_endpoint(
    store: web::Data<ChallengeStore>,
) -> HttpResponse {
    let nonce = store.issue();
    HttpResponse::Ok().json(ChallengeResponse {
        nonce: hex::encode(nonce),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_issue_and_verify() {
        let store = ChallengeStore::new();
        let nonce = store.issue();
        assert!(store.verify_and_consume(&nonce));
    }

    #[test]
    fn test_consumed_nonce_rejected() {
        let store = ChallengeStore::new();
        let nonce = store.issue();
        assert!(store.verify_and_consume(&nonce));
        // Second attempt with same nonce should fail (consumed).
        assert!(!store.verify_and_consume(&nonce));
    }

    #[test]
    fn test_wrong_nonce_rejected() {
        let store = ChallengeStore::new();
        let _nonce = store.issue();
        let wrong = [0xFF; 32];
        assert!(!store.verify_and_consume(&wrong));
    }

    #[test]
    fn test_new_challenge_replaces_old() {
        let store = ChallengeStore::new();
        let nonce1 = store.issue();
        let nonce2 = store.issue();
        // Old nonce should be invalid.
        assert!(!store.verify_and_consume(&nonce1));
        // New nonce should be valid.
        assert!(store.verify_and_consume(&nonce2));
    }

    #[test]
    fn test_no_challenge_issued() {
        let store = ChallengeStore::new();
        let nonce = [0xAB; 32];
        assert!(!store.verify_and_consume(&nonce));
    }
}
```

**Step 3: Rewrite `secrets.rs` — add owner authentication**

Replace the entire `crates/aleph-attest-agent/src/secrets.rs` with:

```rust
use std::collections::HashMap;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Mutex;

use actix_web::{HttpResponse, web};
use k256::ecdsa::{Signature, VerifyingKey, signature::Verifier};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::info;
use zeroize::Zeroizing;

use crate::challenge::ChallengeStore;

/// Directory where injected secrets are written as individual files.
const SECRETS_DIR: &str = "/tmp/secrets";

/// Maximum number of secrets that can be injected in a single request.
const MAX_SECRETS: usize = 16;

/// Maximum length of a secret key name.
const MAX_KEY_LEN: usize = 64;

/// Maximum size of a single secret value in bytes (64 KiB).
const MAX_VALUE_SIZE: usize = 64 * 1024;

/// One-shot injection guard. `None` = not yet injected, `Some(())` = already injected.
static INJECTION_LOCK: Mutex<Option<()>> = Mutex::new(None);

/// Cached HOSTDATA from the VM's own attestation report, set at startup.
/// Used to verify that the caller's pubkey matches the VM's owner binding.
pub struct HostDataCache {
    pub host_data: [u8; 32],
}

#[derive(Deserialize)]
pub struct InjectSecretRequest {
    /// Hex-encoded compressed secp256k1 public key (33 bytes).
    pub pubkey: String,
    /// Hex-encoded ECDSA signature over the challenge nonce.
    pub signature: String,
    /// Key-value secrets to inject.
    pub secrets: HashMap<String, String>,
}

#[derive(Serialize)]
pub struct InjectSecretResponse {
    pub injected: Vec<String>,
}

/// POST /confidential/inject-secret
///
/// Requires owner authentication: the caller must provide their secp256k1
/// public key (which must hash to the VM's HOSTDATA) and a signature over
/// a previously-issued challenge nonce.
///
/// After authentication, writes each secret as a file under /tmp/secrets/<key>.
/// One-shot: returns 409 on subsequent calls.
pub async fn inject_secret_handler(
    body: web::Json<InjectSecretRequest>,
    host_data_cache: web::Data<HostDataCache>,
    challenge_store: web::Data<ChallengeStore>,
) -> HttpResponse {
    // ── Owner authentication ────────────────────────────────────────────

    // 1. Decode pubkey.
    let pubkey_bytes = match hex::decode(&body.pubkey) {
        Ok(b) => b,
        Err(e) => {
            return HttpResponse::BadRequest()
                .json(serde_json::json!({"error": format!("invalid pubkey hex: {e}")}));
        }
    };

    // 2. Verify pubkey binding: SHA-256(pubkey) must equal HOSTDATA.
    let pubkey_hash = Sha256::digest(&pubkey_bytes);
    if pubkey_hash.as_slice() != host_data_cache.host_data.as_slice() {
        return HttpResponse::Forbidden().json(serde_json::json!({
            "error": "pubkey does not match HOSTDATA binding"
        }));
    }

    // 3. Parse the verifying key.
    let verifying_key = match VerifyingKey::from_sec1_bytes(&pubkey_bytes) {
        Ok(vk) => vk,
        Err(e) => {
            return HttpResponse::BadRequest()
                .json(serde_json::json!({"error": format!("invalid secp256k1 pubkey: {e}")}));
        }
    };

    // 4. Decode signature.
    let sig_bytes = match hex::decode(&body.signature) {
        Ok(b) => b,
        Err(e) => {
            return HttpResponse::BadRequest()
                .json(serde_json::json!({"error": format!("invalid signature hex: {e}")}));
        }
    };
    let signature = match Signature::from_der(&sig_bytes) {
        Ok(s) => s,
        Err(e) => {
            return HttpResponse::BadRequest()
                .json(serde_json::json!({"error": format!("invalid ECDSA signature: {e}")}));
        }
    };

    // 5. Verify and consume the challenge nonce.
    //    We need the nonce bytes to verify the signature, so we retrieve it
    //    from the store before consuming.
    //    The ChallengeStore gives us a bool — but we need the nonce to verify.
    //    Refactor: the store should return the nonce on consume.
    //    For now, the signature is over the nonce, and we verify by checking
    //    the signature against each possible nonce — but that's wrong.
    //    Actually: the client knows the nonce (received from GET /challenge),
    //    so we need to reconstruct it. Let's change the approach: the request
    //    must also include the nonce, and we verify it matches the stored one.

    // Actually, simpler: extract nonce from ChallengeStore, verify sig against it.
    // We need ChallengeStore to return the nonce on consume.
    // Let's update ChallengeStore::verify_and_consume to return Option<[u8; 32]>.

    // For this to work, we need a slightly different ChallengeStore API.
    // See the updated challenge.rs below.

    // This handler will be finalized after updating ChallengeStore.
    // For now, placeholder — the actual implementation is in the combined step.

    // ── One-shot guard + secret writing (existing logic) ────────────────

    let mut guard = match INJECTION_LOCK.lock() {
        Ok(g) => g,
        Err(_) => {
            return HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": "internal lock error"}));
        }
    };

    if guard.is_some() {
        return HttpResponse::Conflict()
            .json(serde_json::json!({"error": "secrets already injected"}));
    }

    if body.secrets.is_empty() {
        return HttpResponse::BadRequest()
            .json(serde_json::json!({"error": "no secrets provided"}));
    }

    if body.secrets.len() > MAX_SECRETS {
        return HttpResponse::BadRequest()
            .json(serde_json::json!({"error": format!("too many secrets: max {MAX_SECRETS}, got {}", body.secrets.len())}));
    }

    for (key, value) in &body.secrets {
        if key.is_empty() || key.len() > MAX_KEY_LEN {
            return HttpResponse::BadRequest()
                .json(serde_json::json!({"error": format!("secret key length must be 1-{MAX_KEY_LEN}, got {}", key.len())}));
        }
        if !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return HttpResponse::BadRequest()
                .json(serde_json::json!({"error": format!("invalid secret key: must be alphanumeric/underscore/hyphen, got: {key}")}));
        }
        if value.len() > MAX_VALUE_SIZE {
            return HttpResponse::BadRequest()
                .json(serde_json::json!({"error": format!("secret value too large for key '{key}': max {MAX_VALUE_SIZE} bytes, got {}", value.len())}));
        }
    }

    let secrets_dir = Path::new(SECRETS_DIR);
    if let Err(e) = std::fs::create_dir_all(secrets_dir) {
        tracing::error!("failed to create secrets directory: {e}");
        return HttpResponse::InternalServerError()
            .json(serde_json::json!({"error": "failed to create secrets directory"}));
    }

    let mut injected = Vec::new();
    for (key, value) in &body.secrets {
        let secret_value = Zeroizing::new(value.as_bytes().to_vec());
        let path = secrets_dir.join(key);
        let result = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .and_then(|mut f| {
                use std::io::Write;
                f.write_all(&secret_value)
            });

        if let Err(e) = result {
            tracing::error!("failed to write secret {key}: {e}");
            *guard = Some(());
            return HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": format!("failed to write secret: {key}")}));
        }
        info!(key = %key, "injected secret");
        injected.push(key.clone());
    }

    *guard = Some(());
    HttpResponse::Ok().json(InjectSecretResponse { injected })
}
```

**Important refinement:** The above has a problem — we need the challenge nonce bytes to verify the signature against, but `verify_and_consume` returns a `bool`. Let's fix `ChallengeStore` to return `Option<[u8; 32]>`:

Update `ChallengeStore::verify_and_consume` in `challenge.rs` to:

```rust
/// Consume the current challenge if it hasn't expired.
/// Returns the nonce bytes on success, None if no challenge or expired.
pub fn consume(&self) -> Option<[u8; 32]> {
    let mut guard = self.inner.lock().unwrap();
    match guard.take() {
        Some(c) if Instant::now() < c.expires_at => Some(c.nonce),
        _ => None,
    }
}
```

Then in `inject_secret_handler`, replace the challenge verification section with:

```rust
// 5. Consume the challenge nonce and verify the signature.
let nonce = match challenge_store.consume() {
    Some(n) => n,
    None => {
        return HttpResponse::BadRequest()
            .json(serde_json::json!({"error": "no active challenge — call GET /confidential/challenge first"}));
    }
};

if verifying_key.verify(&nonce, &signature).is_err() {
    return HttpResponse::Forbidden()
        .json(serde_json::json!({"error": "signature verification failed"}));
}

info!("owner authentication successful");
```

**Step 4: Update `proxy.rs` — add `ChallengeStore` and `HostDataCache` to AppState is NOT needed**

`ChallengeStore` and `HostDataCache` are injected as separate `web::Data<>` resources, not as fields on `AppState`. No changes to `proxy.rs` are needed (the struct stays the same).

**Step 5: Update `main.rs` — register new endpoint and inject app data**

In `crates/aleph-attest-agent/src/main.rs`:

Add `mod challenge;` to the module declarations.

Add imports:

```rust
use challenge::{ChallengeStore, challenge_endpoint};
use secrets::HostDataCache;
```

At startup, after generating the TLS identity, cache the host_data from the attestation report:

```rust
// 3.5. Cache HOSTDATA from our own attestation report.
let host_data = identity.report.host_data;
info!(host_data = %hex::encode(host_data), "cached HOSTDATA from attestation report");
```

Create the shared data:

```rust
let challenge_store = web::Data::new(ChallengeStore::new());
let host_data_cache = web::Data::new(HostDataCache { host_data });
```

Update the `HttpServer::new` closure to register the new endpoint and inject the new data:

```rust
HttpServer::new(move || {
    App::new()
        .app_data(app_state.clone())
        .app_data(challenge_store.clone())
        .app_data(host_data_cache.clone())
        .route(
            "/.well-known/attestation",
            web::get().to(attestation_endpoint),
        )
        .route(
            "/confidential/challenge",
            web::get().to(challenge_endpoint),
        )
        .route(
            "/confidential/inject-secret",
            web::post().to(inject_secret_handler),
        )
        .default_service(web::to(proxy_handler))
})
```

Note: The `AttestedTlsIdentity` struct (in `tls.rs`) already has a `report: AttestationReport` field, so `identity.report.host_data` is available.

**Step 6: Run tests and verify**

Run: `cargo test -p aleph-attest-agent`
Expected: All pass (challenge.rs unit tests + existing tests).

Run: `cargo build -p aleph-attest-agent`
Expected: Compiles (the handler signature change is compatible with actix-web's extractor system).

**Step 7: Commit**

```
feat(attest-agent): challenge endpoint and owner-authenticated secret injection
```

---

### Task 5: Add authenticated injection to attest-cli

**Files:**
- Modify: `crates/aleph-attest-cli/Cargo.toml`
- Modify: `crates/aleph-attest-cli/src/main.rs`
- Modify: `crates/aleph-attest-cli/src/client.rs`
- Modify: `crates/aleph-attest-cli/src/verify.rs`

**Step 1: Add `k256` dependency to attest-cli**

In `crates/aleph-attest-cli/Cargo.toml`, add:

```toml
k256 = { workspace = true }
```

**Step 2: Add `--signing-key` to the `InjectSecret` command**

In `crates/aleph-attest-cli/src/main.rs`, update the `InjectSecret` variant:

```rust
InjectSecret {
    #[command(flatten)]
    common: CommonArgs,

    /// Hex-encoded secp256k1 private key for owner authentication
    #[arg(long)]
    signing_key: String,

    /// Secret to inject as key=value (can be repeated)
    #[arg(long = "secret", value_parser = parse_key_value)]
    secrets: Vec<(String, String)>,
},
```

Update the match arm for `InjectSecret`:

```rust
Command::InjectSecret {
    common,
    signing_key,
    secrets,
} => {
    let expected = common.parse_expected_measurement()?;

    if secrets.is_empty() {
        anyhow::bail!("at least one --secret key=value is required");
    }

    let sk_bytes =
        hex::decode(&signing_key).context("--signing-key must be valid hex")?;

    println!(
        "Injecting {} secret(s) into {}...",
        secrets.len(),
        common.url
    );

    let resp = client::inject_secret(
        &common.url,
        &common.amd_product,
        expected.as_deref(),
        &sk_bytes,
        &secrets,
    )
    .await?;

    println!("Secrets injected successfully!");
    for key in &resp.injected {
        println!("  - {}", key);
    }
}
```

**Step 3: Rewrite `client::inject_secret` to use challenge-response**

In `crates/aleph-attest-cli/src/client.rs`, add the imports:

```rust
use k256::ecdsa::{SigningKey, Signature as K256Signature, signature::Signer};
use sha2::{Digest, Sha256};
```

Add a struct for the challenge response:

```rust
#[derive(Deserialize)]
struct ChallengeResponse {
    nonce: String,
}
```

Rewrite `inject_secret`:

```rust
pub async fn inject_secret(
    base_url: &str,
    product: &str,
    expected_measurement: Option<&[u8]>,
    signing_key_bytes: &[u8],
    secrets: &[(String, String)],
) -> Result<InjectSecretResponse> {
    let base = url::Url::parse(base_url).context("failed to parse base URL")?;

    // Parse the signing key and derive the public key.
    let signing_key = SigningKey::from_bytes(signing_key_bytes.into())
        .context("invalid secp256k1 private key")?;
    let verifying_key = signing_key.verifying_key();
    let pubkey_bytes = verifying_key.to_sec1_bytes();

    // Build the attested client (verifies TLS cert + attestation).
    let verifier = SnpCertVerifier::new(expected_measurement.map(|m| m.to_vec()));
    let client = build_attested_client(&verifier)?;

    // 1. Verify attestation via TLS handshake + check host_data.
    let challenge_url = base
        .join("confidential/challenge")
        .context("failed to construct challenge URL")?;

    let challenge_resp = client
        .get(challenge_url.as_str())
        .send()
        .await
        .context("failed to request challenge")?;

    // Verify attestation from TLS handshake.
    let report = verifier
        .get_report()
        .context("no attestation report extracted from TLS handshake")?;
    let result = verify_sev_snp_report(&report, product)
        .await
        .context("SEV-SNP report verification failed")?;
    if !result.valid {
        bail!("attestation invalid: {}", result.summary);
    }

    // Verify host_data matches our pubkey.
    let expected_host_data = Sha256::digest(&pubkey_bytes);
    if report.host_data != expected_host_data.as_slice() {
        bail!(
            "HOSTDATA mismatch: VM is bound to a different owner.\n  Expected: {}\n  Got:      {}",
            hex::encode(expected_host_data),
            hex::encode(report.host_data)
        );
    }

    // 2. Parse the challenge nonce.
    let challenge: ChallengeResponse = challenge_resp
        .json()
        .await
        .context("failed to parse challenge response")?;
    let nonce = hex::decode(&challenge.nonce).context("invalid nonce hex")?;
    let nonce: [u8; 32] = nonce
        .try_into()
        .map_err(|v: Vec<u8>| anyhow::anyhow!("nonce must be 32 bytes, got {}", v.len()))?;

    // 3. Sign the nonce.
    let signature: K256Signature = signing_key.sign(&nonce);
    let sig_der = signature.to_der();

    // 4. POST the authenticated inject-secret request.
    let inject_url = base
        .join("confidential/inject-secret")
        .context("failed to construct inject-secret URL")?;

    let secrets_map: HashMap<&str, &str> = secrets
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let payload = serde_json::json!({
        "pubkey": hex::encode(&pubkey_bytes),
        "signature": hex::encode(sig_der.as_bytes()),
        "secrets": secrets_map,
    });

    let response = client
        .post(inject_url.as_str())
        .json(&payload)
        .send()
        .await
        .context("failed to send inject-secret request")?;

    let status = response.status().as_u16();
    if status == 409 {
        bail!("secrets already injected (409 Conflict)");
    }
    if status == 403 {
        let body = response.text().await.unwrap_or_default();
        bail!("owner authentication failed (403): {body}");
    }
    if status != 200 {
        let body = response.text().await.unwrap_or_default();
        bail!("inject-secret failed with status {status}: {body}");
    }

    let resp: InjectSecretResponse = response
        .json()
        .await
        .context("failed to parse inject-secret response")?;
    Ok(resp)
}
```

**Step 4: Add `host_data` to verification output**

In `crates/aleph-attest-cli/src/main.rs`, update the `Attest` arm to print `host_data`:

```rust
Command::Attest(args) => {
    // ... existing code ...
    println!("Measurement:       {}", hex::encode(&response.measurement));
    println!("Host data:         {}", hex::encode(&response.host_data));
    // ...
}
```

Similarly for `FreshAttest`:

```rust
println!("  Host data:    {}", hex::encode(report.host_data));
```

For this, `AttestedResponse` needs a `host_data` field. Add it:

```rust
pub struct AttestedResponse {
    pub attestation_valid: bool,
    pub attestation_summary: String,
    pub measurement: Vec<u8>,
    pub host_data: [u8; 32],
    pub status: u16,
    pub body: String,
}
```

And set it in `attested_request`:

```rust
Ok(AttestedResponse {
    attestation_valid: result.valid,
    attestation_summary: result.summary,
    measurement: result.measurement,
    host_data: report.host_data,
    status,
    body,
})
```

**Step 5: Run tests and verify**

Run: `cargo build -p aleph-attest-cli`
Expected: Compiles.

Run: `cargo test -p aleph-attest-cli`
Expected: All pass.

**Step 6: Commit**

```
feat(attest-cli): owner-authenticated secret injection with challenge-response
```

---

### Task 6: Add SSH authorized keys injection to init.sh

**Files:**
- Modify: `nix/init.sh`

**Step 1: Add SSH key injection after rootfs mount**

In `nix/init.sh`, add SSH authorized keys injection in both the LUKS and non-LUKS paths, right after `prepare_chroot` and before starting `/sbin/init`.

In the LUKS path (after `prepare_chroot` around line 128), add:

```sh
# Inject SSH authorized keys if provided via secret injection.
if [ -f /tmp/secrets/ssh_authorized_keys ]; then
    echo "init: injecting SSH authorized keys"
    /bin/busybox mkdir -p /mnt/root/root/.ssh
    /bin/busybox cp /tmp/secrets/ssh_authorized_keys /mnt/root/root/.ssh/authorized_keys
    /bin/busybox chmod 600 /mnt/root/root/.ssh/authorized_keys
    /bin/busybox chmod 700 /mnt/root/root/.ssh
fi
```

In the non-LUKS path (after `prepare_chroot` around line 259), add the same block before the `/sbin/init` check.

**Step 2: Commit**

```
feat(init): inject SSH authorized keys from secrets into rootfs
```

---

### Task 7: Update demo scripts and verify build

**Files:**
- Modify: `scripts/demo.sh` (optional — update to pass `host-data` if testing owner auth e2e)
- Modify: `scripts/demo-encrypted.sh` (optional — same)

**Step 1: Verify full workspace builds**

Run: `cargo build --workspace`
Expected: All crates compile.

**Step 2: Run full test suite**

Run: `cargo test --workspace`
Expected: All tests pass.

**Step 3: Verify clippy is clean**

Run: `cargo clippy --workspace -- -D warnings`
Expected: No warnings.

**Step 4: Verify formatting**

Run: `cargo fmt --check`
Expected: No formatting issues.

**Step 5: Commit any remaining fixes**

```
chore: fix clippy and formatting for hostdata owner auth
```
