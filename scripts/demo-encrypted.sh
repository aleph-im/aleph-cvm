#!/usr/bin/env bash
#
# CVM Encrypted Rootfs Demo
#
# Same as demo.sh, but with a LUKS-encrypted rootfs. Demonstrates:
#   1. Building a LUKS image from the demo rootfs
#   2. Booting a CVM with --encrypted (attest-agent starts before rootfs mount)
#   3. Injecting the LUKS passphrase via attested TLS
#   4. Verifying the app works through the attestation proxy
#
# Usage: sudo ./scripts/demo-encrypted.sh <artifacts-dir> [--amd-product Genoa] [--keep-bridge] [--ipv6-pool 2001:db8::/48]
#
# Artifacts dir must contain: bzImage, initrd, rootfs.ext4, OVMF.fd,
#   aleph-compute-node, aleph-cvm, aleph-attest-cli
# Requires: cryptsetup, curl

set -euo pipefail

# ── Defaults ──────────────────────────────────────────────────────────────────

BRIDGE="br-aleph"
GATEWAY="10.0.100.1"
SUBNET="10.0.100.0/24"
DHCP_RANGE="10.0.100.10,10.0.100.200,12h"
DHCP_HOSTSDIR="/run/aleph-cvm/dhcp-hosts"
GRPC_SOCKET="/run/aleph-cvm/compute.sock"
VM_ID="enc-demo"
AMD_PRODUCT="Genoa"
KEEP_BRIDGE=false
IPV6_POOL=""
LUKS_PASSPHRASE="demo-luks-passphrase-$(date +%s)"

DNSMASQ_PID=""
NODE_PID=""
VM_IP=""
BRIDGE_CREATED=false
LUKS_IMG=""

# ── Colours ───────────────────────────────────────────────────────────────────

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

info()  { echo -e "${CYAN}[INFO]${NC}  $*"; }
ok()    { echo -e "${GREEN}[OK]${NC}    $*"; }
warn()  { echo -e "${YELLOW}[WARN]${NC}  $*"; }
fail()  { echo -e "${RED}[FAIL]${NC}  $*"; }
header(){ echo -e "\n${BOLD}── $* ──${NC}"; }

# ── Argument parsing ──────────────────────────────────────────────────────────

usage() {
    echo "Usage: sudo $0 <artifacts-dir> [--amd-product <product>] [--keep-bridge] [--ipv6-pool <cidr>]"
    echo
    echo "  <artifacts-dir>   Directory containing: bzImage, initrd, rootfs.ext4, OVMF.fd, aleph-compute-node, aleph-cvm, aleph-attest-cli"
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

    # Remove TAP interface
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

    # Remove DHCP hostsdir and LUKS image
    rm -rf "$DHCP_HOSTSDIR"
    if [[ -n "$LUKS_IMG" && -f "$LUKS_IMG" ]]; then
        info "Removing LUKS image ${LUKS_IMG}..."
        rm -f "$LUKS_IMG"
    fi

    info "Done."
}
trap cleanup EXIT

# ── 1. Preflight checks ──────────────────────────────────────────────────────

header "Preflight checks"

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

for bin in qemu-system-x86_64 dnsmasq curl ip cryptsetup; do
    if ! command -v "$bin" &>/dev/null; then
        fail "Required binary not found: $bin"
        exit 1
    fi
done
ok "Required binaries found"

for artifact in bzImage initrd rootfs.ext4 OVMF.fd aleph-compute-node aleph-cvm aleph-attest-cli; do
    if [[ ! -f "${ARTIFACTS_DIR}/${artifact}" ]]; then
        fail "Artifact not found: ${ARTIFACTS_DIR}/${artifact}"
        exit 1
    fi
done
ok "All artifacts present in ${ARTIFACTS_DIR}"

# ── 2. Build LUKS-encrypted rootfs ───────────────────────────────────────────

header "Build LUKS-encrypted rootfs"

PLAIN_ROOTFS="${ARTIFACTS_DIR}/rootfs.ext4"
LUKS_IMG="${ARTIFACTS_DIR}/rootfs-luks.img"

PLAIN_SIZE=$(stat -c %s "$PLAIN_ROOTFS")
LUKS_SIZE=$((PLAIN_SIZE + 16 * 1024 * 1024))  # +16MB for LUKS header

info "Plain rootfs: ${PLAIN_ROOTFS} ($(( PLAIN_SIZE / 1024 / 1024 ))MB)"
info "LUKS image:   ${LUKS_IMG} ($(( LUKS_SIZE / 1024 / 1024 ))MB)"
info "Passphrase:   ${LUKS_PASSPHRASE}"

