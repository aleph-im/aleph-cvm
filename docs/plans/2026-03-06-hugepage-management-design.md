# Hugepage Management Design

**Date:** 2026-03-06
**Status:** Draft

## Context

SEV-SNP VMs require hugepage-backed memory. The `pvalidate` instruction runs
per-page at boot — 2 MiB hugepages reduce this from ~60 ms to <1 ms per page,
and 1 GiB hugepages reduce it further. Currently, hugepage allocation is handled
ad-hoc by the demo script (`scripts/demo.sh`), which hardcodes a global page
count. The orchestrator reads the existing pool but never manages it.

**Goal:** The orchestrator (`aleph-compute-node`) owns the full hugepage
lifecycle on a dedicated CVM host.

## Design

### Two-tier hugepage model

- **1 GiB hugepages** — reserved at boot via kernel command line
  (`hugepagesz=1G hugepages=N`). The orchestrator reads the per-node count
  at startup but never allocates or frees them.
- **2 MiB hugepages** — allocated by the orchestrator at startup to fill the
  remaining memory budget. Written to per-node sysfs paths.

### Per-node budget calculation

At startup, for each NUMA node:

```
node_ram_mb      = MemTotal from /sys/devices/system/node/nodeN/meminfo
headroom_mb      = configured headroom (default 4096 MB), same on every node
node_cap_mb      = min(global_limit_mb / num_nodes, node_ram_mb - headroom_mb)
reserved_1g_mb   = node_1g_pages * 1024
budget_2m_mb     = max(0, node_cap_mb - reserved_1g_mb)
pages_2m         = budget_2m_mb / 2
```

The orchestrator writes `pages_2m` to:
```
/sys/devices/system/node/nodeN/hugepages/hugepages-2048kB/nr_hugepages
```

It then reads back the actual count. If fewer pages were allocated than
requested (fragmentation), it logs a warning and operates with reduced capacity.

### Why per-node headroom (not global)

A global headroom (e.g. 4 GiB split across N nodes) shrinks per-node headroom
as nodes increase, risking kernel OOM. A fixed per-node headroom (e.g. 4 GiB on
every node) wastes some memory on multi-node systems but guarantees the OS
always has enough on each node.

### VM page size selection

Each VM uses a single hugepage size — no mixing within a VM.

- If `vm_memory_mb % 1024 == 0` **and** enough 1 GiB pages are free on the
  target NUMA node → use `hugetlbsize=1G`.
- Otherwise → use `hugetlbsize=2M`.

QEMU memory backend (unchanged shape, only `hugetlbsize` varies):
```
memory-backend-memfd,id=ram1,size={size}M,share=true,hugetlb=on,hugetlbsize={1G|2M}[,host-nodes=N,policy=bind]
```

### NUMA allocator changes

The `NumaAllocator` tracks two pools per node instead of one:

- `free_1g_pages[node]` — decremented/incremented as 1G-backed VMs are
  created/destroyed.
- `free_2m_pages[node]` — same for 2M-backed VMs.

The pack-first strategy is unchanged: try nodes in order, pick the first with
enough CPUs and memory (in the appropriate page-size pool).

### Configuration

| Flag | Default | Description |
|------|---------|-------------|
| `--memory-limit <SIZE>` | all RAM minus headroom | Global cap on hugepage-backed memory, auto-distributed across nodes |
| `--hugepage-headroom <SIZE>` | `4G` | Per-node OS reservation (same on every node) |

No per-node configuration. The operator does not need to know the NUMA topology.

### Startup failure handling

If the orchestrator cannot allocate the desired 2 MiB pages (fragmentation,
insufficient RAM), it logs a warning and starts with whatever it got. VMs that
don't fit the reduced pool are rejected at creation time. No hard failure at
startup.

### What happens without hugepages

If a node ends up with zero hugepages, VMs placed there fall back to regular
4 KiB pages (QEMU `memory-backend-memfd` without `hugetlb=on`). Boot is slower
(`pvalidate` per 4K page) but functional.
