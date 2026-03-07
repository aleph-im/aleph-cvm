use std::collections::BTreeSet;
use std::path::Path;

use aleph_tee::types::HugePageSize;
use anyhow::{Context, Result};
use tracing::info;

/// A single NUMA node with its CPU set and hugepage count.
#[derive(Debug, Clone)]
pub struct NumaNode {
    /// NUMA node ID (e.g. 0, 1, …).
    pub id: u32,
    /// Set of logical CPU IDs belonging to this node.
    pub cpus: BTreeSet<u32>,
    /// Number of 2 MiB hugepages on this node.
    pub total_2m_hugepages: u32,
    /// Number of 1 GiB hugepages on this node (boot-time reserved).
    pub total_1g_hugepages: u32,
    /// Total RAM on this node in MB.
    pub total_ram_mb: u32,
}

/// Detected NUMA topology of the host.
#[derive(Debug, Clone)]
pub struct NumaTopology {
    pub nodes: Vec<NumaNode>,
}

impl NumaTopology {
    /// Detect NUMA topology from the real sysfs.
    pub fn detect() -> Result<Self> {
        Self::from_sysfs(Path::new("/sys/devices/system/node"))
    }

    /// Detect NUMA topology from an arbitrary sysfs-like directory (for testing).
    pub fn from_sysfs(base: &Path) -> Result<Self> {
        let mut nodes = Vec::new();

        let mut entries: Vec<_> = std::fs::read_dir(base)
            .with_context(|| format!("failed to read {}", base.display()))?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with("node"))
            })
            .collect();

        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            let name = entry.file_name();
            let name = name.to_str().context("non-UTF-8 node directory name")?;
            let id: u32 = name
                .strip_prefix("node")
                .context("unexpected directory name")?
                .parse()
                .with_context(|| format!("failed to parse node ID from {name}"))?;

            let node_path = entry.path();

            let cpulist_raw = std::fs::read_to_string(node_path.join("cpulist"))
                .with_context(|| format!("failed to read cpulist for {name}"))?;
            let cpus = parse_cpulist(cpulist_raw.trim())?;

            let hp_2m_path = node_path.join("hugepages/hugepages-2048kB/nr_hugepages");
            let total_2m_hugepages: u32 = std::fs::read_to_string(&hp_2m_path)
                .with_context(|| format!("failed to read 2M hugepages for {name}"))?
                .trim()
                .parse()
                .with_context(|| format!("failed to parse 2M hugepage count for {name}"))?;

            // 1 GiB hugepages (may not exist if not configured at boot)
            let hp_1g_path = node_path.join("hugepages/hugepages-1048576kB/nr_hugepages");
            let total_1g_hugepages: u32 = std::fs::read_to_string(&hp_1g_path)
                .unwrap_or_else(|_| "0".to_string())
                .trim()
                .parse()
                .unwrap_or(0);

            // Per-node MemTotal from meminfo
            let meminfo_path = node_path.join("meminfo");
            let meminfo = std::fs::read_to_string(&meminfo_path)
                .with_context(|| format!("failed to read meminfo for {name}"))?;
            let total_ram_mb = parse_memtotal_kb(&meminfo)
                .with_context(|| format!("failed to parse MemTotal for {name}"))?
                / 1024;

            info!(
                node = id,
                ?cpus,
                total_2m_hugepages,
                total_1g_hugepages,
                total_ram_mb,
                "detected NUMA node"
            );
            nodes.push(NumaNode {
                id,
                cpus,
                total_2m_hugepages,
                total_1g_hugepages,
                total_ram_mb,
            });
        }

        Ok(Self { nodes })
    }

    /// Number of NUMA nodes.
    pub fn num_nodes(&self) -> usize {
        self.nodes.len()
    }
}

/// Placement decision: which NUMA node and what cpuset string to use.
#[derive(Debug, Clone)]
pub struct NumaPlacement {
    /// The NUMA node ID where the VM should be placed.
    pub node: u32,
    /// Compact cpulist string (e.g. "0-3,8-11") for systemd AllowedCPUs.
    pub cpuset: String,
    /// Hugepage size selected for this VM.
    pub hugepage_size: HugePageSize,
}

