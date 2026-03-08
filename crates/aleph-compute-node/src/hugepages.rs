use std::path::Path;

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::numa::NumaTopology;

/// Compute how many 2 MiB hugepages should be allocated on a NUMA node.
pub fn compute_2m_budget(
    node_ram_mb: u32,
    existing_1g_pages: u32,
    headroom_mb: u32,
    per_node_cap_mb: u32,
) -> u32 {
    let node_cap = per_node_cap_mb.min(node_ram_mb.saturating_sub(headroom_mb));
    let reserved_1g_mb = existing_1g_pages * 1024;
    let budget_mb = node_cap.saturating_sub(reserved_1g_mb);
    budget_mb / 2
}

/// Write the desired 2M hugepage count to a NUMA node's sysfs path.
/// Returns the actual count after write (may be less due to fragmentation).
pub fn allocate_2m_pages_on_node(sysfs_base: &Path, node_id: u32, count: u32) -> Result<u32> {
    let hp_path = sysfs_base
        .join(format!("node{node_id}"))
        .join("hugepages/hugepages-2048kB/nr_hugepages");

    std::fs::write(&hp_path, count.to_string())
        .with_context(|| format!("failed to write hugepage count to {}", hp_path.display()))?;

    let actual: u32 = std::fs::read_to_string(&hp_path)
        .with_context(|| {
            format!(
                "failed to read back hugepage count from {}",
                hp_path.display()
            )
        })?
        .trim()
        .parse()
        .with_context(|| format!("failed to parse hugepage count from {}", hp_path.display()))?;

    Ok(actual)
}

/// Allocate 2M hugepages across all NUMA nodes based on the budget formula.
/// Updates `topology.nodes[i].total_2m_hugepages` with the actual allocated count.
pub fn allocate_hugepages(
    topology: &mut NumaTopology,
    headroom_mb: u32,
    global_limit_mb: Option<u32>,
    sysfs_base: &Path,
) -> Result<()> {
    let num_nodes = topology.nodes.len() as u32;
    let total_ram: u32 = topology.nodes.iter().map(|n| n.total_ram_mb).sum();
    let effective_limit =
        global_limit_mb.unwrap_or(total_ram.saturating_sub(headroom_mb * num_nodes));
    let per_node_cap = effective_limit / num_nodes;

    for node in &mut topology.nodes {
        let desired = compute_2m_budget(
            node.total_ram_mb,
            node.total_1g_hugepages,
            headroom_mb,
            per_node_cap,
        );

        if desired == 0 {
            info!(node = node.id, "no 2M hugepage budget for this node");
            continue;
        }

        match allocate_2m_pages_on_node(sysfs_base, node.id, desired) {
            Ok(actual) => {
                if actual < desired {
                    warn!(
                        node = node.id,
                        desired,
                        actual,
                        "allocated fewer 2M hugepages than requested (fragmentation?)"
                    );
                } else {
                    info!(node = node.id, count = actual, "allocated 2M hugepages");
                }
                node.total_2m_hugepages = actual;
            }
            Err(e) => {
                warn!(node = node.id, error = %e, "failed to allocate 2M hugepages");
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_2m_budget_basic() {
        // Node: 64000 MB RAM, 4x1G pages, 4096 MB headroom, 60000 MB per-node cap
        // node_cap = min(60000, 64000 - 4096) = min(60000, 59904) = 59904
        // reserved_1g = 4 * 1024 = 4096
        // budget_mb = 59904 - 4096 = 55808
        // pages = 55808 / 2 = 27904
        let budget = compute_2m_budget(64000, 4, 4096, 60000);
        assert_eq!(budget, 27904);
    }

    #[test]
    fn test_compute_2m_budget_1g_exceeds_cap() {
        // More 1G pages than the cap allows -> 0 budget for 2M
        let budget = compute_2m_budget(64000, 100, 4096, 60000);
        assert_eq!(budget, 0);
    }

    #[test]
    fn test_compute_2m_budget_no_1g_pages() {
        // No 1G pages, simple case
        // node_cap = min(60000, 64000 - 4096) = 59904
        // budget_mb = 59904
        // pages = 59904 / 2 = 29952
        let budget = compute_2m_budget(64000, 0, 4096, 60000);
        assert_eq!(budget, 29952);
    }

    #[test]
    fn test_compute_2m_budget_headroom_exceeds_ram() {
        // Headroom larger than node RAM -> 0 budget
        let budget = compute_2m_budget(2048, 0, 4096, 60000);
        assert_eq!(budget, 0);
    }

    #[test]
    fn test_allocate_2m_pages_sysfs() {
        let dir = tempfile::tempdir().unwrap();
        let node_path = dir.path().join("node0/hugepages/hugepages-2048kB");
        std::fs::create_dir_all(&node_path).unwrap();
        std::fs::write(node_path.join("nr_hugepages"), "0\n").unwrap();

        let actual = allocate_2m_pages_on_node(dir.path(), 0, 100).unwrap();
        // In test, writing to a regular file always "succeeds" -- readback returns what we wrote
        assert_eq!(actual, 100);
    }

    #[test]
    fn test_allocate_hugepages_full() {
        use crate::numa::{NumaNode, NumaTopology};
        use std::collections::BTreeSet;

        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();

        // Create mock sysfs for 2 nodes
        for node_id in 0..2 {
            let hp_path = base.join(format!("node{node_id}/hugepages/hugepages-2048kB"));
            std::fs::create_dir_all(&hp_path).unwrap();
            std::fs::write(hp_path.join("nr_hugepages"), "0\n").unwrap();
        }

        let mut topo = NumaTopology {
            nodes: vec![
                NumaNode {
                    id: 0,
                    cpus: BTreeSet::from([0, 1, 2, 3]),
                    total_2m_hugepages: 0,
                    total_1g_hugepages: 2,
                    total_ram_mb: 64000,
                },
                NumaNode {
                    id: 1,
                    cpus: BTreeSet::from([4, 5, 6, 7]),
                    total_2m_hugepages: 0,
                    total_1g_hugepages: 0,
                    total_ram_mb: 64000,
                },
            ],
        };

        allocate_hugepages(&mut topo, 4096, None, base).unwrap();

        // Both nodes should have 2M pages allocated
        // effective_limit = total_ram - headroom*nodes = 128000 - 8192 = 119808
        // per_node_cap = 119808 / 2 = 59904
        // Node 0: cap = min(59904, 64000-4096) = 59904, reserved_1g = 2048, budget = 57856/2 = 28928
        // Node 1: cap = min(59904, 64000-4096) = 59904, reserved_1g = 0, budget = 59904/2 = 29952
        assert_eq!(topo.nodes[0].total_2m_hugepages, 28928);
        assert_eq!(topo.nodes[1].total_2m_hugepages, 29952);
    }
}
