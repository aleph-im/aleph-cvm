mod common;

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;

use aleph_tee::traits::TeeBackend;
use aleph_tee::types::{AttestationReport, TeeType, VerificationResult, VmConfig};

use aleph_compute_node::vm::VmManager;

/// A mock TEE backend that returns dummy data without touching hardware.
struct MockTeeBackend;

impl TeeBackend for MockTeeBackend {
    fn tee_type(&self) -> TeeType {
        TeeType::SevSnp
    }

    fn get_report(&self, report_data: &[u8; 64]) -> anyhow::Result<AttestationReport> {
        Ok(AttestationReport {
            tee_type: TeeType::SevSnp,
            data: vec![0u8; 64],
            report_data: *report_data,
            measurement: vec![0xAB; 48],
        })
    }

    fn verify_report(&self, report: &AttestationReport) -> anyhow::Result<VerificationResult> {
        Ok(VerificationResult {
            valid: true,
            tee_type: report.tee_type,
            summary: "mock verification".to_string(),
            measurement: report.measurement.clone(),
            details: serde_json::json!({"mock": true}),
        })
    }

    fn qemu_args(&self, _config: &VmConfig) -> Vec<String> {
        vec!["-machine".to_string(), "q35".to_string()]
    }

    fn parse_report(&self, raw: &[u8]) -> anyhow::Result<AttestationReport> {
        Ok(AttestationReport {
            tee_type: TeeType::SevSnp,
            data: raw.to_vec(),
            report_data: [0u8; 64],
            measurement: vec![0xAB; 48],
        })
    }
}

/// Create a VmManager backed by the mock TEE backend.
fn mock_manager() -> Arc<VmManager> {
    let backend: Arc<dyn TeeBackend> = Arc::new(MockTeeBackend);
    Arc::new(VmManager::new(
        PathBuf::from("/tmp/aleph-cvm-test"),
        PathBuf::from("/tmp/aleph-cvm-test/state"),
        "br-test".to_string(),
        Ipv4Addr::new(10, 0, 200, 1),
        backend,
        None,
        "eth0".to_string(),
        None,  // ipv6_pool
        false, // use_ndp_proxy
    ))
}

// ─── VmManager direct tests ────────────────────────────────────────────────

#[tokio::test]
async fn test_get_nonexistent_vm() {
    let manager = mock_manager();
    let result = manager.get_vm("nonexistent").await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[tokio::test]
async fn test_delete_nonexistent_vm() {
    let manager = mock_manager();
    let result = manager.delete_vm("nonexistent").await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[tokio::test]
async fn test_list_vms_empty() {
    let manager = mock_manager();
    let vms = manager.list_vms().await;
    assert!(vms.is_empty());
}

// ─── Test common utility ───────────────────────────────────────────────────

#[tokio::test]
async fn test_wait_for_port_timeout() {
    use std::time::Duration;

    // Connect to a port that is (almost certainly) not listening
    let result =
        common::wait_for_port("127.0.0.1:19999", Duration::from_millis(200)).await;
    assert!(!result, "should time out when nothing is listening");
}
