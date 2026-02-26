use std::sync::{Arc, Mutex};

use aleph_tee::types::AttestationReport;
use aleph_tee::x509::extract_attestation_from_cert;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error, SignatureScheme};

/// A custom TLS certificate verifier that extracts SEV-SNP attestation reports
/// from the server's X.509 certificate extension.
///
/// Instead of performing standard certificate chain validation, this verifier
/// looks for a custom attestation extension in the end-entity certificate.
/// If found, it stores the extracted report for later retrieval and verification.
#[derive(Debug)]
pub struct SnpCertVerifier {
    extracted_report: Mutex<Option<AttestationReport>>,
    provider: Arc<CryptoProvider>,
}

impl SnpCertVerifier {
    /// Create a new `SnpCertVerifier` wrapped in an `Arc` for use with rustls.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            extracted_report: Mutex::new(None),
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
        let report = extract_attestation_from_cert(end_entity.as_ref()).map_err(|e| {
            Error::General(format!("failed to extract attestation from certificate: {e}"))
        })?;

        match report {
            Some(report) => {
                let mut stored = self.extracted_report.lock().unwrap();
                *stored = Some(report);
                Ok(ServerCertVerified::assertion())
            }
            None => Err(Error::General(
                "certificate does not contain an attestation extension".to_string(),
            )),
        }
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