/// Allocator that packs VMs onto NUMA nodes, trying node 0 first, then node 1, etc.
///
/// Tracks two separate memory pools per node: 2M hugepages and 1G hugepages.
/// When allocating, 1G pages are preferred for 1G-aligned requests (better
/// pvalidate performance), falling back to 2M pages.
#[derive(Debug)]
pub struct NumaAllocator {
    topology: NumaTopology,
    /// Per-node count of currently allocated vCPUs.
    allocated_vcpus: Vec<u32>,
    /// Per-node count of currently allocated 2M hugepages.
    allocated_2m_pages: Vec<u32>,
    /// Per-node count of currently allocated 1G hugepages.
    allocated_1g_pages: Vec<u32>,
}

impl NumaAllocator {
    /// Create a new allocator with zero allocations.
    pub fn new(topology: NumaTopology) -> Self {
        let n = topology.nodes.len();
        Self {
            topology,
            allocated_vcpus: vec![0; n],
            allocated_2m_pages: vec![0; n],
            allocated_1g_pages: vec![0; n],
        }
    }

    /// Allocate vCPUs and memory on a NUMA node using pack-first strategy.
    ///
    /// If `hint` is `Some(node_id)`, only that specific node is tried.
    /// Otherwise, nodes are tried in order (0, 1, 2, …) and the first one
    /// with enough free CPUs and memory is selected.
    ///
    /// Page-size selection:
    /// - If `memory_mb` is a multiple of 1024, try 1G pages first.
    /// - If 1G pages don't fit (or memory isn't 1G-aligned), use 2M pages.
    pub fn allocate(
        &mut self,
        vcpus: u32,
        memory_mb: u32,
        hint: Option<u32>,
    ) -> Result<NumaPlacement> {
        let candidates: Vec<usize> = if let Some(node_id) = hint {
            self.topology
                .nodes
                .iter()
                .position(|n| n.id == node_id)
                .map(|i| vec![i])
                .unwrap_or_default()
        } else {
            (0..self.topology.nodes.len()).collect()
        };

        for idx in candidates {
            let node = &self.topology.nodes[idx];
            let available_cpus = node.cpus.len() as u32 - self.allocated_vcpus[idx];

            if vcpus > available_cpus {
                continue;
            }

            // Try 1G pages first if memory is 1G-aligned.
            if memory_mb.is_multiple_of(1024) {
                let pages_needed = memory_mb / 1024;
                let available_1g = node
                    .total_1g_hugepages
                    .saturating_sub(self.allocated_1g_pages[idx]);
                if pages_needed <= available_1g {
                    self.allocated_vcpus[idx] += vcpus;
                    self.allocated_1g_pages[idx] += pages_needed;
                    return Ok(NumaPlacement {
                        node: node.id,
                        cpuset: format_cpuset(&node.cpus),
                        hugepage_size: HugePageSize::Size1G,
                    });
                }
            }

            // Fall back to 2M pages.
            let pages_needed = memory_mb / 2;
            let available_2m = node
                .total_2m_hugepages
                .saturating_sub(self.allocated_2m_pages[idx]);
            if pages_needed <= available_2m {
                self.allocated_vcpus[idx] += vcpus;
                self.allocated_2m_pages[idx] += pages_needed;
                return Ok(NumaPlacement {
                    node: node.id,
                    cpuset: format_cpuset(&node.cpus),
                    hugepage_size: HugePageSize::Size2M,
                });
            }
        }

        anyhow::bail!("no NUMA node has enough resources for {vcpus} vCPUs and {memory_mb} MB")
    }

