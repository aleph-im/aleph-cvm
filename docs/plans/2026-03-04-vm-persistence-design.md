# VM Persistence via Systemd Transient Units

## Problem

When `aleph-compute-node` (the orchestrator) dies or restarts, all VM state is lost. QEMU processes become orphans with no way to reconnect. VMs must be recreated from scratch by the scheduler-agent.

## Solution

Decouple QEMU process lifecycle from the orchestrator by running each VM as a **systemd transient service**. Persist VM metadata to JSON files on disk. On restart, the orchestrator recovers state from disk and reconnects to still-running VMs.

## Architecture

```
                    aleph-compute-node (orchestrator)
                         │
                         │ creates/queries/stops (D-Bus)
                         ▼
                    systemd
                    ├── aleph-cvm-vm@{id1}.service  →  QEMU process 1
                    ├── aleph-cvm-vm@{id2}.service  →  QEMU process 2
                    └── aleph-cvm-vm@{id3}.service  →  QEMU process 3

    /var/lib/aleph-cvm/vms/
    ├── {id1}.json   (VmConfig + IP + ports + created_at)
    ├── {id2}.json
    └── {id3}.json
```

## Key Design Decisions

### All VMs survive orchestrator restarts

No persistent/ephemeral distinction at the orchestrator level. Every VM runs under systemd and survives orchestrator crashes/restarts.

### Systemd transient units for process management

Each QEMU process runs as a systemd transient service (`aleph-cvm-vm@{vm_id}.service`), created via D-Bus. Transient units:
- Survive orchestrator restarts (QEMU keeps running)
- Do NOT survive host reboots (transient = runtime-only)
- Get `Restart=on-failure` for automatic crash recovery
- Get `KillMode=mixed` and `TimeoutStopSec=30` for clean shutdown

### JSON files for metadata persistence

One JSON file per VM at `/var/lib/aleph-cvm/vms/{vm_id}.json` containing:
- `VmConfig` (kernel, initrd, disks, vcpus, memory, TEE config)
- IP assignment (v4 + v6)
- Port forward mappings
- Creation timestamp

Written on VM creation, updated on port forward changes, deleted on VM deletion.

### Scheduler-agent is authoritative for reconciliation

The orchestrator never decides on its own whether to restart or clean up a stopped VM. The scheduler-agent's reconciliation loop handles this:
- Scheduler sends allocation list
- VMs in allocation but not running → recreate (reusing saved config/ports from JSON)
- VMs running but not in allocation → delete

## Startup Recovery Flow

```
Orchestrator starts
    │
    ├── Scan /var/lib/aleph-cvm/vms/*.json
    │   └── For each JSON file:
    │       ├── Query systemd: is aleph-cvm-vm@{id}.service active?
    │       │   ├── Yes → Load as Running, reconnect QMP socket
    │       │   └── No  → Load as Stopped (no process, but config preserved)
    │       └── Insert into in-memory VmHandle map
    │
    ├── Restore network state for Running VMs
    │   └── Skip TAP/bridge/nftables setup (still intact)
    │
    ├── Rebuild network state for Stopped VMs
    │   └── Recreate TAP/nftables if scheduler-agent asks to restart
    │
    └── Ready to serve gRPC API
```

## VM Creation Flow (revised)

1. Allocate IP, create TAP, set up nftables (unchanged)
2. Build QEMU command-line args (unchanged)
3. **New:** Serialize VM metadata to JSON file
4. **New:** Create systemd transient unit via D-Bus with the QEMU command
5. **New:** Start the unit (replaces direct `Child` process spawn)
6. Insert `VmHandle` into in-memory map (now without `Child`, with unit name)
7. Connect QMP socket for health checks

## VM Deletion Flow (revised)

1. **New:** Stop systemd unit via D-Bus (replaces killing child process)
2. Clean up port forwards, nftables, NDP, TAP (unchanged)
3. **New:** Delete JSON file
4. Remove from in-memory map

## Reboot Scenario

After host reboot:
- Transient systemd units are gone (QEMU not running)
- JSON files in `/var/lib/aleph-cvm/vms/` survive (persistent storage)
- Orchestrator loads VMs as `Stopped`
- Scheduler-agent reconnects, sends current allocations
- Orchestrator recreates VMs that are still allocated (reusing saved port mappings)
- Orchestrator cleans up JSON files for VMs no longer allocated

## Network Partition / Split-Brain

If the node loses network and the scheduler migrates VMs elsewhere:
- VMs keep running locally under systemd
- When network recovers, scheduler-agent reconnects and sends updated allocations
- Orchestrator kills VMs not in the allocation list
- Brief duplicate window is accepted (scheduler converges quickly)

## Components Changed

### New: `systemd` module (`aleph-compute-node/src/systemd.rs`)
- Uses `zbus` crate for async D-Bus communication
- `create_vm_unit(vm_id, qemu_args)` → creates and starts transient unit
- `stop_vm_unit(vm_id)` → stops and removes transient unit
- `is_unit_active(vm_id)` → queries unit state
- `list_vm_units()` → lists all `aleph-cvm-vm@*` units

### New: `persistence` module (`aleph-compute-node/src/persistence.rs`)
- `save_vm(vm_id, metadata)` → write JSON to `/var/lib/aleph-cvm/vms/`
- `load_all_vms()` → read all JSON files, return Vec of metadata
- `delete_vm(vm_id)` → remove JSON file

### Modified: `QemuProcess`
- No longer holds `std::process::Child`
- Holds systemd unit name instead
- `wait_or_kill()` → delegates to systemd stop
- Drop impl → delegates to systemd stop

### Modified: `VmManager`
- `create_vm()` → uses systemd module instead of direct spawn
- `delete_vm()` → uses systemd module instead of killing child
- New `recover_vms()` called on startup → loads JSON + queries systemd

### Unchanged
- gRPC API surface
- QEMU command-line building (`qemu/args.rs`)
- Network setup (TAP, bridge, nftables)
- Scheduler-agent (unaware of the change)

## Dependencies

- `zbus` crate for D-Bus communication with systemd
- `serde_json` (already in use) for JSON persistence

## Systemd Unit Properties

```
Type=simple
ExecStart=/usr/bin/qemu-system-x86_64 {args...}
Restart=on-failure
RestartSec=5s
KillMode=mixed
TimeoutStopSec=30
```