truncate -s "$LUKS_SIZE" "$LUKS_IMG"

info "Formatting as LUKS..."
echo -n "$LUKS_PASSPHRASE" | cryptsetup luksFormat --batch-mode --pbkdf pbkdf2 "$LUKS_IMG" -

info "Opening LUKS container..."
echo -n "$LUKS_PASSPHRASE" | cryptsetup luksOpen "$LUKS_IMG" demo-cryptroot -

info "Copying rootfs into LUKS container..."
dd if="$PLAIN_ROOTFS" of=/dev/mapper/demo-cryptroot bs=4M status=progress 2>&1 || true

info "Closing LUKS container..."
cryptsetup luksClose demo-cryptroot

ok "LUKS-encrypted rootfs built"

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

mkdir -p "$DHCP_HOSTSDIR"
ok "DHCP hostsdir: ${DHCP_HOSTSDIR}"

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

sysctl -q -w net.ipv4.ip_forward=1
ok "IPv4 forwarding enabled"

if [[ -n "$IPV6_POOL" ]]; then
    sysctl -q -w net.ipv6.conf.all.forwarding=1
    ok "IPv6 forwarding enabled"
fi

info "NAT masquerade will be set up by aleph-compute-node (nftables)"

# ── 4. Huge pages ────────────────────────────────────────────────────────────

header "Huge pages"
info "Hugepage allocation is handled by aleph-compute-node at startup"
info "Use --memory-limit and --hugepage-headroom to configure"

# ── 5. Start aleph-compute-node ──────────────────────────────────────────────

header "aleph-compute-node"

NODE_BIN="${ARTIFACTS_DIR}/aleph-compute-node"
NODE_LOG="${ARTIFACTS_DIR}/aleph-compute-node.log"

NODE_EXTRA_ARGS=()
if [[ -n "$IPV6_POOL" ]]; then
    NODE_EXTRA_ARGS+=(--ipv6-pool "$IPV6_POOL")
fi

OVMF_PATH="${ARTIFACTS_DIR}/OVMF.fd"

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

# ── 6. Create encrypted VM ───────────────────────────────────────────────────

header "Create encrypted VM"

KERNEL="${ARTIFACTS_DIR}/bzImage"
INITRD="${ARTIFACTS_DIR}/initrd"

info "Creating VM '${VM_ID}' with --encrypted flag..."
info "  (VM will boot, start attest-agent, and wait for LUKS key)"
CREATE_RC=0
CREATE_RESPONSE=$("$CVM_CLI" --socket "$GRPC_SOCKET" create-vm \
    --vm-id "$VM_ID" --kernel "$KERNEL" --initrd "$INITRD" \
    --disk "${LUKS_IMG}:raw:rw" --vcpus 2 --memory-mb 1024 \
    --tee-backend sev-snp --encrypted 2>&1) || CREATE_RC=$?

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

# ── 7. Wait for attest-agent (before rootfs unlock) ──────────────────────────

header "Wait for attest-agent"

VM_URL="https://${VM_IP}:8443"
info "Waiting for attest-agent at ${VM_URL} (timeout 30s)..."
info "  (attest-agent starts before rootfs is unlocked)"

for i in $(seq 1 30); do
    # The attest-agent is up if we can do a TLS handshake.
    # The upstream app won't be available yet (502), but the agent itself responds.
    if curl -sk "${VM_URL}/.well-known/attestation?nonce=deadbeef" >/dev/null 2>&1; then
        break
    fi
    if (( i % 10 == 0 )); then
        info "Still waiting... (${i}s)"
    fi
    sleep 1
done

if ! curl -sk "${VM_URL}/.well-known/attestation?nonce=deadbeef" >/dev/null 2>&1; then
    fail "Attest-agent did not come up within 30s"
    fail "Check VM status: ${CVM_CLI} --socket ${GRPC_SOCKET} get-vm --vm-id ${VM_ID}"
    fail "Node log (last 20 lines):"
    tail -20 "${ARTIFACTS_DIR}/aleph-compute-node.log" >&2
    exit 1
fi
ok "Attest-agent is up (rootfs still locked)"

# ── 8. Inject LUKS passphrase via attested TLS ──────────────────────────────

header "Inject LUKS passphrase"

ATTEST_CLI="${ARTIFACTS_DIR}/aleph-attest-cli"

