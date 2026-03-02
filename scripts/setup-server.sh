#!/usr/bin/env bash
#
# Set up an SEV-SNP host for running confidential VMs.
#
# Usage: sudo ./scripts/setup-server.sh [--skip-qemu] [--skip-ovmf] [--skip-kernel]
#
# This script:
#   1. Installs a newer kernel (6.14.x) with SEV-SNP support
#   2. Builds and installs QEMU 10.2 from source with SEV-SNP support
#   3. Builds and installs OVMF firmware from AMD's EDK2 fork
#   4. Installs runtime dependencies (dnsmasq, etc.)
#   5. Configures kernel command line for memory encryption
#
# After running, a reboot is required if the kernel was installed or
# the GRUB command line was modified.
#
# Prerequisites:
#   - Ubuntu 24.04 on AMD EPYC (Milan/Genoa/Turin/Siena)
#   - BIOS configured: SME enabled, min SEV non-ES ASID > 1,
#     SNP enabled, SNP Memory Coverage enabled

set -euo pipefail

# ── Versions ──────────────────────────────────────────────────────────────────

QEMU_VERSION="10.2.0"
KERNEL_VERSION="6.14.0-37"
OVMF_BRANCH="snp-latest"

# ── Options ───────────────────────────────────────────────────────────────────

SKIP_QEMU=false
SKIP_OVMF=false
SKIP_KERNEL=false

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

info()  { echo -e "${CYAN}[INFO]${NC}  $*"; }
ok()    { echo -e "${GREEN}[OK]${NC}    $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC}  $*"; }
fail()  { echo -e "${RED}[FAIL]${NC}  $*"; exit 1; }
header(){ echo -e "\n${BOLD}══ $* ══${NC}"; }

usage() {
    echo "Usage: sudo $0 [--skip-qemu] [--skip-ovmf] [--skip-kernel]"
    exit 1
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --skip-qemu)   SKIP_QEMU=true; shift ;;
        --skip-ovmf)   SKIP_OVMF=true; shift ;;
        --skip-kernel) SKIP_KERNEL=true; shift ;;
        --help|-h)     usage ;;
        *)             echo "Unknown option: $1"; usage ;;
    esac
done

if [[ $EUID -ne 0 ]]; then
    fail "Run as root: sudo $0"
fi

NEEDS_REBOOT=false

# ── 1. Kernel ─────────────────────────────────────────────────────────────────

if [[ "$SKIP_KERNEL" == false ]]; then
    header "Kernel"

    CURRENT_KERNEL="$(uname -r)"
    info "Current kernel: ${CURRENT_KERNEL}"

    if [[ "$CURRENT_KERNEL" == "${KERNEL_VERSION}-generic" ]]; then
        ok "Already running kernel ${KERNEL_VERSION}"
    else
        info "Installing kernel ${KERNEL_VERSION}..."

        apt-get update

        # Kernel packages
        apt-get install -y \
            linux-image-unsigned-${KERNEL_VERSION}-generic \
            linux-modules-${KERNEL_VERSION}-generic \
            linux-modules-extra-${KERNEL_VERSION}-generic

        ok "Kernel ${KERNEL_VERSION} installed"
        NEEDS_REBOOT=true
    fi
else
    info "Skipping kernel install (--skip-kernel)"
fi

# ── 2. GRUB / kernel command line ────────────────────────────────────────────

header "Kernel command line"

# Scaleway cloud images override GRUB_CMDLINE_LINUX_DEFAULT in this file
GRUB_CLOUD="/etc/default/grub.d/50-cloudimg-settings.cfg"
GRUB_DEFAULT="/etc/default/grub"

# Pick whichever exists
if [[ -f "$GRUB_CLOUD" ]]; then
    GRUB_FILE="$GRUB_CLOUD"
elif [[ -f "$GRUB_DEFAULT" ]]; then
    GRUB_FILE="$GRUB_DEFAULT"
else
    fail "No GRUB config found"
fi

info "GRUB config: ${GRUB_FILE}"

if grep -q "mem_encrypt=on" "$GRUB_FILE"; then
    ok "mem_encrypt=on already present"
else
    info "Adding mem_encrypt=on to kernel command line..."
    sed -i 's/GRUB_CMDLINE_LINUX_DEFAULT="\(.*\)"/GRUB_CMDLINE_LINUX_DEFAULT="\1 mem_encrypt=on"/' "$GRUB_FILE"
    update-grub
    ok "GRUB updated — mem_encrypt=on added"
    NEEDS_REBOOT=true
