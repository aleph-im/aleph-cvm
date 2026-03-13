# CVM Migration Design

**Goal**: Fast migration of confidential VMs between nodes when a source node is winding down, leveraging spare capacity on destination nodes.

## Reality Check: TEE Live Migration

True live migration (transferring encrypted memory state between hosts) is **not available** for either AMD SEV-SNP or Intel TDX. Both have spec'd the cryptographic protocols (AMD's Migration Agent + VMRK; Intel's MigTD), but neither is implemented in mainline QEMU or the Linux kernel. Google Cloud, AWS, and Azure all handle confidential VM maintenance by stopping and restarting — not live migrating.

**We cannot transfer VM memory state. We must design around this constraint.**

## Architecture: Fast Recreate

Since we can't move running memory state, migration means: **start a new identical VM on the destination, then cut over traffic and kill the old one.** The goal is to make this fast enough that downtime is acceptable (seconds, not minutes).

```
Timeline:

t0: Scheduler decides to evacuate source node
t1: Scheduler sends MigrateVm to destination node
t2: Destination node creates VM (boots, app starts)  ← bulk of latency
t3: Destination VM passes health check
t4: Scheduler updates routing (DNS/service registry)
t5: Scheduler sends DeleteVm to source node

Downtime window: t4 only (~1s for DNS/service registry update)
```

### What Makes This Fast

1. **Disk images are already cached.** The VolumeCache on each node downloads rootfs/runtime/code once. If the destination node already has the same images (common when running the same workloads), VM creation skips all downloads.

2. **Boot is fast.** Our initrd boots in ~1 second (busybox init, static IP, dm-verity mount). The app startup time dominates.

3. **Overlap period.** The old VM keeps running while the new one boots. There's no downtime during boot — only during the routing cutover.

## Networking Model

### Current State: Node-Local IPv6 Pools

Each node owns a public IPv6 range (e.g., node-A has `2001:db8:a::/48`, node-B has `2001:db8:b::/48`). VMs get addresses from their host node's pool. These addresses **cannot follow the VM** across nodes — upstream routing delivers `2001:db8:a::/48` to node-A only.

When a VM migrates from node-A to node-B, it gets a **new IPv6 from node-B's pool**. The old address becomes unreachable once node-A deletes the VM.

**For stateless apps this is fine.** Clients discover the VM's address via DNS or a service registry. The scheduler updates the record to the new address after migration. Clients reconnect (they should already handle transient failures). No session state is lost because there is none.

### Future: Gateway Layer with Stable VIPs

A future gateway/router layer will provide stable virtual IPs that survive migration. The gateway maintains a mapping of `stable_vip → current_node_address` and forwards traffic accordingly. This is a separate project.

### Dual-Address Model (Future)

Once the gateway layer exists, VMs will have two addresses:
- **Stable VIP** (via gateway): survives migration, primary address for clients
- **Node-local address** (direct): always available, useful as fallback if the gateway is down, for monitoring, or for node-local debugging

Both addresses reach the same VM. The node-local address changes on migration; the VIP doesn't.

### How NDP Proxy Works (Reference)

IPv6 uses NDP (Neighbor Discovery Protocol) instead of ARP. When the upstream router wants to reach `2001:db8:a::42`, it sends a Neighbor Solicitation ("who has this IP?"). Our NDP proxy (ndppd) runs on the host's external interface and answers on behalf of the VM: "I do, here's my MAC." The router sends packets to the host, which forwards them to the VM via the bridge/TAP interface.

NDP proxy only works for addresses that the upstream network routes to this host. That's why node-A can't proxy addresses from node-B's range — the upstream router would never send those solicitations to node-A in the first place.

## Components

### 1. Migration Coordinator (in scheduler-agent)

The scheduler-agent on each node already handles allocation reconciliation. Migration extends this with a cross-node coordination flow:

```
Source scheduler-agent                    Destination scheduler-agent
         │                                          │
         │  POST /control/migrate                   │
         │  { vm_id, source_node, config, volumes } │
         │ ──────────────────────────────────────── >│
         │                                          │
         │                                    Create VM (same config)
         │                                    New IPv6 from local pool
         │                                    Wait for health check
         │                                          │
         │  200 OK { new_vm_ipv4, new_vm_ipv6 }     │
         │< ──────────────────────────────────────── │
         │                                          │
    Update DNS/service registry                     │
    (old_ipv6 → new_ipv6)                           │
    Delete old VM                                   │
         │                                          │
```

### 2. New gRPC/HTTP Endpoints

#### On scheduler-agent: `POST /control/migrate`

Request:
```json
{
  "vm_id": "abc123",
  "source_node": "node-1.example.com",
  "message": { /* full ExecutableMessage */ },
  "port_forwards": [
    { "host_port": 0, "vm_port": 8080, "protocol": "tcp" }
  ]
}
```

Response (after VM is healthy):
```json
{
  "vm_id": "abc123",
  "ipv4": "10.0.100.2",
  "ipv6": "2001:db8:b::17/128",
  "port_forwards": [
    { "host_port": 10042, "vm_port": 8080, "protocol": "tcp" }
  ],
  "status": "running"
}
```

Note: no `requested_ipv6` field — the destination node allocates from its own pool. The caller (scheduler) is responsible for updating DNS/service registry with the new address.

This endpoint:
1. Downloads any missing volumes (should be cached already)
2. Creates the VM via the compute node gRPC (same as normal allocation)
3. Waits for a health check (TCP connect to the app port, or attestation handshake)
4. Returns the new VM's network info (including the newly assigned IPv6)

