//! Tier 2 integration tests — require SEV-SNP hardware.
//!
//! These tests are marked `#[ignore]` and only run when explicitly enabled
//! via `cargo test -- --ignored` on a machine with AMD SEV-SNP support.

mod common;

#[tokio::test]
#[ignore] // Requires SEV-SNP hardware
async fn test_tls_attestation_valid() {
    // Boot VM, connect to it, verify TLS cert contains valid attestation
    todo!("Requires SEV-SNP hardware and built VM images")
}

#[tokio::test]
#[ignore] // Requires SEV-SNP hardware
async fn test_fresh_attestation_with_nonce() {
    // Boot VM, request /.well-known/attestation with nonce, verify report
    todo!("Requires SEV-SNP hardware and built VM images")
}

#[tokio::test]
#[ignore] // Requires SEV-SNP hardware
async fn test_cli_end_to_end() {
    // Boot VM, run aleph-attest-cli against it, verify output
    todo!("Requires SEV-SNP hardware and built VM images")
}

#[tokio::test]
#[ignore] // Requires SEV-SNP hardware
async fn test_vm_lifecycle_full() {
    // Create VM -> verify running -> delete -> verify gone
    todo!("Requires SEV-SNP hardware and built VM images")
}

#[tokio::test]
#[ignore] // Requires SEV-SNP hardware
async fn test_attestation_report_measurement() {
    // Boot VM, get attestation report, verify measurement matches expected
    todo!("Requires SEV-SNP hardware and built VM images")
}

/// Boot the compose runtime with a demo workload volume and verify the
/// fail-closed contract: the stack answers on 8443, and killing the stack
/// powers the VM off. Requires SEV-SNP hardware and `nix build
/// .#vm-compose-demo` artifacts; run via `cargo test -- --ignored` there.
#[tokio::test]
#[ignore]
async fn test_compose_runner_boot() {
    todo!("Requires SEV-SNP hardware and built VM images")
}
