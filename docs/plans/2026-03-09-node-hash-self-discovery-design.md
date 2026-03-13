# Node Hash Self-Discovery

## Problem

Nodes on the Aleph Cloud network do not know their own node ID (the node hash
from the corechannel aggregate). This prevents the scheduler agent from looking
up its own allocation plan or validating that it's running the correct VMs.

## Background

- The corechannel aggregate (`data.corechannel.nodes`) lists CCNs, each with a
  `resource_nodes` array of hashes.
- Each resource node hash is the `item_hash` of a `create-resource-node` POST
  message on the Aleph network.
- That message contains: `sender` (owner ETH address),
  `content.content.details.address` (node URL), `content.content.details.name`,
  `content.content.details.type`.
- The scheduler agent currently has zero node identity awareness.

## Design

### Configuration

New CLI args / env vars for `aleph-scheduler-agent`:

| Flag | Env var | Purpose |
|------|---------|---------|
| `--owner-address <ETH>` | `OWNER_ADDRESS` | Operator's ETH address (used to register the CRN) |
| `--node-hash <HASH>` | `NODE_HASH` | Hardcoded override, skips auto-discovery |
| `--domain-name <DOMAIN>` | `DOMAIN_NAME` | Node's public domain (like Aleph VM's `ALEPH_VM_DOMAIN_NAME`) |

The node constructs its public URL as `https://<DOMAIN_NAME>` for validation.

### Three paths to set identity

1. **Auto-discovery** — `--owner-address` + `--domain-name` configured. Node
   queries the Aleph API, matches by owner + URL, caches the result.
2. **CLI subcommand** — `aleph-scheduler-agent set-node-hash <HASH>`. Writes
   to cache file and signals the running process. One-time manual operation.
3. **Startup flag** — `--node-hash <HASH>`. Always explicit, no file, no API.

Priority: `--node-hash` flag > cached file > auto-discovery.

### Auto-discovery flow

At startup (and periodically every 5 minutes if not yet resolved):

1. **If `--node-hash` is set** — use it directly.
2. **If cached hash exists** (`<state-dir>/node-hash`) — use it. Schedule
   background re-validation after 1 hour.
3. **If `--owner-address` is set** — query Aleph API:
   - `GET <connector-url>/api/v0/posts.json?addresses=<owner_addr>&types=corechan-operation`
   - Filter results for `content.content.action == "create-resource-node"`.
   - For each result, validate that `content.content.details.address` matches
     `https://<DOMAIN_NAME>`.
   - **1 match** — `item_hash` is the node hash. Cache it.
   - **0 matches** — node not registered (or URL mismatch). Log info, retry in
     5 minutes.
   - **Multiple matches** — log all (hash + name + URL), use none, tell operator
     to use `--node-hash` or `set-node-hash` to disambiguate.
4. **If neither is set** — log warning, operate without identity.

### `set-node-hash` subcommand

```
aleph-scheduler-agent set-node-hash <HASH>
```

1. Write hash to `<state-dir>/node-hash`.
2. Read PID from `<state-dir>/scheduler-agent.pid`.
3. Send SIGHUP to the running process (if any).
4. Print confirmation.

### SIGHUP handling

The scheduler agent:

- Writes a PID file to `<state-dir>/scheduler-agent.pid` on startup.
- Registers a SIGHUP handler that re-reads `<state-dir>/node-hash`.
- Cleans up the PID file on shutdown.

SIGHUP is a standard Unix "reload config" signal and can be reused for other
config reloads in the future.

### File-based cache

- Location: `<state-dir>/node-hash` (plain text, just the hex hash).
- Written on successful auto-discovery.
- Read on startup (before attempting API calls).
- Cleared if background re-validation finds the registration disappeared.

### Error handling

| Scenario | Behavior |
|----------|----------|
| Cached hash exists on startup | Use it, background re-validate after 1h |
| Cache miss + `--owner-address` set | Query API, validate URL, cache on success, retry every 5 min |
| `--node-hash` hardcoded | Use directly, no caching, no API calls |
| API unreachable | Log warning, retry later, operate without identity |
| Multiple CRNs for address, all URL-matched | Log all, use none, suggest `--node-hash` |
| 1 CRN found but URL mismatch | Log mismatch, use none |
| Registration disappears (re-validation) | Clear cache, log error, re-enter discovery loop |
| No identity configured | Log warning, node runs but can't fetch allocation plan |

### URL matching

The `details.address` field in the registration is an HTTPS URL (e.g.,
`https://vm5.alephvision.eu/`). The node constructs its own URL as
`https://<DOMAIN_NAME>`. Comparison must normalize trailing slashes since the
spec examples include them inconsistently.

### Immutability of registrations

Per the corechan-operation spec, **no update operation exists**. To change a
CRN's URL or name, operators must `drop-node` + `create-resource-node`, which
produces a new node hash. This means:

- URL matching against registrations is reliable (URLs don't change in-place).
- If the operator re-registers, the old cached hash becomes stale. Background
  re-validation detects this: the old hash's message will show as dropped, the
  cache is cleared, and discovery finds the new registration.

### What the node does with the hash

Once discovered, the node hash is available to:

- Look up the node's allocation from the scheduler.
- Validate that received allocations match network expectations.
- Expose in `GET /health` response for operator visibility.
