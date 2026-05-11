#!/usr/bin/env bash
# Dump CLUU framebuffer (or any phys range) from a running harness QEMU,
# via the QEMU monitor socket. Optionally render to PNG.
#
# Usage:
#   scripts/fb_dump.sh -p PHYS [-w WIDTH] [-h HEIGHT] [-s SIZE] [-o OUT_PREFIX]
#
# Defaults match xtask's bootboot config (screen=1280x720) and assume BGRA32:
#   pitch = WIDTH * 4    size = pitch * HEIGHT
#
# Discovering fb_phys (the bootboot/EFI GOP picks it dynamically each run):
#   1. Boot CLUU, then run a probe such as devfb0_probe — it printf's
#      "DEVFB0: geom WxH pitch=P bpp=B size=S phys=PHYS" to the serial log.
#   2. Or read /dev/fb0 directly (40-byte header, bytes 32..40 = u64 phys LE).
#
# Pulls in zero data from the running guest's CPU state — only inspects
# the QEMU monitor socket (path: $MONITOR_SOCK, default
# /tmp/cluu-qemu-monitor.sock per scripts/harness_run.sh).
#
# Requires: socat. ImageMagick `convert` is optional (PNG step skipped if
# absent). KEEP_BIN=0 in env to delete the raw .bin after rendering.

set -euo pipefail

MONITOR_SOCK="${MONITOR_SOCK:-/tmp/cluu-qemu-monitor.sock}"
WIDTH="${FB_WIDTH:-1280}"
HEIGHT="${FB_HEIGHT:-720}"
SIZE="${FB_SIZE:-}"
PHYS="${FB_PHYS:-}"
OUT="${OUT:-/tmp/fb_dump}"
KEEP_BIN="${KEEP_BIN:-1}"

usage() {
    sed -n '2,22p' "$0" | sed 's/^# \?//'
    exit "${1:-0}"
}

while [ $# -gt 0 ]; do
    case "$1" in
        -w) WIDTH="$2"; shift 2 ;;
        -h) HEIGHT="$2"; shift 2 ;;
        -p) PHYS="$2"; shift 2 ;;
        -s) SIZE="$2"; shift 2 ;;
        -o) OUT="$2"; shift 2 ;;
        --help|-help) usage 0 ;;
        *) echo "unknown arg: $1" >&2; usage 1 ;;
    esac
done

if [ -z "$PHYS" ]; then
    echo "fb_dump: -p PHYS required (see header for discovery hints)" >&2
    exit 1
fi

if [ -z "$SIZE" ]; then
    SIZE=$((WIDTH * 4 * HEIGHT))
fi

if [ ! -S "$MONITOR_SOCK" ]; then
    echo "fb_dump: monitor socket not found at $MONITOR_SOCK" >&2
    echo "  is the harness QEMU running? scripts/harness_run.sh sets this up." >&2
    exit 2
fi

SOCAT_OK=0
if command -v socat >/dev/null 2>&1; then
    SOCAT_OK=1
elif ! command -v nc >/dev/null 2>&1; then
    echo "fb_dump: neither socat nor nc found; install one (apt install socat)" >&2
    exit 3
fi

BIN="${OUT}.bin"
PNG="${OUT}.png"

echo "fb_dump: pmemsave phys=$PHYS size=$SIZE -> $BIN" >&2

SIZE_DEC=$(printf '%d' "$SIZE")

# pmemsave addr size filename — filename is from the QEMU process's perspective.
# QEMU monitor protocol: send command, read greeting+response, then disconnect.
if [ "$SOCAT_OK" = "1" ]; then
    {
        printf 'pmemsave %s %s "%s"\n' "$PHYS" "$SIZE_DEC" "$BIN"
        sleep 0.2
    } | socat - "UNIX-CONNECT:$MONITOR_SOCK" >/dev/null 2>&1 || true
else
    printf 'pmemsave %s %s "%s"\n' "$PHYS" "$SIZE_DEC" "$BIN" \
        | nc -U -q0 "$MONITOR_SOCK" >/dev/null 2>&1 || true
fi

# Wait briefly for QEMU to flush the file.
for _ in 1 2 3 4 5; do
    [ -s "$BIN" ] && break
    sleep 0.2
done

if [ ! -s "$BIN" ]; then
    echo "fb_dump: $BIN is empty — pmemsave may have failed (bad phys?)" >&2
    exit 4
fi

ACTUAL_BYTES=$(stat -c %s "$BIN")
EXPECTED_BYTES="$SIZE_DEC"
if [ "$ACTUAL_BYTES" != "$EXPECTED_BYTES" ]; then
    echo "fb_dump: warning — dumped $ACTUAL_BYTES bytes, expected $EXPECTED_BYTES" >&2
fi
echo "fb_dump: dumped $ACTUAL_BYTES bytes" >&2

if command -v convert >/dev/null 2>&1; then
    # `-alpha off` drops the BGRA alpha channel so the PNG isn't shown as
    # transparent in viewers that respect alpha. The fb is always opaque.
    convert -size "${WIDTH}x${HEIGHT}" -depth 8 "bgra:$BIN" -alpha off "$PNG"
    echo "fb_dump: rendered $PNG" >&2
else
    echo "fb_dump: imagemagick convert not found; skipping PNG render" >&2
fi

if [ "$KEEP_BIN" = "0" ]; then
    rm -f "$BIN"
fi
