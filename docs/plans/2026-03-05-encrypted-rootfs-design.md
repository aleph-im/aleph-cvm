# Encrypted Rootfs via Attested TLS

**Goal**: Support user-provided LUKS-encrypted rootfs images, with the decryption key delivered securely over the attested TLS channel — no host-mediated ceremony needed.

## Why This Is Better Than the aleph-vm Approach

AMD SEV (used by aleph-vm) has no in-guest attestation. The guest can't prove its identity to anyone — only the host can query the launch measurement via QMP. This forces a host-mediated ceremony: the user uploads session certificates, the host starts the VM in stopped mode, the user queries the host for the measurement, then the host injects the secret via QMP. The host orchestrates every step.

AMD SEV-SNP (used by aleph-cvm) adds `/dev/sev-guest` — the guest can generate attestation reports signed by the CPU's hardware key (VCEK). The guest proves itself directly to the user. This means the decryption key can travel over a TLS channel whose certificate is cryptographically bound to the attestation report. The host never sees the key, never mediates the exchange, and doesn't need special endpoints for the ceremony.

## Architecture

Two boot modes, selected by kernel cmdline:

| Mode | Kernel cmdline | Rootfs type | Flow |
|------|---------------|-------------|------|
| **dm-verity** (existing) | `roothash=<hash>` | Plain ext4 + hash tree | Mount immediately, verify integrity |
| **LUKS** (new) | `luks=1` | LUKS-encrypted image | Wait for key via attest-agent, unlock, mount |

These modes are **mutually exclusive**. LUKS images skip dm-verity. Integrity under LUKS is implicit — LUKS2 uses authenticated encryption (AES-XTS + HMAC). Wrong key = I/O errors, not a valid rootfs.

## Boot Flow (LUKS Mode)

```
Timeline:

t0: VM boots (OVMF → kernel → initrd)
t1: init.sh: mount /proc, /sys, /dev
t2: init.sh: configure networking (static IP or DHCP)
t3: init.sh: load dm-crypt kernel module
t4: init.sh: start attest-agent (HTTPS on port 8443)    ← BEFORE rootfs mount
t5: init.sh: wait for /tmp/secrets/luks_passphrase       ← blocks here

    --- user interaction ---
t6: User connects to attest-agent over HTTPS
t7: User verifies attestation (TLS cert → SNP report → measurement)
t8: User POSTs secret to /confidential/inject-secret
t9: attest-agent writes secret to /tmp/secrets/luks_passphrase

t10: init.sh: cryptsetup luksOpen /dev/vda cryptroot < /tmp/secrets/luks_passphrase
t11: init.sh: shred + delete /tmp/secrets/luks_passphrase
t12: init.sh: mount /dev/mapper/cryptroot /mnt/root
t13: init.sh: chroot /mnt/root /sbin/init &
t14: init.sh: wait (supervise children)
```

### Comparison with dm-verity mode

In dm-verity mode, init.sh skips steps t3-t11 and goes straight from networking to mounting the rootfs. The attest-agent starts after the rootfs mount (as it does today). The key difference in LUKS mode is that the attest-agent must start **before** the rootfs mount, because it needs to receive the decryption key.

## Secret Injection Endpoint

### `POST /confidential/inject-secret`

A new endpoint on the attest-agent that accepts a generic secret payload. The attest-agent writes each key-value pair to `/tmp/secrets/<key>`. init.sh watches for the specific key it needs (`luks_passphrase`).

Request:
```json
{
  "luks_passphrase": "my-secret-password"
}
```

Response (200 OK):
```json
{
  "injected": ["luks_passphrase"]
}
```

Error cases:
- 409 Conflict if secrets have already been injected (one-shot endpoint)
- 400 Bad Request if payload is empty or malformed

The endpoint is **one-shot**: once secrets are injected, subsequent calls return 409. This prevents an attacker who compromises the network after boot from overwriting secrets.

### Why generic, not LUKS-specific

The same mechanism supports future secret types:
- `luks_passphrase` — disk decryption key
- `env_vars` — environment variables passed to the rootfs app (future)
- `api_keys` — application secrets (future)

init.sh only reads the keys it knows about. Unknown keys are stored but ignored.

### Security properties

