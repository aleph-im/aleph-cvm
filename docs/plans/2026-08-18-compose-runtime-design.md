# aleph.compose/1: Runtime and Workload Contract

**Goal**: Define the cross-repo contract of record for compose workloads on V-PROGRAM runtimes: the runtime manifest a `vprogram create` points at, the disk-role layout the compute node validates, the `aleph.compose/1` workload volume layout, the compose subset the guest runner accepts, and the fail-closed guarantees that hold in both directions. This is the document the aleph-rs CLI plan cites; nothing here is an open question, and any change to these shapes is a breaking change to the contract.

## Approach

A compose V-PROGRAM is two pieces glued together by a manifest:

1. A **runtime bundle** (kernel, initrd, OVMF, compose-runner rootfs, and that rootfs's dm-verity hash tree), published once as an `aleph-vprogram-runtime/1` manifest STORE message. `aleph vprogram create --runtime <hash>` points at it.
2. A **workload volume** the caller supplies per V-PROGRAM: an ext4 image holding a `docker-compose.yml` and pre-built container images, attached as a second disk with `role: workload`.

The compute node boots the two disks in a validated order, dm-verity-protects both (the manager auto-inserts the hash trees), and folds the workload volume's root hash into the kernel cmdline alongside the platform rootfs's, so both are covered by the SEV-SNP launch measurement. Guest PID 1 mounts the rootfs, chroots into it, and hands off to the compose-runner's own `/sbin/init`, which loads the images and runs `podman-compose up`. Every failure path on both sides of that handoff ends in `poweroff -f`: a compose V-PROGRAM that isn't running the exact attested workload never sits around answering the network.

## 1. Runtime Manifest (`aleph-vprogram-runtime/1`)

Built by `nix build ./nix#vprogram-compose-bundle` (flake in `nix/`). The derivation emits a deterministic `bundle.tar.gz` and a `manifest.template.json` alongside it.

`bundle.tar.gz` members (path within the tar, always `image/...`, deterministic: sorted names, zero owner/group, fixed mtime):

| Member | Content |
|---|---|
| `image/OVMF.fd` | UEFI firmware |
| `image/bzImage` | kernel |
| `image/initrd` | initrd (contains platform `/init`, `veritysetup`, `cryptsetup`) |
| `image/rootfs.ext4` | compose-runner rootfs (podman, crun, conmon, fuse-overlayfs, cni-plugins, podman-compose, busybox, slirp4netns) |
| `image/rootfs.ext4.verity` | dm-verity hash tree for the rootfs above |
| `image/rootfs.ext4.roothash` | the rootfs's dm-verity root hash, as a bare hex string (same value as `boot.platform_roothash` below) |

`manifest.template.json` fields:

| Field | Value | Notes |
|---|---|---|
| `format` | `"aleph-vprogram-runtime"` | |
| `format_version` | `1` | |
| `name` | `"aleph-compose-runtime"` | |
| `version` | `"2026.08.18"` | runtime build version, distinct from `format_version` |
| `platform` | `"sev_snp"` | |
| `bundle.ref` | `"FILL-AFTER-UPLOAD"` in the template | replaced with the bundle's STORE item hash by `scripts/publish-compose-runtime.sh` |
| `bundle.sha256`, `bundle.size` | computed by nix at build time | the publish script re-verifies `sha256sum bundle.tar.gz` against this after uploading the bundle but before uploading the manifest, so a mismatch fails loudly before the manifest (the gate that matters: it's the manifest that callers resolve and trust) ever points at content that doesn't hash-match |
| `bundle.members` | `{ ovmf, kernel, initrd, platform_rootfs, platform_hash_tree, platform_roothash_file }` | maps the logical roles above to their in-tar paths; `platform_roothash_file` is additive (the aleph-rs manifest parser ignores unknown member keys, so an older parser tolerates it being present or absent) |
| `boot.method` | `"qemu-direct-kernel"` | |
| `boot.kernel_hashes` | `true` | selects OVMF's kernel-hashes measurement mode for direct-kernel boot, folding the kernel, initrd, and cmdline into the SEV-SNP launch measurement (matches `boot.method: qemu-direct-kernel`) |
| `boot.cpu_models` | `["EPYC-v4"]` | |
| `boot.platform_roothash` | the rootfs's verity root hash | |
| `boot.cmdline_template` | `console=ttyS0 root=/dev/mapper/verity-root ro roothash={platform_roothash} workload_roothash={workload_roothash}` | `{platform_roothash}` is filled from `boot.platform_roothash`; `{workload_roothash}` is filled per-V-PROGRAM from the caller's workload volume, computed by the compute node when it attaches that disk |
| `attestation` | `[{ protocol: "aleph.ra-tls", version: "1", transport: { type: "tcp", port: 8443 } }]` | |
| `workload` | `{ contract: "aleph.compose/1", upstream_port: 8080 }` | tells the caller (and any future non-compose runtime) which workload contract this runtime expects and which guest port the attestation proxy forwards to |
| `source` | `{ repo: "https://github.com/aleph-im/aleph-cvm", build: "nix build ./nix#vprogram-compose-bundle" }` | reproducibility pointer, not consumed by the platform |

Publishing: `scripts/publish-compose-runtime.sh` builds the bundle, uploads `bundle.tar.gz` via `aleph file upload --storage-engine storage` (forced explicitly; the CLI would otherwise auto-select `ipfs` above 100 MiB, and `fetch_bundle_artifacts` always fetches by `bundle.sha256` against native storage, so an IPFS upload would publish a runtime nothing could fetch), verifies the local sha256 against the manifest template's declared sha256, patches `bundle.ref` with the bundle's STORE item hash, uploads the patched manifest the same way, and prints the manifest's STORE message hash. That hash is what `aleph vprogram create --runtime <hash>` consumes.

## 2. Disk Roles and Boot Layout

`DiskConfig.role` (proto `compute.proto`, field 4, string):

| Value | Meaning |
|---|---|
| `""` (empty) | Unspecified: legacy positional interpretation (first disk is rootfs, an optional second disk is the workload volume) |
| `"rootfs"` | Platform rootfs disk |
| `"workload"` | Workload volume (compose files + images) |
| `"verified_volume"` | Reserved; structurally recognized but rejected by the VM manager until guest support lands (Section 6) |

Validation (`aleph-compute-node/src/vm/manager.rs::classify_disks`), enforced server-side on every `CreateVm`: the gRPC handler (`grpc/service.rs`) calls it directly on the parsed disk list, right after the role-string allowlist check and before any host-side side effect (IP allocation, TAP creation, nftables setup), so a malformed layout is rejected with `Status::invalid_argument` up front. The VM manager (`vm/manager.rs::create_vm`) also calls it later, but only on the confidential, non-LUKS path, because that's where it needs the workload-disk flag to decide whether to run verity on a second disk; that second call is redundant for validation purposes and exists only to compute `has_workload_disk`.

- **No mixing.** Either every disk in the request carries an explicit role, or none do (all `Unspecified`, legacy mode). A request that mixes the two is rejected (`MixedRoleModes`).
- **`verified_volume` is rejected outright** in role mode (`UnsupportedRole("verified_volume")`), regardless of position.
- **At most one `workload` disk.** A second one is rejected (`MultipleWorkloads`).
- **`rootfs` must be the first disk**, and no other disk may also claim the `rootfs` role (`RootfsNotFirst`).
- **If a `workload` disk is present, it must be the second disk**, immediately after `rootfs` (`WorkloadNotSecond`). With today's 4-variant `DiskRole` (`Unspecified`, `Rootfs`, `Workload`, `VerifiedVolume`), every other role is filtered out by an earlier check by the time this one runs, so a disk between `rootfs` and `workload` can't actually occur yet and this branch is unreachable in practice; it's reserved for a future role that could legitimately sit at that position.

Once a request passes validation, the VM manager auto-inserts a dm-verity hash tree disk immediately after each data disk it manages verity for. The caller only ever supplies the data disks (with roles); the manager computes and attaches the hash trees itself:

| # data disks supplied | Resulting guest block devices |
|---|---|
| 1 (`rootfs` only) | `vda`=rootfs, `vdb`=rootfs hashtree |
| 2 (`rootfs`, `workload`) | `vda`=rootfs, `vdb`=rootfs hashtree, `vdc`=workload, `vdd`=workload hashtree |

**Authority model.** The published verity artifacts are authoritative, not whatever the compute node happens to derive locally: for the platform rootfs, that's the bundle's `image/rootfs.ext4.verity` and `image/rootfs.ext4.roothash` members (Section 1); for a workload volume, it's the V-PROGRAM message's `VerifiedWorkload`/`VerifiedVolume` ref, `hash_tree`, and `roothash` fields. `ensure_verity` (`crates/aleph-compute-node/src/verity.rs`) is a demo convenience, not the source of truth: it reuses `{path}.verity`/`{path}.roothash` sidecars when both already sit next to the data file - which is how `scripts/demo-compose.sh` stages every disk - and only falls back to running `veritysetup format` itself, with a fresh random salt, when one or both are missing. That fallback re-derivation can never reproduce the salt baked into a published roothash, so the hash it computes never matches the value measured into the manifest's `cmdline_template`. A launcher path that lets a disk reach `ensure_verity` without its published sidecars in place doesn't fail loudly; it silently boots a VM whose SEV-SNP measurement doesn't match the manifest, which fails attestation 100% of the time. `bundle.tar.gz` therefore ships `image/rootfs.ext4.roothash` alongside `image/rootfs.ext4.verity` (Section 1) so the platform rootfs always has its sidecars in place; the workload volume's sidecars are the caller's responsibility to stage the same way.

CLI disk syntax (`aleph-cvm-cli`): `--disk path[:format[:ro|rw[:role]]]`, e.g. `--disk /data/rootfs.ext4:raw:ro:rootfs --disk /data/workload.ext4:raw:ro:workload`.

## 3. Workload Volume Layout (`aleph.compose/1`)

The volume is an ext4 filesystem. Its root must contain:

| Path | Required | Contents |
|---|---|---|
| `docker-compose.yml` | yes | the compose subset (Section 5) |
| `images/*.tar` | yes, at least one | container image archives (`podman load`-compatible: OCI-archive or Docker-archive format) |

Images are matched to the compose file's `image:` references by the repo tag **embedded in the archive**, not by filename; archive filenames are informational only (the runner globs `images/*.tar` and loads every one it finds, regardless of name). Extra files anywhere on the volume are permitted and ignored by the runner.

This layout is what a runtime's manifest declares support for via `workload: { contract: "aleph.compose/1", upstream_port: 8080 }` (Section 1); a future runtime with a different `workload.contract` value would define its own volume layout.

## 4. Single Entrypoint

Exactly one service in the compose file listens on `127.0.0.1:8080` inside the guest, matching the runtime manifest's `workload.upstream_port`. The in-guest attestation agent (`aleph-attest-agent`) serves RA-TLS on `:8443` and reverse-proxies every other request to `127.0.0.1:8080`. There is no per-service port exposure model: `ports:` mappings are rejected by the CLI at create time (Section 5.2), and `network_mode: host` (required on every service, Section 5.1) means services reach each other over the guest's loopback/host network directly, without compose-managed inter-container networking.

## 5. Compose Subset

### 5.1 Accepted Keys

| Key | Notes |
|---|---|
| `image` | required; digest-pinned by the CLI before the compose file is written to the workload volume |
| `command` | passed through |
| `entrypoint` | passed through |
| `environment` | public and measured: values live in `docker-compose.yml` on the dm-verity-protected workload volume, so they're covered by `workload_roothash` and visible to anyone who can read the volume; there is no mechanism to keep an environment value secret in v1 |
| `depends_on` | passed through to `podman-compose` |
| `network_mode` | must be `host` on every service (Section 4) |
| `tmpfs` | passed through |
| `restart` | passed through verbatim: the CLI does not strip or rewrite it, so `podman-compose` sees and acts on whatever a service declares. At the platform level it has no effect on VM lifecycle: `podman-compose up --no-build` exiting for any reason, including one container's own restart policy giving up, still ends in `poweroff -f` (Section 7) rather than the VM being kept alive by that policy |

### 5.2 Rejected Keys

The CLI rejects a compose file containing any of the following (or any key it doesn't recognize at all):

| Key | Rejected because |
|---|---|
| `build` | there is no build path in the guest; only pre-built, pinned images loaded from `images/*.tar` are trusted (Section 8) |
| `volumes` | no persistent or bind-mounted storage in v1; state doesn't survive a reboot (Section 6, `persistent_volumes`) |
| `ports` | the guest has exactly one entrypoint on `127.0.0.1:8080` behind the attestation proxy (Section 4); per-service port publishing has no meaning here |
| `secrets` | there is no secret-injection mechanism for the workload volume in v1; everything on it is public and measured (see `environment` above) |
| `env_file` | pulls in a file whose contents the CLI doesn't parse or pin; environment values must be inline in `docker-compose.yml` so what's measured is what's declared |
| `privileged` | would let a container step outside the isolation podman/crun already provide inside the guest, for no capability the v1 contract needs |
| `devices` | no host/guest device passthrough model is defined for compose services |
| `cap_add` | same rationale as `privileged`: no v1 workload needs elevated Linux capabilities, and granting them widens the guest's attack surface for no declared benefit |
| any unknown key | fail closed: an unrecognized key means the CLI can't be sure what it would do, so the compose file is rejected rather than silently ignoring it |

## 6. Out of Scope, Reserved

- **`persistent_volumes`**: not supported in v1. Reserved as an additive field on the V-PROGRAM create message for a future writable-storage mechanism; adding it later is backward compatible. Today, everything under `/var`, `/tmp`, and `/dev/shm` in the compose-runner rootfs is tmpfs, wiped on every boot.
- **`verified_volume` disks, guest-side**: the role value is defined in the proto and recognized by the ordering validator, but the VM manager rejects it (`UnsupportedRole("verified_volume")`) until guest init gains the ability to mount and verify an arbitrary third measured volume. Only `rootfs` and `workload` are usable roles today.
- **Registry pulls**: no registry is configured anywhere in the compose-runner rootfs (no `registries.conf` entries pointing at a remote), and `podman-compose up --no-build` never pulls. All images must already be on the workload volume as `images/*.tar` (Section 3, Section 8).
- **Restart policies (V-PROGRAM level)**: the compose `restart:` key is parsed but ignored (Section 5.1). More importantly, there is no platform-level restart-on-crash policy: every guest failure path in Section 7 ends in `poweroff -f` and the VM stays off. A V-PROGRAM is not a batch job in v1; recovering from a stopped V-PROGRAM is an operator action, not automatic.
- **`resolv.conf` is never actually written for a compose V-PROGRAM**: `nix/init.sh`'s `prepare_chroot()` writes `/mnt/root/etc/resolv.conf` (pointing at the gateway as nameserver) only when `$gateway` is set, which only happens on the static-IP branch (kernel cmdline carries an `ip=` parameter); the DHCP branch (`udhcpc`) never sets it, so DNS is unconfigured there. But even on the static-IP branch the write itself is a no-op in practice: by the time `prepare_chroot()` runs, `/mnt/root` is the dm-verity-protected platform rootfs mounted `ro` (Section 7, Phase C), the script has no `set -e` and none of `prepare_chroot()`'s commands check their exit status, so `echo ... > /mnt/root/etc/resolv.conf` fails with "Read-only file system" and that failure is silently swallowed. The net effect: no compose V-PROGRAM gets a `resolv.conf` today, on either networking path. This is a known gap, not fixed as part of this contract.

## 7. Guest Failure Semantics (Fail-Closed)

Every failure on both sides of the platform/workload boundary powers the VM off rather than leaving it half-booted and reachable. Nothing here retries or falls back.

**Platform init** (`nix/init.sh`, guest PID 1). Every `exec /bin/busybox poweroff -f` in the script (15 call sites) falls into one of these five phases:

Phase A - shared, before the LUKS/non-LUKS split:

| Condition | Action |
|---|---|
| No block device (`/dev/vda` or `/dev/sda`) appears within the poll window | `poweroff -f` |

Phase B - LUKS-encrypted rootfs mode (`luks=1`):

| Condition | Action |
|---|---|
| LUKS passphrase not injected within 300s | `poweroff -f` |
| `cryptsetup luksOpen` fails (wrong passphrase / corrupt header) | `poweroff -f` |
| Mount of `/dev/mapper/cryptroot` fails | `poweroff -f` |
| `/sbin/init` missing or not executable in the mounted rootfs | `poweroff -f` |
| Guest `/sbin/init` process exits, for any reason | `poweroff -f` |

Phase C - non-LUKS mode, platform rootfs dm-verity (`roothash` set; this is always the case for a compose V-PROGRAM, whose manifest `cmdline_template` always carries `roothash={platform_roothash}`):

| Condition | Action |
|---|---|
| Rootfs hash-tree device `/dev/vdb` not found within the poll window | `poweroff -f` |
| Rootfs dm-verity verification fails (`veritysetup open` on `/dev/vda` against `/dev/vdb`) - rootfs may be tampered | `poweroff -f` |
| Mount of `/dev/mapper/verity-root` fails | `poweroff -f` |

Phase D - non-LUKS mode, workload volume dm-verity (`workload_roothash` set; always the case for a compose V-PROGRAM's workload disk). **These are the paths Section 8's image-trust argument rests on**: they're what make it true that podman never sees a workload volume that hasn't already passed dm-verity.

| Condition | Action |
|---|---|
| Workload data disk `/dev/vdc` not found within the poll window | `poweroff -f` |
| Workload hash-tree disk `/dev/vdd` not found within the poll window | `poweroff -f` |
| Workload dm-verity verification fails (`veritysetup open` on `/dev/vdc` against `/dev/vdd`) | `poweroff -f` |
| Mount of the verified workload volume fails | `poweroff -f` |

Phase E - non-LUKS mode, common tail (after rootfs, and optionally workload, are mounted):

| Condition | Action |
|---|---|
| `/sbin/init` missing or not executable in the mounted rootfs | `poweroff -f` |
| Guest `/sbin/init` process exits, for any reason | `poweroff -f` |

**Compose-runner init** (`nix/compose-rootfs.nix`, the rootfs's own `/sbin/init`, started by platform init as described above):

| Condition | Action |
|---|---|
| Mounting `/run`, `/tmp`, `/var`, or `/dev/shm` (tmpfs) fails | `poweroff -f` |
| Mounting cgroup2 on `/sys/fs/cgroup` fails | `poweroff -f` |
| `insmod` of the fuse kernel module fails | `poweroff -f` |
| `/mnt/workload/docker-compose.yml` missing | `poweroff -f` |
| `podman load` fails on any `images/*.tar` archive | `poweroff -f` |
| Zero image archives found under `/mnt/workload/images` | `poweroff -f` |
| `cd /mnt/workload` fails | `poweroff -f` |
| `podman-compose up --no-build` exits, for any reason (including a clean exit) | `poweroff -f` |

The consequence: a compose V-PROGRAM that loses its workload volume, or whose stack exits on its own, never keeps serving RA-TLS on 8443 with nothing behind it. It goes away instead.

That guarantee is exact for a single-service stack: `podman-compose up` blocking on the one container is what Phase E's "exits, for any reason" hinges on. With multiple services it's weaker. `podman-compose up` (unlike `docker compose up`) does not exit when one of several containers dies; it keeps running and keeps the others up. So if the service on `127.0.0.1:8080` (Section 4) crashes while a sibling service stays alive, `podman-compose up` itself never exits, platform init's Phase E wait never fires, and RA-TLS on 8443 keeps answering with nothing listening behind it on 8080 - the attestation proxy would see connection-refused on every proxied request instead of the VM powering off. The fail-closed guarantee in this section holds for the stack as a whole exiting; it does not cover one service among several dying while its siblings keep `podman-compose up` alive. This is a residual gap, not fixed as part of this contract.

## 8. Image Trust Model

`/etc/containers/policy.json` in the compose-runner rootfs is `{"default": [{"type": "insecureAcceptAnything"}]}`: podman accepts any image without signature verification. That is normally a dangerous default. It is sound here, and only here, for two reasons that must both hold:

1. **Images arrive on a measured volume.** The workload volume is dm-verity protected, and its root hash (`workload_roothash`) is folded into the kernel cmdline, which is part of the SEV-SNP launch measurement. Anything `policy.json` would evaluate has already passed integrity verification before podman ever sees it; there's no path for a tampered image to reach `podman load` undetected.
2. **No pull path is reachable at runtime.** `podman-compose up` is invoked with `--no-build`, and no registry is configured anywhere in the rootfs. The only images podman can ever load are the ones already sitting in `images/*.tar` on the verified workload volume (Section 3).

`insecureAcceptAnything` is therefore not a trust decision at the image-signature layer; the trust decision is made once, upstream, by dm-verity and the SEV-SNP measurement. If either of the two conditions above changes (a registry gets configured, or images start arriving by some other unverified path), this policy stops being sound and must change with it.

## Files

- `nix/compose-rootfs.nix` - compose-runner rootfs derivation: packages, init script (Section 7), `policy.json` (Section 8), `storage.conf`, CNI config
- `nix/flake.nix` - `vprogram-compose-bundle` (Section 1), `compose-workload` (Section 3 demo instance), `compose-rootfs-verity` / `compose-workload-verity`, `vm-compose-demo`
- `nix/init.sh` - platform init (PID 1): rootfs/workload mount, verity setup, LUKS branch, resolv.conf (Section 6)
- `nix/compose-demo/docker-compose.yml` - demo compose file baked into `compose-workload`
- `proto/compute.proto` - `DiskConfig.role` (Section 2)
- `crates/aleph-compute-node/src/vm/manager.rs` - `classify_disks`, `DiskLayoutError`, hash-tree auto-insertion, cmdline construction (Section 2)
- `crates/aleph-compute-node/src/grpc/service.rs` - role string parsing/allowlist validation and the `classify_disks` ordering check on `CreateVm`
- `crates/aleph-cvm-cli/src/main.rs` - `--disk path:format:ro|rw:role` CLI syntax
- `scripts/publish-compose-runtime.sh` - builds the bundle, uploads it and the patched manifest to aleph.im STORE (Section 1)
- `scripts/demo-compose.sh` - local SNP-box demo/test harness (role-tagged disks, health/attestation checks)
- `scripts/deploy-demo-compose.sh` - remote build + deploy + run wrapper around `demo-compose.sh`

## Verification

- `nix build ./nix#vprogram-compose-bundle` produces a deterministic `bundle.tar.gz` and `manifest.template.json`; `scripts/publish-compose-runtime.sh` re-derives `sha256sum bundle.tar.gz` after uploading the bundle and fails before uploading the manifest if it doesn't match `manifest.bundle.sha256`.
- `cargo test -p aleph-compute-node` covers `classify_disks` (legacy positional mode, role-mode ordering, `MixedRoleModes`, `RootfsNotFirst`, `MultipleWorkloads`, and `verified_volume_role_is_not_yet_supported`). `WorkloadNotSecond` has no test: it's unreachable with today's 4-variant `DiskRole` (Section 2).
- Manual hardware checklist, run on an SEV-SNP box via `scripts/deploy-demo-compose.sh`:
  - (a) confirm the demo passes with role-tagged disks
  - (b) confirm a workload volume with the compose file deleted powers off instead of serving 502
  - (c) confirm poweroff inside the stack container takes the VM down
