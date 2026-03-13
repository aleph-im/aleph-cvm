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

// ── NodeHash newtype ─────────────────────────────────────────────────────────

/// A validated CRN node hash (64 hex-character [`ItemHash`](super::messages::ItemHash)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeHash(String);

impl NodeHash {
    /// Parse and validate a string as a node hash (64 hex characters).
    pub fn parse(hash: impl Into<String>) -> Result<Self> {
        let hash = hash.into();
        if hash.len() != 64 {
            bail!("node hash must be 64 hex characters, got {}", hash.len());
        }
        if !hash.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("node hash must be hexadecimal");
        }
        Ok(Self(hash))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for NodeHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Result of a node hash discovery attempt.
#[derive(Debug, Clone)]
pub struct DiscoveredNode {
    pub hash: NodeHash,
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

/// Read the raw cached hash string from `<state_dir>/node-hash`.
fn read_cached_hash_raw(state_dir: &Path) -> Option<String> {
    let path = state_dir.join("node-hash");
    let hash = std::fs::read_to_string(&path).ok()?.trim().to_string();
    if hash.is_empty() { None } else { Some(hash) }
}

/// Read and validate the cached node hash from `<state_dir>/node-hash`.
///
/// Returns `None` if the file doesn't exist, is empty, or contains an invalid hash.
/// Logs a warning if the cached value is corrupted.
pub fn read_cached_hash(state_dir: &Path) -> Option<NodeHash> {
    let raw = read_cached_hash_raw(state_dir)?;
    match NodeHash::parse(&raw) {
        Ok(hash) => Some(hash),
        Err(e) => {
            warn!(error = %e, "ignoring corrupted cached node hash");
            None
        }
    }
}

/// Write the node hash to `<state_dir>/node-hash`.
pub fn write_cached_hash(state_dir: &Path, hash: &NodeHash) -> Result<()> {
    std::fs::create_dir_all(state_dir).context("creating state directory")?;
    let path = state_dir.join("node-hash");
    std::fs::write(&path, hash.as_str()).context("writing node-hash cache")?;
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

// ── Resolution ──────────────────────────────────────────────────────────────

/// Resolve the initial node hash from an explicit CLI flag or cache.
///
/// Priority: explicit `--node-hash` flag > cached file > `None`.
pub fn resolve_initial_hash(
    explicit_hash: Option<&str>,
    state_dir: &Path,
) -> Result<Option<NodeHash>> {
    if let Some(hash) = explicit_hash {
        let node_hash = NodeHash::parse(hash)?;
        info!(node_hash = %node_hash, "using node hash from --node-hash flag");
        return Ok(Some(node_hash));
    }

    if let Some(cached) = read_cached_hash(state_dir) {
        info!(node_hash = %cached, "using cached node hash");
        return Ok(Some(cached));
    }

    Ok(None)
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
            let hash = NodeHash::parse(post.original_item_hash.to_string())
                .context("invalid item hash from API")?;
            let node = DiscoveredNode {
                hash,
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

    use aleph_sdk::client::{GetPostsV0Response, GetPostsV1Response, MessageError};
    use aleph_types::{chain::Chain, item_hash::ItemHash, timestamp::Timestamp};

    /// Mock Aleph post client that returns a canned response.
    struct MockPostClient {
        posts: Vec<PostV0>,
    }

    impl MockPostClient {
        fn new(posts: Vec<PostV0>) -> Self {
            Self { posts }
        }
    }

    impl AlephPostClient for MockPostClient {
        async fn get_posts_v0(
            &self,
            _filter: &PostFilter,
        ) -> Result<GetPostsV0Response, MessageError> {
            let posts = self.posts.clone();
            let total = posts.len() as u32;
            Ok(GetPostsV0Response {
                posts,
                pagination_per_page: 200,
                pagination_page: 1,
                pagination_total: total,
            })
        }

        async fn get_posts_v1(
            &self,
            _filter: &PostFilter,
        ) -> Result<GetPostsV1Response, MessageError> {
            unimplemented!("v1 not used")
        }
    }

    /// Build a PostV0 that looks like a `create-resource-node` corechan-operation.
    fn make_crn_post(hash: &str, name: &str, address: &str) -> PostV0 {
        let item_hash: ItemHash = hash.parse().expect("valid test hash");
        let sender = Address::from("0xdeadbeef".to_string());
        PostV0 {
            chain: Chain::Ethereum,
            item_hash: item_hash.clone(),
            sender: sender.clone(),
            post_type: "corechan-operation".to_string(),
            channel: None,
            confirmed: false,
            content: serde_json::json!({
                "action": "create-resource-node",
                "details": {
                    "name": name,
                    "address": address,
                }
            }),
            time: Timestamp::from(1700000000.0),
            confirmations: vec![],
            original_item_hash: item_hash.clone(),
            original_type: None,
            hash: item_hash,
            address: sender,
            reference: None,
        }
    }

    /// Build a PostV0 with a non-CRN action (should be ignored).
    fn make_non_crn_post(hash: &str) -> PostV0 {
        let item_hash: ItemHash = hash.parse().expect("valid test hash");
        let sender = Address::from("0xdeadbeef".to_string());
        PostV0 {
            chain: Chain::Ethereum,
            item_hash: item_hash.clone(),
            sender: sender.clone(),
            post_type: "corechan-operation".to_string(),
            channel: None,
            confirmed: false,
            content: serde_json::json!({
                "action": "link-resource-node",
                "details": { "ccn_hash": "abc123" }
            }),
            time: Timestamp::from(1700000000.0),
            confirmations: vec![],
            original_item_hash: item_hash.clone(),
            original_type: None,
            hash: item_hash,
            address: sender,
            reference: None,
        }
    }

    const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const HASH_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const HASH_C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    #[tokio::test]
    async fn test_discover_single_match() {
        let client = MockPostClient::new(vec![
            make_crn_post(HASH_A, "my-node", "https://my-node.example.com"),
            make_crn_post(HASH_B, "other-node", "https://other.example.com"),
        ]);

        let result = discover_node_hash(&client, "0xowner", "my-node.example.com")
            .await
            .unwrap();

        let node = result.expect("should find a match");
        assert_eq!(node.hash.as_str(), HASH_A);
        assert_eq!(node.name, "my-node");
        assert_eq!(node.address, "https://my-node.example.com");
    }

    #[tokio::test]
    async fn test_discover_no_match() {
        let client = MockPostClient::new(vec![
            make_crn_post(HASH_A, "node-a", "https://a.example.com"),
            make_crn_post(HASH_B, "node-b", "https://b.example.com"),
        ]);

        let result = discover_node_hash(&client, "0xowner", "not-registered.example.com")
            .await
            .unwrap();

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_discover_multiple_matches_returns_none() {
        let client = MockPostClient::new(vec![
            make_crn_post(HASH_A, "node-1", "https://same.example.com"),
            make_crn_post(HASH_B, "node-2", "https://same.example.com"),
        ]);

        let result = discover_node_hash(&client, "0xowner", "same.example.com")
            .await
            .unwrap();

        assert!(result.is_none(), "multiple matches should return None");
    }

    #[tokio::test]
    async fn test_discover_url_normalization() {
        let client = MockPostClient::new(vec![make_crn_post(
            HASH_A,
            "my-node",
            "https://My-Node.Example.COM/",
        )]);

        let result = discover_node_hash(&client, "0xowner", "my-node.example.com")
            .await
            .unwrap();

        let node = result.expect("URL normalization should match");
        assert_eq!(node.hash.as_str(), HASH_A);
    }

    #[tokio::test]
    async fn test_discover_ignores_non_crn_ops() {
        let client = MockPostClient::new(vec![
            make_non_crn_post(HASH_A),
            make_crn_post(HASH_B, "target", "https://target.example.com"),
            make_non_crn_post(HASH_C),
        ]);

        let result = discover_node_hash(&client, "0xowner", "target.example.com")
            .await
            .unwrap();

        let node = result.expect("should match only the CRN post");
        assert_eq!(node.hash.as_str(), HASH_B);
    }

    #[tokio::test]
    async fn test_discover_empty_response() {
        let client = MockPostClient::new(vec![]);
        let result = discover_node_hash(&client, "0xowner", "any.example.com")
            .await
            .unwrap();
        assert!(result.is_none());
    }

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
    fn test_node_hash_parse() {
        let valid = "b93eaba554318bd074819477e48147bb7bf4121bb771a6074022b0bf412cacc0";
        let hash = NodeHash::parse(valid).unwrap();
        assert_eq!(hash.as_str(), valid);

        // Too short
        assert!(NodeHash::parse("abc123").is_err());

        // Not hex
        let bad = "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz";
        assert!(NodeHash::parse(bad).is_err());

        // Too long
        let long = "b93eaba554318bd074819477e48147bb7bf4121bb771a6074022b0bf412cacc0aa";
        assert!(NodeHash::parse(long).is_err());
    }

    #[test]
    fn test_cache_read_write() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path();

        // No cache file yet
        assert!(read_cached_hash(state_dir).is_none());

        // Write and read back
        let hash =
            NodeHash::parse("b93eaba554318bd074819477e48147bb7bf4121bb771a6074022b0bf412cacc0")
                .unwrap();
        write_cached_hash(state_dir, &hash).unwrap();
        assert_eq!(read_cached_hash(state_dir).unwrap(), hash);

        // Overwrite
        let hash2 =
            NodeHash::parse("0000000000000000000000000000000000000000000000000000000000000001")
                .unwrap();
        write_cached_hash(state_dir, &hash2).unwrap();
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
