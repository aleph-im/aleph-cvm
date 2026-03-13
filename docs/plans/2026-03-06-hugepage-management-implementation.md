# Hugepage Management Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** The orchestrator owns the full hugepage lifecycle — reading 1G pages, allocating 2M pages at startup, and selecting the right page size per VM.

**Architecture:** Extend `NumaTopology`/`NumaAllocator` to track two hugepage pools (1G + 2M) per node. Add a `hugepages` module for sysfs read/write operations. Plumb a `HugePageSize` field through `VmConfig` into QEMU args. Wire up budget calculation and 2M allocation at startup via new CLI flags.

**Tech Stack:** Rust, sysfs, QEMU memfd backend, clap CLI

---

### Task 1: Add `HugePageSize` type to `aleph-tee`

**Files:**
- Modify: `crates/aleph-tee/src/types.rs:1-66`

**Step 1: Add the enum and field**

Add above the `VmConfig` struct:

```rust
/// Hugepage size used for the VM's memory backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HugePageSize {
    /// 2 MiB hugepages (default, always available).
    Size2M,
    /// 1 GiB hugepages (requires boot-time reservation).
    Size1G,
}
```

Add to `VmConfig`:

```rust
    /// Hugepage size for this VM's memory backend (set by the allocator, not the user).
    #[serde(default)]
    pub hugepage_size: Option<HugePageSize>,
```

**Step 2: Run existing tests to verify nothing breaks**

Run: `cargo test -p aleph-tee`
Expected: all existing tests PASS (new field defaults to `None` via serde)

**Step 3: Commit**

```
git add crates/aleph-tee/src/types.rs
git commit -m "feat(types): add HugePageSize enum and field to VmConfig"
```

---

### Task 2: Update QEMU args to use hugepage size from config

**Files:**
- Modify: `crates/aleph-tee/src/sev_snp/qemu.rs:24-54`

**Step 1: Write the failing test**

Add to the `tests` module in `qemu.rs`:

```rust
    #[test]
    fn test_memory_backend_1g_hugepages() {
        let mut config = make_config(4096, None);
        config.numa_node = Some(0);
        config.hugepage_size = Some(crate::types::HugePageSize::Size1G);
        let args = sev_snp_qemu_args(&config, DEFAULT_OVMF_PATH);

        let mem_arg = args
            .iter()
            .find(|a| a.contains("memory-backend-memfd"))
            .expect("should have memory-backend-memfd arg");

        assert!(
            mem_arg.contains("hugetlbsize=1G"),
            "should use 1G hugepages but got: {mem_arg}"
        );
    }

    #[test]
    fn test_memory_backend_no_hugepages() {
        let mut config = make_config(1024, None);
        config.hugepage_size = None;
        let args = sev_snp_qemu_args(&config, DEFAULT_OVMF_PATH);

        let mem_arg = args
            .iter()
            .find(|a| a.contains("memory-backend-memfd"))
            .expect("should have memory-backend-memfd arg");

        assert!(
            !mem_arg.contains("hugetlb"),
            "should NOT have hugetlb when hugepage_size is None: {mem_arg}"
        );
    }
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p aleph-tee test_memory_backend_1g_hugepages test_memory_backend_no_hugepages`
Expected: FAIL — currently hardcoded to `hugetlbsize=2M`

**Step 3: Update `sev_snp_qemu_args` to use config.hugepage_size**

Replace the `memfd_opts` construction (lines 27-37) with:

```rust
    let hugetlb_opts = match config.hugepage_size {
        Some(crate::types::HugePageSize::Size1G) => ",hugetlb=on,hugetlbsize=1G",
        Some(crate::types::HugePageSize::Size2M) => ",hugetlb=on,hugetlbsize=2M",
        None => "",
    };

    let numa_opts = if let Some(node) = config.numa_node {
        format!(",host-nodes={node},policy=bind")
    } else {
        String::new()
    };

    let memfd_opts = format!(
        "memory-backend-memfd,id=ram1,size={}M,share=true{hugetlb_opts}{numa_opts}",
        config.memory_mb
    );
```

**Step 4: Fix existing tests**

