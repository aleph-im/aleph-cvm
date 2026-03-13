#!/usr/bin/env bash
#
# CVM Demo Script
#
# Starts aleph-compute-node (gRPC on Unix socket), sets up networking,
# launches a confidential VM, tests the fib service through the attestation
# proxy, and cleans up.
#
# Usage: sudo ./scripts/demo.sh <artifacts-dir> [--amd-product Genoa] [--keep-bridge] [--ipv6-pool 2001:db8::/48]
#
# Artifacts dir must contain: bzImage, initrd, rootfs.ext4, OVMF.fd, measurement.hex,
#   rootfs.ext4.verity, rootfs.ext4.roothash, aleph-compute-node, aleph-cvm
# Requires: curl, veritysetup (for dm-verity rootfs integrity)

set -euo pipefail

# ── Defaults ──────────────────────────────────────────────────────────────────

BRIDGE="br-aleph"
GATEWAY="10.0.100.1"
SUBNET="10.0.100.0/24"
DHCP_RANGE="10.0.100.10,10.0.100.200,12h"
DHCP_HOSTSDIR="/run/aleph-cvm/dhcp-hosts"
GRPC_SOCKET="/run/aleph-cvm/compute.sock"
VM_ID="fib-demo"
AMD_PRODUCT="Genoa"
KEEP_BRIDGE=false
IPV6_POOL=""

DNSMASQ_PID=""
NODE_PID=""
VM_IP=""
BRIDGE_CREATED=false

# ── Colours ───────────────────────────────────────────────────────────────────

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m' # No Colour

info()  { echo -e "${CYAN}[INFO]${NC}  $*"; }
ok()    { echo -e "${GREEN}[OK]${NC}    $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC}  $*"; }
fail()  { echo -e "${RED}[FAIL]${NC}  $*"; }
header(){ echo -e "\n${BOLD}── $* ──${NC}"; }

# ── Argument parsing ──────────────────────────────────────────────────────────

usage() {
    echo "Usage: sudo $0 <artifacts-dir> [--amd-product <product>] [--keep-bridge] [--ipv6-pool <cidr>]"
    echo
    echo "  <artifacts-dir>   Directory containing: bzImage, initrd, rootfs.ext4, OVMF.fd, measurement.hex, aleph-compute-node"
    echo "  --amd-product     AMD product name for SEV-SNP (default: Genoa)"
    echo "  --keep-bridge     Don't remove the bridge on exit"
    echo "  --ipv6-pool       IPv6 pool for VM addresses (e.g. 2001:db8::/48)"
    exit 1
}

if [[ $# -lt 1 ]]; then
    usage
fi

ARTIFACTS_DIR="$(realpath "$1")"
shift

while [[ $# -gt 0 ]]; do
    case "$1" in
        --amd-product)
            AMD_PRODUCT="${2:?--amd-product requires a value}"
            shift 2
            ;;
        --keep-bridge)
            KEEP_BRIDGE=true
            shift
            ;;
        --ipv6-pool)
            IPV6_POOL="${2:?--ipv6-pool requires a value}"
            shift 2
            ;;
        *)
            echo "Unknown option: $1"
            usage
            ;;
    esac
done

# ── Cleanup trap ──────────────────────────────────────────────────────────────

cleanup() {
    header "Cleanup"

    # Delete VM via gRPC (best-effort)
    info "Deleting VM ${VM_ID}..."
    "${ARTIFACTS_DIR}/aleph-cvm" --socket "$GRPC_SOCKET" delete-vm --vm-id "$VM_ID" >/dev/null 2>&1 || true

    # Kill any leftover QEMU for this VM
    pkill -f "tap-${VM_ID}" 2>/dev/null || true

    # Kill aleph-compute-node
    if [[ -n "$NODE_PID" ]] && kill -0 "$NODE_PID" 2>/dev/null; then
        info "Stopping aleph-compute-node (PID ${NODE_PID})..."
        kill "$NODE_PID" 2>/dev/null || true
        wait "$NODE_PID" 2>/dev/null || true
    fi

    # Remove TAP interface (in case API/aleph-compute-node didn't clean it up)
    if ip link show "tap-${VM_ID}" &>/dev/null; then
        info "Removing leftover TAP tap-${VM_ID}..."
        ip link delete "tap-${VM_ID}" 2>/dev/null || true
    fi

    # Kill dnsmasq
    if [[ -n "$DNSMASQ_PID" ]] && kill -0 "$DNSMASQ_PID" 2>/dev/null; then
        info "Stopping dnsmasq (PID ${DNSMASQ_PID})..."
        kill "$DNSMASQ_PID" 2>/dev/null || true
    fi

    # Remove bridge
    if [[ "$BRIDGE_CREATED" == true && "$KEEP_BRIDGE" == false ]]; then
        info "Removing bridge ${BRIDGE}..."
        ip link set "$BRIDGE" down 2>/dev/null || true
        ip link delete "$BRIDGE" type bridge 2>/dev/null || true
    fi

    # Remove DHCP hostsdir
    rm -rf "$DHCP_HOSTSDIR"

    info "Done."
}
trap cleanup EXIT

