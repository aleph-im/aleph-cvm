{
  description = "Aleph CVM - Confidential VM images";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-24.11";
    rust-overlay.url = "github:oxalica/rust-overlay";
    crane.url = "github:ipetkov/crane";
  };

  outputs = { self, nixpkgs, rust-overlay, crane, ... }:
    let
      system = "x86_64-linux";
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ rust-overlay.overlays.default ];
      };
      craneLib = crane.mkLib pkgs;

      # Rust toolchain with musl target for static linking.
      rustToolchain = pkgs.rust-bin.stable.latest.default.override {
        targets = [ "x86_64-unknown-linux-musl" ];
      };
      craneToolchain = craneLib.overrideToolchain rustToolchain;

      # Fibonacci service (static musl binary).
      fib-service = craneToolchain.buildPackage {
        src = ./fib-service;
        CARGO_BUILD_TARGET = "x86_64-unknown-linux-musl";
        CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static";
      };

      # Attestation agent (static musl binary).
      # Built from the workspace root, selecting just the agent crate.
      attest-agent = craneToolchain.buildPackage {
        src = ../.;
        cargoExtraArgs = "-p aleph-attest-agent";
        CARGO_BUILD_TARGET = "x86_64-unknown-linux-musl";
        CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static";
      };

    in {
      packages.${system} = {
        inherit fib-service attest-agent;

        kernel = pkgs.callPackage ./kernel.nix {};
        initrd = pkgs.callPackage ./initrd.nix {
          inherit attest-agent;
          init-script = ./init.sh;
        };
        rootfs = pkgs.callPackage ./rootfs.nix {
          inherit fib-service;
        };

        # Convenience: build all three artifacts.
        vm-fib-demo = pkgs.symlinkJoin {
          name = "vm-fib-demo";
          paths = [
            self.packages.${system}.kernel
            self.packages.${system}.initrd
            self.packages.${system}.rootfs
          ];
        };
      };
    };
}