Tests that previously expected `hugetlb=on,hugetlbsize=2M` by default now need to set
`config.hugepage_size = Some(HugePageSize::Size2M)`. Update `make_config` to set
`hugepage_size: None`, and update tests `test_memory_backend_numa_binding` and
`test_memory_backend_no_numa` to explicitly set `Size2M`.

**Step 5: Run all tests**

Run: `cargo test -p aleph-tee`
Expected: all PASS

**Step 6: Commit**

```
git add crates/aleph-tee/src/sev_snp/qemu.rs
git commit -m "feat(qemu): derive hugetlbsize from VmConfig.hugepage_size"
```

---

### Task 3: Extend `NumaTopology` to read 1G hugepages and per-node MemTotal

**Files:**
- Modify: `crates/aleph-compute-node/src/numa.rs:7-77`

**Step 1: Write the failing test**

Add a test that expects the new fields:

```rust
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
        ).unwrap();
        std::fs::write(
            node0.join("hugepages/hugepages-1048576kB/nr_hugepages"),
            "4\n",
        ).unwrap();
        std::fs::write(
            node0.join("meminfo"),
            "Node 0 MemTotal:       65536000 kB\nNode 0 MemFree:        32000000 kB\n",
        ).unwrap();

        let topo = NumaTopology::from_sysfs(base).unwrap();
        assert_eq!(topo.nodes[0].total_2m_hugepages, 100);
        assert_eq!(topo.nodes[0].total_1g_hugepages, 4);
        assert_eq!(topo.nodes[0].total_ram_mb, 64000); // 65536000 kB ≈ 64000 MB
    }
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p aleph-compute-node test_from_sysfs_reads_1g`
Expected: FAIL — fields don't exist

**Step 3: Update `NumaNode` struct**

```rust
pub struct NumaNode {
    pub id: u32,
    pub cpus: BTreeSet<u32>,
    /// Number of 2 MiB hugepages on this node.
    pub total_2m_hugepages: u32,
    /// Number of 1 GiB hugepages on this node (boot-time reserved).
    pub total_1g_hugepages: u32,
    /// Total RAM on this node in MB.
    pub total_ram_mb: u32,
}
```

**Step 4: Update `from_sysfs` to read the new fields**

After reading 2M hugepages, add:

```rust
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
```

Add a helper function:

```rust
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
```

**Step 5: Rename `total_hugepages` → `total_2m_hugepages` everywhere**

Update all references in the file (struct construction, tests, allocator).

**Step 6: Run all tests**

Run: `cargo test -p aleph-compute-node`
Expected: all PASS (update existing test fixtures to include the new sysfs files)

**Step 7: Commit**

```
git add crates/aleph-compute-node/src/numa.rs
git commit -m "feat(numa): read 1G hugepages and per-node MemTotal from sysfs"
```

---

### Task 4: Dual-pool NUMA allocator with page-size selection

**Files:**
- Modify: `crates/aleph-compute-node/src/numa.rs:94-166`

**Step 1: Write the failing tests**

```rust
    #[test]
    fn test_allocator_selects_1g_pages() {
        let topo = two_node_topology_full(
            BTreeSet::from([0, 1, 2, 3]), 512, 4, 64000,
            BTreeSet::from([4, 5, 6, 7]), 512, 4, 64000,
        );
        let mut alloc = NumaAllocator::new(topo);

        // 2048 MB = 2 * 1024, fits in 2 × 1G pages
        let p = alloc.allocate(2, 2048, None).unwrap();
        assert_eq!(p.node, 0);
        assert_eq!(p.hugepage_size, HugePageSize::Size1G);
    }

    #[test]
    fn test_allocator_falls_back_to_2m() {
        let topo = two_node_topology_full(
            BTreeSet::from([0, 1, 2, 3]), 512, 4, 64000,
            BTreeSet::from([4, 5, 6, 7]), 512, 4, 64000,
        );
        let mut alloc = NumaAllocator::new(topo);

        // 1500 MB is not a multiple of 1024 → must use 2M
        let p = alloc.allocate(2, 1500, None).unwrap();
        assert_eq!(p.hugepage_size, HugePageSize::Size2M);
    }

    #[test]
    fn test_allocator_1g_exhausted_falls_back_to_2m() {
        let topo = two_node_topology_full(
            BTreeSet::from([0, 1, 2, 3]), 512, 2, 64000, // only 2 × 1G pages
            BTreeSet::from([4, 5, 6, 7]), 512, 2, 64000,
        );
        let mut alloc = NumaAllocator::new(topo);

        // Use up all 1G pages on node 0
        let p = alloc.allocate(1, 2048, None).unwrap();
        assert_eq!(p.hugepage_size, HugePageSize::Size1G);

        // Next 1G-aligned request should fall back to 2M (no 1G pages left on node 0)
        // but node 0 still has 2M pages
        let p = alloc.allocate(1, 2048, None).unwrap();
        assert_eq!(p.node, 0);
        assert_eq!(p.hugepage_size, HugePageSize::Size2M);
    }
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p aleph-compute-node test_allocator_selects`
Expected: FAIL

