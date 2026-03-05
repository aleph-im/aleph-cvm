{ pkgs, attest-agent, kernel, init-script, ... }:

let
  # cryptsetup/veritysetup need to be statically linked for the initrd environment.
  staticCryptsetup = pkgs.pkgsStatic.cryptsetup;

  # dm-verity and dm-crypt kernel modules (default =m in the kernel config).
  # Load order: dax → dm-mod → dm-bufio → dm-verity / dm-crypt.
  modDir = "${kernel}/lib/modules/${kernel.modDirVersion}/kernel";
  dmModules = pkgs.runCommand "dm-modules" {
    nativeBuildInputs = [ pkgs.xz ];
  } ''
    mkdir -p $out
    xz -d -k -c ${modDir}/drivers/dax/dax.ko.xz > $out/dax.ko
    xz -d -k -c ${modDir}/drivers/md/dm-mod.ko.xz > $out/dm-mod.ko
    xz -d -k -c ${modDir}/drivers/md/dm-bufio.ko.xz > $out/dm-bufio.ko
    xz -d -k -c ${modDir}/drivers/md/dm-verity.ko.xz > $out/dm-verity.ko
    xz -d -k -c ${modDir}/drivers/md/dm-crypt.ko.xz > $out/dm-crypt.ko
  '';
in
pkgs.makeInitrd {
  contents = [
    { object = "${pkgs.busybox}/bin/busybox"; symlink = "/bin/busybox"; }
    { object = init-script; symlink = "/init"; }
    { object = "${attest-agent}/bin/aleph-attest-agent"; symlink = "/bin/aleph-attest-agent"; }
    { object = "${staticCryptsetup}/bin/veritysetup"; symlink = "/bin/veritysetup"; }
    { object = "${staticCryptsetup}/bin/cryptsetup"; symlink = "/bin/cryptsetup"; }
    # dm-verity and dm-crypt kernel modules (loaded by init.sh).
    { object = "${dmModules}/dax.ko"; symlink = "/lib/modules/dax.ko"; }
    { object = "${dmModules}/dm-mod.ko"; symlink = "/lib/modules/dm-mod.ko"; }
    { object = "${dmModules}/dm-bufio.ko"; symlink = "/lib/modules/dm-bufio.ko"; }
    { object = "${dmModules}/dm-verity.ko"; symlink = "/lib/modules/dm-verity.ko"; }
    { object = "${dmModules}/dm-crypt.ko"; symlink = "/lib/modules/dm-crypt.ko"; }
  ];
}
