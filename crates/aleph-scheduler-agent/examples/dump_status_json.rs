//! Emits sample JSON for each of the three scheduler-polled endpoints so we can
//! byte-compare against a live aleph-vm CRN. Not used in production.
//!
//! Usage:
//!   cargo run --example dump_status_json -p aleph-scheduler-agent -- <out_dir>

use std::path::PathBuf;

use aleph_compute_proto::compute::VmInfo;
use aleph_scheduler_agent::status::config::{
    ComputingConfig, CrnConfig, DebugConfig, NetworkingConfig, PaymentConfig, ReferencesConfig,
    SecurityConfig, default_available_payments,
};
use aleph_scheduler_agent::status::executions::map_executions;
use aleph_scheduler_agent::status::usage::collect_usage;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let out: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/crn-samples"));
    std::fs::create_dir_all(&out)?;

    // /about/usage/system — runs against the host running this binary
    let usage = collect_usage(&PathBuf::from("/tmp")).await?;
    std::fs::write(
        out.join("ours-usage.json"),
        serde_json::to_string_pretty(&usage)?,
    )?;

    // /about/executions/list — synthetic, two VMs
    let vms = vec![
        VmInfo {
            vm_id: "2a0110af56f9341c13b184bd3f7a991c0c5b959801c2d216827c637045f8dde9".to_string(),
            status: "running".to_string(),
            ipv4: "10.0.200.5".to_string(),
            ipv6: "2604:4300:a:6a:3:2a01:10af:56f0/124".to_string(),
            tee_backend: "SevSnp".to_string(),
            uptime_secs: 3600,
            numa_node: 0,
        },
        VmInfo {
            vm_id: "13d9539bf3831ea9171270c2373032090049a492c1c5c2702cd2e668af2105b0".to_string(),
            status: "running".to_string(),
            ipv4: "10.0.200.6".to_string(),
            ipv6: "2604:4300:a:6a:3:13d9:539b:f380/124".to_string(),
            tee_backend: "None".to_string(),
            uptime_secs: 120,
            numa_node: 1,
        },
    ];
    let executions = map_executions(&vms);
    std::fs::write(
        out.join("ours-executions.json"),
        serde_json::to_string_pretty(&executions)?,
    )?;

    // /status/config — matches the construction in main.rs
    let config = CrnConfig {
        domain_name: "demo-crn.example.com".to_string(),
        node_hash: Some("01249788857fc7fc1b3ad95cc9caab6c1c25aace8bee0298313282c9f2373e90".to_string()),
        version: env!("CARGO_PKG_VERSION").to_string(),
        references: ReferencesConfig {
            api_server: "https://official.aleph.cloud".to_string(),
            check_fastapi_vm_id: String::new(),
            connector_url: "https://official.aleph.cloud".to_string(),
        },
        security: SecurityConfig {
            use_jailer: false,
            print_system_logs: false,
            watch_for_updates: true,
            allow_vm_networking: true,
            use_developer_ssh_keys: false,
        },
        networking: NetworkingConfig {
            ipv6_address_pool: "2604:4300:a:6a::/64".to_string(),
            ipv6_allocation_policy: "IPv6AllocationPolicy.static".to_string(),
            ipv6_subnet_prefix: 124,
            ipv6_forwarding_enabled: true,
            use_ndp_proxy: true,
        },
        debug: DebugConfig {
            sentry_dsn_configured: false,
            debug_asyncio: false,
            execution_log_enabled: false,
        },
        payment: PaymentConfig {
            payment_receiver_address: Some("0xd5aa3c5Fe47eDA35b40c00280Af7457729bf883C".to_string()),
            available_payments: default_available_payments(),
            payment_monitor_interval: 60.0,
        },
        computing: ComputingConfig {
            enable_qemu_support: true,
            instance_default_hypervisor: "qemu".to_string(),
            enable_confidential_computing: true,
            enable_gpu_support: false,
        },
    };
    std::fs::write(
        out.join("ours-config.json"),
        serde_json::to_string_pretty(&config)?,
    )?;

    println!("Wrote samples to {}", out.display());
    Ok(())
}
