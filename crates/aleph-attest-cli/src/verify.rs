use std::sync::{Arc, Mutex};

use aleph_tee::types::AttestationReport;
use aleph_tee::x509::extract_attestation_from_cert;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error, SignatureScheme};
use sha2::{Digest, Sha384};

/// A custom TLS certificate verifier that extracts SEV-SNP attestation reports
/// from the server's X.509 certificate extension and verifies them during the
/// TLS handshake.
///
/// Verification performed at handshake time:
/// 1. The certificate must contain an attestation extension.
/// 2. The `report_data` field must equal `SHA-384(server_public_key) || zeros`,
///    proving the attestation report is bound to this specific TLS key.
/// 3. If an expected measurement is configured, the report's measurement must match.
///
/// The full AMD certificate chain verification (VCEK -> ASK -> ARK) is performed
/// after the handshake by the caller, since it requires async network access.
#[derive(Debug)]
pub struct SnpCertVerifier {
    extracted_report: Mutex<Option<AttestationReport>>,
    expected_measurement: Option<Vec<u8>>,
    provider: Arc<CryptoProvider>,
}

impl SnpCertVerifier {
    /// Create a new `SnpCertVerifier` wrapped in an `Arc` for use with rustls.
    ///
    /// If `expected_measurement` is `Some`, the handshake will be rejected if the
    /// attestation report's measurement doesn't match. This prevents the client
    /// from even completing a TLS connection to a VM running unexpected code.
    pub fn new(expected_measurement: Option<Vec<u8>>) -> Arc<Self> {
        Arc::new(Self {
            extracted_report: Mutex::new(None),
            expected_measurement,
            provider: Arc::new(rustls::crypto::ring::default_provider()),
        })
    }

    /// Retrieve the attestation report extracted during the TLS handshake.
    ///
    /// Returns `None` if no report was extracted (i.e., the handshake has not
    /// completed or the certificate did not contain an attestation extension).
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
        // 1. Extract attestation report from the certificate extension.
        let report = extract_attestation_from_cert(end_entity.as_ref())
            .map_err(|e| {
                Error::General(format!(
                    "failed to extract attestation from certificate: {e}"
                ))
            })?
            .ok_or_else(|| {
                Error::General("certificate does not contain an attestation extension".to_string())
            })?;

        // 2. Verify key binding: report_data must equal SHA-384(public_key) || zeros.
        //    This proves the attestation report was generated for this specific TLS key,
        //    preventing replay of someone else's attestation report.
        let (_, cert) = x509_parser::parse_x509_certificate(end_entity.as_ref()).map_err(|e| {
            Error::General(format!("failed to parse certificate for key binding: {e}"))
        })?;
        let public_key_bytes = cert.tbs_certificate.subject_pki.subject_public_key.data;

        let hash = Sha384::digest(public_key_bytes);
        let mut expected_report_data = [0u8; 64];
        expected_report_data[..48].copy_from_slice(&hash);

        if report.report_data != expected_report_data {
            return Err(Error::General(format!(
                "key binding verification failed: report_data does not match SHA-384(public_key). \
                 Expected {}, got {}",
                hex::encode(expected_report_data),
                hex::encode(report.report_data),
            )));
        }

        // 3. If an expected measurement is configured, verify it matches.
        if let Some(ref expected) = self.expected_measurement
            && report.measurement != *expected
        {
            return Err(Error::General(format!(
                "measurement mismatch: expected {}, got {}",
                hex::encode(expected),
                hex::encode(&report.measurement),
            )));
        }

        let mut stored = self.extracted_report.lock().unwrap();
        *stored = Some(report);
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