info "Injecting LUKS passphrase via attested TLS channel..."
info "  (attest-cli verifies SNP attestation, then sends secret)"

INJECT_RC=0
INJECT_OUTPUT=$("$ATTEST_CLI" inject-secret \
    --url "${VM_URL}" \
    --amd-product "$AMD_PRODUCT" \
    --secret "luks_passphrase=${LUKS_PASSPHRASE}" \
    2>&1) || INJECT_RC=$?

if [[ "$INJECT_RC" -ne 0 ]]; then
    fail "Secret injection failed (exit code ${INJECT_RC})"
    fail "Output: ${INJECT_OUTPUT}"
    exit 1
fi

echo "$INJECT_OUTPUT"
ok "LUKS passphrase injected — VM should be unlocking rootfs now"

# ── 9. Wait for app boot (after rootfs unlock) ──────────────────────────────

header "Wait for app boot"

info "Waiting for fib-service at ${VM_URL}/health (timeout 60s)..."
info "  (VM is unlocking LUKS, mounting rootfs, starting /sbin/init)"

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
    fail "App did not become healthy within 60s after key injection"
    fail "Node log (last 20 lines):"
    tail -20 "${ARTIFACTS_DIR}/aleph-compute-node.log" >&2
    exit 1
fi
ok "App is running (rootfs unlocked and mounted)"

# ── 10. Run tests ────────────────────────────────────────────────────────────

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
    "Health check (through attested proxy)" \
    "${VM_URL}/health" \
    '{"status": "ok"}'

run_test \
    "Fibonacci(10) (through attested proxy)" \
    "${VM_URL}/fib/10" \
    '{"n": 10, "result": 55}'

# Attestation test — Layer 2 (TLS-bound)
TOTAL=$((TOTAL + 1))
info "Test: TLS-bound attestation (Layer 2)"
ATTEST_OUTPUT=$("$ATTEST_CLI" attest \
    --url "${VM_URL}/health" \
    --amd-product "$AMD_PRODUCT" \
    2>&1) || true
if echo "$ATTEST_OUTPUT" | grep -q "Attestation valid: true"; then
    ok "PASS — TLS-bound attestation"
    PASS=$((PASS + 1))
    echo "$ATTEST_OUTPUT" | while IFS= read -r line; do info "  $line"; done
else
    fail "FAIL — TLS-bound attestation"
    echo "$ATTEST_OUTPUT" | while IFS= read -r line; do fail "  $line"; done
fi

# Attestation test — Layer 3 (fresh nonce)
TOTAL=$((TOTAL + 1))
info "Test: Fresh attestation + nonce (Layer 3)"
FRESH_OUTPUT=$("$ATTEST_CLI" fresh-attest \
    --url "${VM_URL}" \
    --amd-product "$AMD_PRODUCT" \
    2>&1) || true
if echo "$FRESH_OUTPUT" | grep -q "Fresh attestation verified successfully"; then
    ok "PASS — Fresh attestation + nonce"
    PASS=$((PASS + 1))
    echo "$FRESH_OUTPUT" | while IFS= read -r line; do info "  $line"; done
else
    fail "FAIL — Fresh attestation + nonce"
    echo "$FRESH_OUTPUT" | while IFS= read -r line; do fail "  $line"; done
fi

# Test that secret injection is one-shot (409 on second attempt)
TOTAL=$((TOTAL + 1))
info "Test: Secret injection is one-shot (409 on second attempt)"
REINJECT_OUTPUT=$("$ATTEST_CLI" inject-secret \
    --url "${VM_URL}" \
    --amd-product "$AMD_PRODUCT" \
    --secret "luks_passphrase=should-fail" \
    2>&1) || true
if echo "$REINJECT_OUTPUT" | grep -qi "409\|already injected\|Conflict"; then
    ok "PASS — Second injection correctly rejected"
    PASS=$((PASS + 1))
else
    fail "FAIL — Second injection was not rejected"
    fail "  Output: ${REINJECT_OUTPUT}"
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
info "Encrypted VM is running at ${VM_URL}"
info "  curl -k ${VM_URL}/health"
info "  curl -k ${VM_URL}/fib/20"
info "  curl -k ${VM_URL}/.well-known/attestation?nonce=cafe"
info "Node gRPC:"
info "  ${CVM_CLI} --socket ${GRPC_SOCKET} health"
info "  ${CVM_CLI} --socket ${GRPC_SOCKET} list-vms"
echo
info "Press Enter to tear down, or Ctrl+C to keep running..."
read -r