**Step 3: Update `NumaPlacement` to include hugepage size**

```rust
pub struct NumaPlacement {
    pub node: u32,
    pub cpuset: String,
    pub hugepage_size: HugePageSize,
}
```

Import `HugePageSize` from `aleph_tee::types::HugePageSize`.
Add `aleph-tee` as a dependency of `aleph-compute-node` if not already present.

**Step 4: Rewrite `NumaAllocator` for dual pools**

```rust
pub struct NumaAllocator {
    topology: NumaTopology,
    allocated_vcpus: Vec<u32>,
    /// Per-node allocated 2M hugepages.
    allocated_2m_pages: Vec<u32>,
    /// Per-node allocated 1G hugepages.
    allocated_1g_pages: Vec<u32>,
}

impl NumaAllocator {
    pub fn new(topology: NumaTopology) -> Self {
        let n = topology.nodes.len();
        Self {
            topology,
            allocated_vcpus: vec![0; n],
            allocated_2m_pages: vec![0; n],
            allocated_1g_pages: vec![0; n],
        }
    }

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

            // Try 1G pages first if memory is a multiple of 1024 MB
            if memory_mb % 1024 == 0 {
                let pages_needed = memory_mb / 1024;
                let available_1g = node.total_1g_hugepages.saturating_sub(self.allocated_1g_pages[idx]);
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

            // Fall back to 2M pages
            let pages_needed = memory_mb / 2;
            let available_2m = node.total_2m_hugepages.saturating_sub(self.allocated_2m_pages[idx]);
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

    pub fn release(&mut self, node: u32, vcpus: u32, memory_mb: u32, hugepage_size: HugePageSize) {
        if let Some(idx) = self.topology.nodes.iter().position(|n| n.id == node) {
            self.allocated_vcpus[idx] = self.allocated_vcpus[idx].saturating_sub(vcpus);
            match hugepage_size {
                HugePageSize::Size1G => {
                    let pages = memory_mb / 1024;
                    self.allocated_1g_pages[idx] = self.allocated_1g_pages[idx].saturating_sub(pages);
                }
                HugePageSize::Size2M => {
                    let pages = memory_mb / 2;
                    self.allocated_2m_pages[idx] = self.allocated_2m_pages[idx].saturating_sub(pages);
                }
            }
        }
    }
}
```

**Step 5: Update test helpers**

Add `two_node_topology_full` helper that includes 1G pages and RAM:

```rust
    fn two_node_topology_full(
        cpus0: BTreeSet<u32>, hp_2m_0: u32, hp_1g_0: u32, ram_mb_0: u32,
        cpus1: BTreeSet<u32>, hp_2m_1: u32, hp_1g_1: u32, ram_mb_1: u32,
    ) -> NumaTopology {
        NumaTopology {
            nodes: vec![
                NumaNode { id: 0, cpus: cpus0, total_2m_hugepages: hp_2m_0, total_1g_hugepages: hp_1g_0, total_ram_mb: ram_mb_0 },
                NumaNode { id: 1, cpus: cpus1, total_2m_hugepages: hp_2m_1, total_1g_hugepages: hp_1g_1, total_ram_mb: ram_mb_1 },
            ],
        }
    }
```

Update existing `two_node_topology` to call `two_node_topology_full` with `1g=0, ram=64000`.

**Step 6: Run all tests**

Run: `cargo test -p aleph-compute-node`
Expected: all PASS

