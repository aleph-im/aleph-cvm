mod common;

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;

use actix_web::{test, web, App};
use serde_json::Value;

use aleph_tee::traits::TeeBackend;
use aleph_tee::types::{AttestationReport, TeeType, VerificationResult, VmConfig};

use aleph_node::api;
use aleph_node::vm::VmManager;

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

/// Create a `web::Data<VmManager>` backed by the mock TEE backend.
fn mock_manager() -> web::Data<VmManager> {
    let backend: Arc<dyn TeeBackend> = Arc::new(MockTeeBackend);
    web::Data::new(VmManager::new(
        PathBuf::from("/tmp/aleph-cvm-test"),
        "br-test".to_string(),
        Ipv4Addr::new(10, 0, 200, 1),
        backend,
    ))
}

// ─── Health endpoint ───────────────────────────────────────────────────────

#[actix_web::test]
async fn test_health_endpoint() {
    let app = test::init_service(
        App::new().service(api::health::health),
    )
    .await;

    let req = test::TestRequest::get().uri("/health").to_request();
    let resp = test::call_service(&app, req).await;

    assert_eq!(resp.status(), 200);

    let body: Value = test::read_body_json(resp).await;
    assert_eq!(body["status"], "ok");
}

// ─── VM endpoints — error cases ────────────────────────────────────────────

#[actix_web::test]
async fn test_create_vm_invalid_paths() {
    let manager = mock_manager();
    let app = test::init_service(
        App::new()
            .app_data(manager)
            .service(api::vms::create_vm),
    )
    .await;

    // POST /vms with nonexistent image paths — should fail during VM creation
    // (TAP creation or QEMU spawn will fail, returning 500).
    let payload = serde_json::json!({
        "vm_id": "test-invalid",
        "kernel": "/nonexistent/vmlinuz",
        "initrd": "/nonexistent/initrd.img",
        "rootfs": null,
        "vcpus": 1,
        "memory_mb": 512,
        "tee": {
            "backend": "sev-snp",
            "policy": null
        }
    });

    let req = test::TestRequest::post()
        .uri("/vms")
        .set_json(&payload)
        .to_request();

    let resp = test::call_service(&app, req).await;

    // The create will fail because we can't create TAP interfaces or spawn QEMU
    // in the test environment. The handler returns 500 on error.
    assert_eq!(resp.status(), 500);

    let body: Value = test::read_body_json(resp).await;
    assert!(body["error"].is_string(), "error field should be a string");
}

#[actix_web::test]
async fn test_get_nonexistent_vm() {
    let manager = mock_manager();
    let app = test::init_service(
        App::new()
            .app_data(manager)
            .service(api::vms::get_vm),
    )
    .await;

    let req = test::TestRequest::get()
        .uri("/vms/nonexistent")
        .to_request();

    let resp = test::call_service(&app, req).await;

    assert_eq!(resp.status(), 404);

    let body: Value = test::read_body_json(resp).await;
    assert!(body["error"].as_str().unwrap().contains("not found"));
}

#[actix_web::test]
async fn test_delete_nonexistent_vm() {
    let manager = mock_manager();
    let app = test::init_service(
        App::new()
            .app_data(manager)
            .service(api::vms::delete_vm),
    )
    .await;

    let req = test::TestRequest::delete()
        .uri("/vms/nonexistent")
        .to_request();

    let resp = test::call_service(&app, req).await;

    assert_eq!(resp.status(), 404);

    let body: Value = test::read_body_json(resp).await;
    assert!(body["error"].as_str().unwrap().contains("not found"));
}

// ─── Combined app — all routes together ────────────────────────────────────

#[actix_web::test]
async fn test_full_app_health_with_manager() {
    let manager = mock_manager();
    let app = test::init_service(
        App::new()
            .app_data(manager)
            .service(api::health::health)
            .service(api::vms::create_vm)
            .service(api::vms::get_vm)
            .service(api::vms::delete_vm),
    )
    .await;

    // Health should work even with the full app
    let req = test::TestRequest::get().uri("/health").to_request();
    let resp = test::call_service(&app, req).await;
    assert_eq!(resp.status(), 200);
}

#[actix_web::test]
async fn test_create_vm_invalid_json() {
    let manager = mock_manager();
    let app = test::init_service(
        App::new()
            .app_data(manager)
            .service(api::vms::create_vm),
    )
    .await;

    // Send malformed JSON — actix-web should return 400
    let req = test::TestRequest::post()
        .uri("/vms")
        .insert_header(("content-type", "application/json"))
        .set_payload(r#"{"not": "a valid VmConfig"}"#)
        .to_request();

    let resp = test::call_service(&app, req).await;

    // actix-web returns 400 for deserialization errors
    assert_eq!(resp.status(), 400);
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
