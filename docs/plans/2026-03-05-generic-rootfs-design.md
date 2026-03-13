# Generic Rootfs Design

**Goal**: Make the CVM init script rootfs-agnostic so arbitrary user-provided rootfs images work out of the box.

## Convention

Every rootfs provides `/sbin/init`. The platform init.sh (PID 1 in the initrd) handles platform concerns (networking, dm-verity, attestation agent), then runs the rootfs's `/sbin/init` as a child process.

## User Tiers

| Tier | Rootfs `/sbin/init` | Status |
|------|---------------------|--------|
| Power user (minimal binary) | Their binary or a wrapper script | **This design** |
| Docker Compose runner | Compose runtime entrypoint | Future |
| Full Linux distro (systemd) | systemd — needs PID namespace | Future |

## Architecture

The attestation agent stays in the **initrd** (platform layer), not the rootfs. This ensures:

- **Security**: Users can't tamper with or omit the attestation agent. It's part of the SEV-SNP measured platform.
- **Simplicity**: Users just provide their app. No attestation plumbing needed.

Init.sh acts as a minimal supervisor (PID 1):
1. Set up networking, dm-verity, mount rootfs (platform)
2. Start attest-agent (platform)
3. Run rootfs's `/sbin/init` as a child (user workload)

## Changes

### 1. `nix/init.sh`

Replace the hardcoded `fib-service` startup with a generic entrypoint:

```sh
# Before (hardcoded):
if [ -x /mnt/root/bin/fib-service ]; then
    /mnt/root/bin/fib-service &
fi

# After (generic):
if [ -x /mnt/root/sbin/init ]; then
    /mnt/root/sbin/init &
else
    echo "init: WARNING: no /sbin/init found in rootfs"
fi
```

### 2. `nix/rootfs.nix`

Add a `/sbin/init` wrapper script to the demo rootfs and include busybox for the shell interpreter:

```nix
mkdir -p rootfs/sbin rootfs/bin
cp ${fib-service}/bin/fib-service rootfs/bin/
cp ${pkgs.busybox}/bin/busybox rootfs/bin/
cat > rootfs/sbin/init <<'EOF'
#!/bin/busybox sh
exec /bin/fib-service
EOF
chmod +x rootfs/sbin/init
```

### 3. No API or orchestrator changes

- `VmConfig.disks` already accepts arbitrary paths.
- The manager already runs `ensure_verity` on the first disk regardless of contents.
- Kernel cmdline, QEMU args, and dm-verity flow are all rootfs-agnostic.

## Future Enhancements

- **PID namespace for systemd**: Add `unshare --pid --fork` to give the rootfs's init its own PID 1 view, enabling full systemd distros.
- **Docker Compose tier**: Build a rootfs image containing a container runtime + compose runner with `/sbin/init` as the entrypoint.
- **mdev**: Consider adding busybox mdev to the initrd for better device management if more subsystems are needed.
