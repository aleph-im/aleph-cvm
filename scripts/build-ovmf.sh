#!/usr/bin/env bash
#
# Build OVMF firmware with SEV-SNP support from AMD's EDK2 fork.
#
# Usage: sudo ./scripts/build-ovmf.sh [--output-dir /path/to/output]
#
# Clones AMD's OVMF fork, builds with SEV-SNP flags, and installs
# OVMF.fd to /usr/local/share/ovmf-snp/.
#
# Dependencies (OVMF-specific):
#   nasm           — x86 assembler used by EDK2
#   acpica-tools   — ACPI compiler (iasl) for ACPI table generation
#   python-is-python3 — EDK2 build scripts expect 'python' in PATH
#   uuid-dev       — UUID library headers
#   build-essential — gcc, make, etc.
#   git            — to clone the AMD fork

set -euo pipefail

OVMF_BRANCH="snp-latest"
OVMF_REPO="https://github.com/AMDESE/ovmf.git"
BUILD_DIR="/tmp/ovmf-build"
OUTPUT_DIR="/usr/local/share/ovmf-snp"
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

# ── Argument parsing ──────────────────────────────────────────────────────────

while [[ $# -gt 0 ]]; do
    case "$1" in
        --output-dir)
            OUTPUT_DIR="${2:?--output-dir requires a value}"
            shift 2
            ;;
        *)
            echo "Unknown option: $1"
            echo "Usage: sudo $0 [--output-dir /path/to/output]"
            exit 1
            ;;
    esac
done

if [[ $EUID -ne 0 ]]; then
    fail "Run as root: sudo $0"
fi

# ── Dependencies ──────────────────────────────────────────────────────────────

header "Install OVMF build dependencies"

apt-get update
apt-get install -y \
    build-essential \
    git \
    uuid-dev \
    nasm \
    acpica-tools \
    python-is-python3

ok "Dependencies installed"

# ── Clone ─────────────────────────────────────────────────────────────────────

header "Clone AMD OVMF (branch: ${OVMF_BRANCH})"

rm -rf "$BUILD_DIR"
git clone --depth 1 --branch "$OVMF_BRANCH" "$OVMF_REPO" "$BUILD_DIR"
cd "$BUILD_DIR"
git submodule update --init --depth 1

ok "Source cloned"

# ── Build ─────────────────────────────────────────────────────────────────────

header "Build OVMF (${JOBS} jobs)"

make -C BaseTools -j"$JOBS"

# shellcheck disable=SC1091
source edksetup.sh

build -a X64 -t GCC5 -p OvmfPkg/OvmfPkgX64.dsc \
    -n "$JOBS" \
    -D SMM_REQUIRE=FALSE \
    -D SECURE_BOOT_ENABLE=FALSE \
    -D DEBUG_ON_SERIAL_PORT=TRUE \
    -D TPM2_ENABLE=FALSE

ok "Build complete"

# ── Install ───────────────────────────────────────────────────────────────────

header "Install"

mkdir -p "$OUTPUT_DIR"

FV_DIR="Build/OvmfX64/DEBUG_GCC5/FV"

for fd in OVMF.fd OVMF_CODE.fd OVMF_VARS.fd; do
    if [[ -f "${FV_DIR}/${fd}" ]]; then
        cp "${FV_DIR}/${fd}" "${OUTPUT_DIR}/${fd}"
        ok "Installed ${fd} ($(du -h "${OUTPUT_DIR}/${fd}" | cut -f1))"
    fi
done

# ── Verify ────────────────────────────────────────────────────────────────────

header "Verify"

if [[ ! -f "${OUTPUT_DIR}/OVMF.fd" ]]; then
    fail "OVMF.fd not found — build may have failed"
fi

# Check for SEV metadata (GUID table)
if strings "${OUTPUT_DIR}/OVMF.fd" | grep -q "4c2eb361-7d9b-4cc3-8081-127c90d3d294" 2>/dev/null; then
    ok "SEV metadata GUID found in firmware"
else
    info "Could not confirm SEV metadata GUID (may still work)"
fi

ok "OVMF installed to ${OUTPUT_DIR}/"
ls -lh "${OUTPUT_DIR}/"

# ── Cleanup ───────────────────────────────────────────────────────────────────

header "Done"

info "Build dir left at ${BUILD_DIR} — remove with: rm -rf ${BUILD_DIR}"
info "Use with QEMU: -bios ${OUTPUT_DIR}/OVMF.fd"
