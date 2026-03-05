use std::path::PathBuf;

use aleph_compute_proto::{
    AddPortForwardRequest, CreateVmRequest, DeleteVmRequest, DiskConfig, GetVmRequest,
    HealthRequest, ListPortForwardsRequest, ListVmsRequest, RemovePortForwardRequest, TeeConfig,
    compute_node_client::ComputeNodeClient,
};
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;

/// CLI for the aleph-cvm compute node.
///
/// Thin wrapper around the ComputeNode gRPC service. Outputs JSON to stdout.
#[derive(Parser)]
#[command(name = "aleph-cvm", version)]
struct Cli {
    /// Path to the compute-node gRPC Unix socket.
    #[arg(long, default_value = "/run/aleph-cvm/compute.sock")]
    socket: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check node health.
    Health,

    /// Create a new VM.
    CreateVm {
        /// Unique VM identifier.
        #[arg(long)]
        vm_id: String,

        /// Path to the kernel image.
        #[arg(long)]
        kernel: String,

        /// Path to the initrd.
        #[arg(long)]
        initrd: String,

        /// Disk specification as path:format:ro|rw (e.g. /data/rootfs.ext4:raw:ro).
        /// Can be specified multiple times.
        #[arg(long)]
        disk: Vec<String>,

        /// Number of vCPUs.
        #[arg(long, default_value_t = 1)]
        vcpus: u32,

        /// Memory in megabytes.
        #[arg(long, default_value_t = 512)]
        memory_mb: u32,

        /// TEE backend (sev-snp, tdx, nvidia-cc).
        #[arg(long)]
        tee_backend: Option<String>,

        /// Requested IPv6 address (empty = random from pool).
        #[arg(long, default_value = "")]
        ipv6_address: String,

        /// Requested IPv6 prefix length (0 = /128).
        #[arg(long, default_value_t = 0)]
        ipv6_prefix_len: u32,

        /// LUKS encrypted rootfs (user injects key via attest-agent).
        #[arg(long, default_value_t = false)]
        encrypted: bool,
    },

    /// Get information about a VM.
    GetVm {
        /// VM identifier.
        #[arg(long)]
        vm_id: String,
    },

    /// Delete a VM.
    DeleteVm {
        /// VM identifier.
        #[arg(long)]
        vm_id: String,
    },

    /// List all VMs.
    ListVms,

    /// Add a port forwarding rule.
    AddPortForward {
        /// VM identifier.
        #[arg(long)]
        vm_id: String,

        /// Port inside the VM.
        #[arg(long)]
        vm_port: u32,

        /// Port on the host (0 = auto-allocate).
        #[arg(long, default_value_t = 0)]
        host_port: u32,

        /// Protocol (tcp or udp).
        #[arg(long, default_value = "tcp")]
        protocol: String,
    },

    /// Remove a port forwarding rule.
    RemovePortForward {
        /// VM identifier.
        #[arg(long)]
        vm_id: String,

        /// Host port to remove.
        #[arg(long)]
        host_port: u32,

        /// Protocol (tcp or udp).
        #[arg(long, default_value = "tcp")]
        protocol: String,
    },

    /// List port forwarding rules for a VM.
    ListPortForwards {
        /// VM identifier (omit to list all).
        #[arg(long, default_value = "")]
        vm_id: String,
    },
}

async fn connect(socket_path: &std::path::Path) -> Result<ComputeNodeClient<Channel>> {
    let socket_path = socket_path.to_path_buf();

    let channel = Endpoint::try_from("http://[::]:0")?
        .connect_with_connector(service_fn(move |_: Uri| {
            let path = socket_path.clone();
            async move {
                let stream = tokio::net::UnixStream::connect(path).await?;
                Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
            }
        }))
        .await
        .context("failed to connect to compute-node socket")?;

    Ok(ComputeNodeClient::new(channel))
}

fn parse_disk(spec: &str) -> Result<DiskConfig> {
    let parts: Vec<&str> = spec.split(':').collect();
    let (path, format, readonly) = match parts.len() {
        1 => (parts[0], "raw", false),
        2 => (parts[0], parts[1], false),
        3 => (parts[0], parts[1], parts[2] == "ro"),
        _ => anyhow::bail!("invalid disk spec '{spec}': expected path[:format[:ro|rw]]"),
    };
    Ok(DiskConfig {
        path: path.to_string(),
        format: format.to_string(),
        readonly,
    })
}

