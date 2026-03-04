use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use ipnet::Ipv6Net;
use tracing::info;

use aleph_tee::sev_snp::SevSnpBackend;

use aleph_compute_node::grpc::ComputeNodeServer;
use aleph_compute_node::network;
use aleph_compute_node::vm::VmManager;

#[derive(Parser)]
#[command(name = "aleph-compute-node", about = "Aleph CVM Compute Node — manages confidential QEMU VMs")]
struct Cli {
    /// Path to the gRPC Unix domain socket.
    #[arg(long, default_value = "/run/aleph-cvm/compute.sock")]
    grpc_socket: PathBuf,

    /// Bridge interface name.
    #[arg(long, default_value = "br-aleph")]
    bridge: String,

    /// Gateway IP address for the VM network.
    #[arg(long, default_value = "10.0.100.1")]
    gateway_ip: Ipv4Addr,

    /// Runtime directory for VM state files.
    #[arg(long, default_value = "/run/aleph-cvm")]
    run_dir: PathBuf,

    /// Directory for persistent VM state files.
    #[arg(long, default_value = "/var/lib/aleph-cvm/vms")]
    state_dir: PathBuf,

    /// AMD product name for SEV-SNP (e.g. Milan, Genoa, Turin).
    #[arg(long, default_value = "Genoa")]
    amd_product: String,

    /// Directory for dnsmasq DHCP host reservation files.
    /// When set, the node writes per-VM files mapping MAC→IP so that
    /// dnsmasq assigns the expected IP via DHCP. Start dnsmasq with
    /// `--dhcp-hostsdir=<this path>`.
    #[arg(long)]
    dhcp_hostsdir: Option<PathBuf>,

    /// Path to the OVMF firmware binary for SEV-SNP VMs.
    #[arg(long)]
    ovmf_path: Option<String>,

    /// External network interface for NAT and port forwarding.
    /// Auto-detected from default route if not specified.
    #[arg(long)]
    external_interface: Option<String>,

    /// IPv6 pool for VM address allocation (e.g. 2001:db8::/48).
    /// When set, VMs get a /128 from this pool via DHCPv6.
    #[arg(long)]
    ipv6_pool: Option<Ipv6Net>,

    /// Enable NDP proxy for IPv6 (defaults to true when --ipv6-pool is set).
    #[arg(long)]
    use_ndp_proxy: Option<bool>,
}

/// Detect the default network interface from /proc/net/route.
fn detect_default_interface() -> Option<String> {
    let content = std::fs::read_to_string("/proc/net/route").ok()?;
    for line in content.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        // Destination 00000000 = default route
        if fields.len() >= 2 && fields[1] == "00000000" {
            return Some(fields[0].to_string());
        }
    }
    None
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();

    // Detect external interface
    let external_interface = cli
        .external_interface
        .unwrap_or_else(|| detect_default_interface().unwrap_or_else(|| "eth0".to_string()));

    let use_ndp_proxy = cli.use_ndp_proxy.unwrap_or(cli.ipv6_pool.is_some());

    info!(
        grpc_socket = %cli.grpc_socket.display(),
        bridge = %cli.bridge,
        gateway = %cli.gateway_ip,
        run_dir = %cli.run_dir.display(),
        state_dir = %cli.state_dir.display(),
        amd_product = %cli.amd_product,
        dhcp_hostsdir = ?cli.dhcp_hostsdir,
        ovmf_path = ?cli.ovmf_path,
        external_interface = %external_interface,
        ipv6_pool = ?cli.ipv6_pool,
        use_ndp_proxy = %use_ndp_proxy,
        "starting aleph-compute-node"
    );

    // Ensure the bridge is set up
    network::ensure_bridge(&cli.bridge, cli.gateway_ip, 24)
        .await
        .expect("failed to ensure bridge");

    // Create the TEE backend
    let mut backend = SevSnpBackend::new(&cli.amd_product);
    if let Some(ref path) = cli.ovmf_path {
        backend = backend.with_ovmf_path(path);
    }
    let tee_backend = Arc::new(backend);

    // Enable IPv6 forwarding if pool is configured
    if cli.ipv6_pool.is_some() {
        if let Err(e) = std::fs::write("/proc/sys/net/ipv6/conf/all/forwarding", "1") {
            tracing::warn!(error = %e, "failed to enable IPv6 forwarding");
        } else {
            info!("IPv6 forwarding enabled");
        }
    }

    // Create the VM manager
    let manager = Arc::new(VmManager::new(
        cli.run_dir.clone(),
        cli.state_dir.clone(),
        cli.bridge,
        cli.gateway_ip,
        tee_backend,
        cli.dhcp_hostsdir,
        external_interface,
        cli.ipv6_pool,
        use_ndp_proxy,
    ));

    // Initialize nftables supervisor chains
    manager.setup_nftables().expect("failed to initialize nftables");

    // Recover VMs from previous run
    if let Err(e) = manager.recover_vms().await {
        tracing::error!(error = %e, "failed to recover VMs from persisted state");
    }

    // Run gRPC server with graceful shutdown on SIGINT/SIGTERM
    let grpc_server = ComputeNodeServer::new(manager);

    info!(socket = %cli.grpc_socket.display(), "gRPC server starting");
    grpc_server.serve(&cli.grpc_socket).await?;

    info!("orchestrator shut down -- VMs continue running under systemd");

    Ok(())
}
