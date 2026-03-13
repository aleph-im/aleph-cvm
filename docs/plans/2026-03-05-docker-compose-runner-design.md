# Docker Compose Runner for Aleph CVM

**Goal**: Build a Nix-built rootfs image containing a container runtime capable of loading a Docker Compose spec and pre-built OCI images from a mounted volume, verifying their integrity, and running the stack — all inside an attested SEV-SNP confidential VM.

## Context

This is the "Docker Compose runner" tier from the [generic rootfs design](2026-03-05-generic-rootfs-design.md). The platform layer (OVMF + kernel + initrd with attest-agent + init.sh) remains unchanged. Only the rootfs changes — it ships a container runtime instead of a bare application.

## Architecture

### Volume Layout

```
/dev/vda  ── compose-runner rootfs (ext4)     ── dm-verity ── /dev/vdb (hash tree)
/dev/vdc  ── workload volume (ext4, readonly)  ── dm-verity ── /dev/vdd (hash tree)
```

**Kernel cmdline** carries two roothashes:

```
roothash=<runner-rootfs-hash> workload_roothash=<workload-volume-hash>
```

Both hashes are embedded at build/deploy time and measured via SEV-SNP launch measurement. This means the attestation covers the full stack: firmware + kernel + initrd + compose runtime + user workload (compose spec + images).

The runner rootfs is **generic and reusable** — the same image serves any workload. The workload volume is user-specific. A deployment is defined by the pair (runner roothash, workload roothash).

### Rootfs Contents (compose-runner)

The rootfs is a minimal Linux userland with a container runtime. No full distro, no systemd.

| Component | Purpose | Notes |
|-----------|---------|-------|
| **podman** (static or mostly-static) | Container runtime + image loading | No daemon, direct CLI invocation |
| **crun** | OCI runtime (runs containers) | Lighter than runc, written in C |
| **conmon** | Container monitor | Manages container stdio/lifecycle |
| **CNI plugins** or **netavark** | Container networking | Bridge mode for inter-container comms |
| **busybox** (static) | Shell, coreutils for /sbin/init script | Already proven in current rootfs |
| **fuse-overlayfs** | Overlay filesystem for containers | Required since we run without kernel overlayfs support in a chroot |
| `/sbin/init` | Entrypoint script | Sets up runtime environment, loads images, runs compose |

**Why podman over Docker?** No daemon. Docker requires dockerd + containerd running as background services. Podman runs containers directly — each `podman run` is a fork/exec of crun. This fits our "init script as PID 1" model. It also means no Unix socket attack surface.

### Workload Volume Contents

The workload volume is a read-only ext4 image provided by the user (or built by a CI pipeline). Structure:

```
/
├── docker-compose.yml          # or compose.yaml
├── images/
│   ├── fib-service.tar         # OCI image tarball (podman save / docker save)
│   ├── nginx.tar               # ...
│   └── ...
└── config/                     # optional: env files, configs, secrets
    ├── .env
    └── ...
```

Each image tarball is a standard OCI or Docker archive (`podman save --format oci-archive` or `docker save`). The compose file references images by name+tag that match the tarballs.

Integrity of this entire volume is guaranteed by dm-verity — the workload roothash in the kernel cmdline covers every byte. Individual image digest verification is therefore not strictly necessary (dm-verity already covers it), but the compose runner can optionally validate `image: name@sha256:...` references as a defense-in-depth measure.

### Boot Flow

