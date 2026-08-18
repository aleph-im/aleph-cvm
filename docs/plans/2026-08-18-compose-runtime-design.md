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

`manifest.template.json` fields:

| Field | Value | Notes |
|---|---|---|
| `format` | `"aleph-vprogram-runtime"` | |
| `format_version` | `1` | |
| `name` | `"aleph-compose-runtime"` | |
| `platform` | `"sev_snp"` | |
| `bundle.ref` | `"FILL-AFTER-UPLOAD"` in the template | replaced with the bundle's STORE item hash by `scripts/publish-compose-runtime.sh` |
| `bundle.sha256`, `bundle.size` | computed by nix at build time | the publish script re-verifies `sha256sum bundle.tar.gz` against this before uploading, so a mismatched upload fails loudly instead of publishing a manifest that points at content it doesn't hash-match |
| `bundle.members` | `{ ovmf, kernel, initrd, platform_rootfs, platform_hash_tree }` | maps the logical roles above to their in-tar paths |
| `boot.method` | `"qemu-direct-kernel"` | |
| `boot.cpu_models` | `["EPYC-v4"]` | |
| `boot.platform_roothash` | the rootfs's verity root hash | |
| `boot.cmdline_template` | `console=ttyS0 root=/dev/mapper/verity-root ro roothash={platform_roothash} workload_roothash={workload_roothash}` | `{platform_roothash}` is filled from `boot.platform_roothash`; `{workload_roothash}` is filled per-V-PROGRAM from the caller's workload volume, computed by the compute node when it attaches that disk |
| `attestation` | `[{ protocol: "aleph.ra-tls", version: "1", transport: { type: "tcp", port: 8443 } }]` | |
| `workload` | `{ contract: "aleph.compose/1", upstream_port: 8080 }` | tells the caller (and any future non-compose runtime) which workload contract this runtime expects and which guest port the attestation proxy forwards to |
| `source` | `{ repo: "https://github.com/aleph-im/aleph-cvm", build: "nix build .#vprogram-compose-bundle" }` | reproducibility pointer, not consumed by the platform |

Publishing: `scripts/publish-compose-runtime.sh` builds the bundle, uploads `bundle.tar.gz` via `aleph file upload`, verifies the local sha256 against the manifest template's declared sha256, patches `bundle.ref` with the bundle's STORE item hash, uploads the patched manifest, and prints the manifest's STORE message hash. That hash is what `aleph vprogram create --runtime <hash>` consumes.

## 2. Disk Roles and Boot Layout

`DiskConfig.role` (proto `compute.proto`, field 4, string):

| Value | Meaning |
|---|---|
| `""` (empty) | Unspecified: legacy positional interpretation (first disk is rootfs, an optional second disk is the workload volume) |
| `"rootfs"` | Platform rootfs disk |
| `"workload"` | Workload volume (compose files + images) |
| `"verified_volume"` | Reserved; structurally recognized but rejected by the VM manager until guest support lands (Section 6) |

Validation (`aleph-compute-node/src/vm/manager.rs::classify_disks`), enforced server-side on every `CreateVm`:

- **No mixing.** Either every disk in the request carries an explicit role, or none do (all `Unspecified`, legacy mode). A request that mixes the two is rejected (`MixedRoleModes`).
- **`verified_volume` is rejected outright** in role mode (`UnsupportedRole("verified_volume")`), regardless of position.
- **At most one `workload` disk.** A second one is rejected (`MultipleWorkloads`).
- **`rootfs` must be the first disk**, and no other disk may also claim the `rootfs` role (`RootfsNotFirst`).
- **If a `workload` disk is present, it must be the second disk**, immediately after `rootfs` (`WorkloadNotSecond`).

Once a request passes validation, the VM manager auto-inserts a dm-verity hash tree disk immediately after each data disk it manages verity for. The caller only ever supplies the data disks (with roles); the manager computes and attaches the hash trees itself:

| # data disks supplied | Resulting guest block devices |
|---|---|
| 1 (`rootfs` only) | `vda`=rootfs, `vdb`=rootfs hashtree |
| 2 (`rootfs`, `workload`) | `vda`=rootfs, `vdb`=rootfs hashtree, `vdc`=workload, `vdd`=workload hashtree |

The compute node computes the workload volume's root hash the same way it does the platform rootfs's (`ensure_verity`), and substitutes it into the manifest's `cmdline_template` as `workload_roothash={hash}`, so a compose V-PROGRAM's exact workload content is part of its SEV-SNP measurement, not just its platform rootfs.

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
| `restart` | accepted and parsed, but ignored in v1: guest failure handling always powers the VM off (Section 7) rather than restarting a container, regardless of what a service declares here |

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
- **`resolv.conf` under DHCP**: `nix/init.sh` writes `/etc/resolv.conf` (pointing at the gateway as nameserver) only on the static-IP branch, when the kernel cmdline carries an `ip=` parameter. When the guest instead brings its interface up via DHCP (`udhcpc`), no `resolv.conf` is written, so DNS resolution is unconfigured on that path for both platform init and anything running inside the compose stack. This is a known gap, not fixed as part of this contract.

## 7. Guest Failure Semantics (Fail-Closed)

Every failure on both sides of the platform/workload boundary powers the VM off rather than leaving it half-booted and reachable. Nothing here retries or falls back.

**Platform init** (`nix/init.sh`, guest PID 1; applies to both the LUKS and non-LUKS boot branches):

| Condition | Action |
|---|---|
| `/sbin/init` missing or not executable in the mounted rootfs | `poweroff -f` |
| Guest `/sbin/init` process exits, for any reason | `poweroff -f` |
| (LUKS branch only) LUKS passphrase not injected within 300s | `poweroff -f` |
| (LUKS branch only) `cryptsetup luksOpen` fails (wrong passphrase / corrupt header) | `poweroff -f` |

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
- `crates/aleph-compute-node/src/grpc/service.rs` - role string parsing/validation on `CreateVm`
- `crates/aleph-cvm-cli/src/main.rs` - `--disk path:format:ro|rw:role` CLI syntax
- `scripts/publish-compose-runtime.sh` - builds the bundle, uploads it and the patched manifest to aleph.im STORE (Section 1)
- `scripts/demo-compose.sh` - local SNP-box demo/test harness (role-tagged disks, health/attestation checks)
- `scripts/deploy-demo-compose.sh` - remote build + deploy + run wrapper around `demo-compose.sh`

## Verification

- `nix build ./nix#vprogram-compose-bundle` produces a deterministic `bundle.tar.gz` and `manifest.template.json`; `scripts/publish-compose-runtime.sh` re-derives `sha256sum bundle.tar.gz` and fails before uploading if it doesn't match `manifest.bundle.sha256`.
- `cargo test -p aleph-compute-node` covers `classify_disks` (legacy positional mode, role-mode ordering, `MixedRoleModes`, `RootfsNotFirst`, `WorkloadNotSecond`, `MultipleWorkloads`, and `verified_volume_role_is_not_yet_supported`).
- Manual hardware checklist, run on an SEV-SNP box via `scripts/deploy-demo-compose.sh`:
  - (a) confirm the demo passes with role-tagged disks
  - (b) confirm a workload volume with the compose file deleted powers off instead of serving 502
  - (c) confirm poweroff inside the stack container takes the VM down
