use std::collections::HashMap;

use crate::types::{PortForward, Protocol};

/// Tracks port forwarding state and checks for conflicts.
pub struct PortForwardState {
    /// Map from (host_port, protocol) → PortForward.
    forwards: HashMap<(u16, Protocol), PortForward>,
}

impl PortForwardState {
    pub fn new() -> Self {
        Self {
            forwards: HashMap::new(),
        }
    }

    /// Check if a host port is available for the given protocol.
    pub fn is_available(&self, host_port: u16, protocol: Protocol) -> bool {
        !self.forwards.contains_key(&(host_port, protocol))
    }

    /// Auto-allocate a host port, starting from `start` and searching upward.
    pub fn auto_allocate(&self, protocol: Protocol, start: u16) -> Option<u16> {
        for port in start..=65535 {
            if self.is_available(port, protocol) {
                return Some(port);
            }
        }
        None
    }

    /// Register a port forward.
    pub fn add(&mut self, forward: PortForward) {
        self.forwards
            .insert((forward.host_port, forward.protocol), forward);
    }

    /// Remove a port forward by host port and protocol.
    pub fn remove(&mut self, host_port: u16, protocol: Protocol) -> Option<PortForward> {
        self.forwards.remove(&(host_port, protocol))
    }

    /// List all port forwards for a given VM.
    pub fn list_for_vm(&self, vm_id: &str) -> Vec<&PortForward> {
        self.forwards
            .values()
            .filter(|f| f.vm_id == vm_id)
            .collect()
    }

    /// List all port forwards.
    pub fn list_all(&self) -> Vec<&PortForward> {
        self.forwards.values().collect()
    }

    /// Remove all port forwards for a VM.
    pub fn remove_all_for_vm(&mut self, vm_id: &str) -> Vec<PortForward> {
        let keys: Vec<_> = self
            .forwards
            .iter()
            .filter(|(_, f)| f.vm_id == vm_id)
            .map(|(k, _)| *k)
            .collect();

        keys.into_iter()
            .filter_map(|k| self.forwards.remove(&k))
            .collect()
    }
}

impl Default for PortForwardState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_port_availability() {
        let mut state = PortForwardState::new();
        assert!(state.is_available(8080, Protocol::Tcp));

        state.add(PortForward {
            vm_id: "vm1".to_string(),
            host_port: 8080,
            vm_port: 80,
            protocol: Protocol::Tcp,
        });

        assert!(!state.is_available(8080, Protocol::Tcp));
        // Same port, different protocol is fine
        assert!(state.is_available(8080, Protocol::Udp));
    }

    #[test]
    fn test_auto_allocate() {
        let mut state = PortForwardState::new();
        state.add(PortForward {
            vm_id: "vm1".to_string(),
            host_port: 10000,
            vm_port: 80,
            protocol: Protocol::Tcp,
        });

        let port = state.auto_allocate(Protocol::Tcp, 10000).unwrap();
        assert_eq!(port, 10001); // 10000 is taken, next is 10001
    }

    #[test]
    fn test_list_and_remove_for_vm() {
        let mut state = PortForwardState::new();
        state.add(PortForward {
            vm_id: "vm1".to_string(),
            host_port: 8080,
            vm_port: 80,
            protocol: Protocol::Tcp,
        });
        state.add(PortForward {
            vm_id: "vm1".to_string(),
            host_port: 8443,
            vm_port: 443,
            protocol: Protocol::Tcp,
        });
        state.add(PortForward {
            vm_id: "vm2".to_string(),
            host_port: 9090,
            vm_port: 80,
            protocol: Protocol::Tcp,
        });

        assert_eq!(state.list_for_vm("vm1").len(), 2);
        assert_eq!(state.list_for_vm("vm2").len(), 1);

        let removed = state.remove_all_for_vm("vm1");
        assert_eq!(removed.len(), 2);
        assert_eq!(state.list_for_vm("vm1").len(), 0);
        assert_eq!(state.list_for_vm("vm2").len(), 1);
    }
}
