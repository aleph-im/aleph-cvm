use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use actix_web::{App, HttpRequest, HttpResponse, HttpServer, web};
use anyhow::Context;
use clap::{Parser, Subcommand};
use tokio::sync::RwLock;
use tonic::transport::Channel;
use tracing::{error, info, warn};

use aleph_compute_proto::compute::compute_node_client::ComputeNodeClient;
use aleph_compute_proto::compute::{DeleteVmRequest, HealthRequest, ListVmsRequest};

use aleph_scheduler_agent::adapter::{self, AdapterConfig};
use aleph_scheduler_agent::aleph::allocations::{self, Allocation};
use aleph_scheduler_agent::aleph::messages::{ExecutableMessage, ItemHash};
use aleph_scheduler_agent::aleph::node_hash;
use aleph_scheduler_agent::aleph::volumes::VolumeCache;
use aleph_scheduler_agent::client::connect_compute_node;
use aleph_scheduler_agent::status::config::{
    ComputingConfig, CrnConfig, DebugConfig, NetworkingConfig, PaymentConfig, ReferencesConfig,
    SecurityConfig, default_available_payments,
};
use aleph_scheduler_agent::status::executions::map_executions;
use aleph_scheduler_agent::status::usage::collect_usage;

#[derive(Parser)]
#[command(name = "aleph-scheduler-agent")]
#[command(about = "Adapter between the Aleph network and the compute node")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the scheduler agent.
    Run(Box<RunArgs>),

    /// Store a node hash in the cache and notify the running process via SIGHUP.
    SetNodeHash(SetNodeHashArgs),
}

/// Arguments for `run` — starts the scheduler agent server.
#[derive(clap::Args)]
struct RunArgs {
    /// Directory for persistent state (node-hash cache, PID file).
    #[arg(long, default_value = "/var/lib/aleph-cvm/scheduler")]
    state_dir: PathBuf,

    /// Compute node gRPC socket path.
    #[arg(long, default_value = "/run/aleph-cvm/compute.sock")]
    compute_socket: PathBuf,

    /// Listen address for the allocation HTTP API.
    #[arg(long, default_value = "127.0.0.1:4021")]
    listen: String,

    /// Aleph API URL for node hash discovery and volume downloads.
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

    // ── Node identity ────────────────────────────────────────────────────
    /// Operator's Ethereum address (for auto-discovering the node hash).
    #[arg(long, env = "OWNER_ADDRESS")]
    owner_address: Option<String>,

    /// Node hash override (skips auto-discovery).
    #[arg(long, env = "NODE_HASH")]
    node_hash: Option<String>,

    /// Node's public domain name (e.g. "my-crn.example.com").
    #[arg(long, env = "DOMAIN_NAME")]
    domain_name: Option<String>,

    // ── Node capabilities ────────────────────────────────────────────────
    /// Enable confidential computing (SEV-SNP).
    #[arg(long, env = "ALEPH_VM_ENABLE_CONFIDENTIAL_COMPUTING")]
    enable_confidential_computing: bool,

    /// IPv6 address pool (non-empty value indicates IPv6 support).
    #[arg(long, env = "ALEPH_VM_IPV6_ADDRESS_POOL")]
    ipv6_address_pool: Option<String>,

    /// Payment receiver Ethereum address.
    #[arg(long, env = "ALEPH_VM_PAYMENT_RECEIVER_ADDRESS")]
    payment_receiver_address: Option<String>,
}

/// Arguments for `set-node-hash` — writes a hash to the cache and signals the running agent.
#[derive(clap::Args)]
struct SetNodeHashArgs {
    /// The CRN node hash (64-character hex string).
    hash: String,

    /// Directory for persistent state (must match the running agent's --state-dir).
    #[arg(long, default_value = "/var/lib/aleph-cvm/scheduler")]
    state_dir: PathBuf,
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
    /// Discovered node hash (may be None if not yet resolved).
    node_hash: Arc<RwLock<Option<node_hash::NodeHash>>>,
    /// CRN configuration for scheduler polling.
    crn_config: CrnConfig,
    /// Cache directory path (for disk usage reporting).
    cache_dir: PathBuf,
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
    let current_node_hash = state.node_hash.read().await.as_ref().map(|h| h.to_string());

    HttpResponse::Ok().json(serde_json::json!({
        "status": if compute_ok { "ok" } else { "degraded" },
        "compute_node": if compute_ok { "connected" } else { "unreachable" },
        "registered_messages": message_count,
        "node_hash": current_node_hash,
    }))
}

/// GET /about/usage/system — host resource usage.
async fn get_system_usage(state: web::Data<Arc<AppState>>) -> HttpResponse {
    match collect_usage(&state.cache_dir).await {
        Ok(usage) => HttpResponse::Ok().json(usage),
        Err(e) => {
            error!("Failed to collect system usage: {e:#}");
            HttpResponse::InternalServerError().finish()
        }
    }
}

