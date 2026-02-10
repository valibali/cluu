#!/bin/bash
# Automated CLUU test harness: build, launch QEMU, type "spawn hello", capture serial output
set -e

PROJECT_ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$PROJECT_ROOT"

SERIAL_LOG="/tmp/cluu-serial-com2.log"
MONITOR_SOCK="/tmp/cluu-qemu-monitor.sock"
OVMF="/usr/share/ovmf/OVMF.fd"
IMG="$PROJECT_ROOT/target/cluu.img"
USER_DISK="$PROJECT_ROOT/target/userdisk.img"
QEMU_PID=""
BOOT_WAIT="${BOOT_WAIT:-8}"
RUN_WAIT="${RUN_WAIT:-5}"
TEST_COMMAND="${TEST_COMMAND:-spawn hello}"
KEY_DELAY="${KEY_DELAY:-0.05}"
MARKER_MODE="${MARKER_MODE:-legacy_p1}"
REQUIRED_MARKERS="${REQUIRED_MARKERS:-}"

cleanup() {
    if [ -n "$QEMU_PID" ] && kill -0 "$QEMU_PID" 2>/dev/null; then
        echo "Killing QEMU (pid $QEMU_PID)..."
        kill "$QEMU_PID" 2>/dev/null || true
        wait "$QEMU_PID" 2>/dev/null || true
    fi
    rm -f "$MONITOR_SOCK"
}
trap cleanup EXIT

# --- Step 1: Build (skip if --no-build) ---
if [ "$1" != "--no-build" ]; then
    echo "=== Full rebuild of CLUU ==="
    rm -rf target/newlib-build target/sysroot/x86_64-cluu-elf
    make clean
    cargo xtask build-newlib
    cargo xtask build-syscalls
    cargo xtask build-crt0
    cargo xtask build
    echo "=== Build complete ==="
fi

if [ ! -f "$IMG" ]; then
    echo "ERROR: $IMG not found. Build failed?"
    exit 1
fi

# --- Step 2: Clear old logs ---
> "$SERIAL_LOG"
rm -f "$MONITOR_SOCK"

# --- Step 3: Launch QEMU ---
echo "=== Starting QEMU (headless) ==="
qemu-system-x86_64 \
    -bios "$OVMF" \
    -m 512M \
    -accel kvm \
    -cpu host \
    -drive "file=$IMG,format=raw,if=ide,index=0" \
    -drive "file=$USER_DISK,format=raw,if=none,id=userblk" \
    -device virtio-blk-pci,drive=userblk \
    -display none \
    -no-reboot \
    -no-shutdown \
    -serial null \
    -serial "file:$SERIAL_LOG" \
    -monitor "unix:$MONITOR_SOCK,server,nowait" \
    &
QEMU_PID=$!
echo "QEMU PID: $QEMU_PID"

# Wait for QEMU to start and the monitor socket to appear
sleep 2
if ! kill -0 "$QEMU_PID" 2>/dev/null; then
    echo "ERROR: QEMU exited prematurely"
    cat "$SERIAL_LOG" 2>/dev/null
    exit 1
fi

# --- Step 4: Wait for boot ---
echo "Waiting ${BOOT_WAIT}s for CLUU to boot..."
sleep "$BOOT_WAIT"

# --- Step 5: Type test command via QEMU monitor ---
echo "Sending command: '$TEST_COMMAND'"
send_key() {
    echo "sendkey $1" | nc -U -q0 "$MONITOR_SOCK" >/dev/null 2>&1 || true
    sleep "$KEY_DELAY"
}

type_ascii_command() {
    local cmd="$1"
    local i ch
    for ((i = 0; i < ${#cmd}; i++)); do
        ch="${cmd:$i:1}"
        case "$ch" in
            ' ') send_key "spc" ;;
            '-') send_key "minus" ;;
            '_') send_key "shift-minus" ;;
            '.') send_key "dot" ;;
            '/') send_key "slash" ;;
            ':') send_key "shift-semicolon" ;;
            [a-z0-9]) send_key "$ch" ;;
            [A-Z]) send_key "shift+${ch,,}" ;;
            *)
                echo "WARN: unsupported character '$ch' in TEST_COMMAND; skipping"
                ;;
        esac
    done
    send_key "ret"
}

type_ascii_command "$TEST_COMMAND"

# --- Step 6: Wait for the test to run ---
echo "Waiting ${RUN_WAIT}s for hello to execute..."
sleep "$RUN_WAIT"

# --- Step 7: Capture and display serial output ---
echo ""
echo "=========================================="
echo "  COM2 Serial Output (debug log)"
echo "=========================================="
cat "$SERIAL_LOG"
echo ""
echo "=========================================="

# Check for faults
if grep -qiE 'PAGE_FAULT|GENERAL_PROTECTION|DOUBLE_FAULT|INVALID_OPCODE' "$SERIAL_LOG"; then
    echo "*** FAULT DETECTED in serial output ***"
    grep -iE 'PAGE_FAULT|GENERAL_PROTECTION|DOUBLE_FAULT|INVALID_OPCODE|PF:|GPF:' "$SERIAL_LOG"
    exit 1
fi

# Check explicit test failures
if grep -qE '\[FAIL\]|test FAILED|PANIC|panic' "$SERIAL_LOG"; then
    echo "*** TEST FAILURE MARKERS DETECTED ***"
    grep -nE '\[FAIL\]|test FAILED|PANIC|panic' "$SERIAL_LOG" || true
    exit 1
fi

# Required success markers.
# Modes:
# - legacy_p1: original timing/TSC fixture checks
# - m0_boot: bootstrap telemetry/manifest checks
# - none: no required marker checks
required_markers=()
case "$MARKER_MODE" in
    legacy_p1)
        required_markers=(
            "TSC calibrated"
            "=== P1 POSIX stubs test ==="
            "[OK] nanosleep(100ms) returned 0"
            "[OK] usleep(50ms) returned 0"
            "=== P1 POSIX stubs test PASSED ==="
        )
        ;;
    m0_boot)
        required_markers=(
            "TSC calibrated"
            "boot-grant: root token handle="
            "boot-grant: clock token handle="
            "telemetry snapshot:"
            "[USER] init: boot manifest"
            "procmgr: exit cookie"
        )
        ;;
    none)
        required_markers=()
        ;;
    *)
        echo "ERROR: unknown MARKER_MODE='$MARKER_MODE'"
        exit 1
        ;;
esac

# Optional override from environment:
# REQUIRED_MARKERS can be newline-separated markers.
if [ -n "$REQUIRED_MARKERS" ]; then
    mapfile -t required_markers <<< "$REQUIRED_MARKERS"
fi

missing=0
if [ "${#required_markers[@]}" -gt 0 ]; then
    for marker in "${required_markers[@]}"; do
        if ! grep -Fq "$marker" "$SERIAL_LOG"; then
            echo "MISSING: $marker"
            missing=1
        fi
    done

    if [ "$missing" -ne 0 ]; then
        echo "*** REQUIRED SUCCESS MARKERS MISSING ***"
        exit 1
    fi
fi

echo "No faults detected and all required markers found."
exit 0
