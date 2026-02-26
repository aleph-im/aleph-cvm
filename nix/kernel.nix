{ pkgs, ... }:

pkgs.linuxPackages_6_6.kernel.override {
  structuredExtraConfig = with pkgs.lib.kernel; {
    # SEV-SNP guest support
    AMD_MEM_ENCRYPT = yes;
    SEV_GUEST = yes;
    CRYPTO_DEV_CCP = yes;
    CRYPTO_DEV_CCP_DD = yes;
    CRYPTO_DEV_SP_PSP = yes;

    # Virtio (for disk and network)
    VIRTIO = yes;
    VIRTIO_PCI = yes;
    VIRTIO_BLK = yes;
    VIRTIO_NET = yes;
    VIRTIO_CONSOLE = yes;

    # Minimal: no modules, everything built-in
    MODULES = no;
  };
}
