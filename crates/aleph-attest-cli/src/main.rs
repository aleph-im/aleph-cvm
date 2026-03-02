mod client;
mod verify;

use anyhow::{Context, Result};
use clap::Parser;

#[derive(Parser)]
#[command(name = "aleph-attest-cli", about = "Client-side TEE attestation verifier")]
struct Cli {
    /// URL to connect to
    #[arg(long)]
    url: String,

    /// Request fresh attestation with a random nonce (Layer 3)
    #[arg(long)]
    fresh_attest: bool,

    /// AMD product name for certificate chain validation
    #[arg(long, default_value = "Genoa")]
    amd_product: String,

    /// Expected VM measurement (launch digest) as a hex string.
    /// If provided, the TLS handshake will be aborted if the attestation
    /// report's measurement doesn't match — the client will never send
    /// application data to a VM running unexpected code.
    #[arg(long)]
    expected_measurement: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls crypto provider");
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    let expected_measurement = cli
        .expected_measurement
        .as_deref()
        .map(|hex_str| hex::decode(hex_str).context("invalid hex in --expected-measurement"))
        .transpose()?;

    if cli.fresh_attest {
        // Layer 3: Fresh attestation with random nonce
        println!("Requesting fresh attestation from {}...", cli.url);

        let report = client::fresh_attestation(
            &cli.url,
            &cli.amd_product,
            expected_measurement.as_deref(),
        )
        .await?;

        println!("Fresh attestation verified successfully!");
        println!("  TEE type:     {:?}", report.tee_type);
        println!("  Measurement:  {}", hex::encode(&report.measurement));
        println!("  Report data:  {}", hex::encode(report.report_data));
    } else {
        // Layer 2: TLS-bound attestation verification
        println!("Making attested request to {}...", cli.url);

        let response = client::attested_request(
            &cli.url,
            &cli.amd_product,
            expected_measurement.as_deref(),
        )
        .await?;

        println!("Attestation valid: {}", response.attestation_valid);
        println!("Summary:           {}", response.attestation_summary);
        println!("Measurement:       {}", hex::encode(&response.measurement));
        println!("HTTP status:       {}", response.status);
        println!("Response body:");
        println!("{}", response.body);
    }

    Ok(())
}
