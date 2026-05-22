//! CRN configuration reported to the scheduler.
//!
//! Mirrors the response schema of aleph-vm 1.12's `GET /status/config`.
//! Fields that don't apply to our CVM platform (e.g. `USE_JAILER` is firecracker-only)
//! are emitted with sensible static defaults so the wire format stays compatible.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferencesConfig {
    #[serde(rename = "API_SERVER")]
    pub api_server: String,
    #[serde(rename = "CHECK_FASTAPI_VM_ID")]
    pub check_fastapi_vm_id: String,
    #[serde(rename = "CONNECTOR_URL")]
    pub connector_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityConfig {
    #[serde(rename = "USE_JAILER")]
    pub use_jailer: bool,
    #[serde(rename = "PRINT_SYSTEM_LOGS")]
    pub print_system_logs: bool,
    #[serde(rename = "WATCH_FOR_UPDATES")]
    pub watch_for_updates: bool,
    #[serde(rename = "ALLOW_VM_NETWORKING")]
    pub allow_vm_networking: bool,
    #[serde(rename = "USE_DEVELOPER_SSH_KEYS")]
    pub use_developer_ssh_keys: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkingConfig {
    #[serde(rename = "IPV6_ADDRESS_POOL")]
    pub ipv6_address_pool: String,
    #[serde(rename = "IPV6_ALLOCATION_POLICY")]
    pub ipv6_allocation_policy: String,
    #[serde(rename = "IPV6_SUBNET_PREFIX")]
    pub ipv6_subnet_prefix: u16,
    #[serde(rename = "IPV6_FORWARDING_ENABLED")]
    pub ipv6_forwarding_enabled: bool,
    #[serde(rename = "USE_NDP_PROXY")]
    pub use_ndp_proxy: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugConfig {
    #[serde(rename = "SENTRY_DSN_CONFIGURED")]
    pub sentry_dsn_configured: bool,
    #[serde(rename = "DEBUG_ASYNCIO")]
    pub debug_asyncio: bool,
    #[serde(rename = "EXECUTION_LOG_ENABLED")]
    pub execution_log_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentConfig {
    #[serde(rename = "PAYMENT_RECEIVER_ADDRESS")]
    pub payment_receiver_address: Option<String>,
    #[serde(rename = "AVAILABLE_PAYMENTS")]
    pub available_payments: BTreeMap<String, serde_json::Value>,
    #[serde(rename = "PAYMENT_MONITOR_INTERVAL")]
    pub payment_monitor_interval: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputingConfig {
    #[serde(rename = "ENABLE_QEMU_SUPPORT")]
    pub enable_qemu_support: bool,
    #[serde(rename = "INSTANCE_DEFAULT_HYPERVISOR")]
    pub instance_default_hypervisor: String,
    #[serde(rename = "ENABLE_CONFIDENTIAL_COMPUTING")]
    pub enable_confidential_computing: bool,
    #[serde(rename = "ENABLE_GPU_SUPPORT")]
    pub enable_gpu_support: bool,
}

/// Default `AVAILABLE_PAYMENTS` map matching aleph-vm 1.12's hardcoded `STREAM_CHAINS`
/// filtered to `active=true` mainnet chains. Mirrors the two chains every aleph-vm CRN
/// emits unless the operator has overridden them.
pub fn default_available_payments() -> BTreeMap<String, serde_json::Value> {
    let mut map = BTreeMap::new();
    map.insert(
        "Chain.AVAX".to_string(),
        serde_json::json!({
            "chain_id": 43114,
            "rpc": "https://api.avax.network/ext/bc/C/rpc",
            "standard_token": null,
            "super_token": "0xc0Fbc4967259786C743361a5885ef49380473dCF",
            "testnet": false,
            "active": true,
        }),
    );
    map.insert(
        "Chain.BASE".to_string(),
        serde_json::json!({
            "chain_id": 8453,
            "rpc": "https://base-mainnet.public.blastapi.io",
            "standard_token": null,
            "super_token": "0xc0Fbc4967259786C743361a5885ef49380473dCF",
            "testnet": false,
            "active": true,
        }),
    );
    map
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrnConfig {
    #[serde(rename = "DOMAIN_NAME")]
    pub domain_name: String,
    /// Resolved CRN node hash. `null` until the background discovery task succeeds.
    pub node_hash: Option<String>,
    pub version: String,
    pub references: ReferencesConfig,
    pub security: SecurityConfig,
    pub networking: NetworkingConfig,
    pub debug: DebugConfig,
    pub payment: PaymentConfig,
    pub computing: ComputingConfig,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_config() -> CrnConfig {
        CrnConfig {
            domain_name: "crn.example.com".to_string(),
            node_hash: Some("01249788857fc7fc1b3ad95cc9caab6c1c25aace8bee0298313282c9f2373e90".to_string()),
            version: "0.1.0".to_string(),
            references: ReferencesConfig {
                api_server: "https://official.aleph.cloud".to_string(),
                check_fastapi_vm_id: "d2b74aa29898457bde0560e47f7cdd4e77287e9f1f7a1456161d2fd7d5c855d7".to_string(),
                connector_url: "http://localhost:4021/".to_string(),
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
                available_payments: BTreeMap::new(),
                payment_monitor_interval: 60.0,
            },
            computing: ComputingConfig {
                enable_qemu_support: true,
                instance_default_hypervisor: "qemu".to_string(),
                enable_confidential_computing: false,
                enable_gpu_support: false,
            },
        }
    }

    #[test]
    fn test_all_top_level_keys_present() {
        let json = serde_json::to_value(sample_config()).unwrap();
        for key in [
            "DOMAIN_NAME",
            "node_hash",
            "version",
            "references",
            "security",
            "networking",
            "debug",
            "payment",
            "computing",
        ] {
            assert!(
                json.get(key).is_some(),
                "missing top-level key `{key}` in /status/config response"
            );
        }
    }

    #[test]
    fn test_uppercase_keys_in_nested_sections() {
        let json = serde_json::to_value(sample_config()).unwrap();
        assert!(json["references"]["API_SERVER"].is_string());
        assert!(json["security"]["USE_JAILER"].is_boolean());
        assert!(json["networking"]["IPV6_ADDRESS_POOL"].is_string());
        assert!(json["debug"]["SENTRY_DSN_CONFIGURED"].is_boolean());
        assert!(json["payment"]["PAYMENT_RECEIVER_ADDRESS"].is_string());
        assert!(json["payment"]["AVAILABLE_PAYMENTS"].is_object());
        assert!(json["payment"]["PAYMENT_MONITOR_INTERVAL"].is_number());
        assert!(json["computing"]["ENABLE_QEMU_SUPPORT"].is_boolean());
        assert!(json["computing"]["INSTANCE_DEFAULT_HYPERVISOR"].is_string());
    }

    #[test]
    fn test_node_hash_null_when_unresolved() {
        let mut cfg = sample_config();
        cfg.node_hash = None;
        let json = serde_json::to_value(cfg).unwrap();
        assert!(json["node_hash"].is_null());
    }

    #[test]
    fn test_roundtrip() {
        let cfg = sample_config();
        let s = serde_json::to_string(&cfg).unwrap();
        let _: CrnConfig = serde_json::from_str(&s).unwrap();
    }
}
