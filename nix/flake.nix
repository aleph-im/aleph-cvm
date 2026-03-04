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

      # Musl cross-compiler for C dependencies (zstd-sys, aws-lc-sys, etc.).
      muslCC = pkgs.pkgsCross.musl64.stdenv.cc;

      # Workspace source with proto files included.
      # Crane's default cleanCargoSource only keeps .rs/Cargo.toml/build.rs.
      # We also need .proto files for tonic-build (aleph-compute-proto).
      workspaceSrc = let
        protoFilter = path: _type: builtins.match ".*\\.proto$" path != null;
        combined = path: type:
          (craneToolchain.filterCargoSources path type) || (protoFilter path type);
      in pkgs.lib.cleanSourceWith {
        src = ../.;
        filter = combined;
      };

      # Attestation agent (static musl binary).
      # Built from the workspace root, selecting just the agent crate.
      # Needs static openssl for openssl-sys (sev crate dependency).
      staticOpenssl = pkgs.pkgsStatic.openssl;
      attest-agent = craneToolchain.buildPackage {
        src = workspaceSrc;
        cargoExtraArgs = "-p aleph-attest-agent";
        CARGO_BUILD_TARGET = "x86_64-unknown-linux-musl";
        CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static";
        nativeBuildInputs = [ pkgs.pkg-config muslCC pkgs.protobuf ];
        buildInputs = [ staticOpenssl.dev ];
        OPENSSL_DIR = "${staticOpenssl.dev}";
        OPENSSL_LIB_DIR = "${staticOpenssl.out}/lib";
        OPENSSL_STATIC = "1";
        OPENSSL_NO_VENDOR = "1";
        # Use musl-targeting C compiler for all C dependencies.
        CC_x86_64_unknown_linux_musl = "${muslCC}/bin/x86_64-unknown-linux-musl-cc";
        AR_x86_64_unknown_linux_musl = "${muslCC}/bin/x86_64-unknown-linux-musl-ar";
        CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER = "${muslCC}/bin/x86_64-unknown-linux-musl-cc";
      };

      # OVMF firmware built with AmdSev variant (kernel hashing support).
      ovmf = import ./ovmf.nix { inherit pkgs; };
      ovmfFd = "${ovmf}/OVMF.fd";

      # nixpkgs 24.11 ships sev-snp-measure 0.0.11 which has a measurement
      # calculation bug.  Override to 0.0.12 which produces correct results.
      sev-snp-measure = pkgs.python3Packages.sev-snp-measure.overridePythonAttrs (old: rec {
        version = "0.0.12";
        src = pkgs.fetchFromGitHub {
          owner = "virtee";
          repo = "sev-snp-measure";
          rev = "v${version}";
          hash = "sha256-UcXU6rNjcRN1T+iWUNrqeJCkSa02WU1/pBwLqHVPRyw=";
        };
      });

    in {
      packages.${system} = {
        inherit fib-service attest-agent ovmf;

        kernel = pkgs.callPackage ./kernel.nix {};
        initrd = pkgs.callPackage ./initrd.nix {
          inherit attest-agent;
          init-script = ./init.sh;
        };
        rootfs = pkgs.callPackage ./rootfs.nix {
          inherit fib-service;
        };

        # Compute dm-verity hash tree and root hash for the demo rootfs.
        # The root hash is embedded in the kernel cmdline, binding rootfs
        # integrity to the SEV-SNP measurement.
        verity = pkgs.runCommand "rootfs-verity" {
          nativeBuildInputs = [ pkgs.cryptsetup ];
        } ''
          mkdir -p $out
          veritysetup format \
            ${self.packages.${system}.rootfs} \
            $out/hashtree \
            | tee /dev/stderr \
            | grep "Root hash:" \
            | awk '{print $NF}' \
            | tr -d '\n' > $out/roothash
        '';

        # Pre-computed SEV-SNP launch measurement for the demo config (2 vCPUs).
        # The kernel cmdline now includes the dm-verity root hash, so the
        # measurement covers the full stack: firmware + kernel + initrd +
        # cmdline (with roothash) → transitively covers rootfs integrity.
        measurement = let
          kernelCmdline = "console=ttyS0 root=/dev/mapper/verity-root ro roothash=${builtins.readFile "${self.packages.${system}.verity}/roothash"}";
        in pkgs.runCommand "sev-snp-measurement" {
          nativeBuildInputs = [ sev-snp-measure ];
        } ''
          sev-snp-measure \
            --mode snp \
            --vcpus 2 \
            --vcpu-type EPYC-v4 \
            --ovmf ${ovmfFd} \
            --kernel ${self.packages.${system}.kernel}/bzImage \
            --initrd ${self.packages.${system}.initrd}/initrd \
            --append "${kernelCmdline}" \
            | tr -d '\n' > $out
        '';

        # Convenience: build all artifacts into one directory.
        # Includes OVMF firmware, pre-computed measurement, and verity artifacts.
        vm-fib-demo = pkgs.runCommand "vm-fib-demo" {} ''
          mkdir -p $out
          ln -s ${self.packages.${system}.kernel}/bzImage $out/bzImage
          ln -s ${self.packages.${system}.initrd}/initrd $out/initrd
          ln -s ${self.packages.${system}.rootfs} $out/rootfs.ext4
          cp ${ovmfFd} $out/OVMF.fd
          cp ${self.packages.${system}.measurement} $out/measurement.hex
          cp ${self.packages.${system}.verity}/hashtree $out/rootfs.ext4.verity
          cp ${self.packages.${system}.verity}/roothash $out/rootfs.ext4.roothash
        '';
      };
    };
}
