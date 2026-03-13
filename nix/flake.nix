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
          kernel = self.packages.${system}.kernel;
          init-script = ./init.sh;
        };
        rootfs = pkgs.callPackage ./rootfs.nix {
          inherit fib-service;
        };
        compose-rootfs = pkgs.callPackage ./compose-rootfs.nix {
          kernel = self.packages.${system}.kernel;
        };

        # OCI image of fib-service for Docker Compose demo.
        fib-service-image = pkgs.dockerTools.buildImage {
          name = "fib-service";
          tag = "latest";
          copyToRoot = pkgs.buildEnv {
            name = "fib-service-root";
            paths = [ fib-service ];
            pathsToLink = [ "/bin" ];
          };
          config.Cmd = [ "${fib-service}/bin/fib-service" ];
        };

        # Workload volume: compose.yml + OCI image tarballs → ext4.
        compose-workload = pkgs.runCommand "compose-workload.ext4" {
          nativeBuildInputs = [ pkgs.e2fsprogs ];
        } ''
          mkdir -p workload/images
          cp ${./compose-demo/docker-compose.yml} workload/docker-compose.yml
          cp ${self.packages.${system}.fib-service-image} workload/images/fib-service.tar
          size=$(du -sm workload | cut -f1)
          size=$((size + 10))
          truncate -s ''${size}M $out
          mkfs.ext4 -b 4096 -d workload $out
        '';

        # dm-verity for the compose-runner rootfs.
        compose-rootfs-verity = pkgs.runCommand "compose-rootfs-verity" {
          nativeBuildInputs = [ pkgs.cryptsetup ];
        } ''
          mkdir -p $out
          veritysetup format \
            ${self.packages.${system}.compose-rootfs} \
            $out/hashtree \
            | tee /dev/stderr \
            | grep "Root hash:" \
            | awk '{print $NF}' \
            | tr -d '\n' > $out/roothash
        '';

        # dm-verity for the workload volume.
        compose-workload-verity = pkgs.runCommand "compose-workload-verity" {
          nativeBuildInputs = [ pkgs.cryptsetup ];
        } ''
          mkdir -p $out
          veritysetup format \
            ${self.packages.${system}.compose-workload} \
            $out/hashtree \
            | tee /dev/stderr \
            | grep "Root hash:" \
            | awk '{print $NF}' \
            | tr -d '\n' > $out/roothash
        '';

        # Convenience: all compose demo artifacts in one directory.
        vm-compose-demo = let
          runnerRoothash = builtins.readFile "${self.packages.${system}.compose-rootfs-verity}/roothash";
          workloadRoothash = builtins.readFile "${self.packages.${system}.compose-workload-verity}/roothash";
          composeCmdline = "console=ttyS0 root=/dev/mapper/verity-root ro roothash=${runnerRoothash} workload_roothash=${workloadRoothash}";
          composeMeasurement = pkgs.runCommand "compose-measurement-2vcpus-EPYC-v4" {
            nativeBuildInputs = [ sev-snp-measure ];
          } ''
            sev-snp-measure \
              --mode snp \
              --vcpus 2 \
              --vcpu-type EPYC-v4 \
              --ovmf ${ovmfFd} \
              --kernel ${self.packages.${system}.kernel}/bzImage \
              --initrd ${self.packages.${system}.initrd}/initrd \
              --append "${composeCmdline}" \
              | tr -d '\n' > $out
          '';
        in pkgs.runCommand "vm-compose-demo" {} ''
          mkdir -p $out
          ln -s ${self.packages.${system}.kernel}/bzImage $out/bzImage
          ln -s ${self.packages.${system}.initrd}/initrd $out/initrd
          ln -s ${self.packages.${system}.compose-rootfs} $out/rootfs.ext4
          ln -s ${self.packages.${system}.compose-workload} $out/workload.ext4
          cp ${ovmfFd} $out/OVMF.fd
          cp ${composeMeasurement} $out/measurement.hex
          cp ${self.packages.${system}.compose-rootfs-verity}/hashtree $out/rootfs.ext4.verity
          cp ${self.packages.${system}.compose-rootfs-verity}/roothash $out/rootfs.ext4.roothash
          cp ${self.packages.${system}.compose-workload-verity}/hashtree $out/workload.ext4.verity
          cp ${self.packages.${system}.compose-workload-verity}/roothash $out/workload.ext4.roothash
        '';

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

        # Pre-computed SEV-SNP launch measurement.
        # The kernel cmdline includes the dm-verity root hash, so the
        # measurement covers the full stack: firmware + kernel + initrd +
        # cmdline (with roothash) → transitively covers rootfs integrity.
        #
        # Usage:
        #   nix build .#measurement                        # default: 2 vCPUs, EPYC-v4
        #   nix build .#measurement --override-input vcpus 4
        #   nix build .#measurement-4vcpus-epyc-v4         # pre-built variant
        #
        # The measurement is a function of (OVMF + kernel + initrd + cmdline +
        # vCPU count + CPU type), so each configuration needs its own value.
        measurement = self.packages.${system}.measurementFor { vcpus = 2; vcpuType = "EPYC-v4"; };

        # Parameterized measurement builder.
        # vcpus: number of vCPUs (affects SEV-SNP launch measurement)
        # vcpuType: QEMU CPU model (e.g. "EPYC-v4" for Genoa, "EPYC-v3" for Milan)
        measurementFor = { vcpus ? 2, vcpuType ? "EPYC-v4" }: let
          kernelCmdline = "console=ttyS0 root=/dev/mapper/verity-root ro roothash=${builtins.readFile "${self.packages.${system}.verity}/roothash"}";
        in pkgs.runCommand "sev-snp-measurement-${toString vcpus}vcpus-${vcpuType}" {
          nativeBuildInputs = [ sev-snp-measure ];
        } ''
          sev-snp-measure \
            --mode snp \
            --vcpus ${toString vcpus} \
            --vcpu-type ${vcpuType} \
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
