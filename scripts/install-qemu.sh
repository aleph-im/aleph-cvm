#!/usr/bin/env bash
#
# Build and install QEMU from source with SEV-SNP support.
#
# Usage: sudo ./scripts/install-qemu.sh
#
# Installs QEMU 10.2.0 to /usr/local, configured for x86_64 with SEV-SNP.

set -euo pipefail

QEMU_VERSION="10.2.0"
QEMU_URL="https://download.qemu.org/qemu-${QEMU_VERSION}.tar.xz"
BUILD_DIR="/tmp/qemu-build"
JOBS="$(nproc)"

RED='\033[0;31m'
GREEN='\033[0;32m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

info()  { echo -e "${CYAN}[INFO]${NC}  $*"; }
ok()    { echo -e "${GREEN}[OK]${NC}    $*"; }
fail()  { echo -e "${RED}[FAIL]${NC}  $*"; exit 1; }
header(){ echo -e "\n${BOLD}── $* ──${NC}"; }

if [[ $EUID -ne 0 ]]; then
    fail "Run as root: sudo $0"
fi

# ── Dependencies ──────────────────────────────────────────────────────────────

header "Install build dependencies"

apt-get update
apt-get install -y \
    build-essential \
    ninja-build \
    python3 \
    python3-venv \
    pkg-config \
    libglib2.0-dev \
    libpixman-1-dev \
    libslirp-dev \
    libfdt-dev \
    zlib1g-dev \
    libaio-dev \
    libcap-ng-dev \
    libattr1-dev \
    wget \
    xz-utils

ok "Dependencies installed"

# ── Download ──────────────────────────────────────────────────────────────────

header "Download QEMU ${QEMU_VERSION}"

rm -rf "$BUILD_DIR"
mkdir -p "$BUILD_DIR"
cd "$BUILD_DIR"

info "Downloading ${QEMU_URL}..."
wget -q --show-progress "$QEMU_URL"
tar xf "qemu-${QEMU_VERSION}.tar.xz"
cd "qemu-${QEMU_VERSION}"

ok "Source extracted"

# ── Configure ─────────────────────────────────────────────────────────────────

header "Configure"

info "Configuring for x86_64-softmmu with SEV support..."
./configure \
    --target-list=x86_64-softmmu \
    --enable-kvm \
    --enable-slirp \
    --enable-linux-aio \
    --enable-cap-ng \
    --enable-attr \
    --prefix=/usr/local \
    --disable-werror

ok "Configured"

# ── Build ─────────────────────────────────────────────────────────────────────

header "Build (${JOBS} jobs)"

make -j"$JOBS"

ok "Build complete"

# ── Install ───────────────────────────────────────────────────────────────────

header "Install"

make install

ok "Installed to /usr/local"

# ── Verify ────────────────────────────────────────────────────────────────────

header "Verify"

INSTALLED_VERSION=$(/usr/local/bin/qemu-system-x86_64 --version | head -1)
info "$INSTALLED_VERSION"

SNP_SUPPORT=$(/usr/local/bin/qemu-system-x86_64 -object help 2>&1 | grep -c "sev-snp-guest" || true)
if [[ "$SNP_SUPPORT" -gt 0 ]]; then
    ok "sev-snp-guest object available"
else
    fail "sev-snp-guest NOT available — something went wrong"
fi

# ── PATH note ─────────────────────────────────────────────────────────────────

header "Done"

info "Installed at: /usr/local/bin/qemu-system-x86_64"

# Check if /usr/local/bin is shadowed by the distro package
WHICH=$(which qemu-system-x86_64 2>/dev/null || true)
if [[ "$WHICH" != "/usr/local/bin/qemu-system-x86_64" ]]; then
    info "Note: '${WHICH}' is still first in PATH"
    info "Either remove the distro package or use the full path:"
    info "  sudo apt remove -y qemu-system-x86"
    info "  # or symlink:"
    info "  sudo ln -sf /usr/local/bin/qemu-system-x86_64 /usr/bin/qemu-system-x86_64"
fi

# ── Cleanup ───────────────────────────────────────────────────────────────────

info "Build dir left at ${BUILD_DIR} — remove with: rm -rf ${BUILD_DIR}"
