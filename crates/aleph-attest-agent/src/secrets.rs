use std::collections::HashMap;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use actix_web::{web, HttpResponse};
use serde::{Deserialize, Serialize};
use tracing::info;
use zeroize::Zeroizing;

/// Directory where injected secrets are written as individual files.
const SECRETS_DIR: &str = "/tmp/secrets";

/// Atomic flag to enforce one-shot injection.
static SECRETS_INJECTED: AtomicBool = AtomicBool::new(false);

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
pub async fn inject_secret_handler(
    body: web::Json<InjectSecretRequest>,
) -> HttpResponse {
    // Enforce one-shot semantics.
    if SECRETS_INJECTED.swap(true, Ordering::SeqCst) {
        return HttpResponse::Conflict()
            .json(serde_json::json!({"error": "secrets already injected"}));
    }

    if body.secrets.is_empty() {
        SECRETS_INJECTED.store(false, Ordering::SeqCst);
        return HttpResponse::BadRequest()
            .json(serde_json::json!({"error": "no secrets provided"}));
    }

    // Create secrets directory.
    let secrets_dir = Path::new(SECRETS_DIR);
    if let Err(e) = std::fs::create_dir_all(secrets_dir) {
        SECRETS_INJECTED.store(false, Ordering::SeqCst);
        tracing::error!("failed to create secrets directory: {e}");
        return HttpResponse::InternalServerError()
            .json(serde_json::json!({"error": "failed to create secrets directory"}));
    }

    // Write each secret as a file.
    let mut injected = Vec::new();
    for (key, value) in &body.secrets {
        // Reject path traversal and unsafe characters.
        // Only allow alphanumeric, underscore, and hyphen.
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            return HttpResponse::BadRequest()
                .json(serde_json::json!({"error": format!("invalid secret key: must be alphanumeric/underscore/hyphen, got: {key}")}));
        }

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
            return HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": format!("failed to write secret: {key}")}));
        }
        info!(key = %key, "injected secret");
        injected.push(key.clone());
    }

    HttpResponse::Ok().json(InjectSecretResponse { injected })
}
