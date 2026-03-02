mod allocator;
mod range_allocator;

pub use allocator::{DynamicIpv6Allocator, Ipv6Allocator, StaticIpv6Allocator, VmType};
pub use range_allocator::Ipv6RangeAllocator;