**Step 7: Commit**

```
git add crates/aleph-compute-node/src/numa.rs
git commit -m "feat(numa): dual-pool allocator with 1G/2M page-size selection"
```

---

### Task 5: Add hugepage sysfs module

**Files:**
- Create: `crates/aleph-compute-node/src/hugepages.rs`
- Modify: `crates/aleph-compute-node/src/lib.rs` (add `pub mod hugepages;`)

This module handles writing 2M hugepage counts to per-node sysfs paths at startup.

**Step 1: Write the test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_2m_budget() {
        // Node has 64000 MB RAM, 4 × 1G pages, headroom 4096 MB, global limit 120000 MB, 2 nodes
        let budget = compute_2m_budget(
            64000, // node_ram_mb
            4,     // existing_1g_pages
            4096,  // headroom_mb
            60000, // per_node_cap_mb (120000 / 2)
        );
        // per_node_cap = min(60000, 64000 - 4096) = min(60000, 59904) = 59904
        // reserved_1g = 4 * 1024 = 4096
        // budget_2m_mb = 59904 - 4096 = 55808
        // pages = 55808 / 2 = 27904
        assert_eq!(budget, 27904);
    }

    #[test]
    fn test_compute_2m_budget_1g_exceeds_cap() {
        // More 1G pages than the cap allows → 0 budget for 2M
        let budget = compute_2m_budget(64000, 100, 4096, 60000);
        assert_eq!(budget, 0);
    }

    #[test]
    fn test_allocate_2m_pages_sysfs() {
        let dir = tempfile::tempdir().unwrap();
        let node_path = dir.path().join("node0/hugepages/hugepages-2048kB");
        std::fs::create_dir_all(&node_path).unwrap();
        std::fs::write(node_path.join("nr_hugepages"), "0\n").unwrap();

        let actual = allocate_2m_pages_on_node(dir.path(), 0, 100).unwrap();
        // In test, writing to a file always "succeeds" — readback returns what we wrote
        assert_eq!(actual, 100);
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test -p aleph-compute-node test_compute_2m_budget`
Expected: FAIL — module doesn't exist

**Step 3: Implement the module**

```rust
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
        .with_context(|| format!("failed to read back hugepage count from {}", hp_path.display()))?
        .trim()
        .parse()
        .with_context(|| format!("failed to parse hugepage count from {}", hp_path.display()))?;

    Ok(actual)
}

