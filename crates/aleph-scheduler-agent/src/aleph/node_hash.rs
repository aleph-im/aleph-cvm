//! Node hash self-discovery.
//!
//! Discovers the node's CRN hash from the Aleph network by querying
//! corechan-operation messages for the operator's address and matching
//! the node's public URL.

use std::path::Path;

use aleph_sdk::client::{AlephPostClient, PostFilter, PostV0};
use aleph_types::chain::Address;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tracing::{debug, info, warn};

/// Result of a node hash discovery attempt.
#[derive(Debug, Clone)]
pub struct DiscoveredNode {
    pub hash: String,
    pub name: String,
    pub address: String,
}

// ── Corechan-operation content types ────────────────────────────────────────

/// Inner content of a `corechan-operation` POST message.
#[derive(Debug, Deserialize)]
struct CorechanContent {
    action: Option<String>,
    details: Option<CrnDetails>,
}

#[derive(Debug, Deserialize)]
struct CrnDetails {
    name: Option<String>,
    address: Option<String>,
}

// ── URL normalization ───────────────────────────────────────────────────────

/// Normalize a URL for comparison: lowercase, strip trailing slashes.
fn normalize_url(url: &str) -> String {
    url.trim_end_matches('/').to_lowercase()
}

// ── Cache and PID file operations ───────────────────────────────────────────

/// Read the cached node hash from `<state_dir>/node-hash`.
pub fn read_cached_hash(state_dir: &Path) -> Option<String> {
    let path = state_dir.join("node-hash");
    let hash = std::fs::read_to_string(&path).ok()?.trim().to_string();
    if hash.is_empty() { None } else { Some(hash) }
}

/// Write the node hash to `<state_dir>/node-hash`.
pub fn write_cached_hash(state_dir: &Path, hash: &str) -> Result<()> {
    std::fs::create_dir_all(state_dir).context("creating state directory")?;
    let path = state_dir.join("node-hash");
    std::fs::write(&path, hash).context("writing node-hash cache")?;
    debug!(path = %path.display(), "cached node hash");
    Ok(())
}

/// Write the current process PID to `<state_dir>/scheduler-agent.pid`.
pub fn write_pid_file(state_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(state_dir).context("creating state directory")?;
    let path = state_dir.join("scheduler-agent.pid");
    std::fs::write(&path, std::process::id().to_string()).context("writing PID file")?;
    debug!(path = %path.display(), pid = std::process::id(), "wrote PID file");
    Ok(())
}

/// Remove the PID file on shutdown.
pub fn remove_pid_file(state_dir: &Path) {
    let path = state_dir.join("scheduler-agent.pid");
    if std::fs::remove_file(&path).is_ok() {
        debug!(path = %path.display(), "removed PID file");
    }
}

/// Read the PID from `<state_dir>/scheduler-agent.pid`.
pub fn read_pid(state_dir: &Path) -> Option<u32> {
    let path = state_dir.join("scheduler-agent.pid");
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Send SIGHUP to the process with the given PID.
pub fn send_sighup(pid: u32) -> Result<()> {
    let status = std::process::Command::new("kill")
        .args(["-HUP", &pid.to_string()])
        .status()
        .context("failed to run kill command")?;
    if !status.success() {
        bail!("kill -HUP {pid} exited with {status}");
    }
    Ok(())
}

// ── Validation ──────────────────────────────────────────────────────────────

/// Validate that a string looks like a valid node hash (64 hex chars).
pub fn validate_node_hash(hash: &str) -> Result<()> {
    if hash.len() != 64 {
        bail!("node hash must be 64 hex characters, got {}", hash.len());
    }
    if !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("node hash must be hexadecimal");
    }
    Ok(())
}

// ── Discovery ───────────────────────────────────────────────────────────────

/// Extract CRN details from a PostV0, returning None if it's not a
/// `create-resource-node` operation.
fn parse_crn_post(post: &PostV0) -> Option<CorechanContent> {
    let content: CorechanContent = post.content_as().ok()?;
    if content.action.as_deref() == Some("create-resource-node") {
        Some(content)
    } else {
        None
    }
}

