use ipnet::Ipv4Net;

/// Allocates /30 (or configured prefix) subnets from a pool for VM TAP interfaces.
///
/// Mirrors aleph-vm's `Network.get_network_for_tap(vm_id)` which slices
/// the pool into subnets and indexes by vm_id.
pub struct Ipv4Allocator {
    pool: Ipv4Net,
    subnet_prefix: u8,
    next_id: u32,
}

impl Ipv4Allocator {
    pub fn new(pool: Ipv4Net, subnet_prefix: u8) -> Self {
        Self {
            pool,
            subnet_prefix,
            next_id: 0,
        }
    }

    /// Allocate the next subnet from the pool.
    pub fn allocate(&mut self) -> Option<Ipv4Net> {
        let subnet = self.get_subnet(self.next_id)?;
        self.next_id += 1;
        Some(subnet)
    }

    /// Get the subnet for a specific VM ID (deterministic indexing).
    pub fn get_subnet(&self, vm_id: u32) -> Option<Ipv4Net> {
        self.pool
            .subnets(self.subnet_prefix)
            .ok()?
            .nth(vm_id as usize)
    }

    /// Total number of subnets available in the pool.
    pub fn capacity(&self) -> u64 {
        if self.subnet_prefix <= self.pool.prefix_len() {
            return 0;
        }
        1u64 << (self.subnet_prefix - self.pool.prefix_len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allocate_sequential() {
        let pool: Ipv4Net = "172.16.0.0/24".parse().unwrap();
        let mut alloc = Ipv4Allocator::new(pool, 30);

        let s0 = alloc.allocate().unwrap();
        assert_eq!(s0.to_string(), "172.16.0.0/30");

        let s1 = alloc.allocate().unwrap();
        assert_eq!(s1.to_string(), "172.16.0.4/30");

        let s2 = alloc.allocate().unwrap();
        assert_eq!(s2.to_string(), "172.16.0.8/30");
    }

    #[test]
    fn test_get_subnet_by_id() {
        let pool: Ipv4Net = "172.16.0.0/24".parse().unwrap();
        let alloc = Ipv4Allocator::new(pool, 30);

        assert_eq!(alloc.get_subnet(0).unwrap().to_string(), "172.16.0.0/30");
        assert_eq!(alloc.get_subnet(5).unwrap().to_string(), "172.16.0.20/30");
    }

    #[test]
    fn test_capacity() {
        let pool: Ipv4Net = "172.16.0.0/24".parse().unwrap();
        let alloc = Ipv4Allocator::new(pool, 30);
        // /24 → /30 = 2^(30-24) = 64 subnets
        assert_eq!(alloc.capacity(), 64);
    }

    #[test]
    fn test_exhaustion() {
        let pool: Ipv4Net = "172.16.0.0/30".parse().unwrap();
        let mut alloc = Ipv4Allocator::new(pool, 31);
        // /30 → /31 = 2 subnets
        assert!(alloc.allocate().is_some());
        assert!(alloc.allocate().is_some());
        assert!(alloc.allocate().is_none());
    }
}
