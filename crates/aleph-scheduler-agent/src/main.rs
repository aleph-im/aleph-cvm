mod adapter;
mod aleph;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use actix_web::{App, HttpRequest, HttpResponse, HttpServer, web};
use anyhow::Context;
use clap::Parser;
use tokio::sync::RwLock;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;
use tracing::{error, info, warn};

use aleph_compute_proto::compute::compute_node_client::ComputeNodeClient;
use aleph_compute_proto::compute::{DeleteVmRequest, HealthRequest, ListVmsRequest};

use crate::adapter::AdapterConfig;
use crate::aleph::allocations::{self, Allocation};
use crate::aleph::messages::{ExecutableMessage, ItemHash};
use crate::aleph::volumes::VolumeCache;

#[derive(Parser)]
#[command(name = "aleph-scheduler-agent")]
#[command(about = "Adapter between the Aleph network and the compute node")]
struct Cli {
    /// Compute node gRPC socket path.
    #[arg(long, default_value = "/run/aleph-cvm/compute.sock")]
    compute_socket: PathBuf,

    /// Listen address for the allocation HTTP API.
    #[arg(long, default_value = "127.0.0.1:4021")]
    listen: String,

    /// Aleph connector URL for downloading volumes.
    #[arg(long, default_value = "https://official.aleph.cloud")]
    connector_url: String,

    /// Local cache directory for downloaded volumes.
    #[arg(long, default_value = "/var/cache/aleph-cvm")]
    cache_dir: PathBuf,

    /// Default kernel path for VMs.
    #[arg(long)]
    kernel: String,

    /// Default initrd path for VMs.
    #[arg(long)]
    initrd: String,

    /// Hex-encoded SHA-256 hash of the allocation auth token.
    #[arg(long, env = "ALLOCATION_TOKEN_HASH")]
    allocation_token_hash: Option<String>,
}

/// Shared application state.
struct AppState {
    compute_client: RwLock<ComputeNodeClient<Channel>>,
    volume_cache: VolumeCache,
    adapter_config: AdapterConfig,
    /// Currently known messages, keyed by item_hash.
    messages: RwLock<std::collections::HashMap<ItemHash, ExecutableMessage>>,
    /// Hex-encoded SHA-256 hash of the allocation auth token.
    allocation_token_hash: Option<[u8; 32]>,
}

/// Connect to the compute-node gRPC server over a Unix domain socket.
async fn connect_compute_node(
    socket_path: &std::path::Path,
) -> anyhow::Result<ComputeNodeClient<Channel>> {
    let socket_path = socket_path.to_path_buf();

    // tonic requires a valid URI even for UDS; the host is ignored.
    // Wrap tokio::net::UnixStream with hyper_util::rt::TokioIo so it
    // implements the hyper rt::Read + rt::Write traits.
    let channel = Endpoint::try_from("http://[::]:0")?
        .connect_with_connector(service_fn(move |_: Uri| {
            let path = socket_path.clone();
            async move {
                let stream = tokio::net::UnixStream::connect(path).await?;
                Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
            }
        }))
        .await?;

    Ok(ComputeNodeClient::new(channel))
}

// ─── HTTP handlers (allocation API) ─────────────────────────────────────────

