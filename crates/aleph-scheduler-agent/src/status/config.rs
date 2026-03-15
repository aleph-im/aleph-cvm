//! CRN configuration reported to the scheduler.
//!
//! Matches the response schema of aleph-vm's `GET /status/config`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkingConfig {
    #[serde(rename = "IPV6_ADDRESS_POOL")]
    pub ipv6_address_pool: Option<String>,
    #[serde(rename = "IPV6_ALLOCATION_POLICY")]
    pub ipv6_allocation_policy: Option<String>,
    #[serde(rename = "IPV6_SUBNET_PREFIX")]
    pub ipv6_subnet_prefix: Option<u16>,
    #[serde(rename = "IPV6_FORWARDING_ENABLED")]
    pub ipv6_forwarding_enabled: Option<bool>,
    #[serde(rename = "USE_NDP_PROXY")]
    pub use_ndp_proxy: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaymentConfig {
    #[serde(rename = "PAYMENT_RECEIVER_ADDRESS")]
    pub payment_receiver_address: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComputingConfig {
    #[serde(rename = "ENABLE_GPU_SUPPORT", default)]
    pub enable_gpu_support: bool,
    #[serde(rename = "ENABLE_CONFIDENTIAL_COMPUTING", default)]
    pub enable_confidential_computing: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrnConfig {
    pub version: String,
    pub networking: NetworkingConfig,
    #[serde(default)]
    pub payment: Option<PaymentConfig>,
    #[serde(default)]
    pub computing: Option<ComputingConfig>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialization_full() {
        let config = CrnConfig {
            version: "0.1.0".to_string(),
            networking: NetworkingConfig {
                ipv6_address_pool: Some("2001:db8::/32".to_string()),
                ipv6_allocation_policy: Some("auto".to_string()),
                ipv6_subnet_prefix: Some(64),
                ipv6_forwarding_enabled: Some(true),
                use_ndp_proxy: Some(false),
            },
            payment: Some(PaymentConfig {
                payment_receiver_address: Some("0x1234abcd".to_string()),
            }),
            computing: Some(ComputingConfig {
                enable_gpu_support: false,
                enable_confidential_computing: true,
            }),
        };
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["version"], "0.1.0");
        assert_eq!(json["networking"]["IPV6_ADDRESS_POOL"], "2001:db8::/32");
        assert_eq!(json["computing"]["ENABLE_CONFIDENTIAL_COMPUTING"], true);
        assert_eq!(json["payment"]["PAYMENT_RECEIVER_ADDRESS"], "0x1234abcd");
    }

    #[test]
    fn test_serialization_minimal() {
        let config = CrnConfig {
            version: "0.1.0".to_string(),
            networking: NetworkingConfig {
                ipv6_address_pool: None,
                ipv6_allocation_policy: None,
                ipv6_subnet_prefix: None,
                ipv6_forwarding_enabled: None,
                use_ndp_proxy: None,
            },
            payment: None,
            computing: None,
        };
        let json = serde_json::to_value(&config).unwrap();
        assert!(json["payment"].is_null());
        assert!(json["computing"].is_null());
    }

    #[test]
    fn test_roundtrip() {
        let config = CrnConfig {
            version: "0.1.0".to_string(),
            networking: NetworkingConfig {
                ipv6_address_pool: Some("fd00::/64".to_string()),
                ipv6_allocation_policy: None,
                ipv6_subnet_prefix: None,
                ipv6_forwarding_enabled: None,
                use_ndp_proxy: None,
            },
            payment: Some(PaymentConfig {
                payment_receiver_address: None,
            }),
            computing: Some(ComputingConfig {
                enable_gpu_support: false,
                enable_confidential_computing: false,
            }),
        };
        let json_str = serde_json::to_string(&config).unwrap();
        let deserialized: CrnConfig = serde_json::from_str(&json_str).unwrap();
        assert_eq!(
            deserialized.networking.ipv6_address_pool,
            Some("fd00::/64".to_string())
        );
    }
}
