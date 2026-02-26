use anyhow::{Context, Result, bail};
use openssl::ecdsa::EcdsaSig;
use openssl::hash::MessageDigest;
use openssl::x509::X509;
use serde_json::json;

use crate::types::{AttestationReport, TeeType, VerificationResult};

use super::certs::{CertChain, TcbParams, fetch_ca_chain, fetch_vcek};
use super::report::{extract_measurement, parse_sev_snp_report};

/// The signed portion of an SEV-SNP attestation report is bytes 0x000..0x2A0.
const SIGNED_REPORT_SIZE: usize = 0x2A0;

/// Verify an SEV-SNP attestation report by checking the full AMD certificate
/// chain and report signature.
///
/// Steps:
/// 1. Parse the raw report to extract chip_id and TCB version
/// 2. Fetch the VCEK certificate from AMD KDS
/// 3. Fetch the ASK/ARK CA chain from AMD KDS
/// 4. Verify the certificate chain (ARK self-signed, ASK signed by ARK, VCEK signed by ASK)
/// 5. Verify the report signature using the VCEK public key
pub async fn verify_sev_snp_report(
    report: &AttestationReport,
    product: &str,
) -> Result<VerificationResult> {
    let raw = &report.data;

    // 1. Parse the report
    let parsed = parse_sev_snp_report(raw)
        .context("failed to parse SEV-SNP attestation report")?;

    let measurement = extract_measurement(&parsed).to_vec();

    // 2. Extract chip_id and TCB version from the parsed report
    let chip_id = parsed.inner.chip_id;
    let reported_tcb = &parsed.inner.reported_tcb;
    let tcb = TcbParams {
        bl_spl: reported_tcb.bootloader,
        tee_spl: reported_tcb.tee,
        snp_spl: reported_tcb.snp,
        ucode_spl: reported_tcb.microcode,
    };

    // 3. Fetch VCEK from AMD KDS
    let vcek_der = fetch_vcek(product, &chip_id, &tcb)
        .await
        .context("failed to fetch VCEK certificate from AMD KDS")?;

    // 4. Fetch ASK/ARK CA chain
    let (ask_der, ark_der) = fetch_ca_chain(product)
        .await
        .context("failed to fetch CA chain from AMD KDS")?;

    let chain = CertChain {
        vcek_der: vcek_der.clone(),
        ask_der,
        ark_der,
    };

    // 5. Verify certificate chain
    verify_cert_chain(&chain)
        .context("certificate chain verification failed")?;

    // 6. Verify report signature
    verify_report_signature(raw, &chain.vcek_der)
        .context("report signature verification failed")?;

    Ok(VerificationResult {
        valid: true,
        tee_type: TeeType::SevSnp,
        summary: format!(
            "SEV-SNP report verified successfully (product: {product})"
        ),
        measurement,
        details: json!({
            "product": product,
            "guest_svn": parsed.inner.guest_svn,
            "vmpl": parsed.inner.vmpl,
            "verified": true,
            "tcb": {
                "bootloader": tcb.bl_spl,
                "tee": tcb.tee_spl,
                "snp": tcb.snp_spl,
                "microcode": tcb.ucode_spl,
            },
        }),
    })
}

/// Verify the AMD certificate chain:
/// - ARK is self-signed
/// - ASK is signed by ARK
/// - VCEK is signed by ASK
pub fn verify_cert_chain(chain: &CertChain) -> Result<()> {
    let ark = X509::from_der(&chain.ark_der)
        .context("failed to parse ARK certificate")?;
    let ask = X509::from_der(&chain.ask_der)
        .context("failed to parse ASK certificate")?;
    let vcek = X509::from_der(&chain.vcek_der)
        .context("failed to parse VCEK certificate")?;

    // Verify ARK is self-signed
    let ark_pubkey = ark.public_key()
        .context("failed to extract ARK public key")?;
    if !ark.verify(&ark_pubkey)
        .context("failed to verify ARK self-signature")?
    {
        bail!("ARK certificate is not validly self-signed");
    }

    // Verify ASK is signed by ARK
    if !ask.verify(&ark_pubkey)
        .context("failed to verify ASK signature")?
    {
        bail!("ASK certificate is not signed by ARK");
    }

    // Verify VCEK is signed by ASK
    let ask_pubkey = ask.public_key()
        .context("failed to extract ASK public key")?;
    if !vcek.verify(&ask_pubkey)
        .context("failed to verify VCEK signature")?
    {
        bail!("VCEK certificate is not signed by ASK");
    }

    Ok(())
}

