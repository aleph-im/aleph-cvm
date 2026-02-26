use anyhow::{Context, Result};

/// A complete AMD SEV-SNP certificate chain containing the VCEK, ASK, and ARK
/// certificates in DER format.
#[derive(Debug, Clone)]
pub struct CertChain {
    /// Versioned Chip Endorsement Key certificate (DER-encoded).
    pub vcek_der: Vec<u8>,
    /// AMD SEV Signing Key certificate (DER-encoded).
    pub ask_der: Vec<u8>,
    /// AMD Root Key certificate (DER-encoded).
    pub ark_der: Vec<u8>,
}

/// TCB security version parameters needed to fetch the correct VCEK
/// certificate from AMD's Key Distribution Service (KDS).
#[derive(Debug, Clone, Copy)]
pub struct TcbParams {
    /// Bootloader security patch level.
    pub bl_spl: u8,
    /// TEE security patch level.
    pub tee_spl: u8,
    /// SNP firmware security patch level.
    pub snp_spl: u8,
    /// Microcode security patch level.
    pub ucode_spl: u8,
}

/// Base URL for AMD's Key Distribution Service.
const KDS_BASE_URL: &str = "https://kdsintf.amd.com/vcek/v1";

/// Fetch the VCEK (Versioned Chip Endorsement Key) certificate from AMD KDS.
///
/// The `product` parameter identifies the CPU product line (e.g., "Milan", "Genoa", "Turin").
/// The `chip_id` is the 64-byte unique chip identifier from the attestation report.
/// The `tcb` parameters specify the TCB version to request.
///
/// Returns the VCEK certificate in DER format.
pub async fn fetch_vcek(
    product: &str,
    chip_id: &[u8; 64],
    tcb: &TcbParams,
) -> Result<Vec<u8>> {
    let chip_id_hex = hex::encode(chip_id);

    let url = format!(
        "{KDS_BASE_URL}/{product}/{chip_id_hex}?blSPL={}&teeSPL={}&snpSPL={}&ucodeSPL={}",
        tcb.bl_spl, tcb.tee_spl, tcb.snp_spl, tcb.ucode_spl
    );

    let response = reqwest::get(&url)
        .await
        .with_context(|| format!("failed to fetch VCEK from {url}"))?;

    let status = response.status();
    if !status.is_success() {
        anyhow::bail!("AMD KDS returned HTTP {status} for VCEK request: {url}");
    }

    let bytes = response
        .bytes()
        .await
        .context("failed to read VCEK response body")?;

    Ok(bytes.to_vec())
}

/// Fetch the CA certificate chain (ASK + ARK) from AMD KDS.
///
/// The `product` parameter identifies the CPU product line (e.g., "Milan", "Genoa", "Turin").
///
/// Returns a tuple of `(ask_der, ark_der)` -- the ASK and ARK certificates in DER format.
///
/// AMD's KDS returns a PKCS#7 / certificate bundle at the `cert_chain` endpoint.
/// The response contains two concatenated DER-encoded X.509 certificates:
/// the VCEK-signing CA (ASK) followed by the root CA (ARK).
pub async fn fetch_ca_chain(product: &str) -> Result<(Vec<u8>, Vec<u8>)> {
    let url = format!("{KDS_BASE_URL}/{product}/cert_chain");

    let response = reqwest::get(&url)
        .await
        .with_context(|| format!("failed to fetch CA chain from {url}"))?;

    let status = response.status();
    if !status.is_success() {
        anyhow::bail!("AMD KDS returned HTTP {status} for CA chain request: {url}");
    }

    let bytes = response
        .bytes()
        .await
        .context("failed to read CA chain response body")?;

    // The cert_chain endpoint returns two DER-encoded X.509 certificates
    // concatenated together. We need to split them by parsing the DER length
    // of the first certificate.
    split_der_certs(&bytes).context("failed to split CA chain into ASK and ARK certificates")
}

