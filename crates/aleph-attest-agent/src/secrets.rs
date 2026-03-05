use std::collections::HashMap;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Mutex;

use actix_web::{web, HttpResponse};
use serde::{Deserialize, Serialize};
use tracing::info;
use zeroize::Zeroizing;

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

#[derive(Deserialize)]
pub struct InjectSecretRequest {
    #[serde(flatten)]
    pub secrets: HashMap<String, String>,
}

#[derive(Serialize)]
pub struct InjectSecretResponse {
    pub injected: Vec<String>,
}

/// POST /confidential/inject-secret
///
/// Accepts a JSON object of key-value pairs. Each key is written as a file
/// under /tmp/secrets/<key> containing the value. One-shot: returns 409 on
/// subsequent calls. Secret values are zeroized from memory after being
/// written to disk. Files are created with mode 0600.
///
/// Limits: max 16 secrets, max 64-char key names, max 64 KiB per value.
pub async fn inject_secret_handler(
    body: web::Json<InjectSecretRequest>,
) -> HttpResponse {
    // Acquire the injection lock for the entire operation.
    // This eliminates the TOCTOU race that existed with AtomicBool.
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
        if !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
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
