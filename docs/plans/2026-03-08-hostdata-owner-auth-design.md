# HOSTDATA Owner Authentication

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Prevent frontrunning attacks on secret injection by cryptographically binding each CVM to its owner's public key via SNP HOSTDATA, and requiring a challenge-response signature before accepting secrets.

**Architecture:** The user's Aleph account public key (secp256k1) is hashed into HOSTDATA at VM launch. The attest-agent gates secret injection on a challenge-response: the caller must sign a nonce with the private key corresponding to the HOSTDATA-bound public key. SSH authorized keys ride as payload in the same injection request.

**Tech Stack:** Rust, secp256k1 (k256 crate), SHA-256, SEV-SNP HOSTDATA, QEMU `host-data=` parameter.

---

## Problem

The `POST /confidential/inject-secret` endpoint is one-shot (first caller wins), but there is no authentication — anyone who can reach the endpoint can claim the VM. Since endpoints are public, a malicious user can monitor for freshly-booted VMs and frontrun the legitimate owner by injecting their own secrets first.

The one-shot guard prevents *double* claiming, but doesn't ensure the *right* user claims.

## Solution: HOSTDATA + Challenge-Response

### SNP HOSTDATA

SEV-SNP attestation reports include a `host_data` field: 32 bytes set by the hypervisor at VM launch via QEMU's `host-data=` parameter on the `sev-snp-guest` object. This field is included in the hardware-signed attestation report — neither the guest nor the host can change it after launch.

We set `HOSTDATA = SHA-256(owner_pubkey)` where `owner_pubkey` is the user's Aleph account public key (compressed secp256k1, 33 bytes).

### Challenge-Response Protocol

```
User                          VM (attest-agent)                   Orchestrator
  |                                  |                                  |
  |-- "launch VM, pubkey=0xABC..." --|--------------------------------->|
  |                                  |   QEMU: host-data=SHA256(0xABC...)
  |                                  |<---------------------------------|
  |                                  | (boot, read own HOSTDATA from    |
  |                                  |  /dev/sev-guest attestation)     |
  |                                  |                                  |
  |-- TLS connect ------------------>|                                  |
  |<-- cert with attestation report -|                                  |
  |  (verify AMD chain, measurement, |                                  |
  |   host_data == SHA256(my_pubkey))|                                  |
  |                                  |                                  |
  |-- GET /confidential/challenge -->|                                  |
  |<-- { nonce: "0xDEAD..." } ------|                                  |
  |                                  |                                  |
  |-- POST /confidential/inject-secret                                  |
  |   { pubkey: "0xABC...",          |                                  |
  |     signature: sign(nonce, sk),  |                                  |
  |     secrets: {                   |                                  |
  |       luks_passphrase: "...",    |                                  |
  |       ssh_authorized_keys: "..." |                                  |
  |     }                            |                                  |
  |   }                              |                                  |
  |                                  |                                  |
  |   (agent checks:                 |                                  |
  |    SHA256(pubkey) == HOSTDATA    |                                  |
  |    verify(sig, nonce, pubkey)    |                                  |
  |    one-shot guard)               |                                  |
  |                                  |                                  |
  |<-- 200 OK -----------------------|                                  |
```

### Security Properties

| Property | Mechanism |
|----------|-----------|
| **Anti-frontrunning** | Attacker doesn't have the private key matching HOSTDATA, so signature verification fails |
| **Owner verification** | User checks `host_data == SHA-256(my_pubkey)` in the attestation report, confirming the VM was launched for them |
| **Replay prevention** | Challenge nonce is random, single-use, and short-lived (5 min TTL) |
| **One-shot** | Existing `Mutex<Option<()>>` guard still prevents double-injection |
| **No orchestrator trust** | Orchestrator knows the public key but not the private key — it cannot frontrun |

### Threat Model

**Malicious user (honest hypervisor):** Fully mitigated. Attacker can connect, verify attestation (it passes — same measurement), request a challenge, but cannot produce a valid signature without the owner's private key.