fi

# ── 3. Kernel modules ────────────────────────────────────────────────────────

header "Kernel modules"

# Ensure tun and kvm_amd load on boot
for mod in tun kvm_amd; do
    if [[ ! -f "/etc/modules-load.d/${mod}.conf" ]]; then
        echo "$mod" > "/etc/modules-load.d/${mod}.conf"
        ok "Added ${mod} to auto-load"
    else
        ok "${mod} already set to auto-load"
    fi
done

# Load them now if possible
modprobe tun 2>/dev/null && ok "tun loaded" || info "tun will load after reboot"
modprobe kvm_amd 2>/dev/null && ok "kvm_amd loaded" || info "kvm_amd will load after reboot"

# ── 4. Runtime dependencies ──────────────────────────────────────────────────

header "Runtime dependencies"

apt-get update
apt-get install -y \
    dnsmasq-base \
    curl \
    iproute2 \
    iptables

#   dnsmasq-base  — DHCP server for VM network (no daemon autostart)
#   curl          — health checks and API calls in demo script
#   iproute2      — ip command for bridge/TAP management
#   iptables      — NAT masquerade for VM outbound traffic

ok "Runtime dependencies installed"

# ── 5. QEMU ──────────────────────────────────────────────────────────────────

if [[ "$SKIP_QEMU" == false ]]; then
    header "QEMU ${QEMU_VERSION} (from source)"

    # Check if already installed at correct version
    INSTALLED_QEMU=$(/usr/local/bin/qemu-system-x86_64 --version 2>/dev/null | grep -o "version [0-9.]*" || true)
    if [[ "$INSTALLED_QEMU" == "version ${QEMU_VERSION}" ]]; then
        ok "QEMU ${QEMU_VERSION} already installed"
    else
        info "Installing QEMU build dependencies..."
        apt-get install -y \
            build-essential \
            ninja-build \
            python3 \
            python3-venv \
            pkg-config \
            wget \
            xz-utils \
            libglib2.0-dev \
            libpixman-1-dev \
            libslirp-dev \
            libfdt-dev \
            zlib1g-dev \
            libaio-dev \
            libcap-ng-dev \
            libattr1-dev

        #   build-essential — compiler toolchain (gcc, make)
        #   ninja-build     — QEMU's build system
        #   python3, -venv  — QEMU build scripts
        #   pkg-config      — dependency resolution during configure
        #   wget, xz-utils  — download and extract source tarball
        #   libglib2.0-dev  — GLib (QEMU core dependency)
        #   libpixman-1-dev — pixel manipulation (display backend)
        #   libslirp-dev    — user-mode networking
        #   libfdt-dev      — device tree (arm/riscv, optional for x86)
        #   zlib1g-dev      — compression
        #   libaio-dev      — async I/O for disk backends
        #   libcap-ng-dev   — Linux capabilities
        #   libattr1-dev    — extended attributes (virtfs)

        QEMU_BUILD_DIR="/tmp/qemu-build"
        QEMU_URL="https://download.qemu.org/qemu-${QEMU_VERSION}.tar.xz"

        rm -rf "$QEMU_BUILD_DIR"
        mkdir -p "$QEMU_BUILD_DIR"
        cd "$QEMU_BUILD_DIR"

        info "Downloading QEMU ${QEMU_VERSION}..."
        wget -q --show-progress "$QEMU_URL"
        tar xf "qemu-${QEMU_VERSION}.tar.xz"
        cd "qemu-${QEMU_VERSION}"

        info "Configuring..."
        ./configure \
            --target-list=x86_64-softmmu \
            --enable-kvm \
            --enable-slirp \
            --enable-linux-aio \
            --enable-cap-ng \
            --enable-attr \
            --prefix=/usr/local \
            --disable-werror

        info "Building ($(nproc) jobs)..."
        make -j"$(nproc)"
        make install

        ok "QEMU ${QEMU_VERSION} installed to /usr/local"

        # Ensure /usr/local/bin takes precedence
        WHICH=$(which qemu-system-x86_64 2>/dev/null || true)
        if [[ "$WHICH" != "/usr/local/bin/qemu-system-x86_64" ]]; then
            ln -sf /usr/local/bin/qemu-system-x86_64 /usr/bin/qemu-system-x86_64
            ok "Symlinked /usr/bin/qemu-system-x86_64 → /usr/local/bin/"
        fi

        info "Build dir left at ${QEMU_BUILD_DIR}"
    fi

    # Verify SEV-SNP support
    if /usr/local/bin/qemu-system-x86_64 -object help 2>&1 | grep -q "sev-snp-guest"; then
        ok "QEMU sev-snp-guest support confirmed"
    else
        fail "QEMU missing sev-snp-guest support"
    fi
