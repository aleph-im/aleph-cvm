#!/usr/bin/env bash
#
# SEV-SNP diagnostic script
# Usage: sudo ./check-sev.sh

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

ok()   { echo -e "  ${GREEN}✓${NC} $*"; }
fail() { echo -e "  ${RED}✗${NC} $*"; }
warn() { echo -e "  ${YELLOW}!${NC} $*"; }
info() { echo -e "  ${CYAN}→${NC} $*"; }
header(){ echo -e "\n${BOLD}── $* ──${NC}"; }

ERRORS=0

# ── CPU ───────────────────────────────────────────────────────────────────────

header "CPU"

MODEL=$(grep -m1 "model name" /proc/cpuinfo | cut -d: -f2 | xargs)
info "Model: ${MODEL}"

FAMILY=$(grep -m1 "cpu family" /proc/cpuinfo | cut -d: -f2 | xargs)
info "Family: ${FAMILY}"

if grep -q "sev" /proc/cpuinfo 2>/dev/null; then
    ok "CPUID: SEV capability present"
else
    # Check via flags
    if grep -q " sev " /proc/cpuinfo 2>/dev/null || grep -qw "sev" /proc/cpuinfo 2>/dev/null; then
        ok "CPUID: SEV flag present"
    else
        warn "SEV flag not visible in /proc/cpuinfo (may still be supported)"
    fi
fi

# ── Kernel ────────────────────────────────────────────────────────────────────

header "Kernel"

KVER=$(uname -r)
info "Version: ${KVER}"

info "Command line: $(cat /proc/cmdline)"

if grep -q "mem_encrypt=on" /proc/cmdline; then
    ok "mem_encrypt=on present in cmdline"
else
    fail "mem_encrypt=on NOT in kernel command line"
    ERRORS=$((ERRORS + 1))
fi

# Check kernel config
KCONFIG=""
for f in "/boot/config-${KVER}" "/proc/config.gz"; do
    if [[ -f "$f" ]]; then
        KCONFIG="$f"
        break
    fi
done

if [[ -n "$KCONFIG" ]]; then
    info "Kernel config: ${KCONFIG}"
    if [[ "$KCONFIG" == *.gz ]]; then
        KCAT="zcat"
    else
        KCAT="cat"
    fi

    for opt in CONFIG_AMD_MEM_ENCRYPT CONFIG_KVM_AMD_SEV CONFIG_KVM_AMD CONFIG_CRYPTO_DEV_CCP CONFIG_CRYPTO_DEV_SP_PSP; do
        val=$($KCAT "$KCONFIG" 2>/dev/null | grep "^${opt}=" | head -1) || true
        if [[ -n "$val" ]]; then
            ok "$val"
        else
            fail "${opt} not set"
            ERRORS=$((ERRORS + 1))
        fi
    done
else
    warn "Kernel config not found at /boot/config-${KVER} or /proc/config.gz"
fi

# ── Modules ───────────────────────────────────────────────────────────────────

header "Modules"

for mod in kvm_amd kvm ccp; do
    if lsmod | grep -qw "$mod"; then
        ok "${mod} loaded"
    else
        fail "${mod} NOT loaded"
        ERRORS=$((ERRORS + 1))
    fi
done

# Module parameters
if [[ -d /sys/module/kvm_amd/parameters ]]; then
    info "kvm_amd parameters:"
    for param in sev sev_es sev_snp; do
        p="/sys/module/kvm_amd/parameters/${param}"
        if [[ -f "$p" ]]; then
            val=$(cat "$p")
            if [[ "$val" == "Y" || "$val" == "1" ]]; then
                ok "  ${param} = ${val}"
            else
                fail "  ${param} = ${val}"
                ERRORS=$((ERRORS + 1))
            fi
        else
            fail "  ${param} — parameter file does not exist"
            ERRORS=$((ERRORS + 1))
        fi
    done
else
    fail "kvm_amd parameters directory not found"
    ERRORS=$((ERRORS + 1))
fi

# ── Devices ───────────────────────────────────────────────────────────────────

header "Devices"

