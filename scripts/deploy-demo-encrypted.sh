#!/usr/bin/env bash
#
# Deploy and run the encrypted CVM demo on a remote SEV-SNP server.
#
# Usage: ./scripts/deploy-demo-encrypted.sh <ssh-host> [--amd-product Genoa] [--keep-bridge]
#
# Builds artifacts locally, copies them to the remote, and runs demo-encrypted.sh via SSH.
# Requires: nix, cargo, ssh, rsync

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

info()  { echo -e "${CYAN}[INFO]${NC}  $*"; }
ok()    { echo -e "${GREEN}[OK]${NC}    $*"; }
fail()  { echo -e "${RED}[FAIL]${NC}  $*"; }
header(){ echo -e "\n${BOLD}── $* ──${NC}"; }

usage() {
    echo "Usage: $0 <ssh-host> [--amd-product <product>] [--keep-bridge]"
    echo
    echo "  <ssh-host>        SSH destination (e.g. user@sev-server)"
    echo "  --amd-product     AMD product name (default: Genoa). Forwarded to demo-encrypted.sh"
    echo "  --keep-bridge     Don't remove bridge on exit. Forwarded to demo-encrypted.sh"
    exit 1
}

if [[ $# -lt 1 ]]; then
    usage
fi

REMOTE="$1"
shift

DEMO_ARGS=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --amd-product)
            DEMO_ARGS+=("$1" "${2:?--amd-product requires a value}")
            shift 2
            ;;
        --keep-bridge)
            DEMO_ARGS+=("$1")
            shift
            ;;
        *)
            echo "Unknown option: $1"
            usage
            ;;
    esac
done

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REMOTE_DIR="/var/lib/aleph-cvm/demo"

# ── Build ─────────────────────────────────────────────────────────────────────

header "Build"

info "Building VM image (nix)..."
(cd "$REPO_ROOT/nix" && nix build .#vm-fib-demo)
ok "VM image built"

info "Building Rust binaries (cargo)..."
(cd "$REPO_ROOT" && cargo build --release -p aleph-compute-node -p aleph-scheduler-agent -p aleph-attest-cli -p aleph-cvm-cli)
ok "Binaries built"

# ── Deploy ────────────────────────────────────────────────────────────────────

header "Deploy to ${REMOTE}"

info "Creating ${REMOTE_DIR} on remote..."
ssh "$REMOTE" "mkdir -p ${REMOTE_DIR}"

info "Copying artifacts..."
rsync -avPL --checksum --chmod=F755 \
    "$REPO_ROOT/nix/result/"* \
    "$REPO_ROOT/target/release/aleph-compute-node" \
    "$REPO_ROOT/target/release/aleph-scheduler-agent" \
    "$REPO_ROOT/target/release/aleph-attest-cli" \
    "$REPO_ROOT/target/release/aleph-cvm" \
    "$REPO_ROOT/scripts/demo-encrypted.sh" \
    "${REMOTE}:${REMOTE_DIR}/"
ok "Artifacts deployed"

# ── Run ───────────────────────────────────────────────────────────────────────

header "Run encrypted demo"

info "Running demo-encrypted.sh on ${REMOTE}..."
echo
ssh -t "$REMOTE" "sudo ${REMOTE_DIR}/demo-encrypted.sh ${REMOTE_DIR} ${DEMO_ARGS[*]+"${DEMO_ARGS[*]}"}"
