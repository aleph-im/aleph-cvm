# Phase 1: Compose Runner Rootfs

**Goal**: Build a Nix derivation producing a compose-runner ext4 rootfs image containing podman and friends, with a `/sbin/init` that loads OCI images and runs `podman-compose up`.

## Approach

Use nixpkgs packages as-is (Approach A from brainstorming). The rootfs contains `/nix/store` paths — acceptable since this is a platform component, not user-provided.

## Rootfs Contents

| Package | Purpose |
|---------|---------|
| podman | Container runtime (daemonless) |
| crun | OCI runtime |
| conmon | Container monitor |
| fuse-overlayfs | Overlay FS for container layers |
| cni-plugins | Container networking (bridge mode) |
| podman-compose | Compose orchestration (Python) |
| busybox (static) | Shell for /sbin/init |
| slirp4netns | Rootless networking |

Closure copied via `pkgs.closureInfo`.

## Files

- `nix/compose-rootfs.nix` — builds compose-rootfs.ext4
- `nix/flake.nix` — adds `compose-rootfs` output

## Verification

Boot with QEMU + existing kernel/initrd, verify `podman --version` works inside the rootfs.