- **Secret travels over attested TLS**: The user verifies the TLS certificate contains a valid SNP attestation report with the expected measurement before sending the secret. If the VM is tampered, the measurement won't match.
- **Host never sees the secret**: The secret goes directly from user → attest-agent inside the encrypted VM. The host can see the ciphertext (it's TLS) but can't decrypt it.
- **Secret is ephemeral**: init.sh shreds the key file immediately after `cryptsetup luksOpen`. The passphrase exists in memory only as long as dm-crypt needs it (kernel holds the derived key, not the passphrase).
- **One-shot injection**: Prevents replay or overwrite attacks after boot.

## Changes

### 1. `nix/init.sh` — Reorder boot, add LUKS support

The init script gains a new code path for `luks=1` in the kernel cmdline:

```sh
# Parse luks flag from kernel cmdline.
luks=$(/bin/busybox sed -n 's/.*luks=\([^ ]*\).*/\1/p' /proc/cmdline)

if [ "$luks" = "1" ]; then
    # LUKS mode: start attest-agent FIRST, wait for key, then unlock.
    echo "init: LUKS mode — starting attest-agent, waiting for key"
    /bin/busybox insmod /lib/modules/dm-crypt.ko
    /bin/aleph-attest-agent --port 8443 &

    # Wait for the user to inject the LUKS passphrase.
    /bin/busybox mkdir -p /tmp/secrets
    n=0
    while [ ! -f /tmp/secrets/luks_passphrase ]; do
        /bin/busybox sleep 0.5
        n=$((n + 1))
        if [ "$n" -ge 600 ]; then
            echo "init: FATAL: LUKS key not received within 300s"
            break
        fi
    done

    if [ -f /tmp/secrets/luks_passphrase ]; then
        echo "init: unlocking LUKS rootfs"
        /bin/cryptsetup luksOpen /dev/vda cryptroot < /tmp/secrets/luks_passphrase
        /bin/busybox rm -f /tmp/secrets/luks_passphrase
        /bin/busybox mount /dev/mapper/cryptroot /mnt/root
    fi
else
    # dm-verity or plain mode (existing flow).
    # ... existing code ...
fi
```

The attest-agent startup also moves: in LUKS mode it starts early (before rootfs mount); in dm-verity mode it starts after (as today).

### 2. `crates/aleph-attest-agent` — Add secret injection endpoint

New handler:

```
POST /confidential/inject-secret
Content-Type: application/json

{ "luks_passphrase": "..." }
```

The handler:
1. Checks if secrets have already been injected (atomic flag) → 409 if yes
2. Creates `/tmp/secrets/` directory
3. Writes each key as a separate file: `/tmp/secrets/<key>` containing the value
4. Sets the one-shot flag
5. Returns 200 with list of injected keys

This is simple file I/O — no IPC protocol needed. init.sh polls the filesystem.

### 3. `nix/initrd.nix` — Add dm-crypt module and cryptsetup

Add to the initrd:
- `dm-crypt.ko` kernel module (for LUKS)
- `cryptsetup` binary (static, for `luksOpen`)
- `libdevmapper` dependency (if not statically linked into cryptsetup)

Note: `cryptsetup` is already in the initrd for `veritysetup`. If they're the same binary (they are in nixpkgs — `cryptsetup` package provides both), no additional binary is needed. Just ensure `dm-crypt.ko` is included.

### 4. `nix/kernel.nix` — Ensure dm-crypt module is built

Add `DM_CRYPT = module` to kernel config. Also ensure the crypto algorithms LUKS needs are available:
- `CRYPTO_XTS` (XTS block cipher mode)
- `CRYPTO_AES` (AES cipher)
- `CRYPTO_SHA256` / `CRYPTO_SHA512` (key derivation)

These are likely already enabled (common kernel defaults), but verify.

### 5. Host-side: `crates/aleph-compute-node` — No changes needed

The host just boots the VM with `luks=1` in the kernel cmdline. It doesn't participate in the key exchange. The existing `CreateVm` flow works — the only difference is the cmdline parameter.

The host needs to know whether a VM uses LUKS (to set the cmdline flag). This comes from the `VmConfig` or the Aleph message's `trusted_execution` field.

### 6. Client-side: `crates/aleph-attest-cli` — Add secret injection command

New subcommand:

```bash
aleph-attest-cli inject-secret \
    --url https://<vm-ip>:8443 \
    --measurement <expected-hex> \
    --secret luks_passphrase=<password>
```

Flow:
1. Connect to attest-agent HTTPS endpoint
2. Extract attestation report from TLS certificate
3. Verify measurement matches expected value
4. Verify AMD certificate chain (VCEK → ASK → ARK)
5. If verified, POST `/confidential/inject-secret` with the payload
6. Print confirmation

## What We Don't Build

- **SEV-style ceremony** (GODH, session certs, QMP measurement, QMP secret injection): Not needed. SNP attestation is guest-initiated, not host-mediated.
- **Platform-side encryption**: User provides pre-built LUKS images. The platform doesn't wrap plain images in LUKS.
- **dm-verity + LUKS stacking**: Mutually exclusive modes. LUKS provides authenticated encryption; verity adds no meaningful security benefit on top.
- **Automated key management / KMS**: User manually injects the key. KMS integration is a future enhancement.
- **Custom OVMF**: No patched firmware needed. Secrets arrive over the network after boot, not via a firmware memory region.

## Future Enhancements

- **KMS integration**: Automatic key release based on measurement verification, without user intervention.
- **Environment variable injection**: Use the same `/confidential/inject-secret` endpoint to pass `env_vars` to the rootfs application.
- **Platform-side LUKS wrapping**: Encrypt plain rootfs images during deployment.
- **Multiple encrypted volumes**: Support LUKS on additional volumes beyond the rootfs.