else
    info "Skipping QEMU build (--skip-qemu)"
fi

# ── 6. OVMF ──────────────────────────────────────────────────────────────────

if [[ "$SKIP_OVMF" == false ]]; then
    header "OVMF (AMD SEV-SNP fork)"

    OVMF_OUTPUT="/usr/local/share/ovmf-snp"

    if [[ -f "${OVMF_OUTPUT}/OVMF.fd" ]]; then
        ok "OVMF already built at ${OVMF_OUTPUT}/OVMF.fd"
    else
        info "Installing OVMF build dependencies..."
        apt-get install -y \
            build-essential \
            git \
            uuid-dev \
            nasm \
            acpica-tools \
            python-is-python3

        #   build-essential    — compiler toolchain (gcc, make)
        #   git                — clone AMD's EDK2 fork
        #   uuid-dev           — UUID library headers for EDK2
        #   nasm               — x86 assembler for EDK2 firmware code
        #   acpica-tools       — ACPI compiler (iasl) for ACPI tables
        #   python-is-python3  — EDK2 build scripts expect 'python' in PATH

        OVMF_BUILD_DIR="/tmp/ovmf-build"
        OVMF_REPO="https://github.com/AMDESE/ovmf.git"

        rm -rf "$OVMF_BUILD_DIR"
        info "Cloning AMD OVMF (branch: ${OVMF_BRANCH})..."
        git clone --depth 1 --branch "$OVMF_BRANCH" "$OVMF_REPO" "$OVMF_BUILD_DIR"
        cd "$OVMF_BUILD_DIR"
        git submodule update --init --depth 1

        info "Building ($(nproc) jobs)..."
        make -C BaseTools -j"$(nproc)"

        # shellcheck disable=SC1091
        source edksetup.sh

        build -a X64 -t GCC5 -p OvmfPkg/OvmfPkgX64.dsc \
            -n "$(nproc)" \
            -D SMM_REQUIRE=FALSE \
            -D SECURE_BOOT_ENABLE=FALSE \
            -D DEBUG_ON_SERIAL_PORT=TRUE \
            -D TPM2_ENABLE=FALSE

        mkdir -p "$OVMF_OUTPUT"
        FV_DIR="Build/OvmfX64/DEBUG_GCC5/FV"
        for fd in OVMF.fd OVMF_CODE.fd OVMF_VARS.fd; do
            if [[ -f "${FV_DIR}/${fd}" ]]; then
                cp "${FV_DIR}/${fd}" "${OVMF_OUTPUT}/${fd}"
                ok "Installed ${fd}"
            fi
        done

        info "Build dir left at ${OVMF_BUILD_DIR}"
    fi

    if [[ -f "${OVMF_OUTPUT}/OVMF.fd" ]]; then
        ok "OVMF ready at ${OVMF_OUTPUT}/OVMF.fd"
    else
        fail "OVMF.fd not found after build"
    fi
else
    info "Skipping OVMF build (--skip-ovmf)"
fi

# ── Summary ───────────────────────────────────────────────────────────────────

header "Summary"

echo
info "Installed components:"
info "  QEMU:   /usr/local/bin/qemu-system-x86_64"
info "  OVMF:   /usr/local/share/ovmf-snp/OVMF.fd"
info "  Kernel: ${KERNEL_VERSION}-generic"
echo

if [[ "$NEEDS_REBOOT" == true ]]; then
    warn "A reboot is required for kernel/GRUB changes to take effect."
    warn "Run: sudo reboot"
else
    ok "No reboot needed."
fi

echo
info "After reboot, verify SEV-SNP with: sudo ./scripts/check-sev.sh"
info "Then run the demo with:            sudo ./scripts/demo.sh <artifacts-dir>"