    /// Release previously allocated resources on a node (saturating subtract).
    ///
    /// The caller must pass the same `hugepage_size` that was returned by
    /// [`allocate`] so the correct pool is decremented.
    pub fn release(&mut self, node: u32, vcpus: u32, memory_mb: u32, hugepage_size: HugePageSize) {
        if let Some(idx) = self.topology.nodes.iter().position(|n| n.id == node) {
            self.allocated_vcpus[idx] = self.allocated_vcpus[idx].saturating_sub(vcpus);
            match hugepage_size {
                HugePageSize::Size1G => {
                    let pages = memory_mb / 1024;
                    self.allocated_1g_pages[idx] =
                        self.allocated_1g_pages[idx].saturating_sub(pages);
                }
                HugePageSize::Size2M => {
                    let pages = memory_mb / 2;
                    self.allocated_2m_pages[idx] =
                        self.allocated_2m_pages[idx].saturating_sub(pages);
                }
            }
        }
    }
}

/// Format a set of CPU IDs as a compact cpulist string (e.g. "0-3,8-11").
pub fn format_cpuset(cpus: &BTreeSet<u32>) -> String {
    let mut result = String::new();
    let mut iter = cpus.iter().copied();

    let Some(first) = iter.next() else {
        return result;
    };

    let mut range_start = first;
    let mut range_end = first;

    for cpu in iter {
        if cpu == range_end + 1 {
            range_end = cpu;
        } else {
            // Flush the current range.
            if !result.is_empty() {
                result.push(',');
            }
            if range_start == range_end {
                result.push_str(&range_start.to_string());
            } else {
                result.push_str(&format!("{range_start}-{range_end}"));
            }
            range_start = cpu;
            range_end = cpu;
        }
    }

    // Flush the last range.
    if !result.is_empty() {
        result.push(',');
    }
    if range_start == range_end {
        result.push_str(&range_start.to_string());
    } else {
        result.push_str(&format!("{range_start}-{range_end}"));
    }

    result
}

/// Parse a Linux cpulist string (e.g. "0-3,8-11") into a sorted set of CPU IDs.
pub fn parse_cpulist(s: &str) -> Result<BTreeSet<u32>> {
    let mut cpus = BTreeSet::new();
    for part in s.split(',') {
        let part = part.trim();
        if let Some((lo, hi)) = part.split_once('-') {
            let lo: u32 = lo.parse().context("invalid cpu range start")?;
            let hi: u32 = hi.parse().context("invalid cpu range end")?;
            cpus.extend(lo..=hi);
        } else {
            let cpu: u32 = part.parse().context("invalid cpu id")?;
            cpus.insert(cpu);
        }
    }
    Ok(cpus)
}

