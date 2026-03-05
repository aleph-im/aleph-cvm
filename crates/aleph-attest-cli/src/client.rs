use std::collections::HashMap;
use std::sync::Arc;

use aleph_tee::sev_snp::verify::verify_sev_snp_report;
use aleph_tee::types::AttestationReport;
use anyhow::{Context, Result, bail};
use rand::Rng;
use serde::Deserialize;

use crate::verify::SnpCertVerifier;

/// The result of an attested HTTP request, combining the HTTP response
/// with attestation verification information.
pub struct AttestedResponse {
    /// Whether the attestation report was cryptographically valid.
    pub attestation_valid: bool,
    /// A human-readable summary of the attestation verification.
    pub attestation_summary: String,
    /// The TEE measurement (launch digest) from the attestation report.
    pub measurement: Vec<u8>,
    /// The HTTP status code of the response.
    pub status: u16,
    /// The HTTP response body as a string.
    pub body: String,
}

#[derive(Deserialize)]
pub struct InjectSecretResponse {
    pub injected: Vec<String>,
}

/// Build a reqwest client with our custom TLS verifier.
fn build_attested_client(
    verifier: &Arc<SnpCertVerifier>,
) -> Result<reqwest::Client> {
    let tls_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier.clone())
        .with_no_client_auth();

    reqwest::Client::builder()
        .use_preconfigured_tls(tls_config)
        .build()
        .context("failed to build HTTP client with custom TLS config")
}

/// Layer 2: Make an API call with TLS-bound attestation verification.
///
/// During the TLS handshake, the verifier checks:
/// - The server cert contains an attestation extension
/// - The report_data is bound to the server's TLS public key (SHA-384 hash)
/// - If `expected_measurement` is provided, the measurement matches
///
/// After the handshake, the full AMD certificate chain is verified
/// (VCEK -> ASK -> ARK) and the report signature is checked.
pub async fn attested_request(
    url: &str,
    product: &str,
    expected_measurement: Option<&[u8]>,
) -> Result<AttestedResponse> {
    let verifier = SnpCertVerifier::new(expected_measurement.map(|m| m.to_vec()));
    let client = build_attested_client(&verifier)?;

    let response = client
        .get(url)
        .send()
        .await
        .context("failed to send GET request")?;

    let status = response.status().as_u16();
    let body = response
        .text()
        .await
        .context("failed to read response body")?;

    let report = verifier
        .get_report()
        .context("no attestation report extracted from TLS handshake")?;

    let result = verify_sev_snp_report(&report, product)
        .await
        .context("SEV-SNP report verification failed")?;

    Ok(AttestedResponse {
        attestation_valid: result.valid,
        attestation_summary: result.summary,
        measurement: result.measurement,
        status,
        body,
    })
}

/// Layer 3: Request a fresh attestation report with a random nonce.
///
/// This combines TLS-bound verification (Layer 2) with a fresh nonce challenge:
/// 1. TLS handshake verifies key binding and optional measurement
/// 2. Random nonce is sent to the attestation endpoint
/// 3. Response proves the TEE can produce a fresh report containing our nonce
/// 4. Full AMD certificate chain is verified
pub async fn fresh_attestation(
    base_url: &str,
    product: &str,
    expected_measurement: Option<&[u8]>,
) -> Result<AttestationReport> {
    // Generate a random 32-byte nonce
    let mut nonce = [0u8; 32];
    rand::thread_rng().fill(&mut nonce);
    let nonce_hex = hex::encode(nonce);

    // Build the attestation URL
    let base = url::Url::parse(base_url).context("failed to parse base URL")?;
    let attestation_url = base
        .join(&format!(
            ".well-known/attestation?nonce={}",
            nonce_hex
        ))
        .context("failed to construct attestation URL")?;

    let verifier = SnpCertVerifier::new(expected_measurement.map(|m| m.to_vec()));
    let client = build_attested_client(&verifier)?;

    let response = client
        .get(attestation_url.as_str())
        .send()
        .await
        .context("failed to send attestation request")?;

    let body = response
        .text()
        .await
        .context("failed to read attestation response body")?;

    let report: AttestationReport =
        serde_json::from_str(&body).context("failed to parse attestation response as JSON")?;

    // Verify the nonce appears in the report_data (first 32 bytes)
    if report.report_data[..32] != nonce {
        bail!(
            "nonce mismatch: expected {} in report_data, got {}",
            nonce_hex,
            hex::encode(&report.report_data[..32])
        );
    }

    // Verify the SEV-SNP report against the AMD certificate chain
    let result = verify_sev_snp_report(&report, product)
        .await
        .context("SEV-SNP report verification failed")?;

    if !result.valid {
        bail!("SEV-SNP attestation report is not valid: {}", result.summary);
    }

    Ok(report)
}

/// Inject secrets into a confidential VM via attested TLS.
///
/// Sends a POST request with a JSON map of key-value secrets to the
/// `confidential/inject-secret` endpoint. The TLS channel is attested
/// (and optionally measurement-pinned), so secrets are only ever sent
/// to a verified TEE.
pub async fn inject_secret(
    base_url: &str,
    product: &str,
    expected_measurement: Option<&[u8]>,
    secrets: &[(String, String)],
) -> Result<InjectSecretResponse> {
    let base = url::Url::parse(base_url).context("failed to parse base URL")?;
    let inject_url = base
        .join("confidential/inject-secret")
        .context("failed to construct inject-secret URL")?;

    let verifier = SnpCertVerifier::new(expected_measurement.map(|m| m.to_vec()));
    let client = build_attested_client(&verifier)?;

    let secrets_map: HashMap<&str, &str> = secrets
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let response = client
        .post(inject_url.as_str())
        .json(&secrets_map)
        .send()
        .await
        .context("failed to send inject-secret request")?;

    // Verify attestation after TLS handshake.
    let report = verifier
        .get_report()
        .context("no attestation report extracted from TLS handshake")?;
    let result = verify_sev_snp_report(&report, product)
        .await
        .context("SEV-SNP report verification failed")?;
    if !result.valid {
        bail!("attestation invalid: {}", result.summary);
    }

    let status = response.status().as_u16();
    if status == 409 {
        bail!("secrets already injected (409 Conflict)");
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
