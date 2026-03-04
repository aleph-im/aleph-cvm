{ pkgs, fib-service, ... }:

pkgs.runCommand "rootfs.ext4" {
  nativeBuildInputs = [ pkgs.e2fsprogs ];
} ''
  # Create a minimal ext4 image.
  mkdir -p rootfs/bin
  cp ${fib-service}/bin/fib-service rootfs/bin/

  # Calculate size (add 10MB padding).
  size=$(du -sm rootfs | cut -f1)
  size=$((size + 10))

  # Create ext4 image.
  truncate -s ''${size}M $out
  mkfs.ext4 -b 4096 -d rootfs $out
''