**Malicious hypervisor:** HOSTDATA is hypervisor-controlled, so a malicious hypervisor could set it to any value. But:
- It cannot read secrets (VM memory is encrypted)
- It cannot forge attestation reports (AMD signature chain)
- Setting wrong HOSTDATA causes the user to reject the VM (DoS, already in the accepted threat model)
- Setting HOSTDATA to the attacker's own pubkey lets the attacker inject secrets into a VM they can't read the memory of — useless

**Replay of challenge-response:** The nonce is generated per-request and stored in a single-slot `Mutex<Option<Nonce>>`. A new challenge overwrites the previous one, so only the most recent nonce is valid. Combined with the one-shot injection guard, replay is not viable.

## Key Types

### Aleph Account Key (secp256k1)

Users already have an Aleph account key (Ethereum-compatible secp256k1 keypair). This key is available in both the CLI and the web UI, making it the natural choice for the challenge-response signature.

- **Public key:** 33 bytes (compressed) or 65 bytes (uncompressed). We use compressed format for HOSTDATA binding: `HOSTDATA = SHA-256(compressed_pubkey)`.
- **Signature:** ECDSA over secp256k1, signing `SHA-256(nonce)`. The signature is a standard DER-encoded or (r, s) pair.

### SSH Keys (Payload Only)

SSH public keys are submitted at VM creation time and injected as a secret alongside the LUKS passphrase. They don't participate in the crypto flow — they're just data riding the attested, owner-authenticated channel.

The attest-agent writes them to `/tmp/secrets/ssh_authorized_keys`. The init script copies them to the rootfs at `/root/.ssh/authorized_keys` after mounting.

## Changes

### 1. `aleph-tee` — Expose `host_data` in AttestationReport

The `sev` crate's `AttestationReport` already has `host_data: [u8; 32]`. We need to:

- Add `host_data: [u8; 32]` to `aleph_tee::types::AttestationReport`
- Extract it in `report.rs::parse_sev_snp_report()` via a new `extract_host_data()` function
- Include it in JSON serialization (hex-encoded, like the other byte fields)

This is a data-model change that flows through the entire stack: the attestation report JSON returned by `GET /.well-known/attestation` will now include `host_data`, and the client can verify it.

### 2. `aleph-tee` / QEMU — Pass `host-data` to QEMU

Add `host_data: Option<[u8; 32]>` to `TeeConfig`. When present, append `host-data=<hex>` to the `sev-snp-guest` QEMU object string in `sev_snp_qemu_args()`.

QEMU accepts `host-data=<hex-string>` on the `sev-snp-guest` object:
```
-object sev-snp-guest,id=sev0,...,host-data=<64-hex-chars>
```

### 3. `proto/compute.proto` — Add `host_data` to CreateVmRequest

```protobuf
message TeeConfig {
  string backend = 1;
  string policy = 2;
  bytes host_data = 3;  // 32 bytes: SHA-256(owner_pubkey), set by orchestrator
}
```

The gRPC service maps this to `TeeConfig.host_data` and passes it through to QEMU args.

### 4. `aleph-attest-agent` — Challenge endpoint + authenticated injection

#### New endpoint: `GET /confidential/challenge`

Returns a fresh random 32-byte nonce, hex-encoded:

```json
{ "nonce": "a1b2c3..." }
```

The nonce is stored in `AppState` in a `Mutex<Option<Challenge>>` where `Challenge` contains the nonce bytes and an expiry timestamp (5 minutes from creation). Only one active challenge at a time — requesting a new one replaces the old one.

#### Modified endpoint: `POST /confidential/inject-secret`

New request format:

```json
{
  "pubkey": "<hex-encoded compressed secp256k1 pubkey>",
  "signature": "<hex-encoded ECDSA signature over the challenge nonce>",
  "secrets": {
    "luks_passphrase": "...",
    "ssh_authorized_keys": "ssh-ed25519 AAAA... user@host"
  }
}
```

Verification steps (before existing validation):
1. **Read own HOSTDATA**: The agent requests its own attestation report at startup and caches `host_data` from it. If HOSTDATA is all zeros (no owner binding), skip authentication (backwards-compatible for dev/test).
2. **Verify challenge**: Check that a valid (non-expired) challenge exists. Consume it (set to `None`) to prevent replay.
3. **Verify pubkey binding**: `SHA-256(pubkey) == cached_host_data`. Reject if mismatch.
4. **Verify signature**: Recover/verify the secp256k1 ECDSA signature over the challenge nonce using the provided pubkey. Reject if invalid.
5. **Proceed with existing flow**: Validate secrets, write files, set one-shot flag.