/// Split a buffer containing two concatenated DER-encoded certificates
/// into separate (first, second) byte vectors.
///
/// DER encoding starts with a tag byte, then a length encoding.
/// We parse the length of the first certificate to find where the second begins.
fn split_der_certs(data: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    if data.len() < 2 {
        anyhow::bail!("certificate data too short");
    }

    let first_len = der_object_length(data).context("failed to parse first certificate length")?;

    if first_len > data.len() {
        anyhow::bail!(
            "first certificate length ({first_len}) exceeds data length ({})",
            data.len()
        );
    }

    let first = data[..first_len].to_vec();
    let second = data[first_len..].to_vec();

    if second.is_empty() {
        anyhow::bail!("CA chain contains only one certificate, expected two (ASK + ARK)");
    }

    Ok((first, second))
}

/// Calculate the total length of a DER-encoded object (tag + length + value).
///
/// This reads the DER tag and length encoding to determine where the object ends.
fn der_object_length(data: &[u8]) -> Result<usize> {
    if data.is_empty() {
        anyhow::bail!("empty DER data");
    }

    // Skip the tag byte
    if data.len() < 2 {
        anyhow::bail!("DER data too short for tag + length");
    }

    let length_byte = data[1];

    if length_byte & 0x80 == 0 {
        // Short form: length is directly in this byte
        let content_len = length_byte as usize;
        Ok(2 + content_len)
    } else {
        // Long form: lower 7 bits tell how many subsequent bytes encode the length
        let num_length_bytes = (length_byte & 0x7F) as usize;
        if num_length_bytes == 0 {
            anyhow::bail!("indefinite DER length not supported");
        }
        if 2 + num_length_bytes > data.len() {
            anyhow::bail!("DER data too short for length encoding");
        }

        let mut content_len: usize = 0;
        for i in 0..num_length_bytes {
            content_len = content_len
                .checked_shl(8)
                .context("DER length overflow")?
                | data[2 + i] as usize;
        }

        // Total = tag(1) + length_byte(1) + num_length_bytes + content_len
        Ok(2 + num_length_bytes + content_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_der_object_length_short_form() {
        // Tag 0x30 (SEQUENCE), length 0x05, then 5 bytes of content
        let data = [0x30, 0x05, 0x01, 0x02, 0x03, 0x04, 0x05];
        assert_eq!(der_object_length(&data).unwrap(), 7);
    }

    #[test]
    fn test_der_object_length_long_form() {
        // Tag 0x30, length in 2 bytes: 0x82 0x01 0x00 = 256 bytes of content
        let mut data = vec![0x30, 0x82, 0x01, 0x00];
        data.extend(vec![0u8; 256]);
        assert_eq!(der_object_length(&data).unwrap(), 4 + 256);
    }

    #[test]
    fn test_split_der_certs() {
        // Create two fake DER objects
        let cert1 = vec![0x30, 0x03, 0xAA, 0xBB, 0xCC]; // 5 bytes total
        let cert2 = vec![0x30, 0x02, 0xDD, 0xEE]; // 4 bytes total

        let mut combined = cert1.clone();
        combined.extend_from_slice(&cert2);

        let (first, second) = split_der_certs(&combined).unwrap();
        assert_eq!(first, cert1);
        assert_eq!(second, cert2);
    }

    #[test]
    fn test_split_der_certs_too_short() {
        let result = split_der_certs(&[0x30]);
        assert!(result.is_err());
    }

    #[test]
    fn test_tcb_params() {
        let params = TcbParams {
            bl_spl: 3,
            tee_spl: 0,
            snp_spl: 10,
            ucode_spl: 169,
        };
        assert_eq!(params.bl_spl, 3);
        assert_eq!(params.tee_spl, 0);
        assert_eq!(params.snp_spl, 10);
        assert_eq!(params.ucode_spl, 169);
    }
}
