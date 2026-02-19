#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"

fail() {
    echo "[repo-hygiene] ERROR: $*" >&2
    exit 1
}

echo "[repo-hygiene] checking tracked external source trees..."
if git ls-files | grep -Eq '^external/newlib-|^external/micropython/'; then
    fail "downloaded external sources are tracked in git"
fi

echo "[repo-hygiene] checking tracked generated output roots..."
if git ls-files | grep -Eq '^(target/|tmp/|userfs/)'; then
    fail "generated output roots are tracked in git"
fi

echo "[repo-hygiene] checking tracked generated binaries/objects..."
binary_violations="$(
    git ls-files -z \
        | xargs -0 file --mime-type \
        | while IFS= read -r line; do
            path="${line%%:*}"
            mime="${line#*: }"
            case "$mime" in
                application/x-executable|application/x-pie-executable|application/x-object|application/x-archive|application/x-sharedlib)
                    echo "$path ($mime)"
                    ;;
                application/octet-stream)
                    if [ "$path" != "kernel/font.psf" ]; then
                        echo "$path ($mime)"
                    fi
                    ;;
            esac
        done
)"

if [ -n "$binary_violations" ]; then
    printf '%s\n' "$binary_violations" >&2
    fail "tracked generated binary/object files detected"
fi

echo "[repo-hygiene] checking clean idempotence..."
before_tracked_status="$(git status --porcelain=v1 --untracked-files=no)"
cargo xtask clean-full >/dev/null
after_tracked_status="$(git status --porcelain=v1 --untracked-files=no)"
if [ "$before_tracked_status" != "$after_tracked_status" ]; then
    echo "[repo-hygiene] tracked status before clean-full:" >&2
    printf '%s\n' "$before_tracked_status" >&2
    echo "[repo-hygiene] tracked status after clean-full:" >&2
    printf '%s\n' "$after_tracked_status" >&2
    git status --short >&2
    fail "'cargo xtask clean-full' changed tracked-file status"
fi

echo "[repo-hygiene] OK"
