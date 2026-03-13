//! Maps compute-node VmInfo to scheduler-facing execution records.

use aleph_compute_proto::compute::VmInfo;
use serde::Serialize;
use std::collections::HashMap;

#[derive(Debug, Serialize)]
pub struct ExecutionRecord {
    pub status: String,
    pub ipv4: String,
    pub ipv6: String,
    pub is_confidential: bool,
    pub uptime_secs: u64,
}

/// Convert a single VmInfo to an ExecutionRecord.
pub fn vm_info_to_execution(vm: &VmInfo) -> ExecutionRecord {
    ExecutionRecord {
        status: vm.status.clone(),
        ipv4: vm.ipv4.clone(),
        ipv6: vm.ipv6.clone(),
        is_confidential: !vm.tee_backend.is_empty(),
        uptime_secs: vm.uptime_secs,
    }
}

/// Map a list of VmInfo into a HashMap keyed by vm_id (which is the ItemHash).
pub fn map_executions(vms: &[VmInfo]) -> HashMap<String, ExecutionRecord> {
    vms.iter()
        .map(|vm| (vm.vm_id.clone(), vm_info_to_execution(vm)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_vm(vm_id: &str, tee: &str) -> VmInfo {
        VmInfo {
            vm_id: vm_id.to_string(),
            status: "running".to_string(),
            ipv4: "10.0.200.2".to_string(),
            ipv6: "fd00::2".to_string(),
            tee_backend: tee.to_string(),
            uptime_secs: 3600,
            numa_node: 0,
        }
    }

    #[test]
    fn test_confidential_vm() {
        let vm = make_vm("abc123", "SevSnp");
        let record = vm_info_to_execution(&vm);
        assert_eq!(record.status, "running");
        assert_eq!(record.ipv4, "10.0.200.2");
        assert_eq!(record.ipv6, "fd00::2");
        assert!(record.is_confidential);
        assert_eq!(record.uptime_secs, 3600);
    }

    #[test]
    fn test_non_confidential_vm() {
        let vm = make_vm("xyz789", "");
        let record = vm_info_to_execution(&vm);
        assert!(!record.is_confidential);
    }

    #[test]
    fn test_map_executions() {
        let vms = vec![make_vm("hash1", "SevSnp"), make_vm("hash2", "")];
        let map = map_executions(&vms);
        assert_eq!(map.len(), 2);
        assert!(map.contains_key("hash1"));
        assert!(map.contains_key("hash2"));
        assert!(map["hash1"].is_confidential);
        assert!(!map["hash2"].is_confidential);
    }

    #[test]
    fn test_map_executions_empty() {
        let map = map_executions(&[]);
        assert!(map.is_empty());
    }
}
