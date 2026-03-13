//! CRN configuration reported to the scheduler.

use serde::Serialize;

/// Node capabilities and configuration, served at GET /status/config.
#[derive(Debug, Clone, Serialize)]
pub struct CrnConfig {
    pub enable_confidential_computing: bool,
    pub ipv6_support: bool,
    pub gpu_support: bool,
    pub payment_receiver_address: Option<String>,
    pub version: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialization_full() {
        let config = CrnConfig {
            enable_confidential_computing: true,
            ipv6_support: true,
            gpu_support: false,
            payment_receiver_address: Some("0x1234abcd".to_string()),
            version: "0.1.0".to_string(),
        };
        let json = serde_json::to_value(&config).unwrap();
        assert_eq!(json["enable_confidential_computing"], true);
        assert_eq!(json["ipv6_support"], true);
        assert_eq!(json["gpu_support"], false);
        assert_eq!(json["payment_receiver_address"], "0x1234abcd");
        assert_eq!(json["version"], "0.1.0");
    }

    #[test]
    fn test_serialization_no_payment() {
        let config = CrnConfig {
            enable_confidential_computing: false,
            ipv6_support: false,
            gpu_support: false,
            payment_receiver_address: None,
            version: "0.1.0".to_string(),
        };
        let json = serde_json::to_value(&config).unwrap();
        assert!(json["payment_receiver_address"].is_null());
    }
}
