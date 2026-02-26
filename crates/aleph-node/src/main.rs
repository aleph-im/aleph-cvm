mod api;
mod network;
mod qemu;
mod vm;

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;

use actix_web::{web, App, HttpServer};
use clap::Parser;
use tracing::info;

use aleph_tee::sev_snp::SevSnpBackend;

use crate::vm::VmManager;

#[derive(Parser)]
#[command(name = "aleph-node", about = "Aleph CVM Node — manages confidential QEMU VMs")]
struct Cli {
    /// Address and port to listen on.
    #[arg(long, default_value = "127.0.0.1:4020")]
    listen: String,

    /// Bridge interface name.
    #[arg(long, default_value = "br-aleph")]
    bridge: String,

    /// Gateway IP address for the VM network.
    #[arg(long, default_value = "10.0.100.1")]
    gateway_ip: Ipv4Addr,

    /// Runtime directory for VM state files.
    #[arg(long, default_value = "/run/aleph-cvm")]
    run_dir: PathBuf,

    /// AMD product name for SEV-SNP (e.g. Milan, Genoa, Turin).
    #[arg(long, default_value = "Genoa")]
    amd_product: String,
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();

    info!(
        listen = %cli.listen,
        bridge = %cli.bridge,
        gateway = %cli.gateway_ip,
        run_dir = %cli.run_dir.display(),
        amd_product = %cli.amd_product,
        "starting aleph-node"
    );

    // Ensure the bridge is set up
    network::ensure_bridge(&cli.bridge, cli.gateway_ip, 24)
        .await
        .expect("failed to ensure bridge");

    // Create the TEE backend
    let tee_backend = Arc::new(SevSnpBackend::new(&cli.amd_product));

    // Create the VM manager
    let manager = web::Data::new(VmManager::new(
        cli.run_dir,
        cli.bridge,
        cli.gateway_ip,
        tee_backend,
    ));

    info!(listen = %cli.listen, "HTTP server starting");

    HttpServer::new(move || {
        App::new()
            .app_data(manager.clone())
            .service(api::health::health)
            .service(api::vms::create_vm)
            .service(api::vms::get_vm)
            .service(api::vms::delete_vm)
    })
    .bind(&cli.listen)?
    .run()
    .await
}
