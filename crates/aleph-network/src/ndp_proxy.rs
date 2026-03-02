use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use ipnet::Ipv6Net;
use tokio::sync::Mutex;
use tracing::{info, warn};

/// Manages ndppd (NDP Proxy Daemon) configuration.
///
/// Port of aleph-vm's `NdpProxy`. Rebuilds `/etc/ndppd.conf` and restarts
/// the ndppd service each time a VM range is added or removed.
pub struct NdpProxy {
    host_interface: String,
    ranges: Arc<Mutex<HashMap<String, Ipv6Net>>>,
}

impl NdpProxy {
    pub fn new(host_interface: &str) -> Self {
        Self {
            host_interface: host_interface.to_string(),
            ranges: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Add an IPv6 range for a TAP interface and update ndppd config.
    pub async fn add_range(&self, tap_name: &str, range: Ipv6Net) {
        {
            let mut ranges = self.ranges.lock().await;
            ranges.insert(tap_name.to_string(), range);
        }
        self.update_config().await;
    }

    /// Remove an IPv6 range for a TAP interface and update ndppd config.
    pub async fn delete_range(&self, tap_name: &str) {
        {
            let mut ranges = self.ranges.lock().await;
            ranges.remove(tap_name);
        }
        self.update_config().await;
    }

    /// Rebuild /etc/ndppd.conf and restart ndppd.
    async fn update_config(&self) {
        let ranges = self.ranges.lock().await;

        let mut config = format!("proxy {} {{\n", self.host_interface);
        for (tap_name, range) in ranges.iter() {
            config.push_str(&format!(
                "    rule {} {{\n        iface {}\n    }}\n",
                range, tap_name
            ));
        }
        config.push_str("}\n");

        let conf_path = Path::new("/etc/ndppd.conf");
        if let Err(e) = tokio::fs::write(conf_path, &config).await {
            warn!(error = %e, "failed to write ndppd.conf");
            return;
        }

        info!("ndppd.conf updated with {} ranges", ranges.len());

        // Restart ndppd service
        match tokio::process::Command::new("systemctl")
            .args(["restart", "ndppd"])
            .output()
            .await
        {
            Ok(output) if output.status.success() => {
                info!("ndppd restarted");
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!(stderr = %stderr, "ndppd restart failed");
            }
            Err(e) => {
                warn!(error = %e, "failed to restart ndppd");
            }
        }
    }
}
