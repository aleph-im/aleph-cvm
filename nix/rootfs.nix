{ pkgs, fib-service, ... }:

let
  staticBusybox = pkgs.busybox.override { enableStatic = true; };
in
pkgs.runCommand "rootfs.ext4" {
  nativeBuildInputs = [ pkgs.e2fsprogs ];
} ''
  # Create a minimal ext4 image with /sbin/init entrypoint.
  mkdir -p rootfs/sbin rootfs/bin
  cp ${fib-service}/bin/fib-service rootfs/bin/
  cp ${staticBusybox}/bin/busybox rootfs/bin/

  # /sbin/init is the rootfs entrypoint convention.
  # Heredoc must not be indented (shebang needs to start at column 0).
  cat > rootfs/sbin/init <<'INIT'
#!/bin/busybox sh
exec /bin/fib-service
INIT
  chmod +x rootfs/sbin/init

  # Calculate size (add 10MB padding).
  size=$(du -sm rootfs | cut -f1)
  size=$((size + 10))

  # Create ext4 image.
  truncate -s ''${size}M $out
  mkfs.ext4 -b 4096 -d rootfs $out
''