# ── 1. Preflight checks ──────────────────────────────────────────────────────

header "Preflight checks"

# Root check
if [[ $EUID -ne 0 ]]; then
    fail "This script must be run as root (TAP/bridge require it)."
    exit 1
fi
ok "Running as root"

# SEV-SNP check
if [[ ! -e /dev/sev ]]; then
    fail "/dev/sev not found — is the SEV driver loaded?"
    exit 1
fi
ok "/dev/sev exists"

SEV_SNP_PARAM="/sys/module/kvm_amd/parameters/sev_snp"
if [[ ! -f "$SEV_SNP_PARAM" ]]; then
    fail "kvm_amd sev_snp parameter not found"
    exit 1
fi
SNP_ENABLED="$(cat "$SEV_SNP_PARAM")"
if [[ "$SNP_ENABLED" != "Y" && "$SNP_ENABLED" != "1" ]]; then
    fail "SEV-SNP is not enabled (sev_snp=${SNP_ENABLED})"
    exit 1
fi
ok "SEV-SNP enabled"

# Required binaries
for bin in qemu-system-x86_64 dnsmasq curl ip veritysetup; do
    if ! command -v "$bin" &>/dev/null; then
        fail "Required binary not found: $bin"
        exit 1
    fi
done
ok "Required binaries found"

# Artifacts (including OVMF firmware, pre-computed measurement, and verity)
for artifact in bzImage initrd rootfs.ext4 OVMF.fd measurement.hex rootfs.ext4.verity rootfs.ext4.roothash aleph-compute-node aleph-cvm; do
    if [[ ! -f "${ARTIFACTS_DIR}/${artifact}" ]]; then
        fail "Artifact not found: ${ARTIFACTS_DIR}/${artifact}"
        exit 1
    fi
done
ok "All artifacts present in ${ARTIFACTS_DIR}"

# ── 2. Load pre-computed measurement ─────────────────────────────────────────

header "Expected measurement"

KERNEL="${ARTIFACTS_DIR}/bzImage"
INITRD="${ARTIFACTS_DIR}/initrd"
ROOTFS="${ARTIFACTS_DIR}/rootfs.ext4"
OVMF_PATH="${ARTIFACTS_DIR}/OVMF.fd"

EXPECTED_MEASUREMENT="$(cat "${ARTIFACTS_DIR}/measurement.hex")"
if [[ -z "$EXPECTED_MEASUREMENT" ]]; then
    fail "measurement.hex is empty"
    exit 1
fi
ok "Expected measurement: ${EXPECTED_MEASUREMENT}"
info "  (pre-computed at build time from OVMF + kernel + initrd)"

# ── 3. Set up networking ─────────────────────────────────────────────────────

header "Networking"

if ip link show "$BRIDGE" &>/dev/null; then
    info "Bridge ${BRIDGE} already exists, skipping creation"
else
    info "Creating bridge ${BRIDGE}..."
    ip link add name "$BRIDGE" type bridge
    ip addr add "${GATEWAY}/24" dev "$BRIDGE"
    ip link set "$BRIDGE" up
    BRIDGE_CREATED=true
    ok "Bridge ${BRIDGE} created with gateway ${GATEWAY}"
fi

# Create DHCP hostsdir for MAC→IP reservations
mkdir -p "$DHCP_HOSTSDIR"
ok "DHCP hostsdir: ${DHCP_HOSTSDIR}"