/// Verify the SEV-SNP report signature using the VCEK public key.
///
/// The signed portion of the report is bytes 0x000..0x2A0, hashed with SHA-384.
/// The signature is an ECDSA P-384 signature with r and s components of 72 bytes each
/// (little-endian, zero-padded), starting at offset 0x2A0 in the raw report.
pub fn verify_report_signature(report_raw: &[u8], vcek_der: &[u8]) -> Result<()> {
    if report_raw.len() < SIGNED_REPORT_SIZE + 144 {
        bail!(
            "report too short for signature verification: need at least {} bytes, got {}",
            SIGNED_REPORT_SIZE + 144,
            report_raw.len()
        );
    }

    // Extract the signed portion (bytes 0..0x2A0)
    let signed_data = &report_raw[..SIGNED_REPORT_SIZE];

    // Extract signature components (r and s, each 72 bytes, little-endian)
    let sig_offset = SIGNED_REPORT_SIZE;
    let r_bytes_le = &report_raw[sig_offset..sig_offset + 72];
    let s_bytes_le = &report_raw[sig_offset + 72..sig_offset + 144];

    // Convert from little-endian to big-endian (openssl expects big-endian)
    let r_bytes_be: Vec<u8> = r_bytes_le.iter().rev().collect::<Vec<_>>().into_iter().copied().collect();
    let s_bytes_be: Vec<u8> = s_bytes_le.iter().rev().collect::<Vec<_>>().into_iter().copied().collect();

    // Strip leading zeros but keep at least 1 byte
    let r_trimmed = strip_leading_zeros(&r_bytes_be);
    let s_trimmed = strip_leading_zeros(&s_bytes_be);

    // Build ECDSA signature from r and s components
    let r_bn = openssl::bn::BigNum::from_slice(r_trimmed)
        .context("failed to create BigNum from r component")?;
    let s_bn = openssl::bn::BigNum::from_slice(s_trimmed)
        .context("failed to create BigNum from s component")?;

    let ecdsa_sig = EcdsaSig::from_private_components(r_bn, s_bn)
        .context("failed to create ECDSA signature")?;

    // Hash the signed portion with SHA-384
    let digest = openssl::hash::hash(MessageDigest::sha384(), signed_data)
        .context("failed to compute SHA-384 digest")?;

    // Extract VCEK public key
    let vcek = X509::from_der(vcek_der)
        .context("failed to parse VCEK certificate for signature verification")?;
    let vcek_pkey = vcek.public_key()
        .context("failed to extract VCEK public key")?;
    let ec_key = vcek_pkey.ec_key()
        .context("VCEK public key is not an EC key")?;

    // Verify the ECDSA signature
    let valid = ecdsa_sig.verify(&digest, &ec_key)
        .context("ECDSA signature verification failed")?;

    if !valid {
        bail!("SEV-SNP report signature is invalid");
    }

    Ok(())
}

/// Strip leading zero bytes from a big-endian byte slice, keeping at least one byte.
fn strip_leading_zeros(bytes: &[u8]) -> &[u8] {
    let first_nonzero = bytes.iter().position(|&b| b != 0);
    match first_nonzero {
        Some(pos) => &bytes[pos..],
        None => &bytes[bytes.len().saturating_sub(1)..], // all zeros, keep last byte
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_leading_zeros() {
        assert_eq!(strip_leading_zeros(&[0, 0, 1, 2, 3]), &[1, 2, 3]);
        assert_eq!(strip_leading_zeros(&[1, 2, 3]), &[1, 2, 3]);
        assert_eq!(strip_leading_zeros(&[0, 0, 0]), &[0]);
        assert_eq!(strip_leading_zeros(&[0]), &[0]);
        assert_eq!(strip_leading_zeros(&[5]), &[5]);
    }

    #[test]
    fn test_verify_report_signature_too_short() {
        let short = vec![0u8; 100];
        let result = verify_report_signature(&short, &[]);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("too short"), "unexpected error: {err}");
    }

    #[test]
    fn test_verify_cert_chain_invalid_certs() {
        let chain = CertChain {
            vcek_der: vec![0x30, 0x00],
            ask_der: vec![0x30, 0x00],
            ark_der: vec![0x30, 0x00],
        };
        let result = verify_cert_chain(&chain);
        assert!(result.is_err(), "invalid certs should fail verification");
    }
}
