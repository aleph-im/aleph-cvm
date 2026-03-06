use std::collections::HashMap;
use std::net::Ipv6Addr;

use anyhow::{Result, bail};
use ipnet::Ipv6Net;
use rand::Rng;

/// Pool-based IPv6 allocator with overlap tracking.
///
/// Allocates /128 addresses (or user-specified prefixes) from a managed pool.
/// Tracks all allocations to prevent overlaps and supports explicit release.
pub struct Ipv6RangeAllocator {
    pool: Ipv6Net,
    default_prefix: u8,
    allocations: HashMap<String, Ipv6Net>,
}

impl Ipv6RangeAllocator {
    pub fn new(pool: Ipv6Net, default_prefix: u8) -> Self {
        Self {
            pool,
            default_prefix,
            allocations: HashMap::new(),
        }
    }

    /// Allocate an IPv6 range for a VM.
    ///
    /// - `None` = random address with `default_prefix`
    /// - `Some(net)` = validate within pool + no overlap
    pub fn allocate(&mut self, vm_id: &str, requested: Option<Ipv6Net>) -> Result<Ipv6Net> {
        let net = match requested {
            Some(net) => {
                if !self.contains(&net) {
                    bail!("requested {} is not within pool {}", net, self.pool);
                }
                if self.overlaps(&net) {
                    bail!("requested {} overlaps with an existing allocation", net);
                }
                net
            }
            None => self.random_address()?,
        };

        self.allocations.insert(vm_id.to_string(), net);
        Ok(net)
    }

    /// Release an allocation, returning the freed range if it existed.
    pub fn release(&mut self, vm_id: &str) -> Option<Ipv6Net> {
        self.allocations.remove(vm_id)
    }

    /// Check whether `net` is fully contained within the pool.
    fn contains(&self, net: &Ipv6Net) -> bool {
        let pool_start = u128::from(self.pool.network());
        let pool_end = u128::from(self.pool.broadcast());
        let net_start = u128::from(net.network());
        let net_end = u128::from(net.broadcast());
        net_start >= pool_start && net_end <= pool_end
    }

    /// Check whether `net` overlaps with any existing allocation.
    fn overlaps(&self, net: &Ipv6Net) -> bool {
        let net_start = u128::from(net.network());
        let net_end = u128::from(net.broadcast());
        self.allocations.values().any(|existing| {
            let ex_start = u128::from(existing.network());
            let ex_end = u128::from(existing.broadcast());
            net_start <= ex_end && net_end >= ex_start
        })
    }

    /// Generate a random address within the pool's host portion.
    fn random_address(&self) -> Result<Ipv6Net> {
        let pool_start = u128::from(self.pool.network());
        let host_bits = 128 - self.pool.prefix_len();
        let host_count = 1u128 << host_bits;

        let mut rng = rand::rng();

        for _ in 0..100 {
            let offset = if host_count <= u64::MAX as u128 {
                rng.random_range(1..host_count as u64) as u128
            } else {
                rng.random_range(1..u64::MAX) as u128
            };

            let addr = Ipv6Addr::from(pool_start + offset);
            let net = Ipv6Net::new(addr, self.default_prefix)?;

            if self.contains(&net) && !self.overlaps(&net) {
                return Ok(net);
            }
        }

        bail!(
            "failed to find a non-overlapping address in pool {} after 100 attempts",
            self.pool
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allocate_within_pool() {
        let pool: Ipv6Net = "2001:db8::/48".parse().unwrap();
        let mut alloc = Ipv6RangeAllocator::new(pool, 128);

        let addr: Ipv6Net = "2001:db8::1/128".parse().unwrap();
        let result = alloc.allocate("vm-1", Some(addr)).unwrap();
        assert_eq!(result, addr);
    }

    #[test]
    fn test_allocate_outside_pool() {
        let pool: Ipv6Net = "2001:db8::/48".parse().unwrap();
        let mut alloc = Ipv6RangeAllocator::new(pool, 128);

        let addr: Ipv6Net = "2001:db9::1/128".parse().unwrap();
        let result = alloc.allocate("vm-1", Some(addr));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not within pool"));
    }

    #[test]
    fn test_overlap_rejection() {
        let pool: Ipv6Net = "2001:db8::/48".parse().unwrap();
        let mut alloc = Ipv6RangeAllocator::new(pool, 128);

        let addr: Ipv6Net = "2001:db8::1/128".parse().unwrap();
        alloc.allocate("vm-1", Some(addr)).unwrap();

        // Same address should be rejected
        let result = alloc.allocate("vm-2", Some(addr));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("overlaps"));
    }

    #[test]
    fn test_overlap_subnet() {
        let pool: Ipv6Net = "2001:db8::/48".parse().unwrap();
        let mut alloc = Ipv6RangeAllocator::new(pool, 128);

        // Allocate a /64
        let wide: Ipv6Net = "2001:db8:0:1::/64".parse().unwrap();
        alloc.allocate("vm-1", Some(wide)).unwrap();

        // A /128 inside that /64 should overlap
        let narrow: Ipv6Net = "2001:db8:0:1::5/128".parse().unwrap();
        let result = alloc.allocate("vm-2", Some(narrow));
        assert!(result.is_err());
    }

    #[test]
    fn test_release_and_reuse() {
        let pool: Ipv6Net = "2001:db8::/48".parse().unwrap();
        let mut alloc = Ipv6RangeAllocator::new(pool, 128);

        let addr: Ipv6Net = "2001:db8::1/128".parse().unwrap();
        alloc.allocate("vm-1", Some(addr)).unwrap();

        // Release
        let released = alloc.release("vm-1");
        assert_eq!(released, Some(addr));

        // Reuse should succeed
        alloc.allocate("vm-2", Some(addr)).unwrap();
    }

    #[test]
    fn test_random_produces_valid_address() {
        let pool: Ipv6Net = "2001:db8::/48".parse().unwrap();
        let mut alloc = Ipv6RangeAllocator::new(pool, 128);

        let result = alloc.allocate("vm-1", None).unwrap();
        assert_eq!(result.prefix_len(), 128);

        // Must be within pool
        let addr_u128 = u128::from(result.addr());
        let pool_start = u128::from(pool.network());
        let pool_end = u128::from(pool.broadcast());
        assert!(addr_u128 >= pool_start && addr_u128 <= pool_end);
    }

    #[test]
    fn test_multiple_random_no_overlap() {
        let pool: Ipv6Net = "2001:db8::/48".parse().unwrap();
        let mut alloc = Ipv6RangeAllocator::new(pool, 128);

        for i in 0..10 {
            alloc
                .allocate(&format!("vm-{i}"), None)
                .expect("random allocation should succeed");
        }

        // All allocations should be unique
        let addrs: Vec<_> = alloc.allocations.values().collect();
        for (i, a) in addrs.iter().enumerate() {
            for (j, b) in addrs.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "allocations should be unique");
                }
            }
        }
    }
}