```
t0:  VM boots (OVMF → kernel → initrd)
t1:  init.sh: mount /proc, /sys, /dev, configure networking
t2:  init.sh: load dm-verity modules
t3:  init.sh: veritysetup open /dev/vda verity-root /dev/vdb <runner-roothash>
t4:  init.sh: mount /dev/mapper/verity-root /mnt/root (compose runner rootfs)
t5:  init.sh: parse workload_roothash from cmdline
t6:  init.sh: veritysetup open /dev/vdc verity-workload /dev/vdd <workload-roothash>
t7:  init.sh: mount /dev/mapper/verity-workload /mnt/workload
t8:  init.sh: bind-mount /proc, /sys, /dev, /mnt/workload into /mnt/root
t9:  init.sh: chroot /mnt/root /sbin/init
t10: init.sh: start attest-agent --upstream http://127.0.0.1:<gateway-port>

     ── inside chroot (rootfs /sbin/init) ──

t11: mount tmpfs on /run, /tmp, /var (writable layers containers need)
t12: mount cgroup2 on /sys/fs/cgroup
t13: for each /mnt/workload/images/*.tar: podman load -i <tarball>
t14: podman-compose -f /mnt/workload/docker-compose.yml up
t15: wait (supervise)
```

### Networking

Two network planes:

**1. Inter-container networking (inside the compose stack)**