/// GET /about/executions/list — running VMs with networking info.
async fn get_executions(state: web::Data<Arc<AppState>>) -> HttpResponse {
    let mut client = state.compute_client.write().await;
    match client.list_vms(ListVmsRequest {}).await {
        Ok(resp) => {
            let executions = map_executions(&resp.into_inner().vms);
            HttpResponse::Ok().json(executions)
        }
        Err(e) => {
            error!("Failed to list VMs: {e:#}");
            HttpResponse::InternalServerError().finish()
        }
    }
}

/// GET /status/config — node capabilities.
async fn get_config(state: web::Data<Arc<AppState>>) -> HttpResponse {
    let mut config = state.crn_config.clone();
    config.node_hash = state.node_hash.read().await.as_ref().map(|h| h.to_string());
    HttpResponse::Ok().json(config)
}

// ─── Node hash discovery background task ────────────────────────────────────

/// Background loop that discovers the node hash, retrying every 5 minutes until found.
async fn node_hash_discovery_loop(
    node_hash_state: Arc<RwLock<Option<node_hash::NodeHash>>>,
    aleph_client: aleph_sdk::client::AlephClient,
    owner_address: String,
    domain_name: String,
    state_dir: PathBuf,
) {
    let retry_interval = Duration::from_secs(300); // 5 minutes

    loop {
        // Stop once we have a hash — it won't change.
        if node_hash_state.read().await.is_some() {
            info!("node hash resolved, stopping auto-discovery");
            return;
        }

        match node_hash::discover_node_hash(&aleph_client, &owner_address, &domain_name).await {
            Ok(Some(discovered)) => {
                info!(
                    node_hash = %discovered.hash,
                    name = %discovered.name,
                    "discovered node hash"
                );
                if let Err(e) = node_hash::write_cached_hash(&state_dir, &discovered.hash) {
                    warn!(error = %e, "failed to cache node hash");
                }
                *node_hash_state.write().await = Some(discovered.hash);
                return;
            }
            Ok(None) => {
                tokio::time::sleep(retry_interval).await;
            }
            Err(e) => {
                warn!(error = %e, "node hash discovery failed, will retry");
                tokio::time::sleep(retry_interval).await;
            }
        }
    }
}

/// Spawn a SIGHUP handler that re-reads the cached node hash on signal.
fn spawn_sighup_handler(hash_state: Arc<RwLock<Option<node_hash::NodeHash>>>, state_dir: PathBuf) {
    tokio::spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sighup = signal(SignalKind::hangup()).expect("failed to register SIGHUP handler");

        loop {
            sighup.recv().await;
            info!("received SIGHUP, re-reading node hash cache");
            match node_hash::read_cached_hash(&state_dir) {
                Some(hash) => {
                    let current = hash_state.read().await;
                    if current.as_ref() != Some(&hash) {
                        drop(current);
                        info!(node_hash = %hash, "node hash updated from cache");
                        *hash_state.write().await = Some(hash);
                    }
                }
                None => {
                    warn!("SIGHUP received but no cached node hash found");
                }
            }
        }
    });
}

/// Parse and validate the allocation auth token hash.
fn parse_allocation_token(hex_hash: &Option<String>) -> anyhow::Result<Option<[u8; 32]>> {
    hex_hash
        .as_deref()
        .map(|h| {
            let bytes = hex::decode(h).context("allocation-token-hash must be valid hex")?;
            let arr: [u8; 32] = bytes.try_into().map_err(|v: Vec<u8>| {
                anyhow::anyhow!(
                    "allocation-token-hash must be 32 bytes (SHA-256), got {}",
                    v.len()
                )
            })?;
            Ok(arr)
        })
        .transpose()
}

// ─── Subcommand: set-node-hash ──────────────────────────────────────────────

fn cmd_set_node_hash(args: SetNodeHashArgs) -> anyhow::Result<()> {
    let hash = node_hash::NodeHash::parse(&args.hash)?;
    node_hash::write_cached_hash(&args.state_dir, &hash)?;
    println!(
        "Node hash written to {}/node-hash",
        args.state_dir.display()
    );

    if let Some(pid) = node_hash::read_pid(&args.state_dir) {
        match node_hash::send_sighup(pid) {
            Ok(()) => println!("Sent SIGHUP to running process (PID {pid})"),
            Err(e) => eprintln!("Warning: could not signal running process: {e}"),
        }
    } else {
        println!("No running scheduler agent found (no PID file)");
    }
    Ok(())
}

// ─── Subcommand: run ────────────────────────────────────────────────────────

