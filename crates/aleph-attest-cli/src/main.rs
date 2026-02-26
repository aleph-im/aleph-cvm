mod client;
mod verify;

use anyhow::Result;
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
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    if cli.fresh_attest {
        // Layer 3: Fresh attestation with random nonce
        println!("Requesting fresh attestation from {}...", cli.url);

        let report = client::fresh_attestation(&cli.url, &cli.amd_product).await?;

        println!("Fresh attestation verified successfully!");
        println!("  TEE type:     {:?}", report.tee_type);
        println!("  Measurement:  {}", hex::encode(&report.measurement));
        println!("  Report data:  {}", hex::encode(&report.report_data));
    } else {
        // Layer 2: TLS-bound attestation verification
        println!("Making attested request to {}...", cli.url);

        let response = client::attested_request(&cli.url, &cli.amd_product).await?;

        println!("Attestation valid: {}", response.attestation_valid);
        println!("Summary:           {}", response.attestation_summary);
        println!("Measurement:       {}", hex::encode(&response.measurement));
        println!("HTTP status:       {}", response.status);
        println!("Response body:");
        println!("{}", response.body);
    }

    Ok(())
}
