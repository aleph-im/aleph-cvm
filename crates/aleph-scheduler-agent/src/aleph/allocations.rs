//! Allocation management — receiving and reconciling VM allocations.
//!
//! The Aleph scheduler sends allocation sets via `POST /control/allocations`.
//! This module authenticates these requests and reconciles the desired state
//! with the compute node's actual state.

use std::collections::HashSet;

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tracing::{debug, info, warn};

use super::messages::ItemHash;

type HmacSha256 = Hmac<Sha256>;

/// Allocation request from the Aleph scheduler.
///
/// Describes the set of VMs that should be running on this node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Allocation {
    /// Persistent programs (long-running, item hashes).
    #[serde(default)]
    pub persistent_vms: HashSet<ItemHash>,
    /// Instance VMs (item hashes).
    #[serde(default)]
    pub instances: HashSet<ItemHash>,
}

impl Allocation {
    /// All VM hashes that should be running.
    pub fn all_vms(&self) -> HashSet<&str> {
        self.persistent_vms
            .iter()
            .chain(self.instances.iter())
            .map(|s| s.as_str())
            .collect()
    }

    /// Whether a specific VM hash is included in this allocation.
    pub fn contains(&self, hash: &str) -> bool {
        self.persistent_vms.contains(hash) || self.instances.contains(hash)
    }
}

/// Verify the HMAC-SHA256 signature on an allocation request.
///
/// The Aleph scheduler signs the JSON body with a shared token.
/// The `signature` is the hex-encoded HMAC of the body bytes.
pub fn verify_allocation_signature(
    body: &[u8],
    signature: &str,
    token_hash: &[u8; 32],
) -> bool {
    let Ok(sig_bytes) = hex::decode(signature) else {
        debug!("allocation signature is not valid hex");
        return false;
    };

    let mut mac = HmacSha256::new_from_slice(token_hash)
        .expect("HMAC accepts any key length");
    mac.update(body);

    mac.verify_slice(&sig_bytes).is_ok()
}

/// Reconciliation result — what the scheduler should do.
#[derive(Debug, Clone)]
pub struct ReconcileActions {
    /// VMs to start (present in allocation but not running).
    pub to_start: Vec<ItemHash>,
    /// VMs to stop (running but not in allocation).
    pub to_stop: Vec<ItemHash>,
    /// VMs already running and still allocated (no action).
    pub unchanged: Vec<ItemHash>,
}

/// Compare desired allocation with currently running VMs.
pub fn reconcile(allocation: &Allocation, running_vm_hashes: &HashSet<String>) -> ReconcileActions {
    let desired = allocation.all_vms();

    let to_start: Vec<ItemHash> = desired
        .iter()
        .filter(|h| !running_vm_hashes.contains(**h))
        .map(|h| h.to_string())
        .collect();

    let to_stop: Vec<ItemHash> = running_vm_hashes
        .iter()
        .filter(|h| !desired.contains(h.as_str()))
        .cloned()
        .collect();

    let unchanged: Vec<ItemHash> = running_vm_hashes
        .iter()
        .filter(|h| desired.contains(h.as_str()))
        .cloned()
        .collect();

    info!(
        start = to_start.len(),
        stop = to_stop.len(),
        unchanged = unchanged.len(),
        "reconciliation result"
    );

    if !to_stop.is_empty() {
        warn!(count = to_stop.len(), "stopping VMs not in allocation");
    }

    ReconcileActions {
        to_start,
        to_stop,
        unchanged,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allocation_contains() {
        let alloc = Allocation {
            persistent_vms: ["hash_a".into()].into(),
            instances: ["hash_b".into()].into(),
        };
        assert!(alloc.contains("hash_a"));
        assert!(alloc.contains("hash_b"));
        assert!(!alloc.contains("hash_c"));
    }

    #[test]
    fn test_allocation_all_vms() {
        let alloc = Allocation {
            persistent_vms: ["a".into(), "b".into()].into(),
            instances: ["c".into()].into(),
        };
        let all = alloc.all_vms();
        assert_eq!(all.len(), 3);
        assert!(all.contains("a"));
        assert!(all.contains("b"));
        assert!(all.contains("c"));
    }

    #[test]
    fn test_reconcile_start_only() {
        let alloc = Allocation {
            persistent_vms: Default::default(),
            instances: ["vm1".into(), "vm2".into()].into(),
        };
        let running = HashSet::new();

        let actions = reconcile(&alloc, &running);
        assert_eq!(actions.to_start.len(), 2);
        assert!(actions.to_stop.is_empty());
        assert!(actions.unchanged.is_empty());
    }

    #[test]
    fn test_reconcile_stop_only() {
        let alloc = Allocation {
            persistent_vms: Default::default(),
            instances: Default::default(),
        };
        let running: HashSet<String> = ["vm1".into(), "vm2".into()].into();

        let actions = reconcile(&alloc, &running);
        assert!(actions.to_start.is_empty());
        assert_eq!(actions.to_stop.len(), 2);
        assert!(actions.unchanged.is_empty());
    }

    #[test]
    fn test_reconcile_mixed() {
        let alloc = Allocation {
            persistent_vms: Default::default(),
            instances: ["vm1".into(), "vm3".into()].into(),
        };
        let running: HashSet<String> = ["vm1".into(), "vm2".into()].into();

        let actions = reconcile(&alloc, &running);
        assert_eq!(actions.to_start, vec!["vm3".to_string()]);
        assert_eq!(actions.to_stop, vec!["vm2".to_string()]);
        assert_eq!(actions.unchanged, vec!["vm1".to_string()]);
    }

    #[test]
    fn test_verify_allocation_signature() {
        let body = b"test payload";
        let token_hash = sha2::Sha256::digest(b"secret token");
        let token_hash: [u8; 32] = token_hash.into();

        // Compute valid signature
        let mut mac = HmacSha256::new_from_slice(&token_hash).unwrap();
        mac.update(body);
        let signature = hex::encode(mac.finalize().into_bytes());

        assert!(verify_allocation_signature(body, &signature, &token_hash));
        assert!(!verify_allocation_signature(body, "invalid", &token_hash));
        assert!(!verify_allocation_signature(b"wrong body", &signature, &token_hash));
    }

    #[test]
    fn test_deserialize_allocation() {
        let json = r#"{
            "persistent_vms": ["hash1", "hash2"],
            "instances": ["hash3"]
        }"#;

        let alloc: Allocation = serde_json::from_str(json).unwrap();
        assert_eq!(alloc.persistent_vms.len(), 2);
        assert_eq!(alloc.instances.len(), 1);
    }

    #[test]
    fn test_deserialize_allocation_empty() {
        let json = r#"{}"#;
        let alloc: Allocation = serde_json::from_str(json).unwrap();
        assert!(alloc.persistent_vms.is_empty());
        assert!(alloc.instances.is_empty());
    }

    use sha2::Digest;
}
