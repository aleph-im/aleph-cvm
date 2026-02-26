use anyhow::{Context, Result};

use crate::traits::TeeBackend;
use crate::types::{AttestationReport, TeeType, VerificationResult, VmConfig};

use super::qemu::sev_snp_qemu_args;
use super::report::{extract_measurement, extract_report_data, parse_sev_snp_report};

/// SEV-SNP backend implementing the `TeeBackend` trait.
///
/// This backend handles attestation report retrieval, parsing,
/// verification (stubbed for now), and QEMU argument generation
/// for AMD SEV-SNP confidential VMs.
pub struct SevSnpBackend {
    /// The AMD product name (e.g., "Milan", "Genoa", "Turin").
    pub product: String,
}

impl SevSnpBackend {
    /// Create a new SEV-SNP backend for the given product line.
    pub fn new(product: impl Into<String>) -> Self {
        Self {
            product: product.into(),
        }
    }
}

impl TeeBackend for SevSnpBackend {
    fn tee_type(&self) -> TeeType {
        TeeType::SevSnp
    }

    /// Retrieve an attestation report from the AMD Secure Processor.
    ///
    /// This opens `/dev/sev-guest` and issues a GET_REPORT ioctl with the
    /// provided report_data. Only works on Linux hosts with SEV-SNP hardware.
    fn get_report(&self, report_data: &[u8; 64]) -> Result<AttestationReport> {
        #[cfg(target_os = "linux")]
        {
            let mut fw = sev::firmware::guest::Firmware::open()
                .context("failed to open /dev/sev-guest")?;

            let raw = fw
                .get_report(None, Some(*report_data), None)
                .map_err(|e| anyhow::anyhow!("SEV-SNP get_report failed: {e:?}"))?;

            self.parse_report(&raw)
        }

        #[cfg(not(target_os = "linux"))]
        {
            let _ = report_data;
            anyhow::bail!("SEV-SNP get_report is only supported on Linux")
        }
    }

    /// Verify an attestation report.
    ///
    /// This is a stub implementation that parses the report to confirm it is
    /// structurally valid and returns a placeholder verification result.
    /// Real cryptographic verification (certificate chain validation, signature
    /// checking) will be implemented in Task 4.
    fn verify_report(&self, report: &AttestationReport) -> Result<VerificationResult> {
        // Parse the raw report to confirm structural validity
        let parsed = parse_sev_snp_report(&report.data)
            .context("report data failed structural validation")?;

        let measurement = extract_measurement(&parsed).to_vec();

        Ok(VerificationResult {
            valid: true, // Stub: real verification in Task 4
            tee_type: TeeType::SevSnp,
            summary: format!(
                "SEV-SNP report parsed successfully (product: {}, stub verification)",
                self.product
            ),
            measurement,
            details: serde_json::json!({
                "product": self.product,
                "guest_svn": parsed.inner.guest_svn,
                "vmpl": parsed.inner.vmpl,
                "verified": false,
                "note": "stub verification - cryptographic validation not yet implemented"
            }),
        })
    }

    /// Generate QEMU command-line arguments for launching an SEV-SNP VM.
    fn qemu_args(&self, config: &VmConfig) -> Vec<String> {
        sev_snp_qemu_args(config)
    }

    /// Parse raw bytes into a structured attestation report.
    fn parse_report(&self, raw: &[u8]) -> Result<AttestationReport> {
        let parsed = parse_sev_snp_report(raw)?;

        Ok(AttestationReport {
            tee_type: TeeType::SevSnp,
            data: raw.to_vec(),
            report_data: extract_report_data(&parsed),
            measurement: extract_measurement(&parsed).to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{TeeConfig, TeeType};
    use std::path::PathBuf;

    #[test]
    fn test_sev_snp_backend_tee_type() {
        let backend = SevSnpBackend::new("Milan");
        assert_eq!(backend.tee_type(), TeeType::SevSnp);
    }

    #[test]
    fn test_sev_snp_backend_product() {
        let backend = SevSnpBackend::new("Genoa");
        assert_eq!(backend.product, "Genoa");
    }

    #[test]
    fn test_sev_snp_backend_qemu_args() {
        let backend = SevSnpBackend::new("Milan");
        let config = VmConfig {
            vm_id: "test".to_string(),
            kernel: PathBuf::from("/boot/vmlinuz"),
            initrd: PathBuf::from("/boot/initrd.img"),
            rootfs: None,
            vcpus: 2,
            memory_mb: 2048,
            tee: TeeConfig {
                backend: TeeType::SevSnp,
                policy: Some("0x30000".to_string()),
            },
        };

        let args = backend.qemu_args(&config);
        assert!(!args.is_empty());
        assert!(args.iter().any(|a| a.contains("sev-snp-guest")));
        assert!(args.iter().any(|a| a.contains("2048M")));
    }

    #[test]
    fn test_parse_report_roundtrip() {
        use sev::firmware::guest::AttestationReport as SevAR;
        use sev::parser::Encoder;

        let backend = SevSnpBackend::new("Milan");

        // Create a valid report using the sev crate encoder
        let mut sev_report = SevAR::default();
        sev_report.version = 3;
        sev_report.report_data = [0x42; 64];
        sev_report.measurement = [0xAB; 48];
        sev_report.cpuid_fam_id = Some(0x19);
        sev_report.cpuid_mod_id = Some(0x01);
        sev_report.cpuid_step = Some(0x00);
        sev_report.chip_id[0] = 1;

        let mut buf = Vec::new();
        sev_report.encode(&mut buf, ()).expect("encode should succeed");

        let parsed = backend.parse_report(&buf).expect("parse should succeed");

        assert_eq!(parsed.tee_type, TeeType::SevSnp);
        assert_eq!(parsed.report_data, [0x42; 64]);
        assert_eq!(parsed.measurement, vec![0xAB; 48]);
        assert_eq!(parsed.data, buf);
    }

    #[test]
    fn test_parse_report_invalid_data() {
        let backend = SevSnpBackend::new("Milan");
        let result = backend.parse_report(&[0u8; 10]);
        assert!(result.is_err());
    }

    #[test]
    fn test_verify_report_stub() {
        use sev::firmware::guest::AttestationReport as SevAR;
        use sev::parser::Encoder;

        let backend = SevSnpBackend::new("Milan");

        // Create a valid report
        let mut sev_report = SevAR::default();
        sev_report.version = 3;
        sev_report.report_data = [0x42; 64];
        sev_report.measurement = [0xAB; 48];
        sev_report.cpuid_fam_id = Some(0x19);
        sev_report.cpuid_mod_id = Some(0x01);
        sev_report.cpuid_step = Some(0x00);
        sev_report.chip_id[0] = 1;

        let mut buf = Vec::new();
        sev_report.encode(&mut buf, ()).expect("encode should succeed");

        let report = backend.parse_report(&buf).unwrap();
        let result = backend.verify_report(&report).unwrap();

        assert!(result.valid);
        assert_eq!(result.tee_type, TeeType::SevSnp);
        assert_eq!(result.measurement, vec![0xAB; 48]);
        assert!(result.summary.contains("Milan"));
    }
}
