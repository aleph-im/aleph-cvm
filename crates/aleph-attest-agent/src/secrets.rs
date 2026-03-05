use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use actix_web::{web, HttpResponse};
use serde::{Deserialize, Serialize};
use tracing::info;

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
/// subsequent calls.
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
        // Reject path traversal attempts.
        if key.contains('/') || key.contains("..") || key.is_empty() {
            return HttpResponse::BadRequest()
                .json(serde_json::json!({"error": format!("invalid secret key: {key}")}));
        }

        let path = secrets_dir.join(key);
        if let Err(e) = std::fs::write(&path, value) {
            tracing::error!("failed to write secret {key}: {e}");
            return HttpResponse::InternalServerError()
                .json(serde_json::json!({"error": format!("failed to write secret: {key}")}));
        }
        info!(key = %key, "injected secret");
        injected.push(key.clone());
    }

    HttpResponse::Ok().json(InjectSecretResponse { injected })
}