async fn cmd_run(args: RunArgs) -> anyhow::Result<()> {
    // Write PID file
    node_hash::write_pid_file(&args.state_dir)?;
    let state_dir_cleanup = args.state_dir.clone();

    info!(socket = %args.compute_socket.display(), "connecting to compute node");
    let compute_client = connect_compute_node(&args.compute_socket).await?;
    info!("connected to compute node");

    let allocation_token_hash = parse_allocation_token(&args.allocation_token_hash)?;

    // ── Resolve initial node hash ────────────────────────────────────────

    let initial_hash = node_hash::resolve_initial_hash(args.node_hash.as_deref(), &args.state_dir)?;
    let node_hash_state = Arc::new(RwLock::new(initial_hash));

    // ── Start background discovery (if auto-discovery is configured) ─────

    if args.node_hash.is_none() {
        match (&args.owner_address, &args.domain_name) {
            (Some(owner), Some(domain)) => {
                let api_url =
                    url::Url::parse(&args.connector_url).context("invalid --connector-url")?;
                let aleph_client = aleph_sdk::client::AlephClient::new(api_url);

                info!(
                    owner_address = %owner,
                    domain_name = %domain,
                    "starting node hash auto-discovery"
                );

                tokio::spawn(node_hash_discovery_loop(
                    node_hash_state.clone(),
                    aleph_client,
                    owner.clone(),
                    domain.clone(),
                    args.state_dir.clone(),
                ));
            }
            (None, None) => {
                warn!("no node identity configured (--node-hash, --owner-address, or cached hash)");
            }
            (None, Some(_)) => {
                warn!("--owner-address required for auto-discovery (--domain-name is set)");
            }
            (Some(_), None) => {
                warn!("--domain-name required for auto-discovery (--owner-address is set)");
            }
        }
    }

    // ── SIGHUP handler: re-read cached node hash ─────────────────────────

    spawn_sighup_handler(node_hash_state.clone(), args.state_dir.clone());

    // ── Build app state and start HTTP server ────────────────────────────

    let cache_dir = args.cache_dir.clone();
    let ipv6_pool = args.ipv6_address_pool.clone().unwrap_or_default();
    let ipv6_enabled = !ipv6_pool.is_empty();
    let crn_config = CrnConfig {
        domain_name: args.domain_name.clone().unwrap_or_default(),
        // Populated dynamically at request time from the discovery task.
        node_hash: None,
        version: env!("CARGO_PKG_VERSION").to_string(),
        references: ReferencesConfig {
            api_server: args.connector_url.clone(),
            check_fastapi_vm_id: String::new(),
            connector_url: args.connector_url.clone(),
        },
        security: SecurityConfig {
            use_jailer: false,
            print_system_logs: false,
            watch_for_updates: true,
            allow_vm_networking: true,
            use_developer_ssh_keys: false,
        },
        networking: NetworkingConfig {
            ipv6_address_pool: ipv6_pool,
            ipv6_allocation_policy: "IPv6AllocationPolicy.static".to_string(),
            ipv6_subnet_prefix: 124,
            ipv6_forwarding_enabled: ipv6_enabled,
            use_ndp_proxy: ipv6_enabled,
        },
        debug: DebugConfig {
            sentry_dsn_configured: false,
            debug_asyncio: false,
            execution_log_enabled: false,
        },
        payment: PaymentConfig {
            payment_receiver_address: args.payment_receiver_address.clone(),
            available_payments: default_available_payments(),
            payment_monitor_interval: 60.0,
        },
        computing: ComputingConfig {
            enable_qemu_support: true,
            instance_default_hypervisor: "qemu".to_string(),
            enable_confidential_computing: args.enable_confidential_computing,
            enable_gpu_support: false,
        },
    };

    let state = Arc::new(AppState {
        compute_client: RwLock::new(compute_client),
        volume_cache: VolumeCache::new(args.cache_dir, args.connector_url),
        adapter_config: AdapterConfig {
            kernel_path: args.kernel,
            initrd_path: args.initrd,
        },
        messages: RwLock::new(std::collections::HashMap::new()),
        allocation_token_hash,
        node_hash: node_hash_state,
        crn_config,
        cache_dir,
    });

    let listen = args.listen.clone();
    info!(listen = %listen, "starting scheduler agent HTTP API");

    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(state.clone()))
            .route("/health", web::get().to(health))
            .route("/control/allocations", web::post().to(handle_allocation))
            .route("/control/messages", web::post().to(handle_message))
            .route("/about/usage/system", web::get().to(get_system_usage))
            .route("/about/executions/list", web::get().to(get_executions))
            .route("/status/config", web::get().to(get_config))
    })
    .bind(&listen)?
    .run()
    .await?;

    // Cleanup on shutdown
    node_hash::remove_pid_file(&state_dir_cleanup);

    Ok(())
}

// ─── Entry point ────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Run(args) => cmd_run(*args).await,
        Command::SetNodeHash(args) => cmd_set_node_hash(args),
    }
}