# Start dnsmasq (DHCP only, no DNS) with hostsdir for reservations
rm -f /run/dnsmasq-aleph.pid /tmp/dnsmasq-aleph.log
info "Starting dnsmasq on ${BRIDGE}..."

DNSMASQ_ARGS=(
    --interface="$BRIDGE"
    --bind-interfaces
    --dhcp-range="$DHCP_RANGE"
    --dhcp-option=3,"$GATEWAY"
    --dhcp-hostsdir="$DHCP_HOSTSDIR"
    --port=0
    --no-resolv
    --log-dhcp
    --pid-file=/run/dnsmasq-aleph.pid
    --log-facility=/tmp/dnsmasq-aleph.log
)

if [[ -n "$IPV6_POOL" ]]; then
    DNSMASQ_ARGS+=(
        --enable-ra
        "--dhcp-range=::,static,ra-stateful"
    )
    info "DHCPv6 + RA enabled for IPv6 pool ${IPV6_POOL}"
fi

dnsmasq "${DNSMASQ_ARGS[@]}"
DNSMASQ_PID="$(cat /run/dnsmasq-aleph.pid)"
ok "dnsmasq running (PID ${DNSMASQ_PID})"

# IP forwarding
sysctl -q -w net.ipv4.ip_forward=1
ok "IPv4 forwarding enabled"

if [[ -n "$IPV6_POOL" ]]; then
    sysctl -q -w net.ipv6.conf.all.forwarding=1
    ok "IPv6 forwarding enabled"
fi

# NAT masquerade is now handled by nftables via aleph-compute-node's setup_nftables()
info "NAT masquerade will be set up by aleph-compute-node (nftables)"

# ── 4. Huge pages ────────────────────────────────────────────────────────────

header "Huge pages"
info "Hugepage allocation is handled by aleph-compute-node at startup"
info "Use --memory-limit and --hugepage-headroom to configure"

# ── 5. Start aleph-compute-node ──────────────────────────────────────────────────────

header "aleph-compute-node"

NODE_BIN="${ARTIFACTS_DIR}/aleph-compute-node"
NODE_LOG="${ARTIFACTS_DIR}/aleph-compute-node.log"

NODE_EXTRA_ARGS=()
if [[ -n "$IPV6_POOL" ]]; then
    NODE_EXTRA_ARGS+=(--ipv6-pool "$IPV6_POOL")
fi

info "Starting aleph-compute-node (socket=${GRPC_SOCKET}, product=${AMD_PRODUCT})..."
"$NODE_BIN" \
    --grpc-socket "$GRPC_SOCKET" \
    --bridge "$BRIDGE" \
    --gateway-ip "$GATEWAY" \
    --amd-product "$AMD_PRODUCT" \
    --dhcp-hostsdir "$DHCP_HOSTSDIR" \
    --ovmf-path "$OVMF_PATH" \
    "${NODE_EXTRA_ARGS[@]}" \
    >"$NODE_LOG" 2>&1 &
NODE_PID=$!
info "aleph-compute-node started (PID ${NODE_PID}), log: ${NODE_LOG}"

# Wait for gRPC socket to appear and Health RPC to respond
CVM_CLI="${ARTIFACTS_DIR}/aleph-cvm"
info "Waiting for aleph-compute-node gRPC socket..."
for i in $(seq 1 30); do
    if [[ -S "$GRPC_SOCKET" ]] && "$CVM_CLI" --socket "$GRPC_SOCKET" health >/dev/null 2>&1; then
        break
    fi
    if ! kill -0 "$NODE_PID" 2>/dev/null; then
        fail "aleph-compute-node exited unexpectedly. Check ${NODE_LOG}"
        exit 1
    fi
    sleep 1
done

if ! "$CVM_CLI" --socket "$GRPC_SOCKET" health >/dev/null 2>&1; then
    fail "aleph-compute-node did not become healthy within 30s"
    exit 1
fi
ok "aleph-compute-node is healthy (gRPC)"

# ── 6. Create VM via API ─────────────────────────────────────────────────────

header "Create VM"

info "Creating VM '${VM_ID}' via gRPC..."
CREATE_RC=0
CREATE_RESPONSE=$("$CVM_CLI" --socket "$GRPC_SOCKET" create-vm \
    --vm-id "$VM_ID" --kernel "$KERNEL" --initrd "$INITRD" \
    --disk "${ROOTFS}:raw:ro" --vcpus 2 --memory-mb 1024 \
    --tee-backend sev-snp 2>&1) || CREATE_RC=$?

