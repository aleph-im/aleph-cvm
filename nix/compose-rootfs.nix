{ pkgs, kernel, ... }:

let
  staticBusybox = pkgs.busybox.override { enableStatic = true; };

  # Kernel modules needed by the container runtime.
  modDir = "${kernel}/lib/modules/${kernel.modDirVersion}/kernel";
  containerModules = pkgs.runCommand "container-modules" {
    nativeBuildInputs = [ pkgs.xz ];
  } ''
    mkdir -p $out
    xz -d -k -c ${modDir}/fs/fuse/fuse.ko.xz > $out/fuse.ko
  '';

  # All packages needed in the compose-runner rootfs.
  runtimePackages = [
    pkgs.podman
    pkgs.crun
    pkgs.conmon
    pkgs.fuse-overlayfs
    pkgs.cni-plugins
    pkgs.podman-compose
    pkgs.slirp4netns
    staticBusybox
  ];

  # Merged environment with bin/, etc. symlinks.
  composeEnv = pkgs.buildEnv {
    name = "compose-runner-env";
    paths = runtimePackages;
    pathsToLink = [ "/bin" "/libexec" "/share" "/etc" ];
  };

  # Full Nix store closure needed by the environment.
  closure = pkgs.closureInfo { rootPaths = runtimePackages; };

  initScript = pkgs.writeText "compose-init" ''
#!/bin/busybox sh
set -e

export HOME=/root
export PATH=/bin
export XDG_RUNTIME_DIR=/run
export CONTAINERS_STORAGE_CONF=/etc/containers/storage.conf

# Writable layers containers need (rootfs is read-only dm-verity).
/bin/busybox mount -t tmpfs tmpfs /run
/bin/busybox mount -t tmpfs tmpfs /tmp
/bin/busybox mount -t tmpfs tmpfs /var
/bin/busybox mkdir -p /var/lib/containers /var/tmp /var/run /run/containers

# Shared memory for podman lock manager.
/bin/busybox mkdir -p /dev/shm
/bin/busybox mount -t tmpfs tmpfs /dev/shm

# Containers need cgroup v2.
/bin/busybox mount -t cgroup2 cgroup2 /sys/fs/cgroup

# Load FUSE kernel module for fuse-overlayfs (container layer storage).
/bin/busybox insmod /lib/modules/fuse.ko 2>&1 || echo "compose-init: warning: insmod fuse.ko failed"

# Load all OCI images from the workload volume.
for tarball in /mnt/workload/images/*.tar; do
    [ -f "$tarball" ] || continue
    echo "compose-init: loading image $tarball"
    podman load -i "$tarball"
done

# Start the compose stack.
cd /mnt/workload
exec podman-compose up --no-build
  '';

in
pkgs.runCommand "compose-rootfs.ext4" {
  nativeBuildInputs = [ pkgs.e2fsprogs ];
} ''
  mkdir -p rootfs/nix/store rootfs/sbin rootfs/bin rootfs/mnt/workload rootfs/etc rootfs/var rootfs/run rootfs/tmp rootfs/dev rootfs/proc rootfs/sys rootfs/root rootfs/sys/fs/cgroup rootfs/etc/containers/networks

  # Copy the full Nix store closure into the rootfs.
  for path in $(cat ${closure}/store-paths); do
    cp -a "$path" rootfs/nix/store/
  done

  # Create /bin symlinks from the merged environment.
  for bin in ${composeEnv}/bin/*; do
    name=$(basename "$bin")
    # Resolve the symlink to an absolute /nix/store path.
    target=$(readlink -f "$bin")
    ln -s "$target" "rootfs/bin/$name"
  done

  # Static busybox goes directly into /bin (overwrite symlink if any).
  rm -f rootfs/bin/busybox
  cp ${staticBusybox}/bin/busybox rootfs/bin/busybox

  # Kernel modules for container runtime.
  mkdir -p rootfs/lib/modules
  cp ${containerModules}/fuse.ko rootfs/lib/modules/

  # /sbin/init entrypoint.
  cp ${initScript} rootfs/sbin/init
  chmod +x rootfs/sbin/init

  # Podman needs basic configs — no leading whitespace (TOML/JSON).
  mkdir -p rootfs/etc/containers
  cat > rootfs/etc/containers/policy.json <<'POLICY'
{"default": [{"type": "insecureAcceptAnything"}]}
POLICY

  cat > rootfs/etc/containers/storage.conf <<'STORAGE'
[storage]
driver = "overlay"
graphroot = "/var/lib/containers/storage"
runroot = "/run/containers/storage"

[storage.options.overlay]
mount_program = "/bin/fuse-overlayfs"
STORAGE

  # CNI networking config for podman.
  mkdir -p rootfs/etc/cni/net.d
  cat > rootfs/etc/cni/net.d/87-podman-bridge.conflist <<'CNI'
{
  "cniVersion": "0.4.0",
  "name": "podman",
  "plugins": [
    {
      "type": "bridge",
      "bridge": "cni-podman0",
      "isGateway": true,
      "ipMasq": true,
      "hairpinMode": true,
      "ipam": {
        "type": "host-local",
        "routes": [{ "dst": "0.0.0.0/0" }],
        "ranges": [[{ "subnet": "10.88.0.0/16", "gateway": "10.88.0.1" }]]
      }
    },
    {
      "type": "portmap",
      "capabilities": { "portMappings": true }
    },
    {
      "type": "firewall"
    },
    {
      "type": "tuning"
    }
  ]
}
CNI

  # containers.conf to tell podman where to find crun and conmon.
  cat > rootfs/etc/containers/containers.conf <<'CONTAINERS'
[engine]
runtime = "crun"

[network]
cni_plugin_dirs = ["/bin"]
CONTAINERS

  # Minimal system files needed by podman/crun.
  echo "root:x:0:0:root:/root:/bin/sh" > rootfs/etc/passwd
  echo "root:x:0:" > rootfs/etc/group
  echo "root:100000:65536" > rootfs/etc/subuid
  echo "root:100000:65536" > rootfs/etc/subgid
  mkdir -p rootfs/root

  # Calculate size (add 50MB padding for a larger rootfs).
  size=$(du -sm rootfs | cut -f1)
  size=$((size + 50))

  truncate -s ''${size}M $out
  mkfs.ext4 -b 4096 -d rootfs $out
''