/// Query the Aleph posts API to discover this node's CRN hash.
///
/// Looks for `create-resource-node` operations by `owner_address` whose
/// `details.address` matches `https://<domain_name>`.
///
/// Returns `Ok(Some(node))` if exactly one match is found, `Ok(None)` if zero
/// matches, and logs a warning if multiple matches are found.
pub async fn discover_node_hash(
    client: &impl AlephPostClient,
    owner_address: &str,
    domain_name: &str,
) -> Result<Option<DiscoveredNode>> {
    let filter = PostFilter {
        addresses: Some(vec![Address::from(owner_address.to_string())]),
        post_types: Some(vec!["corechan-operation".to_string()]),
        pagination: Some(200),
        ..Default::default()
    };

    debug!(owner = %owner_address, "querying Aleph API for corechan operations");

    let response = client
        .get_posts_v0(&filter)
        .await
        .context("querying Aleph posts API")?;

    if response.pagination_total > 200 {
        warn!(
            total = response.pagination_total,
            "operator has >200 corechan operations; results may be incomplete"
        );
    }

    let expected_url = normalize_url(&format!("https://{domain_name}"));

    // Parse CRN registrations from the posts.
    let create_ops: Vec<(&PostV0, CorechanContent)> = response
        .posts
        .iter()
        .filter_map(|p| parse_crn_post(p).map(|c| (p, c)))
        .collect();

    let matches: Vec<&(&PostV0, CorechanContent)> = create_ops
        .iter()
        .filter(|(_, content)| {
            content
                .details
                .as_ref()
                .and_then(|d| d.address.as_deref())
                .is_some_and(|a| normalize_url(a) == expected_url)
        })
        .collect();

    match matches.len() {
        0 => {
            info!(
                owner = %owner_address,
                expected_url = %expected_url,
                total_crn_registrations = create_ops.len(),
                "no matching CRN registration found"
            );
            for (post, content) in &create_ops {
                let addr = content
                    .details
                    .as_ref()
                    .and_then(|d| d.address.as_deref())
                    .unwrap_or("(none)");
                debug!(
                    hash = %post.original_item_hash,
                    address = %addr,
                    "  registered CRN (no URL match)"
                );
            }
            Ok(None)
        }
        1 => {
            let (post, content) = matches[0];
            let details = content.details.as_ref();
            let node = DiscoveredNode {
                hash: post.original_item_hash.to_string(),
                name: details
                    .and_then(|d| d.name.as_deref())
                    .unwrap_or("unnamed")
                    .to_string(),
                address: details
                    .and_then(|d| d.address.as_deref())
                    .unwrap_or("")
                    .to_string(),
            };
            info!(
                node_hash = %node.hash,
                name = %node.name,
                address = %node.address,
                "discovered node hash"
            );
            Ok(Some(node))
        }
        n => {
            warn!(
                count = n,
                owner = %owner_address,
                "multiple CRN registrations match URL; \
                 use --node-hash or `set-node-hash` to disambiguate:"
            );
            for (post, content) in matches {
                let details = content.details.as_ref();
                let name = details.and_then(|d| d.name.as_deref()).unwrap_or("unnamed");
                let addr = details
                    .and_then(|d| d.address.as_deref())
                    .unwrap_or("(none)");
                warn!(
                    hash = %post.original_item_hash,
                    name = %name,
                    address = %addr,
                    "  candidate"
                );
            }
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_url() {
        assert_eq!(
            normalize_url("https://My-Node.Example.COM/"),
            "https://my-node.example.com"
        );
        assert_eq!(
            normalize_url("https://node.example.com"),
            "https://node.example.com"
        );
        assert_eq!(normalize_url("https://NODE.COM///"), "https://node.com");
    }

    #[test]
    fn test_validate_node_hash() {
        // Valid
        let valid = "b93eaba554318bd074819477e48147bb7bf4121bb771a6074022b0bf412cacc0";
        assert!(validate_node_hash(valid).is_ok());

        // Too short
        assert!(validate_node_hash("abc123").is_err());

        // Not hex
        let bad = "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz";
        assert!(validate_node_hash(bad).is_err());

        // Too long
        let long = "b93eaba554318bd074819477e48147bb7bf4121bb771a6074022b0bf412cacc0aa";
        assert!(validate_node_hash(long).is_err());
    }

    #[test]
    fn test_cache_read_write() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path();

        // No cache file yet
        assert!(read_cached_hash(state_dir).is_none());

        // Write and read back
        let hash = "b93eaba554318bd074819477e48147bb7bf4121bb771a6074022b0bf412cacc0";
        write_cached_hash(state_dir, hash).unwrap();
        assert_eq!(read_cached_hash(state_dir).unwrap(), hash);

        // Overwrite
        let hash2 = "0000000000000000000000000000000000000000000000000000000000000001";
        write_cached_hash(state_dir, hash2).unwrap();
        assert_eq!(read_cached_hash(state_dir).unwrap(), hash2);
    }

    #[test]
    fn test_pid_file() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path();

        write_pid_file(state_dir).unwrap();
        let pid = read_pid(state_dir).unwrap();
        assert_eq!(pid, std::process::id());

        remove_pid_file(state_dir);
        assert!(read_pid(state_dir).is_none());
    }
}