if [[ "$CREATE_RC" -ne 0 ]]; then
    fail "VM creation failed (exit code ${CREATE_RC})"
    fail "Response: ${CREATE_RESPONSE}"
    fail "Node log (last 20 lines):"
    tail -20 "${ARTIFACTS_DIR}/aleph-compute-node.log" >&2
    exit 1
fi

echo "$CREATE_RESPONSE"

VM_IP=$(echo "$CREATE_RESPONSE" | python3 -c "import sys,json; print(json.load(sys.stdin)['ipv4'])")
VM_IPV6=$(echo "$CREATE_RESPONSE" | python3 -c "import sys,json; print(json.load(sys.stdin).get('ipv6',''))" 2>/dev/null || echo "")
ok "VM created — IPv4: ${VM_IP}"
if [[ -n "$VM_IPV6" ]]; then
    ok "VM created — IPv6: ${VM_IPV6}"
fi

# ── 7. Wait for VM boot ──────────────────────────────────────────────────────

header "Wait for VM boot"

VM_URL="https://${VM_IP}:8443"
info "Waiting for VM at ${VM_URL}/health (timeout 60s)..."

for i in $(seq 1 60); do
    if curl -skf "${VM_URL}/health" >/dev/null 2>&1; then
        break
    fi
    if (( i % 10 == 0 )); then
        info "Still waiting... (${i}s)"
    fi
    sleep 1
done

if ! curl -skf "${VM_URL}/health" >/dev/null 2>&1; then
    fail "VM did not become healthy within 60s"
    fail "Check VM status: ${CVM_CLI} --socket ${GRPC_SOCKET} get-vm --vm-id ${VM_ID}"
    exit 1
fi
ok "VM is responsive"

# ── 8. Port forwarding ──────────────────────────────────────────────────────

header "Port forwarding"

info "Adding port forward: host:0 → VM:8443 (auto-allocate)..."
FORWARD_RESPONSE=$("$CVM_CLI" --socket "$GRPC_SOCKET" add-port-forward \
    --vm-id "$VM_ID" --vm-port 8443 --host-port 0 2>&1) || {
    fail "Failed to add port forward: ${FORWARD_RESPONSE}"
    exit 1
}
echo "$FORWARD_RESPONSE"
HOST_PORT=$(echo "$FORWARD_RESPONSE" | python3 -c "import sys,json; print(json.load(sys.stdin)['hostPort'])")
ok "Port forward: host :${HOST_PORT} → VM :8443"

# Test connectivity through forwarded port.
# Use the bridge gateway IP — localhost traffic bypasses nftables DNAT
# rules because they match on the external interface, not loopback.
info "Testing forwarded port via ${GATEWAY}:${HOST_PORT}..."
for i in $(seq 1 10); do
    if curl -skf "https://${GATEWAY}:${HOST_PORT}/health" >/dev/null 2>&1; then
        break
    fi
    sleep 1
done
if curl -skf "https://${GATEWAY}:${HOST_PORT}/health" >/dev/null 2>&1; then
    ok "Service reachable via forwarded port ${HOST_PORT}"
else
    warn "Service not reachable via forwarded port"
fi

# List port forwards
info "Listing port forwards..."
"$CVM_CLI" --socket "$GRPC_SOCKET" list-port-forwards --vm-id "$VM_ID"

# Remove port forward
info "Removing port forward..."
"$CVM_CLI" --socket "$GRPC_SOCKET" remove-port-forward \
    --vm-id "$VM_ID" --host-port "$HOST_PORT" --protocol tcp
ok "Port forward removed"

# ── 9. Run tests ─────────────────────────────────────────────────────────────

header "Tests"

PASS=0
TOTAL=0

run_test() {
    local name="$1"
    local url="$2"
    local expected="$3"
    TOTAL=$((TOTAL + 1))

    info "Test: ${name}"
    local response
    response=$(curl -sk "$url" 2>/dev/null) || response=""

    if echo "$response" | python3 -c "
import sys, json
actual = json.load(sys.stdin)
expected = json.loads('$expected')
for k, v in expected.items():
    assert actual.get(k) == v, f'{k}: {actual.get(k)} != {v}'
" 2>/dev/null; then
        ok "PASS — ${name}"
        PASS=$((PASS + 1))
    else
        fail "FAIL — ${name}"
        fail "  URL:      ${url}"
        fail "  Expected: ${expected}"
        fail "  Got:      ${response}"
    fi
}

