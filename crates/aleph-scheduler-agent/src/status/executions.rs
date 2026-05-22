//! Maps compute-node VmInfo to scheduler-facing execution records.
//!
//! Matches the response schema of aleph-vm's `GET /about/executions/list` (v1).

use aleph_compute_proto::compute::VmInfo;
use serde::Serialize;
use std::collections::HashMap;

/// Networking info for a running VM, matching aleph-vm v1 schema.
#[derive(Debug, Serialize)]
pub struct NetworkingInfo {
    pub ipv4: String,
    pub ipv6: String,
}

/// Per-VM record returned in the executions list.
#[derive(Debug, Serialize)]
pub struct ExecutionRecord {
    pub networking: NetworkingInfo,
}

/// Map a list of VmInfo into a HashMap keyed by vm_id (which is the ItemHash).
///
/// Only includes VMs with "running" status, matching aleph-vm's behaviour of
/// filtering by running state. IP addresses are emitted as CIDR strings to
/// match aleph-vm's wire format (host IP gets `/32` when no prefix is present).
pub fn map_executions(vms: &[VmInfo]) -> HashMap<String, ExecutionRecord> {
    vms.iter()
        .filter(|vm| vm.status == "running")
        .map(|vm| {
            (
                vm.vm_id.clone(),
                ExecutionRecord {
                    networking: NetworkingInfo {
                        ipv4: ensure_cidr(&vm.ipv4, 32),
                        ipv6: ensure_cidr(&vm.ipv6, 128),
                    },
                },
            )
        })
        .collect()
}

/// Return `s` unchanged if it already contains a `/`, otherwise append `/<default_prefix>`.
fn ensure_cidr(s: &str, default_prefix: u8) -> String {
    if s.contains('/') {
        s.to_string()
    } else {
        format!("{s}/{default_prefix}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_vm(vm_id: &str, status: &str) -> VmInfo {
        VmInfo {
            vm_id: vm_id.to_string(),
            status: status.to_string(),
            ipv4: "10.0.200.2".to_string(),
            ipv6: "fd00::2".to_string(),
            tee_backend: "SevSnp".to_string(),
            uptime_secs: 3600,
            numa_node: 0,
        }
    }

    #[test]
    fn test_running_vm_included() {
        let vms = vec![make_vm("abc123", "running")];
        let map = map_executions(&vms);
        assert_eq!(map.len(), 1);
        let record = &map["abc123"];
        // Bare host IPs get `/32` and `/128` appended to match aleph-vm's CIDR format.
        assert_eq!(record.networking.ipv4, "10.0.200.2/32");
        assert_eq!(record.networking.ipv6, "fd00::2/128");
    }

    #[test]
    fn test_already_cidr_passthrough() {
        let vm = VmInfo {
            vm_id: "abc".to_string(),
            status: "running".to_string(),
            ipv4: "172.16.15.0/24".to_string(),
            ipv6: "2604:4300:a:6a:3::/124".to_string(),
            tee_backend: "None".to_string(),
            uptime_secs: 0,
            numa_node: 0,
        };
        let map = map_executions(&[vm]);
        assert_eq!(map["abc"].networking.ipv4, "172.16.15.0/24");
        assert_eq!(map["abc"].networking.ipv6, "2604:4300:a:6a:3::/124");
    }

    #[test]
    fn test_non_running_vm_excluded() {
        let vms = vec![make_vm("abc123", "stopped"), make_vm("def456", "booting")];
        let map = map_executions(&vms);
        assert!(map.is_empty());
    }

    #[test]
    fn test_mixed_statuses() {
        let vms = vec![
            make_vm("hash1", "running"),
            make_vm("hash2", "stopped"),
            make_vm("hash3", "running"),
        ];
        let map = map_executions(&vms);
        assert_eq!(map.len(), 2);
        assert!(map.contains_key("hash1"));
        assert!(!map.contains_key("hash2"));
        assert!(map.contains_key("hash3"));
    }

    #[test]
    fn test_map_executions_empty() {
        let map = map_executions(&[]);
        assert!(map.is_empty());
    }

    #[test]
    fn test_json_structure() {
        let vms = vec![make_vm("hash1", "running")];
        let map = map_executions(&vms);
        let json = serde_json::to_value(&map).unwrap();
        // Verify the nested "networking" structure matches aleph-vm v1
        assert_eq!(json["hash1"]["networking"]["ipv4"], "10.0.200.2/32");
        assert_eq!(json["hash1"]["networking"]["ipv6"], "fd00::2/128");
        // Should NOT have flat fields like "status" or "is_confidential"
        assert!(json["hash1"]["status"].is_null());
    }
}