/// Print a value as JSON to stdout.
fn print_json(value: &serde_json::Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut client = connect(&cli.socket).await?;

    match cli.command {
        Command::Health => {
            let resp = client
                .health(HealthRequest {})
                .await
                .context("Health RPC failed")?
                .into_inner();
            print_json(&serde_json::json!({
                "status": resp.status,
                "vmCount": resp.vm_count,
            }))?;
        }

        Command::CreateVm {
            vm_id,
            kernel,
            initrd,
            disk,
            vcpus,
            memory_mb,
            tee_backend,
            ipv6_address,
            ipv6_prefix_len,
            encrypted,
        } => {
            let disks: Vec<DiskConfig> = disk
                .iter()
                .map(|s| parse_disk(s))
                .collect::<Result<_>>()?;

            let tee = tee_backend.map(|backend| TeeConfig {
                backend,
                policy: String::new(),
            });

            let resp = client
                .create_vm(CreateVmRequest {
                    vm_id,
                    kernel,
                    initrd,
                    disks,
                    vcpus,
                    memory_mb,
                    tee,
                    ipv6_address,
                    ipv6_prefix_len,
                    encrypted,
                })
                .await
                .context("CreateVm RPC failed")?
                .into_inner();

            print_json(&serde_json::json!({
                "vmId": resp.vm_id,
                "status": resp.status,
                "ipv4": resp.ipv4,
                "ipv6": resp.ipv6,
                "teeBackend": resp.tee_backend,
                "uptimeSecs": resp.uptime_secs,
            }))?;
        }

        Command::GetVm { vm_id } => {
            let resp = client
                .get_vm(GetVmRequest { vm_id })
                .await
                .context("GetVm RPC failed")?
                .into_inner();

            print_json(&serde_json::json!({
                "vmId": resp.vm_id,
                "status": resp.status,
                "ipv4": resp.ipv4,
                "ipv6": resp.ipv6,
                "teeBackend": resp.tee_backend,
                "uptimeSecs": resp.uptime_secs,
            }))?;
        }

        Command::DeleteVm { vm_id } => {
            client
                .delete_vm(DeleteVmRequest { vm_id })
                .await
                .context("DeleteVm RPC failed")?;

            print_json(&serde_json::json!({}))?;
        }

        Command::ListVms => {
            let resp = client
                .list_vms(ListVmsRequest {})
                .await
                .context("ListVms RPC failed")?
                .into_inner();

            let vms: Vec<serde_json::Value> = resp
                .vms
                .iter()
                .map(|vm| {
                    serde_json::json!({
                        "vmId": vm.vm_id,
                        "status": vm.status,
                        "ipv4": vm.ipv4,
                        "ipv6": vm.ipv6,
                        "teeBackend": vm.tee_backend,
                        "uptimeSecs": vm.uptime_secs,
                    })
                })
                .collect();
            print_json(&serde_json::json!({ "vms": vms }))?;
        }

        Command::AddPortForward {
            vm_id,
            vm_port,
            host_port,
            protocol,
        } => {
            let resp = client
                .add_port_forward(AddPortForwardRequest {
                    vm_id,
                    host_port,
                    vm_port,
                    protocol,
                })
                .await
                .context("AddPortForward RPC failed")?
                .into_inner();

            print_json(&serde_json::json!({
                "vmId": resp.vm_id,
                "hostPort": resp.host_port,
                "vmPort": resp.vm_port,
                "protocol": resp.protocol,
            }))?;
        }

        Command::RemovePortForward {
            vm_id,
            host_port,
            protocol,
        } => {
            client
                .remove_port_forward(RemovePortForwardRequest {
                    vm_id,
                    host_port,
                    protocol,
                })
                .await
                .context("RemovePortForward RPC failed")?;

            print_json(&serde_json::json!({}))?;
        }

        Command::ListPortForwards { vm_id } => {
            let resp = client
                .list_port_forwards(ListPortForwardsRequest { vm_id })
                .await
                .context("ListPortForwards RPC failed")?
                .into_inner();

            let forwards: Vec<serde_json::Value> = resp
                .forwards
                .iter()
                .map(|fwd| {
                    serde_json::json!({
                        "vmId": fwd.vm_id,
                        "hostPort": fwd.host_port,
                        "vmPort": fwd.vm_port,
                        "protocol": fwd.protocol,
                    })
                })
                .collect();
            print_json(&serde_json::json!({ "forwards": forwards }))?;
        }
    }

    Ok(())
}
