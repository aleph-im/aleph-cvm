use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};
use tracing::info;

/// A single NUMA node with its CPU set and hugepage count.
#[derive(Debug, Clone)]
pub struct NumaNode {
    /// NUMA node ID (e.g. 0, 1, …).
    pub id: u32,
    /// Set of logical CPU IDs belonging to this node.
    pub cpus: BTreeSet<u32>,
    /// Number of 2 MiB hugepages available on this node.
    pub total_hugepages: u32,
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

            let hp_path = node_path.join("hugepages/hugepages-2048kB/nr_hugepages");
            let total_hugepages: u32 = std::fs::read_to_string(&hp_path)
                .with_context(|| format!("failed to read hugepages for {name}"))?
                .trim()
                .parse()
                .with_context(|| format!("failed to parse hugepage count for {name}"))?;

            info!(node = id, ?cpus, total_hugepages, "detected NUMA node");
            nodes.push(NumaNode {
                id,
                cpus,
                total_hugepages,
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
}

/// Allocator that packs VMs onto NUMA nodes, trying node 0 first, then node 1, etc.
#[derive(Debug)]
pub struct NumaAllocator {
    topology: NumaTopology,
    /// Per-node count of currently allocated vCPUs.
    allocated_vcpus: Vec<u32>,
    /// Per-node count of currently allocated memory in MB.
    allocated_memory_mb: Vec<u32>,
}

impl NumaAllocator {
    /// Create a new allocator with zero allocations.
    pub fn new(topology: NumaTopology) -> Self {
        let n = topology.nodes.len();
        Self {
            topology,
            allocated_vcpus: vec![0; n],
            allocated_memory_mb: vec![0; n],
        }
    }

    /// Allocate vCPUs and memory on a NUMA node using pack-first strategy.
    ///
    /// If `hint` is `Some(node_id)`, only that specific node is tried.
    /// Otherwise, nodes are tried in order (0, 1, 2, …) and the first one
    /// with enough free CPUs and memory is selected.
    ///
    /// Memory capacity per node = `total_hugepages * 2` (2 MiB hugepages).
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
            let capacity_mb = node.total_hugepages * 2;
            let available_mb = capacity_mb.saturating_sub(self.allocated_memory_mb[idx]);

            if vcpus <= available_cpus && memory_mb <= available_mb {
                self.allocated_vcpus[idx] += vcpus;
                self.allocated_memory_mb[idx] += memory_mb;

                return Ok(NumaPlacement {
                    node: node.id,
                    cpuset: format_cpuset(&node.cpus),
                });
            }
        }

        anyhow::bail!("no NUMA node has enough resources for {vcpus} vCPUs and {memory_mb} MB")
    }

    /// Release previously allocated resources on a node (saturating subtract).
    pub fn release(&mut self, node: u32, vcpus: u32, memory_mb: u32) {
        if let Some(idx) = self.topology.nodes.iter().position(|n| n.id == node) {
            self.allocated_vcpus[idx] = self.allocated_vcpus[idx].saturating_sub(vcpus);
            self.allocated_memory_mb[idx] = self.allocated_memory_mb[idx].saturating_sub(memory_mb);
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
        std::fs::write(node0.join("cpulist"), "0-3\n").unwrap();
        std::fs::write(
            node0.join("hugepages/hugepages-2048kB/nr_hugepages"),
            "100\n",
        )
        .unwrap();

        // node1: cpus 4-7, 200 hugepages
        let node1 = base.join("node1");
        std::fs::create_dir_all(node1.join("hugepages/hugepages-2048kB")).unwrap();
        std::fs::write(node1.join("cpulist"), "4-7\n").unwrap();
        std::fs::write(
            node1.join("hugepages/hugepages-2048kB/nr_hugepages"),
            "200\n",
        )
        .unwrap();

        let topo = NumaTopology::from_sysfs(base).unwrap();
        assert_eq!(topo.num_nodes(), 2);

        assert_eq!(topo.nodes[0].id, 0);
        assert_eq!(topo.nodes[0].cpus, BTreeSet::from([0, 1, 2, 3]));
        assert_eq!(topo.nodes[0].total_hugepages, 100);

        assert_eq!(topo.nodes[1].id, 1);
        assert_eq!(topo.nodes[1].cpus, BTreeSet::from([4, 5, 6, 7]));
        assert_eq!(topo.nodes[1].total_hugepages, 200);
    }

    /// Helper: build a 2-node topology for allocator tests.
    fn two_node_topology(
        cpus0: BTreeSet<u32>,
        hp0: u32,
        cpus1: BTreeSet<u32>,
        hp1: u32,
    ) -> NumaTopology {
        NumaTopology {
            nodes: vec![
                NumaNode {
                    id: 0,
                    cpus: cpus0,
                    total_hugepages: hp0,
                },
                NumaNode {
                    id: 1,
                    cpus: cpus1,
                    total_hugepages: hp1,
                },
            ],
        }
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
        alloc.release(0, 4, 1024);

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
}