/// Allocate 2M hugepages across all NUMA nodes based on the budget formula.
///
/// Returns the actual number of 2M pages allocated per node (indexed by topology order).
pub fn allocate_hugepages(
    topology: &mut NumaTopology,
    headroom_mb: u32,
    global_limit_mb: Option<u32>,
    sysfs_base: &Path,
) -> Result<()> {
    let num_nodes = topology.nodes.len() as u32;
    let total_ram: u32 = topology.nodes.iter().map(|n| n.total_ram_mb).sum();
    let effective_limit = global_limit_mb
        .unwrap_or(total_ram.saturating_sub(headroom_mb * num_nodes));
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
```

**Step 4: Register the module**

Add `pub mod hugepages;` to `crates/aleph-compute-node/src/lib.rs`.

**Step 5: Run all tests**

Run: `cargo test -p aleph-compute-node`
Expected: all PASS

**Step 6: Commit**

```
git add crates/aleph-compute-node/src/hugepages.rs crates/aleph-compute-node/src/lib.rs
git commit -m "feat(hugepages): sysfs-based 2M hugepage allocation module"
```

---

### Task 6: Plumb hugepage size through VmManager and persistence

**Files:**
- Modify: `crates/aleph-compute-node/src/vm/manager.rs:30-41,262-267,380-395,589-596`
- Modify: `crates/aleph-compute-node/src/persistence.rs:15-27`

**Step 1: Add `hugepage_size` to `VmHandle` and `PersistedVm`**

In `VmHandle`:
```rust
    hugepage_size: Option<HugePageSize>,
```

In `PersistedVm`:
```rust
    #[serde(default)]
    pub hugepage_size: Option<HugePageSize>,
```

**Step 2: Update `create_vm` to use placement's hugepage_size**

After the NUMA allocation block (line ~267), add:

```rust
        config.hugepage_size = Some(placement.hugepage_size);
```

And store it in the handle and persisted VM.

**Step 3: Update `delete_vm` to pass hugepage_size to release**

The `release` call (line ~394) needs the hugepage size:

```rust
        if let Some(node) = handle.numa_node {
            if let Some(hp_size) = handle.hugepage_size {
                let mut numa = self.numa.lock().await;
                numa.release(node, handle.config.vcpus, handle.config.memory_mb, hp_size);
            }
        }
```

**Step 4: Update `recover_vms` similarly**

The allocation call in recovery (line ~592) should use the persisted hugepage_size.

**Step 5: Run all tests**

Run: `cargo test -p aleph-compute-node`
Expected: all PASS

**Step 6: Commit**

```
git add crates/aleph-compute-node/src/vm/manager.rs crates/aleph-compute-node/src/persistence.rs
git commit -m "feat(manager): plumb hugepage_size through VM lifecycle and persistence"
```

---

### Task 7: Add CLI flags and startup hugepage allocation

**Files:**
- Modify: `crates/aleph-compute-node/src/main.rs:17-71,86-155`

**Step 1: Add CLI flags**

```rust
    /// Global memory limit for hugepage-backed VM memory.
    /// Auto-distributed across NUMA nodes. Default: all RAM minus headroom.
    #[arg(long, value_parser = parse_size_mb)]
    memory_limit: Option<u32>,

    /// Per-node OS memory headroom in MB (same on every node).
    #[arg(long, default_value = "4096", value_parser = parse_size_mb)]
    hugepage_headroom: u32,
```

Add a size parser that accepts "4G", "4096M", "4096" (plain MB):

```rust
fn parse_size_mb(s: &str) -> Result<u32, String> {
    let s = s.trim();
    if let Some(gb) = s.strip_suffix('G').or_else(|| s.strip_suffix("GB")) {
        gb.trim().parse::<u32>().map(|g| g * 1024).map_err(|e| e.to_string())
    } else if let Some(mb) = s.strip_suffix('M').or_else(|| s.strip_suffix("MB")) {
        mb.trim().parse::<u32>().map_err(|e| e.to_string())
    } else {
        s.parse::<u32>().map_err(|e| e.to_string())
    }
}
```

**Step 2: Wire up hugepage allocation after NUMA detection**

After `NumaTopology::detect()` (line ~140), add:

```rust
    // Allocate 2M hugepages across NUMA nodes
    let sysfs_base = std::path::Path::new("/sys/devices/system/node");
    hugepages::allocate_hugepages(
        &mut numa_topology,
        cli.hugepage_headroom,
        cli.memory_limit,
        sysfs_base,
    )?;
```

Note: `numa_topology` must become `mut`.

**Step 3: Run `cargo build` to verify it compiles**

Run: `cargo build -p aleph-compute-node`
Expected: compiles cleanly

**Step 4: Commit**

```
git add crates/aleph-compute-node/src/main.rs
git commit -m "feat(cli): add --memory-limit and --hugepage-headroom flags"
```

---

### Task 8: Update demo scripts to remove ad-hoc hugepage allocation

**Files:**
- Modify: `scripts/demo.sh:260-280`
- Modify: `scripts/demo-encrypted.sh` (same section)

**Step 1: Replace the hugepage section**

Replace the "Huge pages" section with a note that the orchestrator handles it:

```bash
# ── 4. Huge pages ────────────────────────────────────────────────────────────

header "Huge pages"
info "Hugepage allocation is handled by aleph-compute-node at startup"
```

**Step 2: Test the demo script still runs**

Run the demo script manually (if on a machine with SEV-SNP hardware), or verify it parses:
`bash -n scripts/demo.sh`

**Step 3: Commit**

```
git add scripts/demo.sh scripts/demo-encrypted.sh
git commit -m "refactor(demo): remove ad-hoc hugepage allocation (now in orchestrator)"
```

---

### Task 9: Final integration verification

**Step 1: Run the full test suite**

Run: `cargo test --workspace`
Expected: all PASS

**Step 2: Run clippy**

Run: `cargo clippy --workspace -- -D warnings`
Expected: no warnings

**Step 3: Commit any fixes if needed**
