#!/bin/bash
# Quick test: boot CLUU, login, type "top", capture serial output
set -e

cd /home/vlb2bp/git/cluu

SERIAL_LOG="/tmp/cluu-top-test.log"
MONITOR_SOCK="/tmp/cluu-qemu-monitor.sock"
OVMF="/usr/share/ovmf/OVMF.fd"
IMG="target/cluu.img"
USER_DISK="target/userdisk.img"

rm -f "$SERIAL_LOG" "$MONITOR_SOCK"

echo "=== Starting QEMU (headless) ==="
qemu-system-x86_64 \
    -bios "$OVMF" \
    -machine q35 \
    -m 1G \
    -accel kvm \
    -cpu host \
    -drive "file=$IMG,format=raw,if=ide,index=0" \
    -drive "file=$USER_DISK,format=raw,if=none,id=userblk" \
    -device virtio-blk-pci,drive=userblk,disable-legacy=on,disable-modern=off,vectors=0 \
    -display none \
    -no-reboot \
    -no-shutdown \
    -serial null \
    -serial "file:$SERIAL_LOG" \
    -monitor "unix:$MONITOR_SOCK,server,nowait" &
QEMU_PID=$!

echo "QEMU PID: $QEMU_PID"
sleep 2

# Wait for shell ready
echo "=== Waiting for shell ready ==="
for i in $(seq 1 60); do
    if grep -q "shell: ready" "$SERIAL_LOG" 2>/dev/null; then
        echo "Shell ready at ${i}s"
        break
    fi
    sleep 1
done

if ! grep -q "shell: ready" "$SERIAL_LOG" 2>/dev/null; then
    echo "ERROR: Shell not ready after 60s"
    kill $QEMU_PID 2>/dev/null || true
    cat "$SERIAL_LOG"
    exit 1
fi

sleep 2

# Type "top" via QEMU monitor sendkey
echo "=== Typing 'top' ==="
for key in t o p ret; do
    echo "sendkey $key" | nc -U -q0 "$MONITOR_SOCK" 2>/dev/null || true
    sleep 0.3
done

# Wait for output
echo "=== Waiting for top output (10s) ==="
sleep 10

# Kill QEMU
echo "=== Killing QEMU ==="
kill $QEMU_PID 2>/dev/null || true
wait $QEMU_PID 2>/dev/null || true

echo "=== Serial log (last 200 lines) ==="
tail -200 "$SERIAL_LOG"