#### Startup: Cache own HOSTDATA

At startup, before starting the HTTP server, the agent requests its own attestation report (with arbitrary report_data) and caches the `host_data` field. This avoids requesting a report on every injection attempt.

### 5. `aleph-attest-cli` — Authenticated secret injection

The `inject-secret` subcommand gains `--signing-key` (hex-encoded secp256k1 private key or path to key file):

```bash
aleph-attest-cli inject-secret \
    --url https://<vm-ip>:8443 \
    --measurement <expected-hex> \
    --signing-key <hex-or-path> \
    --secret luks_passphrase=<password> \
    --secret ssh_authorized_keys="ssh-ed25519 AAAA..."
```

Flow:
1. Connect to attest-agent, verify attestation (existing)
2. Extract `host_data` from attestation report, verify `== SHA-256(my_pubkey)` — bail if mismatch (wrong VM)
3. `GET /confidential/challenge` to obtain nonce
4. Sign nonce with the secp256k1 private key
5. `POST /confidential/inject-secret` with pubkey, signature, and secrets
6. Print confirmation

### 6. `aleph-attest-cli` — Expose `host_data` in verification output

The `attest` and `fresh-attest` commands should print `host_data` alongside `measurement` so users can verify VM ownership:

```
Attestation valid: true
Measurement: abcdef...
Host data:   123456...   (== SHA-256 of your pubkey? verify manually)
```

### 7. `nix/init.sh` — Write SSH authorized keys to rootfs

After mounting the rootfs (dm-verity or LUKS), if `/tmp/secrets/ssh_authorized_keys` exists:

```sh
if [ -f /tmp/secrets/ssh_authorized_keys ]; then
    mkdir -p /mnt/root/root/.ssh
    cp /tmp/secrets/ssh_authorized_keys /mnt/root/root/.ssh/authorized_keys
    chmod 600 /mnt/root/root/.ssh/authorized_keys
    chmod 700 /mnt/root/root/.ssh
fi
```

This runs before `chroot /mnt/root /sbin/init`, so SSH is ready when the rootfs boots.

### 8. `aleph-scheduler-agent` — Pass owner pubkey through scheduling

The scheduler-agent receives VM allocation messages from the Aleph network. These messages already contain the sender's account address. The scheduler needs to:

1. Include the sender's public key (or derive HOSTDATA from the address) in the `CreateVmRequest`
2. Pass it through `TeeConfig.host_data`

The exact derivation (pubkey from message vs. address-based lookup) depends on the Aleph message format — this is the part that's TBD and depends on the Aleph SDK integration.

## What We Don't Build

- **Multi-key ownership**: One pubkey per VM. No threshold signatures or key rotation.
- **Derived keys**: HOSTDATA binds directly to the account pubkey, not a derived key. Derivation schemes can be added later without changing the protocol.
- **Revocation**: No mechanism to revoke a claimed VM and re-assign it. The VM must be destroyed and re-created.
- **Automatic SSH key injection for non-root users**: Only `/root/.ssh/authorized_keys` is populated. Multi-user SSH setup is the rootfs's responsibility.
- **Web UI signing flow**: The web UI will need to sign challenges using the user's Aleph account key (via browser wallet or similar). This is a frontend concern outside the scope of this design.

## Dependencies

New crate dependency for the attest-agent:
- `k256 = "0.13"` with features `["ecdsa", "sha256"]` — secp256k1 ECDSA signature verification

The `k256` crate is from the RustCrypto project, pure-Rust, no OpenSSL dependency, and widely used for Ethereum-compatible signatures.

## No Backwards Compatibility

There is no production deployment of the current system. Owner authentication is mandatory — there is no unauthenticated fallback. `host_data` must be set at VM launch, `--signing-key` is required for `inject-secret`, and the agent rejects injection requests that fail signature verification.
