#!/bin/busybox sh
# /init — runs inside the VM as PID 1

set -e

# Mount essential filesystems.
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sysfs /sys
/bin/busybox mount -t devtmpfs devtmpfs /dev

# Bring up loopback.
/bin/busybox ip link set lo up

# Bring up eth0 via DHCP.
/bin/busybox ip link set eth0 up
/bin/busybox udhcpc -i eth0 -s /bin/busybox

# Mount rootfs from virtio block device (if present).
if [ -b /dev/vda ]; then
    /bin/busybox mkdir -p /mnt/root
    /bin/busybox mount -o ro /dev/vda /mnt/root

    # Start the user application from rootfs.
    if [ -x /mnt/root/bin/fib-service ]; then
        /mnt/root/bin/fib-service &
    fi
fi

# Start the attestation agent.
/bin/aleph-attest-agent --port 8443 --upstream http://127.0.0.1:8080 &

# Wait for children.
wait
