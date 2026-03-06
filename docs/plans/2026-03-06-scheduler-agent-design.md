# Scheduler Agent Rework — Design

## Context

The `aleph-scheduler-agent` crate bridges the Aleph network scheduler and the local `aleph-compute-node` orchestrator. The current implementation is incomplete:

- Everything lives in a binary crate (no reusable SDK)
- Hand-rolled message types duplicate what `aleph-types` already provides
- A fabricated `POST /control/messages` endpoint that doesn't exist in the real protocol
- No admission control — the agent always tries to run whatever it's allocated
- Missing the three GET endpoints the scheduler polls to understand CRN state
- No operator tooling for runtime policy changes (kick tenants, adjust limits)

## Crate Structure

Single crate, split into lib + bin:

**SDK (`lib.rs` — public modules):**
- `adapter` — translates `InstanceContent`/`ProgramContent` (from `aleph-types`) → `CreateVmRequest` proto
- `allocations` — reconciliation logic (desired vs running)
- `volumes` — volume download + content-addressed cache
- `client` — gRPC client to compute-node over UDS
- `policy` — admission control: resource limits (from env), dynamic deny lists (from sidecar JSON)
- `status` — types + logic for the scheduler polling endpoints (`MachineUsage`, `Executions`, `CrnConfig`)

**Binary (`main.rs`):**
- `serve` subcommand — HTTP server (actix-web) with all endpoints
- Operator subcommands — `deny-vm`, `allow-vm`, `deny-address`, `allow-address`, `set-limit`, `show-policy`

**Deleted (phase 3):**
- `aleph::messages` module — replaced by `aleph-types` crate
- `POST /control/messages` endpoint

**New dependencies (phase 3):**
- `aleph-types = "0.5"` — message types, `ItemHash`
- `aleph-sdk = "0.5"` — `NodeHash`

## Configuration

**Static config (`.env`, `ALEPH_VM_` prefix — matches aleph-vm convention):**

```env
ALEPH_VM_MAX_VCPUS=64
ALEPH_VM_MAX_MEMORY_MB=131072
ALEPH_VM_MAX_DISK_MB=1048576
ALEPH_VM_ALLOCATION_TOKEN_HASH=151ba92...
ALEPH_VM_ENABLE_CONFIDENTIAL_COMPUTING=true
ALEPH_VM_CONNECTOR_URL=https://official.aleph.cloud
ALEPH_VM_CACHE_ROOT=/var/cache/aleph/vm
ALEPH_VM_IPV6_ADDRESS_POOL=...
ALEPH_VM_PAYMENT_RECEIVER_ADDRESS=...
ALEPH_VM_EVICTION_GRACE_PERIOD_SECS=300
```

**Dynamic deny lists (`/var/lib/aleph-cvm/deny.json`):**

```json
{
  "vm_hashes": ["hashXYZ", "hashABC"],
  "addresses": ["0xdeadbeef"]
}
```

The operator CLI modifies this file atomically (write tmp + rename), then calls `POST /control/policy/reload` on the running agent.

## HTTP Endpoints

### Scheduler dispatch + operator control

| Method | Path | Purpose |
|--------|------|---------|
| POST | `/control/allocations` | Receive allocation from scheduler |
| POST | `/control/allocation/notify` | Single VM start notification (PAYG) |
| POST | `/control/policy/reload` | Reload deny lists from disk |
| GET | `/health` | Health check |

### Scheduler polling

| Method | Path | Purpose |
|--------|------|---------|
| GET | `/about/usage/system` | CPU/memory/disk usage (from `/proc/*` + `statvfs`) |
| GET | `/about/executions/list` | Running VMs with networking (proxy `ListVms` gRPC) |
| GET | `/status/config` | Node capabilities (IPv6, confidential, GPU, payment) |

## Allocation Flow (POST /control/allocations)

1. Verify HMAC signature
2. Deserialize `Allocation { persistent_vms, instances, on_demand_vms }`
3. Reconcile: compare against running VMs → `to_start`, `to_stop`, `unchanged`
4. For each VM in `to_stop`: delete via compute-node gRPC
5. For each VM in `to_start`:
   - Check deny lists (VM hash, owner address)
   - Check resource limits (aggregate of running + already-admitted VMs vs max)
   - If denied: add to `failing` with reason, skip
   - Download volumes via `VolumeCache`
   - Translate to `CreateVmRequest` via adapter
   - Submit to compute-node gRPC
6. Return structured response:
```json
{
  "success": true,
  "successful": ["hash1", "hash2"],
  "failing": ["hash3"],
  "stopped": ["hash4"],
  "errors": {"hash3": "denied: vm hash blocklisted"}
}
```

## Eviction Flow

1. Operator: `aleph-scheduler-agent deny-vm hashXYZ`
2. CLI adds `hashXYZ` to `/var/lib/aleph-cvm/deny.json`
3. CLI calls `POST /control/policy/reload` on the running agent
4. Agent reloads deny lists, flags `hashXYZ` for eviction
5. Agent stops reporting `hashXYZ` in `GET /about/executions/list` — scheduler sees it as missing, begins rescheduling
6. After grace period (default 5 min, configurable via `ALEPH_VM_EVICTION_GRACE_PERIOD_SECS`), agent deletes the VM via compute-node gRPC

## Notify Flow (POST /control/allocation/notify)

1. Deserialize `VMNotification { instance: ItemHash }`
2. Run admission checks (deny lists, resources)
3. Fetch message content from Aleph connector
4. Translate + submit to compute-node
5. Return success/failure

## Testability

All SDK modules are pure logic testable without HTTP or gRPC:
- `adapter`: construct `InstanceContent` from `aleph-types`, verify `CreateVmRequest` output
- `allocations`: pass `Allocation` + running set, verify `ReconcileActions`
- `policy`: construct `Policy`, check various VMs against limits and deny lists
- `status`: verify serialization matches scheduler's expected `MachineUsage`/`CrnConfig` shapes

## Phases

### Phase 1: SDK/CLI Split (pure refactor, no behavior changes)

Extract library modules from the binary crate:
- Add `lib.rs` exporting public modules: `adapter`, `allocations`, `volumes`, `client`
- Move `connect_compute_node` from `main.rs` into `client` module
- Move existing `aleph::*` submodules and `adapter` into the library
- `main.rs` becomes a thin binary that imports from the library
- All existing tests continue to pass, no new functionality

### Phase 2: Scheduler Polling Endpoints (GET)

Add the three endpoints the scheduler polls:
- `GET /about/usage/system` — host resource usage from `/proc/*` + `statvfs`
- `GET /about/executions/list` — running VMs via `ListVms` gRPC, mapped to `HashMap<ItemHash, ExecutionInfo>`
- `GET /status/config` — node capabilities (static config from env)
- New `status` module in the SDK with types matching the scheduler's `CrnClient` expectations (`MachineUsage`, `Executions`, `CrnConfig`)

### Phase 3: Admission Control + Allocation Rework

- Replace `aleph::messages` with `aleph-types` crate, rework adapter for `InstanceContent`/`ProgramContent`
- New `policy` module: resource limits (from `.env`), dynamic deny lists (from sidecar JSON)
- Admission checks in allocation flow (deny lists, resource limits)
- Structured allocation response (`successful`, `failing`, `stopped`, `errors`)
- `POST /control/allocation/notify` endpoint
- `POST /control/policy/reload` endpoint
- Operator CLI subcommands (`deny-vm`, `allow-vm`, `deny-address`, `allow-address`, `set-limit`, `show-policy`)
- Eviction flow with grace period
- Delete `POST /control/messages` endpoint