#### On compute-node gRPC: Health check polling

Add a simple readiness check: after CreateVm, poll TCP connect to the VM's app port (e.g., 8080) until it responds. This tells the coordinator the app is ready to serve traffic.

```protobuf
rpc WaitReady(WaitReadyRequest) returns (WaitReadyResponse);

message WaitReadyRequest {
  string vm_id = 1;
  uint32 port = 2;       // TCP port to probe
  uint32 timeout_secs = 3; // max wait time
}

message WaitReadyResponse {
  bool ready = 1;
  uint32 elapsed_ms = 2;
}
```

### 3. Attestation Implications

**The new VM has a different attestation identity.** It generates a fresh TLS keypair and a fresh SEV-SNP attestation report. The measurement (OVMF + kernel + initrd + cmdline with roothash) is identical since we're using the same build artifacts, but:

- The TLS certificate is different (new keypair)
- The attestation report is signed by the destination host's VCEK (different chip)
- Clients that pinned the old TLS cert must re-verify

**This is fine for our architecture.** Clients already verify attestation on every connection (or on reconnect). The measurement match proves it's the same code. The platform certificate chain proves it's running on a genuine AMD SEV-SNP platform. The specific chip doesn't matter.

Clients should:
1. Verify the measurement matches the expected value (same as before)
2. Verify the attestation report is signed by a valid AMD certificate chain
3. Not pin on VCEK or TLS cert fingerprint across migrations

### 4. State Considerations

**Stateless workloads**: Just recreate. No data loss. Client reconnects to new address.

**Stateful workloads**: The application must handle this. Options:
- External state store (database, object storage) — app is effectively stateless
- Persistent volume migration: copy local disks from source to destination before creating the new VM. The new VM boots with the same data. This requires a disk transfer mechanism (direct node-to-node copy, or snapshot to shared storage).
- Application-level checkpointing: app writes state to a known location, new instance reads it on startup

Persistent volume migration is a future enhancement — the mechanism (direct channel, snapshot-based, shared storage) is TBD. The migration endpoint can be extended to accept pre-staged disk paths on the destination.

## What We Don't Build

- **True live migration**: Not available for SEV-SNP or TDX. If mainline support ships, we adopt it.
- **Memory state transfer**: Can't read encrypted memory from outside the VM.
- **IPv6 address portability**: Addresses come from per-node pools; they don't move. The future gateway layer handles stable VIPs.
- **Persistent volume migration (v1)**: v1 assumes stateless workloads or external state stores. Disk transfer is a future enhancement.
- **Connection draining**: We don't implement graceful drain (waiting for in-flight requests). Existing connections break at cutover. Applications should handle reconnection.

## Migration Flow (Detailed)

### Happy Path

1. **Scheduler decides to evacuate** node-A (maintenance, scaling down, etc.)
2. For each VM on node-A:
   a. Scheduler picks destination node-B with spare capacity
   b. Scheduler calls `POST /control/migrate` on node-B's scheduler-agent with the VM's full config
   c. Node-B's scheduler-agent downloads any missing volumes (usually cached), calls CreateVm on its compute node
   d. Node-B's compute node boots the VM, sets up networking (new IPv6 from node-B's pool), dm-verity
   e. Scheduler-agent waits for readiness (TCP probe on app port)
   f. Node-B responds with new VM's network info (new IPv6)
   g. Scheduler updates DNS/service registry: old IPv6 → new IPv6
   h. Scheduler calls DeleteVm on node-A
3. Once all VMs are evacuated, node-A can be shut down

### Failure Cases

| Failure | Handling |
|---------|----------|
| Destination VM fails to boot | Return error, scheduler picks another node |
| Destination VM boots but app doesn't respond | Timeout, return error, scheduler retries on different node |
| Source node dies during migration | Destination VM is already running, just update routing |
| Destination node dies after creation but before cutover | Source VM is still running, scheduler retries |
| Network partition during migration | Timeout, both VMs may run briefly (safe if stateless) |

### Timeouts

| Phase | Expected | Timeout |
|-------|----------|---------|
| Volume download (cached) | 0s | 30s |
| Volume download (uncached) | varies | 300s |
| VM boot (initrd + dm-verity) | ~1s | 10s |
| App readiness | depends on app | 60s (configurable) |
| DNS/service registry update | <1s | 5s |
| Total (cached, fast app) | ~2-3s | — |

## Changes Summary

| Component | Change |
|-----------|--------|
| `scheduler-agent` | Add `POST /control/migrate` endpoint |
| `compute.proto` | Add `WaitReady` RPC |
| `compute-node` | Implement TCP readiness probe |
| `scheduler-agent` | Health-check polling after CreateVm |

## Future Enhancements

- **Gateway layer with stable VIPs**: Stable virtual IPs that survive migration, routed through a gateway/router layer. VMs keep their VIP; only the gateway's backend mapping changes.
- **Persistent volume migration**: Transfer local disks from source to destination before creating the new VM. Mechanism TBD (direct node-to-node copy, snapshot to shared storage).
- **Pre-warming**: When a node is marked for wind-down, start booting VMs on destinations before traffic cutover. Reduces downtime to just the routing switch.
- **Batch evacuation**: Migrate multiple VMs in parallel across different destination nodes.
- **Connection draining**: Signal the source VM to stop accepting new connections and wait for in-flight requests to complete before deleting.
- **True TEE live migration**: When mainline support ships for SEV-SNP/TDX, adopt it for zero-downtime migration with memory state transfer.
