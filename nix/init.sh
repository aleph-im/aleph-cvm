#!/bin/busybox sh
# /init — runs inside the VM as PID 1

# Mount essential filesystems.
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sysfs /sys
/bin/busybox mount -t devtmpfs devtmpfs /dev
/bin/busybox mkdir -p /etc /tmp

# Bring up loopback.
/bin/busybox ip link set lo up

# Wait for a network interface to appear (virtio-net may take a moment).
n=0
while [ "$n" -lt 30 ]; do
    iface=$(/bin/busybox ls /sys/class/net/ | /bin/busybox grep -v lo | /bin/busybox head -1)
    if [ -n "$iface" ]; then
        break
    fi
    /bin/busybox sleep 0.1
    n=$((n + 1))
done

if [ -z "$iface" ]; then
    echo "init: no network interface found"
else
    # Parse ip= from kernel command line: ip=<client>:::<gateway>:<mask>::<iface>:off
    kernel_ip=$(/bin/busybox sed -n 's/.*ip=\([^ ]*\).*/\1/p' /proc/cmdline)
    if [ -n "$kernel_ip" ]; then
        client_ip=$(echo "$kernel_ip" | /bin/busybox cut -d: -f1)
        gateway=$(echo "$kernel_ip" | /bin/busybox cut -d: -f4)
        mask=$(echo "$kernel_ip" | /bin/busybox cut -d: -f5)
        echo "init: static IP ${client_ip}/${mask} gw ${gateway} on ${iface}"
        /bin/busybox ip link set "$iface" up
        /bin/busybox ip addr add "${client_ip}/24" dev "$iface"
        /bin/busybox ip route add default via "$gateway"
    else
        echo "init: bringing up ${iface} via DHCP"
        /bin/busybox ip link set "$iface" up
        /bin/busybox udhcpc -i "$iface" -q -t 5 -A 2 2>&1 || echo "init: DHCP failed on ${iface}"
    fi
fi

# Wait for block device to appear, then find it.
blkdev=""
n=0
while [ "$n" -lt 30 ]; do
    for dev in /dev/vda /dev/sda; do
        if [ -b "$dev" ]; then
            blkdev="$dev"
            break 2
        fi
    done
    /bin/busybox sleep 0.1
    n=$((n + 1))
done

# Mount rootfs and start user application.
if [ -n "$blkdev" ]; then
    echo "init: mounting ${blkdev}"
    /bin/busybox ls -la /dev/vda /dev/sda 2>&1
    /bin/busybox mkdir -p /mnt/root
    if ! /bin/busybox mount -o ro "$blkdev" /mnt/root; then
        echo "init: mount failed, trying without readonly"
        /bin/busybox mount "$blkdev" /mnt/root || echo "init: mount failed completely"
    fi

    if [ -x /mnt/root/bin/fib-service ]; then
        /mnt/root/bin/fib-service &
    fi
else
    echo "init: no block device found, skipping rootfs mount"
fi

# Start the attestation agent.
/bin/aleph-attest-agent --port 8443 --upstream http://127.0.0.1:8080 &

# Wait for children.
wait
