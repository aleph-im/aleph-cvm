use std::collections::HashMap;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Mutex;

use actix_web::{HttpResponse, web};
use k256::ecdsa::signature::Verifier;
use k256::ecdsa::{Signature, VerifyingKey};
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
/// Using a Mutex instead of AtomicBool ensures the entire inject operation is atomic —
/// no TOCTOU race between checking and writing.
static INJECTION_LOCK: Mutex<Option<()>> = Mutex::new(None);

/// Cached HOSTDATA from the VM's attestation report, used to verify owner identity.
pub struct HostDataCache {
    pub host_data: [u8; 32],
}

#[derive(Deserialize)]
pub struct InjectSecretRequest {
    /// Hex-encoded compressed secp256k1 public key (33 bytes).
    pub pubkey: String,
    /// Hex-encoded DER ECDSA signature over the challenge nonce.
    pub signature: String,
    /// Key-value map of secrets to inject.
    pub secrets: HashMap<String, String>,
}

#[derive(Serialize)]
pub struct InjectSecretResponse {
    pub injected: Vec<String>,
}

/// POST /confidential/inject-secret
///
/// Owner-authenticated secret injection. The caller must:
/// 1. Obtain a challenge nonce via GET /confidential/challenge
/// 2. Sign the nonce with the secp256k1 key whose SHA-256 hash matches HOSTDATA
/// 3. Submit the signed request with secrets
///
/// Returns 400 for malformed requests, 403 for auth failures, 409 if already injected.
pub async fn inject_secret_handler(
    body: web::Json<InjectSecretRequest>,
    challenge_store: web::Data<ChallengeStore>,
    host_data_cache: web::Data<HostDataCache>,
) -> HttpResponse {
    // --- Authentication ---

    // 1. Decode the public key from hex.
    let pubkey_bytes = match hex::decode(&body.pubkey) {
        Ok(b) => b,
        Err(e) => {
            return HttpResponse::BadRequest()
                .json(serde_json::json!({"error": format!("invalid hex pubkey: {e}")}));
        }
    };

    // 2. Verify SHA-256(pubkey) == HOSTDATA.
    let pubkey_hash = Sha256::digest(&pubkey_bytes);
    if pubkey_hash.as_slice() != host_data_cache.host_data {
        return HttpResponse::Forbidden()
            .json(serde_json::json!({"error": "pubkey does not match HOSTDATA"}));
    }

    // 3. Parse the public key as a secp256k1 verifying key.
    let verifying_key = match VerifyingKey::from_sec1_bytes(&pubkey_bytes) {
        Ok(k) => k,
        Err(e) => {
            return HttpResponse::BadRequest()
                .json(serde_json::json!({"error": format!("invalid secp256k1 pubkey: {e}")}));
        }
    };

    // 4. Decode the DER signature.
    let sig = match Signature::from_der(&match hex::decode(&body.signature) {
        Ok(b) => b,
        Err(e) => {
            return HttpResponse::BadRequest()
                .json(serde_json::json!({"error": format!("invalid hex signature: {e}")}));
        }
    }) {
        Ok(s) => s,
        Err(e) => {
            return HttpResponse::BadRequest()
                .json(serde_json::json!({"error": format!("invalid DER signature: {e}")}));
        }
    };

    // 5. Consume the challenge nonce.
    // We need the raw nonce bytes to verify against, so we first decode the nonce
    // from the challenge store.
    // But the caller doesn't send the nonce — it's stored server-side. We consume it.
    // The caller signed the raw 32-byte nonce.
    let nonce = match challenge_store.consume_any() {
        Some(n) => n,
        None => {
            return HttpResponse::BadRequest()
                .json(serde_json::json!({"error": "no active challenge or challenge expired"}));
        }
    };

    // 6. Verify the signature over the nonce.
    if verifying_key.verify(&nonce, &sig).is_err() {
        return HttpResponse::Forbidden()
            .json(serde_json::json!({"error": "signature verification failed"}));
    }

    // --- Injection (existing logic) ---

    // Acquire the injection lock for the entire operation.
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

    // Validate all keys and values before writing anything (all-or-nothing).
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

    // Create secrets directory.
    let secrets_dir = Path::new(SECRETS_DIR);
    if let Err(e) = std::fs::create_dir_all(secrets_dir) {
        tracing::error!("failed to create secrets directory: {e}");
        return HttpResponse::InternalServerError()
            .json(serde_json::json!({"error": "failed to create secrets directory"}));
    }

    // Write each secret as a file.
    let mut injected = Vec::new();
    for (key, value) in &body.secrets {
        // Wrap value in Zeroizing so it's wiped from memory when dropped.
        let secret_value = Zeroizing::new(value.as_bytes().to_vec());

        let path = secrets_dir.join(key);
        // Write with mode 0600 (owner read/write only).
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
            // Partial write: some secrets may already be on disk.
            // Still mark as injected to prevent retry with inconsistent state.
            *guard = Some(());
            return HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": format!("failed to write secret: {key}")}));
        }
        info!(key = %key, "injected secret");
        injected.push(key.clone());
    }

    // Mark as injected only after all secrets are successfully written.
    *guard = Some(());

    HttpResponse::Ok().json(InjectSecretResponse { injected })
}
