{ pkgs, attest-agent, init-script, ... }:

let
  # veritysetup needs to be statically linked for the initrd environment.
  staticCryptsetup = pkgs.pkgsStatic.cryptsetup;
in
pkgs.makeInitrd {
  contents = [
    { object = "${pkgs.busybox}/bin/busybox"; symlink = "/bin/busybox"; }
    { object = init-script; symlink = "/init"; }
    { object = "${attest-agent}/bin/aleph-attest-agent"; symlink = "/bin/aleph-attest-agent"; }
    { object = "${staticCryptsetup}/bin/veritysetup"; symlink = "/bin/veritysetup"; }
  ];
}
