//! Node hash self-discovery.
//!
//! Discovers the node's CRN hash from the Aleph network by querying
//! corechan-operation messages for the operator's address and matching
//! the node's public URL.

use std::path::Path;

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

// ── Aleph posts API response types ──────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct PostsResponse {
    posts: Vec<Post>,
    pagination_total: u64,
}

#[derive(Debug, Deserialize)]
struct Post {
    /// Hash of the original post (permanent CRN identifier).
    #[serde(default)]
    original_item_hash: Option<String>,
    /// Hash of this version of the post.
    item_hash: String,
    /// Inner content of the corechan-operation.
    content: PostContent,
}

impl Post {
    /// The permanent CRN identifier (original hash, or item_hash if no amend).
    fn crn_hash(&self) -> &str {
        self.original_item_hash
            .as_deref()
            .unwrap_or(&self.item_hash)
    }
}

#[derive(Debug, Deserialize)]
struct PostContent {
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

/// Query the Aleph posts API to discover this node's CRN hash.
///
/// Looks for `create-resource-node` operations by `owner_address` whose
/// `details.address` matches `https://<domain_name>`.
///
/// Returns `Ok(Some(node))` if exactly one match is found, `Ok(None)` if zero
/// matches, and logs a warning if multiple matches are found.
pub async fn discover_node_hash(
    client: &reqwest::Client,
    api_url: &str,
    owner_address: &str,
    domain_name: &str,
) -> Result<Option<DiscoveredNode>> {
    let url = format!(
        "{}/api/v0/posts.json?addresses={}&types=corechan-operation&pagination=200",
        api_url.trim_end_matches('/'),
        owner_address,
    );

    debug!(url = %url, "querying Aleph API for corechan operations");

    let response: PostsResponse = client
        .get(&url)
        .send()
        .await
        .context("querying Aleph posts API")?
        .error_for_status()
        .context("Aleph posts API returned error")?
        .json()
        .await
        .context("parsing Aleph posts response")?;

    if response.pagination_total > 200 {
        warn!(
            total = response.pagination_total,
            "operator has >200 corechan operations; results may be incomplete"
        );
    }

    let expected_url = normalize_url(&format!("https://{domain_name}"));

    // Filter for create-resource-node operations with matching URL.
    let create_ops: Vec<&Post> = response
        .posts
        .iter()
        .filter(|p| p.content.action.as_deref() == Some("create-resource-node"))
        .collect();

    let matches: Vec<&Post> = create_ops
        .iter()
        .copied()
        .filter(|p| {
            p.content
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
            if !create_ops.is_empty() {
                for op in &create_ops {
                    let addr = op
                        .content
                        .details
                        .as_ref()
                        .and_then(|d| d.address.as_deref())
                        .unwrap_or("(none)");
                    debug!(hash = %op.crn_hash(), address = %addr, "  registered CRN (no URL match)");
                }
            }
            Ok(None)
        }
        1 => {
            let post = matches[0];
            let details = post.content.details.as_ref();
            let node = DiscoveredNode {
                hash: post.crn_hash().to_string(),
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
            for m in &matches {
                let details = m.content.details.as_ref();
                let name = details.and_then(|d| d.name.as_deref()).unwrap_or("unnamed");
                let addr = details
                    .and_then(|d| d.address.as_deref())
                    .unwrap_or("(none)");
                warn!(hash = %m.crn_hash(), name = %name, address = %addr, "  candidate");
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

    #[test]
    fn test_post_crn_hash() {
        // With original_item_hash
        let post = Post {
            original_item_hash: Some("original".to_string()),
            item_hash: "current".to_string(),
            content: PostContent {
                action: None,
                details: None,
            },
        };
        assert_eq!(post.crn_hash(), "original");

        // Without original_item_hash
        let post = Post {
            original_item_hash: None,
            item_hash: "current".to_string(),
            content: PostContent {
                action: None,
                details: None,
            },
        };
        assert_eq!(post.crn_hash(), "current");
    }

    #[test]
    fn test_deserialize_posts_response() {
        let json = r#"{
            "posts": [
                {
                    "item_hash": "abc123",
                    "original_item_hash": "abc123",
                    "content": {
                        "action": "create-resource-node",
                        "tags": ["create-resource-node", "mainnet"],
                        "details": {
                            "name": "my-node",
                            "address": "https://my-node.example.com/",
                            "type": "compute"
                        }
                    }
                },
                {
                    "item_hash": "def456",
                    "content": {
                        "action": "stake-split",
                        "tags": ["stake-split", "mainnet"]
                    }
                }
            ],
            "pagination_page": 1,
            "pagination_total": 2,
            "pagination_per_page": 200,
            "pagination_item": "posts"
        }"#;

        let resp: PostsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.posts.len(), 2);
        assert_eq!(resp.pagination_total, 2);
        assert_eq!(resp.posts[0].crn_hash(), "abc123");
        assert_eq!(
            resp.posts[0].content.action.as_deref(),
            Some("create-resource-node")
        );
        assert_eq!(
            resp.posts[0]
                .content
                .details
                .as_ref()
                .unwrap()
                .address
                .as_deref(),
            Some("https://my-node.example.com/")
        );
        // Second post has no details (stake operation)
        assert!(resp.posts[1].content.details.is_none());
    }
}
