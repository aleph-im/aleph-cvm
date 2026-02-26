use aleph_tee::sev_snp::verify::verify_sev_snp_report;
use aleph_tee::types::AttestationReport;
use anyhow::{Context, Result, bail};
use rand::Rng;

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

/// Layer 2: Make an API call with TLS-bound attestation verification.
///
/// This function:
/// 1. Creates a custom TLS verifier that extracts attestation from the server cert
/// 2. Builds a rustls `ClientConfig` using the custom verifier
/// 3. Makes a GET request to the given URL
/// 4. Extracts the attestation report from the TLS handshake
/// 5. Verifies the SEV-SNP report against the AMD certificate chain
/// 6. Returns an `AttestedResponse` with both attestation and HTTP response info
pub async fn attested_request(url: &str, product: &str) -> Result<AttestedResponse> {
    let verifier = SnpCertVerifier::new();

    let tls_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier.clone())
        .with_no_client_auth();

    let client = reqwest::Client::builder()
        .use_preconfigured_tls(tls_config)
        .build()
        .context("failed to build HTTP client with custom TLS config")?;

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
/// This function:
/// 1. Generates a random 32-byte nonce
/// 2. Sends a GET request to `{base_url}/.well-known/attestation?nonce={hex_nonce}`
/// 3. Parses the JSON response as an `AttestationReport`
/// 4. Verifies the nonce appears in the `report_data` field
/// 5. Verifies the SEV-SNP report against the AMD certificate chain
/// 6. Returns the verified `AttestationReport`
pub async fn fresh_attestation(base_url: &str, product: &str) -> Result<AttestationReport> {
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

    // Use attested_request to make the call with TLS-bound verification
    let verifier = SnpCertVerifier::new();

    let tls_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier.clone())
        .with_no_client_auth();

    let client = reqwest::Client::builder()
        .use_preconfigured_tls(tls_config)
        .build()
        .context("failed to build HTTP client with custom TLS config")?;

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

    // Verify the SEV-SNP report
    let result = verify_sev_snp_report(&report, product)
        .await
        .context("SEV-SNP report verification failed")?;

    if !result.valid {
        bail!("SEV-SNP attestation report is not valid: {}", result.summary);
    }

    Ok(report)
}
