{ pkgs, attest-agent, init-script, ... }:

pkgs.makeInitrd {
  contents = [
    { object = "${pkgs.busybox}/bin/busybox"; symlink = "/bin/busybox"; }
    { object = init-script; symlink = "/init"; }
    { object = "${attest-agent}/bin/aleph-attest-agent"; symlink = "/bin/aleph-attest-agent"; }
  ];
}
