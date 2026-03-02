use std::net::Ipv4Addr;

use anyhow::{Context, Result};
use tracing::info;

/// Ensure a bridge interface exists with the given IP address.
///
/// Idempotent: if the bridge already exists, ignores "already exists" errors.
pub async fn ensure_bridge(bridge: &str, ip: Ipv4Addr, prefix_len: u8) -> Result<()> {
    // Create bridge (ignore "already exists" errors)
    let _ = run_ip(&["link", "add", bridge, "type", "bridge"]).await;

    // Assign address (ignore "already assigned" errors)
    let addr = format!("{ip}/{prefix_len}");
    let _ = run_ip(&["addr", "add", &addr, "dev", bridge]).await;

    // Bring it up
    run_ip(&["link", "set", bridge, "up"])
        .await
        .with_context(|| format!("failed to bring up bridge {bridge}"))?;

    info!(bridge = %bridge, addr = %addr, "bridge ensured");
    Ok(())
}

/// Run an `ip` command and return an error if it fails.
async fn run_ip(args: &[&str]) -> Result<()> {
    let output = tokio::process::Command::new("ip")
        .args(args)
        .output()
        .await
        .context("failed to execute `ip` command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("`ip {}` failed: {}", args.join(" "), stderr.trim());
    }

    Ok(())
}