/// POST /control/allocations — receive a new allocation from the scheduler.
async fn handle_allocation(
    req: HttpRequest,
    body: web::Bytes,
    state: web::Data<Arc<AppState>>,
) -> HttpResponse {
    // Verify signature if token is configured
    if let Some(ref token_hash) = state.allocation_token_hash {
        let signature = req
            .headers()
            .get("X-Auth-Signature")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        if !allocations::verify_allocation_signature(&body, signature, token_hash) {
            return HttpResponse::Unauthorized().json(serde_json::json!({
                "error": "invalid allocation signature"
            }));
        }
    }

    let allocation: Allocation = match serde_json::from_slice(&body) {
        Ok(a) => a,
        Err(e) => {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": format!("invalid allocation JSON: {e}")
            }));
        }
    };

    info!(
        persistent = allocation.persistent_vms.len(),
        instances = allocation.instances.len(),
        "received allocation"
    );

    // Get currently running VMs
    let running: HashSet<String> = {
        let mut client = state.compute_client.write().await;
        match client.list_vms(ListVmsRequest {}).await {
            Ok(resp) => resp.into_inner().vms.into_iter().map(|v| v.vm_id).collect(),
            Err(e) => {
                error!(error = %e, "failed to list VMs from compute node");
                return HttpResponse::InternalServerError().json(serde_json::json!({
                    "error": format!("compute node unreachable: {e}")
                }));
            }
        }
    };

    let actions = allocations::reconcile(&allocation, &running);

    let mut errors = Vec::new();

    // Stop VMs that are no longer allocated
    for vm_id in &actions.to_stop {
        let mut client = state.compute_client.write().await;
        if let Err(e) = client
            .delete_vm(DeleteVmRequest {
                vm_id: vm_id.clone(),
            })
            .await
        {
            warn!(vm_id = %vm_id, error = %e, "failed to stop VM");
            errors.push(format!("stop {vm_id}: {e}"));
        } else {
            info!(vm_id = %vm_id, "stopped VM");
        }
    }

    // Start VMs that are newly allocated
    for vm_hash in &actions.to_start {
        // Look up the message for this hash
        let msg = {
            let messages = state.messages.read().await;
            messages.get(vm_hash).cloned()
        };

        let Some(msg) = msg else {
            warn!(vm_hash = %vm_hash, "no message found for allocated VM; fetch from connector not yet implemented");
            errors.push(format!("start {vm_hash}: message not found"));
            continue;
        };

        match adapter::translate_message(&msg, &state.volume_cache, &state.adapter_config).await {
            Ok(create_req) => {
                let mut client = state.compute_client.write().await;
                match client.create_vm(create_req).await {
                    Ok(resp) => {
                        let vm_info = resp.into_inner();
                        info!(
                            vm_id = %vm_info.vm_id,
                            status = %vm_info.status,
                            ipv4 = %vm_info.ipv4,
                            "started VM"
                        );

                        // Set up port forwarding
                        for pf_req in adapter::translate_port_forwards(&msg) {
                            if let Err(e) = client.add_port_forward(pf_req).await {
                                warn!(vm_id = %vm_hash, error = %e, "failed to add port forward");
                            }
                        }
                    }
                    Err(e) => {
                        error!(vm_id = %vm_hash, error = %e, "failed to create VM");
                        errors.push(format!("start {vm_hash}: {e}"));
                    }
                }
            }
            Err(e) => {
                error!(vm_id = %vm_hash, error = %e, "failed to translate message");
                errors.push(format!("translate {vm_hash}: {e}"));
            }
        }
    }

    if errors.is_empty() {
        HttpResponse::Ok().json(serde_json::json!({
            "started": actions.to_start.len(),
            "stopped": actions.to_stop.len(),
            "unchanged": actions.unchanged.len(),
        }))
    } else {
        HttpResponse::MultiStatus().json(serde_json::json!({
            "started": actions.to_start.len() - errors.iter().filter(|e| e.starts_with("start")).count(),
            "stopped": actions.to_stop.len() - errors.iter().filter(|e| e.starts_with("stop")).count(),
            "unchanged": actions.unchanged.len(),
            "errors": errors,
        }))
    }
}

/// POST /control/messages — register an executable message.
///
/// The scheduler sends messages before allocations so the agent knows
/// how to translate VM hashes into CreateVmRequests.
async fn handle_message(
    body: web::Json<ExecutableMessage>,
    state: web::Data<Arc<AppState>>,
) -> HttpResponse {
    let msg = body.into_inner();
    let hash = msg.item_hash.clone();

    info!(
        item_hash = %hash,
        machine_type = ?msg.machine_type,
        vcpus = msg.resources.vcpus,
        memory = msg.resources.memory,
        "registered message"
    );

    state.messages.write().await.insert(hash.clone(), msg);

    HttpResponse::Ok().json(serde_json::json!({
        "item_hash": hash,
        "status": "registered"
    }))
}

/// GET /health — health check.
async fn health(state: web::Data<Arc<AppState>>) -> HttpResponse {
    // Check compute node connectivity
    let compute_ok = {
        let mut client = state.compute_client.write().await;
        client.health(HealthRequest {}).await.is_ok()
    };

    let message_count = state.messages.read().await.len();

    HttpResponse::Ok().json(serde_json::json!({
        "status": if compute_ok { "ok" } else { "degraded" },
        "compute_node": if compute_ok { "connected" } else { "unreachable" },
        "registered_messages": message_count,
    }))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    info!(socket = %cli.compute_socket.display(), "connecting to compute node");
    let compute_client = connect_compute_node(&cli.compute_socket).await?;
    info!("connected to compute node");

    let allocation_token_hash = cli
        .allocation_token_hash
        .as_deref()
        .map(|h| {
            let bytes = hex::decode(h).context("allocation-token-hash must be valid hex")?;
            let arr: [u8; 32] = bytes.try_into().map_err(|v: Vec<u8>| {
                anyhow::anyhow!(
                    "allocation-token-hash must be 32 bytes (SHA-256), got {}",
                    v.len()
                )
            })?;
            Ok::<[u8; 32], anyhow::Error>(arr)
        })
        .transpose()?;

    let state = Arc::new(AppState {
        compute_client: RwLock::new(compute_client),
        volume_cache: VolumeCache::new(cli.cache_dir, cli.connector_url),
        adapter_config: AdapterConfig {
            kernel_path: cli.kernel,
            initrd_path: cli.initrd,
        },
        messages: RwLock::new(std::collections::HashMap::new()),
        allocation_token_hash,
    });

    let listen = cli.listen.clone();
    info!(listen = %listen, "starting scheduler agent HTTP API");

    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(state.clone()))
            .route("/health", web::get().to(health))
            .route("/control/allocations", web::post().to(handle_allocation))
            .route("/control/messages", web::post().to(handle_message))
    })
    .bind(&listen)?
    .run()
    .await?;

    Ok(())
}