run_test \
    "Health check" \
    "${VM_URL}/health" \
    '{"status": "ok"}'

run_test \
    "Fibonacci(10)" \
    "${VM_URL}/fib/10" \
    '{"n": 10, "result": 55}'

# Attestation test — verify TLS-bound attestation with measurement pinning (Layer 2)
ATTEST_CLI="${ARTIFACTS_DIR}/aleph-attest-cli"
TOTAL=$((TOTAL + 1))
info "Test: TLS-bound attestation + measurement pinning (Layer 2)"
if [[ -x "$ATTEST_CLI" ]]; then
    ATTEST_OUTPUT=$("$ATTEST_CLI" attest \
        --url "${VM_URL}/health" \
        --amd-product "$AMD_PRODUCT" \
        --expected-measurement "$EXPECTED_MEASUREMENT" \
        2>&1) || true
    if echo "$ATTEST_OUTPUT" | grep -q "Attestation valid: true"; then
        ok "PASS — TLS-bound attestation + measurement pinning"
        PASS=$((PASS + 1))
        echo "$ATTEST_OUTPUT" | while IFS= read -r line; do info "  $line"; done
    else
        fail "FAIL — TLS-bound attestation + measurement pinning"
        echo "$ATTEST_OUTPUT" | while IFS= read -r line; do fail "  $line"; done
    fi
else
    warn "aleph-attest-cli not found, falling back to basic check"
    ATTEST_RESPONSE=$(curl -sk "${VM_URL}/.well-known/attestation?nonce=deadbeef" 2>/dev/null) || ATTEST_RESPONSE=""
    if echo "$ATTEST_RESPONSE" | python3 -c "
import sys, json
data = json.load(sys.stdin)
assert 'data' in data, 'missing data field'
assert 'report_data' in data, 'missing report_data field'
" 2>/dev/null; then
        ok "PASS — Attestation report (unverified)"
        PASS=$((PASS + 1))
    else
        fail "FAIL — Attestation report"
        fail "  Got: ${ATTEST_RESPONSE}"
    fi
fi

# Attestation test — fresh attestation with measurement pinning (Layer 3)
TOTAL=$((TOTAL + 1))
info "Test: Fresh attestation + nonce + measurement pinning (Layer 3)"
if [[ -x "$ATTEST_CLI" ]]; then
    FRESH_OUTPUT=$("$ATTEST_CLI" fresh-attest \
        --url "${VM_URL}" \
        --amd-product "$AMD_PRODUCT" \
        --expected-measurement "$EXPECTED_MEASUREMENT" \
        2>&1) || true
    if echo "$FRESH_OUTPUT" | grep -q "Fresh attestation verified successfully"; then
        ok "PASS — Fresh attestation + nonce + measurement pinning"
        PASS=$((PASS + 1))
        echo "$FRESH_OUTPUT" | while IFS= read -r line; do info "  $line"; done
    else
        fail "FAIL — Fresh attestation + nonce + measurement pinning"
        echo "$FRESH_OUTPUT" | while IFS= read -r line; do fail "  $line"; done
    fi
else
    warn "SKIP — aleph-attest-cli not found"
fi

# ── Summary ───────────────────────────────────────────────────────────────────

header "Summary"

echo
if [[ $PASS -eq $TOTAL ]]; then
    ok "${PASS}/${TOTAL} tests passed"
else
    fail "${PASS}/${TOTAL} tests passed"
fi
echo
info "Expected measurement: ${EXPECTED_MEASUREMENT}"
info "VM is still running at ${VM_URL}"
info "  curl -k ${VM_URL}/health"
info "  curl -k ${VM_URL}/fib/20"
info "  curl -k ${VM_URL}/.well-known/attestation?nonce=cafe"
info "Node gRPC:"
info "  ${CVM_CLI} --socket ${GRPC_SOCKET} health"
info "  ${CVM_CLI} --socket ${GRPC_SOCKET} list-vms"
echo
info "Press Enter to tear down, or Ctrl+C to keep running..."
read -r