Podman creates a bridge network (e.g., `compose_default`) via CNI/netavark. Containers talk to each other by service name (DNS resolved by podman's embedded DNS or an aardvark-dns instance). This is standard Compose networking — nothing CVM-specific.

**2. External access (host ↔ VM ↔ container)**

The attest-agent in the initrd listens on `0.0.0.0:8443` and reverse-proxies to `127.0.0.1:<port>`. The compose stack must expose the main service on a localhost port that the attest-agent can reach.

Convention: the compose file exposes its entry-point service on **port 8080** on the host network (i.e., `ports: ["127.0.0.1:8080:8080"]`). This matches the attest-agent's default `--upstream http://127.0.0.1:8080`.

For multi-service stacks with multiple external endpoints, the compose file exposes additional services on different ports (8081, 8082, ...). The attest-agent would need to be extended to support multiple upstreams or path-based routing. This is a future enhancement — MVP is single upstream on 8080.

### Init Script (/sbin/init in compose-runner rootfs)

```sh
#!/bin/busybox sh
set -e

# Writable layers that containers need.
/bin/busybox mount -t tmpfs tmpfs /run
/bin/busybox mount -t tmpfs tmpfs /tmp
/bin/busybox mount -t tmpfs tmpfs /var

# Containers need cgroup v2.
/bin/busybox mkdir -p /sys/fs/cgroup
/bin/busybox mount -t cgroup2 cgroup2 /sys/fs/cgroup

# Load all OCI images from the workload volume.
for tarball in /mnt/workload/images/*.tar; do
    echo "compose-init: loading image ${tarball}"
    podman load -i "$tarball"
done

# Start the compose stack.
# --no-build: images must be pre-built (no build context in CVM).
# podman-compose reads docker-compose.yml by default.
cd /mnt/workload
exec podman-compose up --no-build
```

## Integrity Model

The trust chain:

```
AMD hardware root of trust (VCEK)
  └── SEV-SNP launch measurement
        ├── OVMF firmware
        ├── kernel
        ├── initrd (busybox + attest-agent + veritysetup + dm-verity modules + init.sh)
        └── kernel cmdline
              ├── roothash=<H1>  ──→ compose-runner rootfs (podman, crun, /sbin/init)
              └── workload_roothash=<H2>  ──→ workload volume (compose.yml + OCI tarballs)
```

A verifier checks the measurement and knows exactly what code is running:
- H1 identifies the compose runtime version
- H2 identifies the user's workload (specific images, specific compose config)

**Key property**: the user cannot modify either volume at runtime (both are dm-verity protected, mounted read-only). Container writes go to tmpfs overlay layers, which are ephemeral.

## Changes to Platform Layer

### init.sh

The current init.sh handles a single dm-verity volume pair (vda + vdb). It needs to support an optional second pair (vdc + vdd) for the workload volume.

```sh
# After mounting rootfs via dm-verity...

# Parse optional workload roothash.
workload_roothash=$(sed -n 's/.*workload_roothash=\([0-9a-fA-F]*\).*/\1/p' /proc/cmdline)

if [ -n "$workload_roothash" ]; then
    # Wait for /dev/vdc and /dev/vdd.
    # veritysetup open /dev/vdc verity-workload /dev/vdd "$workload_roothash"
    # mount /dev/mapper/verity-workload /mnt/workload
    # bind-mount into chroot: mount --bind /mnt/workload /mnt/root/mnt/workload
fi
```

### init.sh: bind-mounts for chroot

Container runtimes need access to kernel interfaces. Before chrooting:

```sh
mount --bind /proc /mnt/root/proc
mount --bind /sys /mnt/root/sys
mount --bind /dev /mnt/root/dev
```

This is already implicit for the current simple rootfs (it doesn't need /proc), but becomes mandatory for the compose runner. The init.sh changes are backwards-compatible — bind-mounts are harmless if the rootfs doesn't use them.

### Kernel config

The compose runner may need additional kernel features:

| Feature | Why | Current status |
|---------|-----|---------------|
| `CONFIG_OVERLAY_FS` | Container layered filesystems | Likely =m, needs adding |
| `CONFIG_VETH` | Container virtual ethernet pairs | Likely =m, needs adding |
| `CONFIG_BRIDGE` | Container bridge networking | Already enabled (for dm setup) |
| `CONFIG_NETFILTER` / `CONFIG_NF_NAT` | Container port mapping | May need adding |
| `CONFIG_CGROUP_*` | Container resource isolation | Needs verification |
| `CONFIG_USER_NS` | Rootless podman (optional for MVP) | Needs verification |

These can be built as modules and loaded by the compose runner's /sbin/init, or built-in if they're always needed.

### Measurement computation

The `measurement` Nix derivation needs to include both roothashes in the kernel cmdline:

```nix
kernelCmdline = "console=ttyS0 root=/dev/mapper/verity-root ro "
  + "roothash=${runnerRoothash} "
  + "workload_roothash=${workloadRoothash}";
```

## Demo: Fibonacci Service via Compose

### Workload volume contents

```yaml
# docker-compose.yml
services:
  fib:
    image: fib-service:latest
    ports:
      - "127.0.0.1:8080:8080"
```

```
images/
  fib-service.tar    # podman save fib-service:latest -o fib-service.tar
```

The fib-service image is built from the existing `nix/fib-service/` crate, packaged as an OCI image instead of a bare binary.

### Nix build

```nix
# Build fib-service as an OCI image (not just a binary).
fib-service-image = pkgs.dockerTools.buildImage {
  name = "fib-service";
  tag = "latest";
  copyToRoot = [ fib-service ];
  config.Cmd = [ "${fib-service}/bin/fib-service" ];
};

# Workload volume: compose.yml + image tarballs.
compose-workload = pkgs.runCommand "compose-workload.ext4" {
  nativeBuildInputs = [ pkgs.e2fsprogs ];
} ''
  mkdir -p workload/images
  cp ${./compose-demo/docker-compose.yml} workload/docker-compose.yml
  cp ${fib-service-image} workload/images/fib-service.tar
  size=$(du -sm workload | cut -f1)
  size=$((size + 10))
  truncate -s ''${size}M $out
  mkfs.ext4 -b 4096 -d workload $out
'';

# dm-verity for the workload volume.
workload-verity = pkgs.runCommand "workload-verity" {
  nativeBuildInputs = [ pkgs.cryptsetup ];
} ''
  mkdir -p $out
  veritysetup format ${compose-workload} $out/hashtree \
    | tee /dev/stderr \
    | grep "Root hash:" | awk '{print $NF}' | tr -d '\n' > $out/roothash
'';
```

### Demo flow

```bash
# 1. Build everything
nix build .#vm-compose-demo

# 2. Start node + VM (same as fib demo, but with extra disks)
#    QEMU gets: -drive rootfs -drive rootfs-hashtree -drive workload -drive workload-hashtree

# 3. VM boots → init.sh mounts both volumes → chroot → /sbin/init loads images → compose up

# 4. Test through attested TLS (identical to current demo)
curl -sk https://<vm-ip>:8443/health      # → {"status": "ok"}
curl -sk https://<vm-ip>:8443/fib/10      # → {"n": 10, "result": 55}
aleph-attest-cli --url https://<vm-ip>:8443/fib/10 \
  --expected-measurement <measurement>    # attested request
```

From the outside, this is indistinguishable from the current bare-binary demo. The difference is internal: the fib-service runs inside a container inside the VM, managed by podman-compose.

## Risks & Open Questions

### 1. Rootfs size

Podman + crun + CNI plugins + conmon + fuse-overlayfs is significantly larger than a bare busybox. The compose-runner rootfs will likely be 100-300 MB vs the current ~5 MB. This affects:
- Build time (Nix derivation)
- VM boot time (dm-verity hash tree computation is proportional to image size)
- SEV-SNP pvalidate time (more memory pages to validate — mitigated by huge pages)

Mitigation: strip aggressively, use musl-based builds where possible, exclude unnecessary podman features.

### 2. Static linking

The current rootfs convention requires statically-linked binaries (no /nix/store in chroot). Podman is a Go binary (statically linked by default via CGO_ENABLED=0), but crun has C dependencies (libseccomp, libcap, etc.). Options:
- Build crun statically (possible with musl)
- Ship a minimal /lib with required shared libs
- Use `pkgsStatic` in Nix

### 3. cgroup v2 availability

Podman needs cgroup v2 mounted. The current kernel config enables cgroups, but the initrd doesn't mount them. The compose runner's /sbin/init will mount cgroup2 before starting podman. Need to verify our kernel has `CONFIG_CGROUP_V2=y` (likely already the case with a 6.6 kernel).

### 4. Overlay filesystem

Containers need an overlay filesystem for their writable layers. Options:
- **Kernel overlayfs**: needs `CONFIG_OVERLAY_FS`, must work on top of a tmpfs (since the rootfs is read-only dm-verity)
- **fuse-overlayfs**: userspace alternative, no kernel module needed, but slower

For MVP, fuse-overlayfs is simpler (no kernel config changes). Optimise to kernel overlayfs later if performance matters.

### 5. /dev/mapper/control in chroot

The compose runner rootfs runs in a chroot. It doesn't need device-mapper (dm-verity is handled in the initrd), but podman might expect certain /dev nodes. The bind-mount of /dev from the initrd should cover this.

### 6. Multiple external services

MVP: single service exposed on port 8080, proxied by attest-agent on 8443. For compose stacks with multiple externally-accessible services, we'd need to extend the attest-agent (path-based routing or multiple ports). Defer to a future design.

### 7. Compose vs. standalone containers

Some users may want to run a single container without compose overhead. The runner's /sbin/init could detect the presence of docker-compose.yml and fall back to `podman run` for a single image. Keep this simple for MVP — always use compose.

## Implementation Phases

### Phase 1: Compose runner rootfs (Nix)

- Build podman + crun + conmon + fuse-overlayfs + CNI via Nix (static where possible)
- Create rootfs.nix for compose-runner with /sbin/init script
- Test locally (no CVM, just QEMU + virtio-blk)

### Phase 2: Workload volume support

- Extend init.sh to handle `workload_roothash` and mount /dev/vdc + /dev/vdd
- Add bind-mounts (/proc, /sys, /dev) to chroot setup
- Add kernel modules if needed (overlayfs, veth, netfilter)
- Update QEMU arg builder to attach 4 virtio-blk devices

### Phase 3: Demo with fib-service

- Build fib-service OCI image via `pkgs.dockerTools.buildImage`
- Create workload volume with compose.yml + image tarball
- Compute dm-verity for both volumes
- Compute combined measurement
- Run end-to-end demo: boot → compose up → curl → attestation

### Phase 4: Polish

- Update demo.sh for compose variant
- Add `vm-compose-demo` convenience output to flake.nix
- Document workload volume format for users
- Test with a multi-container compose stack (e.g., app + redis)
