use std::net::Ipv6Addr;

use ipnet::Ipv6Net;

/// VM type discriminator for IPv6 allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmType {
    Microvm = 1,
    PersistentProgram = 2,
    Instance = 3,
}

impl VmType {
    fn hex_nibble(self) -> &'static str {
        match self {
            VmType::Microvm => "1",
            VmType::PersistentProgram => "2",
            VmType::Instance => "3",
        }
    }
}

/// Trait for IPv6 subnet allocation strategies.
pub trait Ipv6Allocator: Send + Sync {
    fn allocate(&mut self, vm_id: u32, vm_hash: &str, vm_type: VmType) -> Option<Ipv6Net>;
}

/// Deterministic IPv6 allocation based on CRN prefix + type + hash.
///
/// Port of aleph-vm's `StaticIPv6Allocator`. Given a /64 range, produces
/// /124 subnets by embedding the VM type nibble and 44 bits of the item hash.
///
/// Layout: `[CRN /64 prefix (4 groups)]:[type nibble]:[hash[0:4]]:[hash[4:8]]:[hash[8:11]+"0"] /124`
pub struct StaticIpv6Allocator {
    ipv6_range: Ipv6Net,
    subnet_prefix: u8,
}

impl StaticIpv6Allocator {
    pub fn new(ipv6_range: Ipv6Net, subnet_prefix: u8) -> Self {
        assert!(
            ipv6_range.prefix_len() == 56 || ipv6_range.prefix_len() == 64,
            "ipv6_range must be /56 or /64, got /{}",
            ipv6_range.prefix_len()
        );
        assert!(
            subnet_prefix >= 124,
            "subnet_prefix must be >= 124, got {}",
            subnet_prefix
        );
        Self {
            ipv6_range,
            subnet_prefix,
        }
    }
}

impl Ipv6Allocator for StaticIpv6Allocator {
    fn allocate(&mut self, _vm_id: u32, vm_hash: &str, vm_type: VmType) -> Option<Ipv6Net> {
        if vm_hash.len() < 11 {
            return None;
        }

        // Take first 4 groups of the range address
        let segments = self.ipv6_range.network().segments();
        let base = format!(
            "{:x}:{:x}:{:x}:{:x}",
            segments[0], segments[1], segments[2], segments[3]
        );

        // Build the address: base:type_nibble:hash[0:4]:hash[4:8]:hash[8:11]+"0"
        let type_nibble = vm_type.hex_nibble();
        let addr_str = format!(
            "{}:{}:{}:{}:{}0",
            base,
            type_nibble,
            &vm_hash[0..4],
            &vm_hash[4..8],
            &vm_hash[8..11],
        );

        let addr: Ipv6Addr = addr_str.parse().ok()?;
        Some(Ipv6Net::new(addr, self.subnet_prefix).ok()?)
    }
}

/// Sequential IPv6 allocation for testing.
///
/// Port of aleph-vm's `DynamicIPv6Allocator`. Slices the pool into subnets
/// and hands them out sequentially, skipping the first (reserved for host).
pub struct DynamicIpv6Allocator {
    pool: Ipv6Net,
    subnet_prefix: u8,
    next_index: u32,
}

impl DynamicIpv6Allocator {
    pub fn new(pool: Ipv6Net, subnet_prefix: u8) -> Self {
        Self {
            pool,
            subnet_prefix,
            next_index: 1, // Skip first subnet (reserved for host)
        }
    }
}

impl Ipv6Allocator for DynamicIpv6Allocator {
    fn allocate(&mut self, _vm_id: u32, _vm_hash: &str, _vm_type: VmType) -> Option<Ipv6Net> {
        let subnet = self
            .pool
            .subnets(self.subnet_prefix)
            .ok()?
            .nth(self.next_index as usize)?;
        self.next_index += 1;
        Some(subnet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_static_allocator() {
        let range: Ipv6Net = "2001:db8::/64".parse().unwrap();
        let mut alloc = StaticIpv6Allocator::new(range, 124);

        let subnet = alloc
            .allocate(0, "aabbccddeeff1234567890", VmType::Instance)
            .unwrap();

        // Expected: 2001:db8:0:0:3:aabb:ccdd:ee00/124
        assert_eq!(subnet.prefix_len(), 124);
        let segments = subnet.network().segments();
        assert_eq!(segments[0], 0x2001);
        assert_eq!(segments[1], 0x0db8);
        assert_eq!(segments[4], 0x0003); // VM type nibble
        assert_eq!(segments[5], 0xaabb); // hash[0:4]
        assert_eq!(segments[6], 0xccdd); // hash[4:8]
        assert_eq!(segments[7], 0xeef0); // hash[8:11] + "0"
    }

    #[test]
    fn test_static_allocator_different_types() {
        let range: Ipv6Net = "2001:db8::/64".parse().unwrap();
        let mut alloc = StaticIpv6Allocator::new(range, 124);

        let s1 = alloc
            .allocate(0, "aabbccddeeff", VmType::Microvm)
            .unwrap();
        let s2 = alloc
            .allocate(0, "aabbccddeeff", VmType::Instance)
            .unwrap();

        // Different type → different subnet
        assert_ne!(s1.network(), s2.network());
        assert_eq!(s1.network().segments()[4], 0x0001); // microvm
        assert_eq!(s2.network().segments()[4], 0x0003); // instance
    }

    #[test]
    fn test_dynamic_allocator() {
        let pool: Ipv6Net = "fd00::/64".parse().unwrap();
        let mut alloc = DynamicIpv6Allocator::new(pool, 124);

        let s1 = alloc.allocate(0, "hash1", VmType::Instance).unwrap();
        let s2 = alloc.allocate(1, "hash2", VmType::Instance).unwrap();

        // Sequential, non-overlapping
        assert_ne!(s1.network(), s2.network());
        assert_eq!(s1.prefix_len(), 124);
        assert_eq!(s2.prefix_len(), 124);
    }

    #[test]
    fn test_static_allocator_short_hash() {
        let range: Ipv6Net = "2001:db8::/64".parse().unwrap();
        let mut alloc = StaticIpv6Allocator::new(range, 124);

        // Hash too short
        assert!(alloc.allocate(0, "short", VmType::Instance).is_none());
    }
}
