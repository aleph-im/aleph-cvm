{ pkgs, lib, ... }:

pkgs.linuxPackages_6_6.kernel.override {
  structuredExtraConfig = with lib.kernel; {
    # SEV-SNP guest support (mkForce to override base config "m" → "y")
    AMD_MEM_ENCRYPT = lib.mkForce yes;
    SEV_GUEST = lib.mkForce yes;
    CRYPTO_DEV_CCP = lib.mkForce yes;
    CRYPTO_DEV_CCP_DD = lib.mkForce yes;
    CRYPTO_DEV_SP_PSP = lib.mkForce yes;

    # Networking (built-in, since we boot from initrd without module loading)
    PACKET = lib.mkForce yes;         # AF_PACKET sockets (required by udhcpc/DHCP)
    UNIX = lib.mkForce yes;           # AF_UNIX sockets

    # Filesystems (built-in for initrd boot)
    EXT4_FS = lib.mkForce yes;        # ext4 rootfs support

    # dm-verity (built-in for rootfs integrity verification).
    # DM_VERITY=y auto-promotes BLK_DEV_DM from m to y.
    # Don't force BLK_DEV_DM directly — that triggers interactive prompts
    # for all DM sub-options (DM_CRYPT, DM_SNAPSHOT, etc.).
    DM_VERITY = lib.mkForce yes;

    # Virtio (for disk and network)
    VIRTIO = lib.mkForce yes;
    VIRTIO_PCI = lib.mkForce yes;
    VIRTIO_BLK = lib.mkForce yes;
    VIRTIO_NET = lib.mkForce yes;
    VIRTIO_CONSOLE = lib.mkForce yes;

    # Note: MODULES left as default (yes) to avoid interactive config questions.
    # For a minimal production kernel, use a fully custom .config instead.
  };
}
