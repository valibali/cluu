#!/usr/bin/env bash
# Harness case: virtio-snd audio boot + mp3player playback.
#
# Usage:
#   scripts/harness_audio.sh [boot|play]
#
# "boot" — checks driver init + self-test markers (no login needed).
# "play" — logs in as root, runs mp3player --raw /tmp/test.pcm.
#
# Requires QEMU 8.0+ with virtio-snd-pci device support.
# Run from the repo root after `cargo xtask build`.

set -euo pipefail

MODE="${1:-boot}"

cd "$(dirname "$0")/.."

case "$MODE" in
    boot)
        MARKER_MODE=l2_audio_boot \
        RUN_WAIT=45 \
        python -m cluu_harness --case l2_audio_boot --no-build
        ;;
    play)
        MARKER_MODE=l2_audio_play \
        RUN_WAIT=60 \
        python -m cluu_harness --case l2_audio_play --no-build
        ;;
    *)
        echo "Usage: $0 [boot|play]"
        exit 1
        ;;
esac