/// Parse MemTotal in kB from a node's meminfo file.
fn parse_memtotal_kb(meminfo: &str) -> Result<u32> {
    for line in meminfo.lines() {
        if line.contains("MemTotal:") {
            let kb_str = line
                .split_whitespace()
                .rev()
                .nth(1) // second from right, before "kB"
                .context("malformed MemTotal line")?;
            return kb_str.parse().context("failed to parse MemTotal value");
        }
    }
    anyhow::bail!("MemTotal not found in meminfo")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cpulist_range() {
        let cpus = parse_cpulist("0-3").unwrap();
        assert_eq!(cpus, BTreeSet::from([0, 1, 2, 3]));
    }

    #[test]
    fn test_parse_cpulist_mixed() {
        let cpus = parse_cpulist("0-3,8-11").unwrap();
        assert_eq!(cpus, BTreeSet::from([0, 1, 2, 3, 8, 9, 10, 11]));
    }

    #[test]
    fn test_parse_cpulist_single() {
        let cpus = parse_cpulist("5").unwrap();
        assert_eq!(cpus, BTreeSet::from([5]));
    }

    #[test]
    fn test_parse_cpulist_comma_singles() {
        let cpus = parse_cpulist("0,2,4").unwrap();
        assert_eq!(cpus, BTreeSet::from([0, 2, 4]));
    }

    #[test]
    fn test_from_sysfs_two_nodes() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        // node0: cpus 0-3, 100 hugepages
        let node0 = base.join("node0");
        std::fs::create_dir_all(node0.join("hugepages/hugepages-2048kB")).unwrap();
        std::fs::create_dir_all(node0.join("hugepages/hugepages-1048576kB")).unwrap();
        std::fs::write(node0.join("cpulist"), "0-3\n").unwrap();
        std::fs::write(
            node0.join("hugepages/hugepages-2048kB/nr_hugepages"),
            "100\n",
        )
        .unwrap();
        std::fs::write(
            node0.join("hugepages/hugepages-1048576kB/nr_hugepages"),
            "2\n",
        )
        .unwrap();
        std::fs::write(
            node0.join("meminfo"),
            "Node 0 MemTotal:       65536000 kB\nNode 0 MemFree:        32000000 kB\n",
        )
        .unwrap();

        // node1: cpus 4-7, 200 hugepages
        let node1 = base.join("node1");
        std::fs::create_dir_all(node1.join("hugepages/hugepages-2048kB")).unwrap();
        std::fs::create_dir_all(node1.join("hugepages/hugepages-1048576kB")).unwrap();
        std::fs::write(node1.join("cpulist"), "4-7\n").unwrap();
        std::fs::write(
            node1.join("hugepages/hugepages-2048kB/nr_hugepages"),
            "200\n",
        )
        .unwrap();
        std::fs::write(
            node1.join("hugepages/hugepages-1048576kB/nr_hugepages"),
            "4\n",
        )
        .unwrap();
        std::fs::write(
            node1.join("meminfo"),
            "Node 1 MemTotal:       65536000 kB\nNode 1 MemFree:        32000000 kB\n",
        )
        .unwrap();

        let topo = NumaTopology::from_sysfs(base).unwrap();
        assert_eq!(topo.num_nodes(), 2);

        assert_eq!(topo.nodes[0].id, 0);
        assert_eq!(topo.nodes[0].cpus, BTreeSet::from([0, 1, 2, 3]));
        assert_eq!(topo.nodes[0].total_2m_hugepages, 100);
        assert_eq!(topo.nodes[0].total_1g_hugepages, 2);
        assert_eq!(topo.nodes[0].total_ram_mb, 64000);

        assert_eq!(topo.nodes[1].id, 1);
        assert_eq!(topo.nodes[1].cpus, BTreeSet::from([4, 5, 6, 7]));
        assert_eq!(topo.nodes[1].total_2m_hugepages, 200);
        assert_eq!(topo.nodes[1].total_1g_hugepages, 4);
        assert_eq!(topo.nodes[1].total_ram_mb, 64000);
    }

    /// Helper: build a 2-node topology with full control over all fields.
    fn two_node_topology_full(
        cpus0: BTreeSet<u32>,
        hp_2m_0: u32,
        hp_1g_0: u32,
        ram_mb_0: u32,
        cpus1: BTreeSet<u32>,
        hp_2m_1: u32,
        hp_1g_1: u32,
        ram_mb_1: u32,
    ) -> NumaTopology {
        NumaTopology {
            nodes: vec![
                NumaNode {
                    id: 0,
                    cpus: cpus0,
                    total_2m_hugepages: hp_2m_0,
                    total_1g_hugepages: hp_1g_0,
                    total_ram_mb: ram_mb_0,
                },
                NumaNode {
                    id: 1,
                    cpus: cpus1,
                    total_2m_hugepages: hp_2m_1,
                    total_1g_hugepages: hp_1g_1,
                    total_ram_mb: ram_mb_1,
                },
            ],
        }
    }

    /// Helper: build a 2-node topology for allocator tests (2M pages only).
    fn two_node_topology(
        cpus0: BTreeSet<u32>,
        hp0: u32,
        cpus1: BTreeSet<u32>,
        hp1: u32,
    ) -> NumaTopology {
        two_node_topology_full(cpus0, hp0, 0, 64000, cpus1, hp1, 0, 64000)
    }

    #[test]
    fn test_allocator_pack_first() {
        let topo = two_node_topology(
            BTreeSet::from([0, 1, 2, 3]),
            512,
            BTreeSet::from([4, 5, 6, 7]),
            512,
        );
        let mut alloc = NumaAllocator::new(topo);

        // 2 vCPUs fits on node 0 (4 available).
        let p = alloc.allocate(2, 256, None).unwrap();
        assert_eq!(p.node, 0);

        // 4 vCPUs does NOT fit on node 0 (only 2 left), should go to node 1.
        let p = alloc.allocate(4, 256, None).unwrap();
        assert_eq!(p.node, 1);
    }

    #[test]
    fn test_allocator_hint() {
        let topo = two_node_topology(
            BTreeSet::from([0, 1, 2, 3]),
            512,
            BTreeSet::from([4, 5, 6, 7]),
            512,
        );
        let mut alloc = NumaAllocator::new(topo);

        // Hint node 1 even though node 0 has room.
        let p = alloc.allocate(2, 256, Some(1)).unwrap();
        assert_eq!(p.node, 1);
    }

    #[test]
    fn test_allocator_reject_when_full() {
        let topo = two_node_topology(
            BTreeSet::from([0, 1, 2, 3]),
            512,
            BTreeSet::from([4, 5, 6, 7]),
            512,
        );
        let mut alloc = NumaAllocator::new(topo);

        // Fill both nodes (4 vCPUs each).
        alloc.allocate(4, 256, None).unwrap();
        alloc.allocate(4, 256, None).unwrap();

        // Next allocation should fail.
        assert!(alloc.allocate(1, 64, None).is_err());
    }

    #[test]
    fn test_allocator_release() {
        let topo = two_node_topology(
            BTreeSet::from([0, 1, 2, 3]),
            512,
            BTreeSet::from([4, 5, 6, 7]),
            512,
        );
        let mut alloc = NumaAllocator::new(topo);

        // Fill node 0.
        alloc.allocate(4, 1024, None).unwrap();

        // Node 0 is full; next would go to node 1.
        let p = alloc.allocate(1, 64, None).unwrap();
        assert_eq!(p.node, 1);

        // Release node 0.
        alloc.release(0, 4, 1024, HugePageSize::Size2M);

        // Now node 0 has room again.
        let p = alloc.allocate(1, 64, None).unwrap();
        assert_eq!(p.node, 0);
    }

    #[test]
    fn test_allocator_memory_overflow() {
        let topo = two_node_topology(
            BTreeSet::from([0, 1, 2, 3]),
            256, // 256 hugepages = 512 MB
            BTreeSet::from([4, 5, 6, 7]),
            256,
        );
        let mut alloc = NumaAllocator::new(topo);

        // Request 600 MB — each node only has 512 MB.
        assert!(alloc.allocate(1, 600, None).is_err());
    }

    #[test]
    fn test_cpuset_string() {
        let cpus = BTreeSet::from([8, 9, 10, 11]);
        assert_eq!(format_cpuset(&cpus), "8-11");
    }

    #[test]
    fn test_from_sysfs_reads_1g_hugepages_and_memtotal() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        let node0 = base.join("node0");
        std::fs::create_dir_all(node0.join("hugepages/hugepages-2048kB")).unwrap();
        std::fs::create_dir_all(node0.join("hugepages/hugepages-1048576kB")).unwrap();
        std::fs::write(node0.join("cpulist"), "0-3\n").unwrap();
        std::fs::write(
            node0.join("hugepages/hugepages-2048kB/nr_hugepages"),
            "100\n",
        )
        .unwrap();
        std::fs::write(
            node0.join("hugepages/hugepages-1048576kB/nr_hugepages"),
            "4\n",
        )
        .unwrap();
        std::fs::write(
            node0.join("meminfo"),
            "Node 0 MemTotal:       65536000 kB\nNode 0 MemFree:        32000000 kB\n",
        )
        .unwrap();

        let topo = NumaTopology::from_sysfs(base).unwrap();
        assert_eq!(topo.nodes[0].total_2m_hugepages, 100);
        assert_eq!(topo.nodes[0].total_1g_hugepages, 4);
        assert_eq!(topo.nodes[0].total_ram_mb, 64000); // 65536000 kB / 1024
    }

    #[test]
    fn test_parse_memtotal_kb() {
        let meminfo = "Node 0 MemTotal:       131072000 kB\nNode 0 MemFree:        64000000 kB\n";
        assert_eq!(parse_memtotal_kb(meminfo).unwrap(), 131072000);
    }

    #[test]
    fn test_from_sysfs_no_1g_hugepages_dir() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        let node0 = base.join("node0");
        std::fs::create_dir_all(node0.join("hugepages/hugepages-2048kB")).unwrap();
        // No hugepages-1048576kB directory
        std::fs::write(node0.join("cpulist"), "0-3\n").unwrap();
        std::fs::write(
            node0.join("hugepages/hugepages-2048kB/nr_hugepages"),
            "100\n",
        )
        .unwrap();
        std::fs::write(
            node0.join("meminfo"),
            "Node 0 MemTotal:       65536000 kB\nNode 0 MemFree:        32000000 kB\n",
        )
        .unwrap();

        let topo = NumaTopology::from_sysfs(base).unwrap();
        assert_eq!(topo.nodes[0].total_1g_hugepages, 0);
    }

    #[test]
    fn test_allocator_selects_1g_pages() {
        let topo = two_node_topology_full(
            BTreeSet::from([0, 1, 2, 3]),
            512,
            4,
            64000,
            BTreeSet::from([4, 5, 6, 7]),
            512,
            4,
            64000,
        );
        let mut alloc = NumaAllocator::new(topo);

        // 2048 MB = 2 x 1024, fits in 2 x 1G pages
        let p = alloc.allocate(2, 2048, None).unwrap();
        assert_eq!(p.node, 0);
        assert_eq!(p.hugepage_size, HugePageSize::Size1G);
    }

    #[test]
    fn test_allocator_falls_back_to_2m() {
        let topo = two_node_topology_full(
            BTreeSet::from([0, 1, 2, 3]),
            2048, // 2048 * 2M = 4096 MB capacity
            4,
            64000,
            BTreeSet::from([4, 5, 6, 7]),
            2048,
            4,
            64000,
        );
        let mut alloc = NumaAllocator::new(topo);

        // 1500 MB is not a multiple of 1024 -> must use 2M
        let p = alloc.allocate(2, 1500, None).unwrap();
        assert_eq!(p.hugepage_size, HugePageSize::Size2M);
    }

    #[test]
    fn test_allocator_1g_exhausted_falls_back_to_2m() {
        let topo = two_node_topology_full(
            BTreeSet::from([0, 1, 2, 3]),
            2048, // 2048 * 2M = 4096 MB capacity via 2M pages
            2,
            64000, // only 2 x 1G pages
            BTreeSet::from([4, 5, 6, 7]),
            2048,
            2,
            64000,
        );
        let mut alloc = NumaAllocator::new(topo);

        // Use up all 1G pages on node 0 (2 pages = 2048 MB)
        let p = alloc.allocate(1, 2048, None).unwrap();
        assert_eq!(p.hugepage_size, HugePageSize::Size1G);

        // Next 1G-aligned request should fall back to 2M (no 1G pages left)
        let p = alloc.allocate(1, 2048, None).unwrap();
        assert_eq!(p.node, 0); // still fits on node 0 via 2M pages
        assert_eq!(p.hugepage_size, HugePageSize::Size2M);
    }

    #[test]
    fn test_allocator_release_1g_pages() {
        let topo = two_node_topology_full(
            BTreeSet::from([0, 1, 2, 3]),
            512,
            2,
            64000,
            BTreeSet::from([4, 5, 6, 7]),
            512,
            2,
            64000,
        );
        let mut alloc = NumaAllocator::new(topo);

        // Use all 1G pages
        alloc.allocate(1, 2048, None).unwrap();

        // Release them
        alloc.release(0, 1, 2048, HugePageSize::Size1G);

        // Should be able to allocate 1G again
        let p = alloc.allocate(1, 2048, None).unwrap();
        assert_eq!(p.hugepage_size, HugePageSize::Size1G);
    }
}