if [[ -e /dev/sev ]]; then
    ok "/dev/sev exists ($(ls -l /dev/sev | awk '{print $1, $3, $4}'))"
else
    fail "/dev/sev does not exist"
    ERRORS=$((ERRORS + 1))
fi

if [[ -e /dev/kvm ]]; then
    ok "/dev/kvm exists"
else
    fail "/dev/kvm does not exist"
    ERRORS=$((ERRORS + 1))
fi

# PSP/CCP device
PSP_DEV=$(find /sys/bus/pci/drivers/ccp -maxdepth 1 -name "0000:*" 2>/dev/null | head -1)
if [[ -n "$PSP_DEV" ]]; then
    ok "CCP/PSP PCI device bound: $(basename "$PSP_DEV")"
else
    warn "No CCP/PSP PCI device found in sysfs"
fi

# ── IOMMU ─────────────────────────────────────────────────────────────────────

header "IOMMU"

if dmesg 2>/dev/null | grep -qi "AMD-Vi"; then
    ok "AMD IOMMU (AMD-Vi) detected in dmesg"
else
    warn "AMD-Vi not found in dmesg (may need sudo)"
fi

if [[ -d /sys/class/iommu ]]; then
    IOMMU_COUNT=$(ls /sys/class/iommu/ 2>/dev/null | wc -l)
    if [[ "$IOMMU_COUNT" -gt 0 ]]; then
        ok "IOMMU groups present (${IOMMU_COUNT} IOMMUs)"
    else
        fail "No IOMMU groups found"
        ERRORS=$((ERRORS + 1))
    fi
else
    fail "/sys/class/iommu does not exist"
    ERRORS=$((ERRORS + 1))
fi

# ── SEV firmware ──────────────────────────────────────────────────────────────

header "SEV firmware (dmesg)"

SEV_LINES=$(dmesg 2>/dev/null | grep -i "sev\|snp\|ccp.*sev\|RMP" || true)
if [[ -n "$SEV_LINES" ]]; then
    echo "$SEV_LINES" | while IFS= read -r line; do
        if echo "$line" | grep -qi "error\|fail\|disabled\|not supported\|denied"; then
            fail "  $line"
        else
            info "  $line"
        fi
    done
else
    warn "No SEV-related messages in dmesg"
fi

# ── GRUB config ───────────────────────────────────────────────────────────────

header "GRUB config"

if [[ -f /etc/default/grub ]]; then
    GRUB_LINE=$(grep "^GRUB_CMDLINE_LINUX_DEFAULT" /etc/default/grub || true)
    if [[ -n "$GRUB_LINE" ]]; then
        info "$GRUB_LINE"
    else
        GRUB_LINE=$(grep "^GRUB_CMDLINE_LINUX=" /etc/default/grub || true)
        info "$GRUB_LINE"
    fi
else
    warn "/etc/default/grub not found"
fi

# ── SNP-specific checks ──────────────────────────────────────────────────────

header "SNP specifics"

# RMP table
RMP_LINE=$(dmesg 2>/dev/null | grep -i "RMP table" || true)
if [[ -n "$RMP_LINE" ]]; then
    ok "RMP table: ${RMP_LINE}"
else
    fail "No RMP table found in dmesg — BIOS may not have SNP Memory Coverage enabled"
    ERRORS=$((ERRORS + 1))
fi

# SNP init
SNP_INIT=$(dmesg 2>/dev/null | grep -i "SEV-SNP" || true)
if [[ -n "$SNP_INIT" ]]; then
    echo "$SNP_INIT" | while IFS= read -r line; do
        if echo "$line" | grep -qi "enabled\|supported\|init"; then
            ok "  $line"
        else
            info "  $line"
        fi
    done
else
    fail "No SEV-SNP messages in dmesg"
    ERRORS=$((ERRORS + 1))
fi

# ── Summary ───────────────────────────────────────────────────────────────────

header "Summary"

if [[ $ERRORS -eq 0 ]]; then
    echo -e "\n  ${GREEN}${BOLD}All checks passed — SEV-SNP should be functional.${NC}\n"
else
    echo -e "\n  ${RED}${BOLD}${ERRORS} issue(s) found.${NC}\n"
fi
