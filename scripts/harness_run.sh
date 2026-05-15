#!/bin/bash
# CLUU harness runner: build, launch QEMU, inject shell commands, capture serial output
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$PROJECT_ROOT"

SERIAL_LOG="${SERIAL_LOG:-/tmp/cluu-serial-com2.log}"
MONITOR_SOCK="${MONITOR_SOCK:-/tmp/cluu-qemu-monitor.sock}"
OVMF="${OVMF:-/usr/share/ovmf/OVMF.fd}"
IMG="${IMG:-$PROJECT_ROOT/target/cluu.img}"
USER_DISK="${USER_DISK:-$PROJECT_ROOT/target/userdisk.img}"
QEMU_GDB="${QEMU_GDB:-0}"
QEMU_EXTRA_ARGS="${QEMU_EXTRA_ARGS:-}"
# Preferred autostart command env name; HARNESS_AUTOSTART_CMD kept as compatibility alias.
HARNESS_AUTOEXEC_CMD="${HARNESS_AUTOEXEC_CMD:-${HARNESS_AUTOSTART_CMD:-}}"
HARNESS_AUTOEXEC_CMD_FILE="${HARNESS_AUTOEXEC_CMD_FILE:-}"
# Extra shell keystroke injection path (one command per line).
KEYSTROKE_COMMANDS="${KEYSTROKE_COMMANDS:-}"
KEYSTROKE_COMMANDS_FILE="${KEYSTROKE_COMMANDS_FILE:-}"
# GDB integration:
# - manual: user attaches and resumes target manually
# - auto-continue: harness attaches gdb then detaches to resume execution
# - script: harness runs gdb with HARNESS_GDB_SCRIPT
HARNESS_GDB_MODE="${HARNESS_GDB_MODE:-manual}"
HARNESS_GDB_BIN="${HARNESS_GDB_BIN:-gdb}"
HARNESS_GDB_TARGET="${HARNESS_GDB_TARGET:-localhost:1234}"
HARNESS_GDB_TIMEOUT="${HARNESS_GDB_TIMEOUT:-20}"
HARNESS_GDB_MANUAL_TIMEOUT="${HARNESS_GDB_MANUAL_TIMEOUT:-120}"
HARNESS_GDB_SCRIPT="${HARNESS_GDB_SCRIPT:-}"
HARNESS_GDB_SYMBOL="${HARNESS_GDB_SYMBOL:-}"
HARNESS_GDB_BATCH="${HARNESS_GDB_BATCH:-1}"
QEMU_PID=""
BOOT_WAIT="${BOOT_WAIT:-0}"
SHELL_READY_WAIT="${SHELL_READY_WAIT:-45}"
SHELL_READY_WAIT_MAX="${SHELL_READY_WAIT_MAX:-45}"
ALLOW_SLOW_SHELL_WAIT="${ALLOW_SLOW_SHELL_WAIT:-0}"
HARNESS_CLEAN_REBUILD="${HARNESS_CLEAN_REBUILD:-0}"
RUN_WAIT="${RUN_WAIT:-12}"
RUN_WAIT_DEFAULT=""
POST_SENDKEY="${POST_SENDKEY:-}"
POST_SENDKEY_DELAY="${POST_SENDKEY_DELAY:-1}"
# Multi-step raw-monitor sendkey sequence (newline-separated).
# Each line is either "sendkey <name>" or "sleep <n>" (integer seconds).
# Processed in order after POST_SENDKEY. Set via SENDKEY_SEQUENCE or
# SENDKEY_SEQUENCE_DEFAULT (the latter is set by harness_case_defaults.sh).
SENDKEY_SEQUENCE="${SENDKEY_SEQUENCE:-}"
SENDKEY_SEQUENCE_DEFAULT=""
# When 1, SENDKEY_SEQUENCE fires immediately after QEMU starts without waiting
# for the "shell: ready" marker. Use for login-flow tests where there is no
# shell yet at the time keystrokes need to be injected.
SENDKEY_SEQUENCE_NOWAIT="${SENDKEY_SEQUENCE_NOWAIT:-0}"
SENDKEY_SEQUENCE_NOWAIT_DEFAULT="0"
TEST_COMMAND_WAS_UNSET=0
# Preserve explicit empty TEST_COMMAND; only auto-fill when it is truly unset.
if [ -z "${TEST_COMMAND+x}" ]; then
    TEST_COMMAND_WAS_UNSET=1
    TEST_COMMAND="__AUTO__"
fi
TEST_COMMAND_REPEAT="${TEST_COMMAND_REPEAT:-1}"
COMMAND_GAP="${COMMAND_GAP:-1}"
KEY_DELAY="${KEY_DELAY:-0.05}"
MARKER_MODE="${MARKER_MODE:-legacy_p1}"
EXPECT_FAULT="${EXPECT_FAULT:-0}"
# shellcheck source=scripts/harness_case_defaults.sh
. "$SCRIPT_DIR/harness_case_defaults.sh"
harness_derive_marker_defaults

# Debug/testing default: when running ad-hoc mode (MARKER_MODE=none) without an explicit
# typed command, autostart MicroPython from procmgr and avoid injecting spawn hello.
if [ "$MARKER_MODE" = "none" ] \
    && [ "$TEST_COMMAND_WAS_UNSET" = "1" ] \
    && [ -z "$HARNESS_AUTOEXEC_CMD" ]; then
    HARNESS_AUTOEXEC_CMD="micropython"
    TEST_COMMAND=""
fi

# Optional autostart command from file: first non-empty, non-comment line.
if [ -n "$HARNESS_AUTOEXEC_CMD_FILE" ] && [ -z "$HARNESS_AUTOEXEC_CMD" ]; then
    if [ ! -f "$HARNESS_AUTOEXEC_CMD_FILE" ]; then
        echo "ERROR: HARNESS_AUTOEXEC_CMD_FILE not found: $HARNESS_AUTOEXEC_CMD_FILE"
        exit 1
    fi
    while IFS= read -r line || [ -n "$line" ]; do
        line="${line%$'\r'}"
        [ -z "$line" ] && continue
        case "$line" in
            \#*) continue ;;
        esac
        HARNESS_AUTOEXEC_CMD="$line"
        break
    done < "$HARNESS_AUTOEXEC_CMD_FILE"
fi

if [ -z "${CLUU_SHELL_AUTOSTART_CMD:-}" ]; then
    if [ -n "$SHELL_AUTOSTART_CMD_DEFAULT" ]; then
        export CLUU_SHELL_AUTOSTART_CMD="$SHELL_AUTOSTART_CMD_DEFAULT"
    elif [ -n "$HARNESS_AUTOEXEC_CMD" ]; then
        export CLUU_SHELL_AUTOSTART_CMD="$HARNESS_AUTOEXEC_CMD"
    else
        export CLUU_SHELL_AUTOSTART_CMD=""
    fi
fi
if [ -n "$POST_SENDKEY_DEFAULT" ] && [ -z "$POST_SENDKEY" ]; then
    POST_SENDKEY="$POST_SENDKEY_DEFAULT"
fi
if [ -n "$SENDKEY_SEQUENCE_DEFAULT" ] && [ -z "$SENDKEY_SEQUENCE" ]; then
    SENDKEY_SEQUENCE="$SENDKEY_SEQUENCE_DEFAULT"
fi
if [ "$SENDKEY_SEQUENCE_NOWAIT" = "0" ] && [ "$SENDKEY_SEQUENCE_NOWAIT_DEFAULT" = "1" ]; then
    SENDKEY_SEQUENCE_NOWAIT="1"
fi
if [ -n "$RUN_WAIT_DEFAULT" ] && [ "$RUN_WAIT" = "12" ]; then
    RUN_WAIT="$RUN_WAIT_DEFAULT"
fi
REQUIRED_MARKERS="${REQUIRED_MARKERS:-}"
MIN_EXIT_COOKIES="${MIN_EXIT_COOKIES:-3}"
MAX_DELTA_SPACES="${MAX_DELTA_SPACES:-}"
MAX_DELTA_TOKENS="${MAX_DELTA_TOKENS:-}"
MAX_DELTA_ENDPOINTS="${MAX_DELTA_ENDPOINTS:-}"
MAX_DELTA_PMM_USED_FRAMES="${MAX_DELTA_PMM_USED_FRAMES:-}"
MAX_IPC_WAIT_P95_MS="${MAX_IPC_WAIT_P95_MS:-}"
MAX_IPC_WAIT_P99_MS="${MAX_IPC_WAIT_P99_MS:-}"
MAX_IPC_SCAN_AVG_STEPS_X100="${MAX_IPC_SCAN_AVG_STEPS_X100:-}"
MAX_IPC_QUEUE_BYTES_PEAK="${MAX_IPC_QUEUE_BYTES_PEAK:-}"
MAX_IPC_QUEUE_MESSAGES_PEAK="${MAX_IPC_QUEUE_MESSAGES_PEAK:-}"
MIN_NOOP_SPAWN_SAMPLES="${MIN_NOOP_SPAWN_SAMPLES:-8}"
MIN_NOOP_MAP_ELF_SAMPLES="${MIN_NOOP_MAP_ELF_SAMPLES:-8}"
MAX_NOOP_SPAWN_REPLY_P95_CYCLES="${MAX_NOOP_SPAWN_REPLY_P95_CYCLES:-}"
MAX_NOOP_MAP_ELF_REPLY_P95_CYCLES="${MAX_NOOP_MAP_ELF_REPLY_P95_CYCLES:-}"
MAX_FB_BLIT_WC_CYCLES="${MAX_FB_BLIT_WC_CYCLES:-}"
HARNESS_START_SECS=$SECONDS
QEMU_START_SECS=0
SHELL_READY_BASE_SECS=0
BUILD_ELAPSED=""

if ! [[ "$SHELL_READY_WAIT" =~ ^[0-9]+$ ]] || [ "$SHELL_READY_WAIT" -lt 1 ]; then
    echo "ERROR: SHELL_READY_WAIT must be a positive integer"
    exit 1
fi
if ! [[ "$SHELL_READY_WAIT_MAX" =~ ^[0-9]+$ ]] || [ "$SHELL_READY_WAIT_MAX" -lt 1 ]; then
    echo "ERROR: SHELL_READY_WAIT_MAX must be a positive integer"
    exit 1
fi
if ! [[ "$HARNESS_GDB_TIMEOUT" =~ ^[0-9]+$ ]] || [ "$HARNESS_GDB_TIMEOUT" -lt 1 ]; then
    echo "ERROR: HARNESS_GDB_TIMEOUT must be a positive integer"
    exit 1
fi
if ! [[ "$HARNESS_GDB_MANUAL_TIMEOUT" =~ ^[0-9]+$ ]] || [ "$HARNESS_GDB_MANUAL_TIMEOUT" -lt 1 ]; then
    echo "ERROR: HARNESS_GDB_MANUAL_TIMEOUT must be a positive integer"
    exit 1
fi
if [ "$ALLOW_SLOW_SHELL_WAIT" != "1" ] && [ "$SHELL_READY_WAIT" -gt "$SHELL_READY_WAIT_MAX" ]; then
    echo "ERROR: SHELL_READY_WAIT=${SHELL_READY_WAIT}s exceeds policy max ${SHELL_READY_WAIT_MAX}s"
    echo "Set ALLOW_SLOW_SHELL_WAIT=1 only for explicit debug sessions."
    exit 1
fi

cleanup() {
    if [ -n "$QEMU_PID" ] && kill -0 "$QEMU_PID" 2>/dev/null; then
        echo "Killing QEMU (pid $QEMU_PID)..."
        kill "$QEMU_PID" 2>/dev/null || true
        wait "$QEMU_PID" 2>/dev/null || true
    fi
    if [ -n "${SHELL_READY_ELAPSED:-}" ]; then
        echo "HARNESS shell_ready_s=${SHELL_READY_ELAPSED}" >> "$SERIAL_LOG"
    fi
    if [ -n "${BUILD_ELAPSED:-}" ]; then
        echo "HARNESS build_s=${BUILD_ELAPSED}" >> "$SERIAL_LOG"
    fi
    if [ "$QEMU_START_SECS" -gt 0 ] && [ -n "${SHELL_READY_ELAPSED:-}" ]; then
        echo "HARNESS shell_ready_from_resume_s=${SHELL_READY_ELAPSED}" >> "$SERIAL_LOG"
        echo "HARNESS qemu_to_shell_ready_s=$((SHELL_READY_BASE_SECS - QEMU_START_SECS + SHELL_READY_ELAPSED))" >> "$SERIAL_LOG"
    fi
    echo "HARNESS total_s=$((SECONDS - HARNESS_START_SECS))" >> "$SERIAL_LOG"
    rm -f "$MONITOR_SOCK"
}
trap cleanup EXIT

ensure_toolchain_prereqs() {
    local need_prep=0
    if [ ! -f "$PROJECT_ROOT/target/sysroot/lib/libcluu_syscalls.a" ]; then
        need_prep=1
    fi
    if [ ! -f "$PROJECT_ROOT/target/sysroot/lib/crt0.o" ]; then
        need_prep=1
    fi
    if [ ! -f "$PROJECT_ROOT/target/sysroot/x86_64-cluu-elf/lib/libc.a" ]; then
        need_prep=1
    fi

    if [ "$need_prep" -eq 1 ]; then
        echo "=== Preparing toolchain/sysroot artifacts ==="
        cargo xtask build-newlib
        cargo xtask build-syscalls
        cargo xtask build-crt0
    fi
}

wait_for_tcp_port() {
    local target="$1"
    local timeout_s="$2"
    local host="${target%:*}"
    local port="${target##*:}"
    local deadline=$((SECONDS + timeout_s))
    while [ "$SECONDS" -lt "$deadline" ]; do
        if nc -z "$host" "$port" >/dev/null 2>&1; then
            return 0
        fi
        sleep 1
    done
    return 1
}

wait_for_serial_activity() {
    local timeout_s="$1"
    local deadline=$((SECONDS + timeout_s))
    while [ "$SECONDS" -lt "$deadline" ]; do
        if [ -s "$SERIAL_LOG" ]; then
            return 0
        fi
        if ! kill -0 "$QEMU_PID" 2>/dev/null; then
            return 1
        fi
        sleep 1
    done
    return 1
}

run_gdb_control() {
    if [ "$QEMU_GDB" != "1" ]; then
        return 0
    fi

    if ! command -v "$HARNESS_GDB_BIN" >/dev/null 2>&1; then
        echo "ERROR: HARNESS_GDB_BIN not found: $HARNESS_GDB_BIN"
        exit 1
    fi

    if ! wait_for_tcp_port "$HARNESS_GDB_TARGET" "$HARNESS_GDB_TIMEOUT"; then
        echo "ERROR: GDB stub $HARNESS_GDB_TARGET was not reachable within ${HARNESS_GDB_TIMEOUT}s"
        exit 1
    fi

    case "$HARNESS_GDB_MODE" in
        manual)
            echo "QEMU is paused for GDB at $HARNESS_GDB_TARGET."
            echo "Attach manually and resume execution (example):"
            echo "  $HARNESS_GDB_BIN -q ${HARNESS_GDB_SYMBOL:+$HARNESS_GDB_SYMBOL }-ex 'target remote $HARNESS_GDB_TARGET'"
            echo "Waiting up to ${HARNESS_GDB_MANUAL_TIMEOUT}s for serial activity after resume..."
            if ! wait_for_serial_activity "$HARNESS_GDB_MANUAL_TIMEOUT"; then
                echo "ERROR: no serial activity observed after waiting for manual GDB resume"
                exit 1
            fi
            ;;
        auto-continue)
            echo "Auto-resuming paused QEMU via GDB detach ($HARNESS_GDB_TARGET)..."
            gdb_cmd=("$HARNESS_GDB_BIN" -q --batch -ex "set pagination off")
            if [ -n "$HARNESS_GDB_SYMBOL" ]; then
                gdb_cmd+=("$HARNESS_GDB_SYMBOL")
            fi
            gdb_cmd+=(
                -ex "target remote $HARNESS_GDB_TARGET"
                -ex "detach"
                -ex "quit"
            )
            "${gdb_cmd[@]}"
            ;;
        script)
            if [ -z "$HARNESS_GDB_SCRIPT" ]; then
                echo "ERROR: HARNESS_GDB_MODE=script requires HARNESS_GDB_SCRIPT"
                exit 1
            fi
            if [ ! -f "$HARNESS_GDB_SCRIPT" ]; then
                echo "ERROR: HARNESS_GDB_SCRIPT not found: $HARNESS_GDB_SCRIPT"
                exit 1
            fi
            echo "Running GDB script: $HARNESS_GDB_SCRIPT"
            gdb_cmd=("$HARNESS_GDB_BIN" -q -ex "set pagination off")
            if [ "$HARNESS_GDB_BATCH" = "1" ]; then
                gdb_cmd+=(--batch)
            fi
            if [ -n "$HARNESS_GDB_SYMBOL" ]; then
                gdb_cmd+=("$HARNESS_GDB_SYMBOL")
            fi
            gdb_cmd+=(
                -ex "target remote $HARNESS_GDB_TARGET"
                -x "$HARNESS_GDB_SCRIPT"
            )
            "${gdb_cmd[@]}"
            ;;
        *)
            echo "ERROR: unsupported HARNESS_GDB_MODE '$HARNESS_GDB_MODE' (manual|auto-continue|script)"
            exit 1
            ;;
    esac
}

declare -a TYPED_COMMANDS=()

append_keystroke_command() {
    local line="$1"
    line="${line%$'\r'}"
    [ -z "$line" ] && return 0
    case "$line" in
        \#*) return 0 ;;
    esac
    TYPED_COMMANDS+=("$line")
}

load_keystroke_commands_from_file() {
    local path="$1"
    if [ ! -f "$path" ]; then
        echo "ERROR: KEYSTROKE_COMMANDS_FILE not found: $path"
        exit 1
    fi
    while IFS= read -r line || [ -n "$line" ]; do
        append_keystroke_command "$line"
    done < "$path"
}

if [ -n "$KEYSTROKE_COMMANDS_FILE" ]; then
    load_keystroke_commands_from_file "$KEYSTROKE_COMMANDS_FILE"
fi
if [ -n "$KEYSTROKE_COMMANDS" ]; then
    while IFS= read -r line || [ -n "$line" ]; do
        append_keystroke_command "$line"
    done <<< "$KEYSTROKE_COMMANDS"
fi

if ! [[ "$TEST_COMMAND_REPEAT" =~ ^[0-9]+$ ]] || [ "$TEST_COMMAND_REPEAT" -lt 1 ]; then
    echo "ERROR: TEST_COMMAND_REPEAT must be a positive integer"
    exit 1
fi

# Backward-compatible path: TEST_COMMAND(+REPEAT) is appended after explicit KEYSTROKE_COMMANDS.
if [ -n "$TEST_COMMAND" ]; then
    for ((i = 1; i <= TEST_COMMAND_REPEAT; i++)); do
        TYPED_COMMANDS+=("$TEST_COMMAND")
    done
fi

# --- Step 1: Build (skip if --no-build, or auto-skip when image is fresh) ---
if [ "$1" = "--no-build" ]; then
    echo "=== Build skipped (--no-build) ==="
elif [ "$HARNESS_FORCE_BUILD" != "1" ] && [ -f "$IMG" ] \
    && [ -z "$(find kernel userspace scripts xtask Cargo.toml Cargo.lock \
                    -newer "$IMG" -type f 2>/dev/null \
                    -not -path '*/target/*' -not -path '*/.git/*' \
                    -print -quit)" ]; then
    echo "=== Build skipped (image newer than all sources) ==="
    echo "    set HARNESS_FORCE_BUILD=1 to override"
else
    build_start=$SECONDS
    if [ "$HARNESS_CLEAN_REBUILD" = "1" ]; then
        echo "=== Clean rebuild of CLUU ==="
        rm -rf target/newlib-build target/sysroot/x86_64-cluu-elf
        make clean
        cargo xtask build-newlib
        cargo xtask build-syscalls
        cargo xtask build-crt0
    else
        echo "=== Incremental full build of CLUU ==="
        ensure_toolchain_prereqs
        # Always rebuild syscalls/crt0 to pick up libcluu changes
        cargo xtask build-syscalls
        cargo xtask build-crt0
    fi
    cargo xtask build
    BUILD_ELAPSED=$((SECONDS - build_start))
    echo "=== Build complete (${BUILD_ELAPSED}s) ==="
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
qemu_args=(
    -bios "$OVMF"
    -machine q35
    -m 1G
    -accel kvm
    -cpu host
    -drive "file=$IMG,format=raw,if=ide,index=0"
    -drive "file=$USER_DISK,format=raw,if=none,id=userblk"
    -device virtio-blk-pci,drive=userblk,disable-legacy=on,disable-modern=off,vectors=0
    -display none
    -no-reboot
    -no-shutdown
    -serial null
    -serial "file:$SERIAL_LOG"
    -monitor "unix:$MONITOR_SOCK,server,nowait"
)

if [ "$QEMU_GDB" = "1" ]; then
    echo "QEMU_GDB=1: enabling -S -s (wait for GDB on tcp:1234)"
    qemu_args+=(-S -s)
fi

if [ -n "$QEMU_EXTRA_ARGS" ]; then
    # shellcheck disable=SC2206
    extra_args=( $QEMU_EXTRA_ARGS )
    qemu_args+=("${extra_args[@]}")
fi

qemu-system-x86_64 "${qemu_args[@]}" &
QEMU_PID=$!
QEMU_START_SECS=$SECONDS
echo "QEMU PID: $QEMU_PID"

# Wait for QEMU to start and the monitor socket to appear
sleep 2
if ! kill -0 "$QEMU_PID" 2>/dev/null; then
    echo "ERROR: QEMU exited prematurely"
    cat "$SERIAL_LOG" 2>/dev/null
    exit 1
fi

run_gdb_control
SHELL_READY_BASE_SECS=$SECONDS

# --- Step 4: Wait for boot ---
if [ "$BOOT_WAIT" -gt 0 ]; then
    echo "Waiting ${BOOT_WAIT}s before shell marker polling..."
    sleep "$BOOT_WAIT"
fi

wait_for_shell_ready() {
    local deadline=$((SHELL_READY_BASE_SECS + SHELL_READY_WAIT))
    while [ "$SECONDS" -lt "$deadline" ]; do
        if grep -Fq "[USER] shell: ready" "$SERIAL_LOG"; then
            SHELL_READY_ELAPSED=$((SECONDS - SHELL_READY_BASE_SECS))
            return 0
        fi
        sleep 1
    done
    return 1
}

_need_shell_ready=0
if [ "${#TYPED_COMMANDS[@]}" -gt 0 ] || [ -n "$POST_SENDKEY" ]; then
    _need_shell_ready=1
fi
if [ -n "$SENDKEY_SEQUENCE" ] && [ "$SENDKEY_SEQUENCE_NOWAIT" != "1" ]; then
    _need_shell_ready=1
fi
if [ "$_need_shell_ready" = "1" ]; then
    echo "Waiting up to ${SHELL_READY_WAIT}s for shell readiness marker..."
    if ! wait_for_shell_ready; then
        echo "ERROR: shell readiness marker not observed within ${SHELL_READY_WAIT}s"
        echo "----- serial tail (last 200 lines) -----"
        tail -n 200 "$SERIAL_LOG" || true
        echo "----------------------------------------"
        exit 1
    fi
    echo "Shell readiness observed in ${SHELL_READY_ELAPSED}s"
fi

# --- Step 5: Type test command(s) via QEMU monitor ---
# When KEY_DELAY=0 and a long string is being typed, the per-key open/close
# of `nc` becomes the rate bottleneck (~10ms/key). FAST_KEYSTROKES=1 batches
# all sendkey lines into a single nc invocation — QEMU then parses them as
# fast as it can read from the socket, hitting saturation rates the regular
# 50/s path can't reach. Useful for stress tests like perf_typing_storm.
FAST_KEYSTROKES_BATCH=()
FAST_MODE="${FAST_KEYSTROKES:-}"

queue_key() {
    if [ -n "$FAST_MODE" ]; then
        FAST_KEYSTROKES_BATCH+=("sendkey $1")
    else
        send_key "$1"
    fi
}

flush_fast_batch() {
    if [ -n "$FAST_MODE" ] && [ "${#FAST_KEYSTROKES_BATCH[@]}" -gt 0 ]; then
        printf '%s\n' "${FAST_KEYSTROKES_BATCH[@]}" \
            | nc -U -w 5 "$MONITOR_SOCK" >/dev/null 2>&1 || true
        FAST_KEYSTROKES_BATCH=()
    fi
}

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
            ' ') queue_key "spc" ;;
            $'\t') queue_key "tab" ;;
            '-') queue_key "minus" ;;
            '_') queue_key "shift-minus" ;;
            '=') queue_key "equal" ;;
            '+') queue_key "shift-equal" ;;
            '.') queue_key "dot" ;;
            ',') queue_key "comma" ;;
            # HU layout: '/' lives on Shift+6, '?' on the comma key with shift.
            # The US 'slash' scancode (0x35) maps to '-' on HU.
            '/') queue_key "shift-6" ;;
            '?') queue_key "shift-comma" ;;
            ';') queue_key "semicolon" ;;
            ':') queue_key "shift-semicolon" ;;
            "'") queue_key "apostrophe" ;;
            '"') queue_key "shift-apostrophe" ;;
            '(') queue_key "shift-9" ;;
            ')') queue_key "shift-0" ;;
            '[') queue_key "bracket_left" ;;
            ']') queue_key "bracket_right" ;;
            '{') queue_key "shift-bracket_left" ;;
            '}') queue_key "shift-bracket_right" ;;
            '\\') queue_key "backslash" ;;
            '|') queue_key "shift-backslash" ;;
            '!') queue_key "shift-1" ;;
            '@') queue_key "shift-2" ;;
            '#') queue_key "shift-3" ;;
            '$') queue_key "shift-4" ;;
            '%') queue_key "shift-5" ;;
            '^') queue_key "shift-6" ;;
            '&') queue_key "shift-7" ;;
            '*') queue_key "shift-8" ;;
            '<') queue_key "shift-comma" ;;
            '>') queue_key "shift-dot" ;;
            '`') queue_key "grave_accent" ;;
            '~') queue_key "shift-grave_accent" ;;
            # HU (QWERTZ) layout swaps y↔z scancodes.  QEMU sendkey uses
            # US key names, so we pre-swap to produce the intended character.
            'y') queue_key "z" ;;
            'z') queue_key "y" ;;
            'Y') queue_key "shift-z" ;;
            'Z') queue_key "shift-y" ;;
            [a-z0-9]) queue_key "$ch" ;;
            [A-Z]) queue_key "shift-${ch,,}" ;;
            *)
                echo "WARN: unsupported character '$ch' in keystroke command; skipping"
                ;;
        esac
    done
    queue_key "ret"
}

if [ "${#TYPED_COMMANDS[@]}" -gt 0 ]; then
    for ((i = 0; i < ${#TYPED_COMMANDS[@]}; i++)); do
        cmd="${TYPED_COMMANDS[$i]}"
        echo "Sending command $((i + 1))/${#TYPED_COMMANDS[@]}: '$cmd'"
        type_ascii_command "$cmd"
        if [ $((i + 1)) -lt "${#TYPED_COMMANDS[@]}" ]; then
            sleep "$COMMAND_GAP"
        fi
    done
    flush_fast_batch
fi

if [ -n "$POST_SENDKEY" ]; then
    sleep "$POST_SENDKEY_DELAY"
    echo "Sending post key: '$POST_SENDKEY'"
    queue_key "$POST_SENDKEY"
    flush_fast_batch
fi

# SENDKEY_SEQUENCE: newline-separated list of "sendkey <name>" or "sleep <n>".
# Executed in order after POST_SENDKEY. Used by compositor harness modes that
# need to inject multiple raw monitor commands (e.g., Ctrl+Alt+F5 then F1).
if [ -n "$SENDKEY_SEQUENCE" ]; then
    echo "Executing SENDKEY_SEQUENCE..."
    while IFS= read -r seq_line || [ -n "$seq_line" ]; do
        seq_line="${seq_line%$'\r'}"
        [ -z "$seq_line" ] && continue
        case "$seq_line" in
            sendkey\ *)
                local_key="${seq_line#sendkey }"
                echo "  sendkey $local_key"
                send_key "$local_key"
                ;;
            sleep\ *)
                local_secs="${seq_line#sleep }"
                echo "  sleep $local_secs"
                sleep "$local_secs"
                ;;
            *)
                echo "  WARN: unknown SENDKEY_SEQUENCE line: $seq_line"
                ;;
        esac
    done <<< "$SENDKEY_SEQUENCE"
fi

# --- Step 6: Wait for the test to run (streaming, deferred) ---
# The actual wait is deferred until after marker dispatch (line ~1345) so we
# can early-exit once all required_markers are present in the serial log.
# Faults / explicit test failures abort the wait early too.

# Required success markers.
# Modes:
# - legacy_p1: original timing/TSC fixture checks
# - m0_boot: bootstrap telemetry/manifest checks
# - m1_recv: recv/wakeup churn checks
# - m2_token_audit: recv churn + token audit telemetry invariants
# - m2_leakdiag: churn + resource delta diagnostics
# - m3_mapfail: kernel map-range failpoint rollback validation via mapfail probe
# - m3_mapcopyfail: copy_from_user failure branch rollback validation via mapcopyfail probe
# - m3_maperror: map_user_page error branch rollback validation via maperror probe
# - m4_sender_auth: authenticated sender binding in VFS (ignore caller-supplied client_id)
# - m4_registry_sender_auth: authenticated sender binding in registry subscribe/register flows
# - m4_notify_lifecycle: sender notify bindings are reclaimed after child lifecycle ends
# - m4_deny_paths: explicit sender-auth denial path regressions (PermissionDenied flows)
# - m4_registry_deny_paths: explicit registry ownership denial path regressions
# - l2_cd: `cd`/`pwd` shell builtins change and report current directory
# - l2_ext2write: end-to-end ext2 write smoke test via ext2io probe
# - l2_ext2append: append-past-EOF ext2 smoke test via ext2io probe
# - l2_ext2mutate: mkdir/rename/rmdir ext2 metadata mutation smoke test via ext2io probe
# - l2_ext2unlink: create+unlink verification smoke test via ext2io probe
# - l2_envelope_mounts: read-only /etc enforced via user envelope mount view (RED until UE10)
# - l2_owner_deny: explicit non-owner mutation denial via ownerdeny probe
# - l2_sigint: foreground spawn interrupted by Ctrl-C (minimal SIGINT path)
# - l2_jobs: background spawn + async reap notification (`SIGCHLD`-style baseline)
# - l2_fg: background spawn promoted to foreground wait path via `fg`
# - l2_stop: background job transitions to stopped state via `stop`
# - l2_jobchurn: repeated stop/resume/foreground cycles with telemetry signal counters
# - l2_jobchurn_heavy: higher-volume jobchurn loop for transition stability
# - l2_jobmix: deterministic two-job stop/bg/fg-style interleaving stress
# - l2_waitpid: libc wait queue + `WNOHANG` behavior via userspace probe
# - l2_mmap: mmap/munmap reuse + strict mprotect region validation
# - a_poll: poll(2) compatibility probe over fd table semantics
# - perf_benchprobe: process/thread-control/IPC benchmark probe
# - b_spawn_warm: spawn warm-cache benchmark + per-run noop spawn/map_elf p95 SLO checks
# - c_futex: futex invoke wait/wake/timeout smoke probe
# - c_futex_race: futex waiter/waker ordering probe with in-process thread_create path
# - m5_fairness: mixed-load fairness/latency telemetry SLO checks
# - m6_ipc_compact: compact IPC queue storage regression smoke under spawn churn
# - m6_ipc_rendezvous: direct sender->waiting-receiver transfer path under churn
# - m6_ring_io: shared-ring bulk read path for VFS
# - l2_vt4_default: boot → compositor pinned to VT4 + compositor ready
# - l2_cluuterm_smoke: boot → cluuterm start + window registered + login spawned
# - l2_cluuterm_login: inject credentials → login: user authenticated
# - l2_cluuterm_ansi: inject printf red SGR → cluuterm ansi sgr fg=AA0000
# - l2_cluuterm_keymap: inject Up arrow → cluuterm input csi 1b
# - l2_cluuterm_exit: inject exit → cluuterm shutdown + compositor window destroyed
# - l2_cluuterm_two_windows: Ctrl+Alt+N → compositor spawn_demo: requested cluuterm
# - l2_cluuterm_raw_mode: autostart edit (legacy TTY) → line_discipline: mode=raw
# - l2_vt_legacy_preserved: Ctrl+Alt+F1 switch + Ctrl+Alt+F5 return
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
        # Removed: boot markers tested implicitly by all other tests
        required_markers=("TSC calibrated")
        ;;
    m1_recv)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "procmgr: exit cookie"
        )
        ;;
    m2_token_audit)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "procmgr: exit cookie"
            "token_audit_next_seq="
            "token_audit_stored="
            "token_audit_dropped="
        )
        ;;
    m2_leakdiag)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "procmgr: exit cookie"
            "resource delta:"
            "delta_spaces="
            "delta_tokens="
            "delta_pmm_used_frames="
        )
        ;;
    m3_mapfail)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "mapfail: PASS"
        )
        ;;
    m3_mapcopyfail)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "mapcpfail: PASS"
        )
        ;;
    m3_maperror)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "maperror: PASS"
        )
        ;;
    m4_sender_auth)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "vfs: open ignoring claimed client_id="
            "authenticated="
        )
        ;;
    m4_registry_sender_auth)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "registry: subscribe"
            "sender"
        )
        ;;
    m4_notify_lifecycle)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "procmgr: cleared sender notify binding sender_tid="
        )
        ;;
    m4_deny_paths)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "killdeny: PASS permission denied"
            "procmgr: deny kill pid"
        )
        ;;
    m4_registry_deny_paths)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "regdeny: PASS permission denied"
            "registry: deny unregister"
        )
        ;;
    m5_fairness)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "procmgr: exit cookie"
            "resource delta:"
            "ipc_wait_p95_ms="
            "ipc_wait_p99_ms="
            "ipc_scan_avg_steps_x100="
        )
        ;;
    m6_ipc_compact)
        # Removed from suite: SLO-heavy, custom env, low regression value
        required_markers=("TSC calibrated" "[USER] shell: ready")
        ;;
    m6_ipc_rendezvous)
        # Removed from suite: SLO-heavy, custom env, low regression value
        required_markers=("TSC calibrated" "[USER] shell: ready")
        ;;
    m6_ring_io)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "ringio: PASS path=/bin/hello"
        )
        ;;
    kernel_suspended_thread)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "suspendprobe: PASS suspended-thread did not run before resume"
        )
        ;;
    l2_argv)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "argvprobe: argc=3"
            "argvprobe: arg0=/bin/argvprobe"
            "argvprobe: arg1=hello"
            "argvprobe: arg2=world"
        )
        ;;
    l2_vqprobe)
        required_markers=(
            "TSC calibrated"
            "vqprobe: ALL OK"
        )
        ;;
    l2_blk_basic)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "blkprobe: ALL OK"
        )
        ;;
    l2_blk_concurrent)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "blkprobe: concurrent=100 OK"
            "blkprobe: ALL OK"
        )
        ;;
    l2_blk_perf)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "blkprobe: perf bytes=67108864"
            "blkprobe: ALL OK"
        )
        ;;
    l2_blk_session_teardown)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "blkprobe: leak SUBMITTED"
            "virtio-blk: session"
        )
        ;;
    l2_bare_cmd)
        # UE17: PATH-based bare-command resolution. Typing `cat /etc/motd`
        # (no `spawn`, no `/`) goes through BuiltinRegistry::execute,
        # falls through to try_path_dispatch, hits /var/images/cat via
        # VfsClient::stat, and dispatches the binary as if `spawn cat
        # /etc/motd` had been typed. The marker below is procmgr's debug
        # print after container_run accepted the spawn — same pattern
        # as l2_cluufile_match. (cat's stdout — the motd text — goes to
        # TTY/stdout, not COM2, so we can't anchor on motd content.)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "procmgr: container 'cat' started"
        )
        ;;
    l2_path_symlink_resolve)
        # Item #1 of open-work queue: typing `/bin/ls /` exercises the
        # new VFS realpath flow that replaced the four strip_prefix("/bin/")
        # hacks in shell pipeline.rs. The shell calls vfs.realpath("/bin/ls")
        # which follows the ext2 symlink to /var/images/ls/bin/ls, extracts
        # image_name=ls, and dispatches via procmgr. ls's output prints to
        # the framebuffer (not COM2), so we anchor on procmgr's debug print
        # confirming a bare-name dispatch (same pattern as l2_bare_cmd).
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "procmgr: container 'ls' started"
        )
        ;;
    l2_cd)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "shell: pwd=/etc"
        )
        ;;
    l2_cd_inherit)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "pwdprobe: cwd=/tmp"
        )
        ;;
    l2_ext2write)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "ext2write: PASS path=/home/root/ext2io_scratch"
        )
        ;;
    l2_ext2append)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "ext2append: PASS path=/home/root/ext2io_scratch"
        )
        ;;
    l2_ext2mutate)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "ext2mutate: PASS mkdir+rename+rmdir"
        )
        ;;
    l2_ext2unlink)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "ext2unlink: PASS create+unlink+verify"
        )
        ;;
    l2_owner_deny)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "ownerprobe: PASS permission denied"
            "ext2ownerdeny: PASS non-owner denied + owner cleanup"
        )
        ;;
    d7_container_storage)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "containerprobe: PASS"
        )
        ;;
    e13_container_run)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "procmgr: container 'hello' started"
            "[RBX TEST] PASSED"
        )
        ;;
    f8_nested_container_run)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "nestprobe: PASS nested spawn"
        )
        ;;
    f9_escalation)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "escalateprobe: PASS escalation denied"
        )
        ;;
    f10_view_passthrough)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "viewchild: PASS view passthrough"
        )
        ;;
    f11_deny_inherit)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "denyprobe: PASS deny_inherit"
        )
        ;;
    f12_cascade_cleanup)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "procmgr: cascading kill container"
        )
        ;;
    f13_detach_survive)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "survivor: PASS survived parent exit"
        )
        ;;
    g7_vt_container)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "tty:0: auto-login wired, terminal mode"
        )
        ;;
    l2_sigint)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "spawn: SIGINT pid="
            "procmgr: signal 2 pid"
        )
        ;;
    l2_jobs)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "spawnbg: started pid="
            "shell: bg done pid="
        )
        ;;
    l2_jobs_basic)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "[1]"
            "Running"
            "sleep"
        )
        ;;
    l2_jobs_pipeline)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "shell: pipeline done stages=2 status=0"
        )
        ;;
    l2_jobs_kill)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "[1]"
        )
        ;;
    l2_alias_basic)
        # alias ll='ls -l' ; alias ll  →  prints "alias ll='ls -l'"
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "alias ll="
        )
        ;;
    l2_type_basic)
        # type cd  →  "cd is a shell builtin"
        # type ls  →  "ls is /bin/ls"
        # type nope  →  "type: nope: not found"
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "cd is a shell builtin"
        )
        ;;
    l2_help_basic)
        # help  →  "Shell builtins:" + list
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "Shell builtins:"
        )
        ;;
    l2_exit_status)
        # false ; echo $?  →  "1"
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
        )
        ;;
    l2_fg)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "spawnbg: started pid="
            "fg: pid="
            "procmgr: exit cookie"
        )
        ;;
    l2_stop)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "spawnbg: started pid="
            "procmgr: signal 19 pid"
        )
        ;;
    l2_jobchurn)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "jobchurn: PASS iterations=3"
            "procmgr: signal 19 pid"
            "procmgr: signal 18 pid"
            "thread_suspend_success="
            "thread_resume_success="
        )
        ;;
    l2_jobchurn_heavy)
        # Removed from suite: redundant with l2_jobchurn (same test, 60s wait)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "jobchurn: PASS iterations=8"
        )
        ;;
    l2_jobmix)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "jobmix: PASS pids="
            "procmgr: signal 19 pid"
            "procmgr: signal 18 pid"
            "procmgr: exit cookie"
        )
        ;;
    l2_mkdir)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "mkdir: ok /tmp/a"
            "mkdir: ok /tmp/b/c/d"
        )
        ;;
    l2_cp)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "cp: usage: cp <src> <dst>"
        )
        ;;
    l2_mv)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "mv: usage: mv <src> <dst>"
        )
        ;;
    l2_envelope_mounts)
        # TDD-red until UE10 wires the user envelope into send_vfs_set_view.
        # With envelope-driven mounts /etc becomes read-only, so /bin/touch
        # fails on open(O_WRONLY|O_CREAT) with PermissionDenied. The marker
        # below comes from userspace/touch/src/main.rs:51 — `format!("touch:
        # {}: {:?}\n", path, e)` where e is libcluu::error::Error::PermissionDenied.
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "touch: /etc/probefile: PermissionDenied"
        )
        ;;
    l2_cluufile_match)
        # UE14 happy path: cat container has no MOUNT directives, so
        # validate_cluufile_against_parent succeeds and the spawn proceeds.
        # The marker below (emitted at procmgr/main.rs after cluufile
        # validation passes and the container child is created) confirms
        # validation didn't reject the spawn. The actual motd text goes
        # to TTY/stdout, not the COM2 debug channel, so we anchor on the
        # debug-print marker — same pattern as l2_pipe_basic.
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "procmgr: container 'cat' started"
        )
        ;;
    l2_cluufile_mismatch)
        # UE14 mismatch path: cfmismatch's Cluufile demands MOUNT /etc
        # readwrite. Alice's user envelope provides /etc as ro, so
        # validate_cluufile_against_parent rejects with the marker
        # below (procmgr/main.rs ~line 4774). Procmgr replies
        # PermissionDenied before the binary's main() runs — the probe's
        # own debug_print MUST NOT fire.
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "procmgr: cluufile mismatch"
        )
        ;;
    l2_edit_smoke)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "edit: starting up"
        )
        # `edit: bye` is dropped intentionally for v0: edit boots into raw
        # mode and blocks on stdin. Without a reliable Ctrl-Q route through
        # the kbd→TTY→edit stdin pipeline (TBD post-T18 rendering), the
        # smoke can only verify boot. Once visual rendering lands we'll
        # add an `l2_edit_quit` case that injects `:q\n` and verifies exit.
        ;;
    l2_edit_insert)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "edit: starting up"
            # Future: "hello world" once injectable INSERT lands
        )
        ;;
    l2_edit_undo)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "edit: starting up"
            # Future: undo round-trip verification once byte-injection works
        )
        ;;
    l2_edit_eacces)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "edit: starting up"
            # Future: "Cannot save" once :w EACCES path is reachable from harness
        )
        ;;
    l2_envelope_user)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "envprobe: HOME=/home/alice"
            "envprobe: USER=alice"
            "envprobe: PATH=/bin:/usr/bin"
            "envprobe: SHELL=/bin/shell"
            "envprobe: PASS"
        )
        ;;
    l2_export)
        # UE15+UE16: bash `export` semantics through the ENV trailer.
        # X is set locally only — the spawned child must see "X=(null)".
        # Y is exported — the child must see "Y=exported".
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "envprobe: X=(null)"
            "envprobe: Y=exported"
        )
        ;;
    l2_mount_private)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "mountprobe: PASS /tmp isolation verified"
        )
        ;;
    l2_mp_etc)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "procmgr: container 'mp' started"
            "micropython: exit 0"
        )
        ;;
    l2_ls)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "ls: ok (exit 0)"
        )
        ;;
    l2_ls_long)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "ls: ok (exit 0)"
        )
        ;;
    l2_ls_color)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "ls: ok (exit 0)"
        )
        ;;
    l2_ls_recursive)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "ls: ok (exit 0)"
        )
        ;;
    l2_rm)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "rm: ok /tmp/rmtest"
        )
        ;;
    l2_shellrc)
        # UE18+UE19+UE20: /home/root/.shellrc must be sourced at
        # startup; its `export PATH=/bin:/usr/bin:/home/root/bin`
        # must reach the spawned envprobe child via the ENV trailer.
        # If sourcing failed we'd see supervisor's envelope default
        # (PATH=/sbin:/bin:/usr/sbin:/usr/bin) instead.
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "envprobe: HOME=/home/root"
            "envprobe: PATH=/bin:/usr/bin:/home/root/bin"
        )
        ;;
    l2_waitpid)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "waitprobe: PASS wnohang no-exit"
            "waitprobe: PASS waitpid queue+wnohang"
            "procmgr: exit cookie"
        )
        ;;
    l2_mmap)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "mmapprobe: PASS basic map/write"
            "mmapprobe: PASS mprotect exact"
            "mmapprobe: PASS mprotect to rx"
            "mmapprobe: PASS mprotect prot-none unsupported"
            "mmapprobe: PASS mprotect restore rw"
            "mmapprobe: PASS reuse hole"
            "mmapprobe: PASS complete"
        )
        ;;
    a_poll)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "pollprobe: PASS"
        )
        ;;
    l2_poll_pipes)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "pollprobe: pipe PASS"
            "pollprobe: PASS"
        )
        ;;
    perf_benchprobe)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "benchprobe: ipc_clock"
            "benchprobe: spawn_wait"
            "benchprobe: thread_ctl"
            "benchprobe: PASS"
        )
        ;;
    b_spawn_perf)
        # Removed from suite: redundant with b_spawn_warm
        required_markers=("TSC calibrated" "[USER] shell: ready")
        ;;
    b_spawn_warm)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "benchprobe: ipc_clock"
            "benchprobe: spawn_wait"
            "procmgr: spawn path /bin/noop"
            "vfs: open '/bin/noop'"
            "vfs: map_elf_trace"
        )
        ;;
    c_futex)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "futexprobe: PASS"
        )
        ;;
    c_futex_race)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "futexrace: PASS"
        )
        ;;
    p1_setjmp)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "setjmpprobe: PASS"
        )
        ;;
    p1_env)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "envprobe: PASS"
        )
        ;;
    p1_stubs)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "stubsprobe: PASS"
        )
        ;;
    p2_pipe)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "pipeprobe: PASS"
        )
        ;;
    p2_spawn_pipe)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "spawnpipeprobe: PASS"
        )
        ;;
    p3_tls)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "tlsprobe: PASS"
        )
        ;;
    p3_pthread)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "pthreadprobe: PASS"
        )
        ;;
    p4_dev)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "devprobe: PASS"
        )
        ;;
    p4_framebuf)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "fbprobe: PASS"
        )
        ;;
    b_console_blit)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "BENCH_CONSOLE_BLIT: cycles_per_full_screen="
        )
        ;;
    l2_devfb0)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "DEVFB0: PASS"
        )
        ;;
    p4_mmap)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "mmapprobe: PASS complete"
        )
        ;;
    l2_pipe_builtin)
        # Phase 4 Plan B Stage 0: builtin (echo) piped to container (cat).
        # Verifies WriteSink::Pipe path: PIPE_DATA_LABEL delivered to cat.
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "shell: pipeline done stages=2 status=0"
        )
        ;;
    l2_pipe_builtin_chain)
        # Phase 4 Plan B Stage 0: builtin (echo) piped to container (tr).
        # "abc" uppercased to "ABC" via tr a-z A-Z.
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "shell: pipeline done stages=2 status=0"
        )
        ;;
    l2_pipe_builtin_3stage)
        # Phase 4 Plan B Stage 0: 3-stage builtin→container→container.
        # echo (inline builtin) | cat | cat. Verifies that a builtin feeding
        # the head of a multi-container pipeline closes the pipe and the
        # second container sees EOF.
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "shell: pipeline done stages=3 status=0"
        )
        ;;
    l2_pipe_3stage)
        # Phase 4 Plan E diagnostic: 3-stage cat|grep|head on synthetic
        # data. "alpha" must appear in the debug-print channel output
        # (shell echoes pipeline stdout) and EXIT=0 from the echo.
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "shell: pipeline done stages=3 status=0"
        )
        ;;
    l2_pipe_basic)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "shell: pipeline done stages=2 status=0"
        )
        ;;
    l2_pipe_env)
        # Phase 4 Plan E Stage 2: env propagates through pipeline spawn.
        # wc -c is the spawned binary; "hello\n" = 6 bytes.
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "shell: pipeline done stages=2 status=0"
        )
        ;;
    l2_pipe_three)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "shell: pipeline done stages=3 status=0"
        )
        ;;
    l2_redir_stdout_file)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "shell: pipeline done stages=1 status=0"
            "shell: pipeline done stages=2 status=0"
        )
        ;;
    l2_tab_complete)
        # TAB pressed after "cat /etc/m" completes to "cat /etc/motd " and
        # Enter runs the command. Post-UE17 bare-command path goes through
        # spawn_and_wait, so the observable success marker is the
        # container-run-done line with status=0 for the cat invocation.
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "shell: PATH resolved 'cat' -> /var/images/cat"
            "shell: container run done status=0"
        )
        ;;
    perf_typing_storm)
        # Diagnostic-only mode: type 500 chars at zero key delay then idle.
        # We only require boot + shell ready; the rest of the analysis is
        # post-hoc (rate counters from each pipeline layer printed during
        # the run).
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
        )
        ;;
    hr6_shell_crash)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "shellcrash: triggering null-write fault"
            "shell crash"
            "session persists"
        )
        ;;
    hr7_su_equal)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "suequaltest: PASS"
        )
        ;;
    l2_cat_basic)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "cat: ok (exit 0)"
        )
        ;;
    l2_cp_recursive)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "cp: ok (exit 0)"
        )
        ;;
    l2_head_bytes)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "head: ok (exit 0)"
        )
        ;;
    l2_wc_lines)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "wc: ok (exit 0)"
        )
        ;;
    l2_grep_recursive)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "grep: ok (exit 0)"
        )
        ;;
    l2_basename_basic)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "basename: ok (exit 0)"
        )
        ;;
    l2_dirname_basic)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "dirname: ok (exit 0)"
        )
        ;;
    l2_sleep_basic)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "sleep: ok (exit 0)"
        )
        ;;
    l2_which_basic)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "which: ok (exit 0)"
        )
        ;;
    l2_printf_basic)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "printf: ok (exit 0)"
        )
        ;;
    l2_date_basic)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "date: ok (exit 0)"
        )
        ;;
    l2_env_basic)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "env: ok (exit 0)"
        )
        ;;
    l2_kill_basic)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "kill: ok (exit 0)"
        )
        ;;
    l2_sort_basic)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "sort: ok (exit 0)"
        )
        ;;
    l2_uniq_basic)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "uniq: ok (exit 0)"
        )
        ;;
    l2_cut_basic)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "cut: ok (exit 0)"
        )
        ;;
    l2_tr_basic)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "tr: ok (exit 0)"
        )
        ;;
    l2_stat_basic)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "stat: ok (exit 0)"
        )
        ;;
    l2_du_basic)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "du: ok (exit 0)"
        )
        ;;
    l2_find_basic)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "find: ok (exit 0)"
        )
        ;;
    l2_compositor_smoke)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "compositor: ready"
            "compositor: window registered"
        )
        ;;
    l2_compositor_focus)
        required_markers=(
            "TSC calibrated"
            "compositor: window registered"
            "compositor: focus -> "
        )
        ;;
    l2_compositor_destroy)
        required_markers=(
            "TSC calibrated"
            "compositor: window registered"
            "compositor: window destroyed"
        )
        ;;
    l2_compositor_legacy_vt)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "vtmgr: vt switch 0 -> 4"
            "compositor: VT activate"
            "vtmgr: vt switch 4 -> 0"
        )
        ;;
    b_compositor_blit)
        required_markers=(
            "TSC calibrated"
            "compositor: VT activate"
            "BENCH_COMP_BLIT: cycles_per_frame="
        )
        ;;
    l2_vt4_default)
        # Boot with no input: compositor is pinned to VT4 from the start.
        # vtmgr: compositor control subscribed confirms vtmgr accepted the pin,
        # and compositor: ready confirms the compositor loop is running on VT4.
        required_markers=(
            "TSC calibrated"
            "compositor: pinned to VT4"
            "compositor: ready"
        )
        ;;
    l2_cluuterm_smoke)
        # Boot: autostart.toml launches cluuterm which registers a compositor
        # window and a PTS slot.  The PTS open for login spawn is deferred until
        # the dev/pts open path is fixed (Task 23 prerequisite); for now the
        # smoke verifies the three registration steps that do work at boot.
        required_markers=(
            "TSC calibrated"
            "cluuterm: start"
            "cluuterm: window registered"
            "cluuterm: pts registered"
        )
        ;;
    l2_cluuterm_login)
        # After boot, inject credentials through the compositor's input path
        # (cluuterm is the focused window on VT4).  login emits a COM2 marker
        # on successful authentication.
        required_markers=(
            "TSC calibrated"
            "cluuterm: /bin/shell spawned"
            "login: user authenticated"
        )
        ;;
    l2_compositor_swap_login)
        # Verify the VT4 compositor handoff: system compositor boots, is killed
        # at SESSION_LOGIN, and a user-mode compositor is spawned under the user
        # envelope.  All markers up to "compositor: session_mode=1 (user)" fire
        # at boot/login time; "cluuterm: start" requires the login modal to
        # accept the injected credentials.
        required_markers=(
            "TSC calibrated"
            "compositor: session_mode=0 (system)"
            "killing system compositor"
            "system compositor reaped"
            "compositor: session_mode=1 (user)"
            "user compositor spawned"
            "cluuterm: start"
        )
        ;;
    l2_text_shell_input)
        # Three serial-visible proof points that the legacy VT0 stdin
        # round-trip works via POSIX read(0) -> /dev/tty0 -> tty service:
        #   - shell starts with fd 0 already VFS-backed (procmgr FDAC worked)
        #   - shell's handle_line_payload received at least one line
        #   - shell parsed the line and dispatched to the (empty) builtin set
        required_markers=(
            "TSC calibrated"
            "tty:0: showing login prompt"
            "shell: stdin path = vfs-backed"
            "shell: read 4 bytes from fd 0"
            "shell: unsupported command"
        )
        ;;
    l2_envelope_dev_filter)
        # Proves that a VT0 text session boots with the envelope-derived view
        # that includes /dev/tty0. Required markers are COM2-visible (serial):
        #   - VT0 login prompt appeared
        #   - shell received the session with VFS-backed fd 0
        #   - ls exited cleanly (ls: ok (exit 0) debug_print from /bin/ls)
        # The actual tty0/tty1..3 content of `ls /dev` goes to the framebuffer,
        # not COM2. Post-hoc check for the view narrowing assertion:
        #   grep -E "tty[0-3]" /tmp/cluu-serial-com2.log
        # There is no forbidden_markers infra in this harness; the absence of
        # tty1..3 in the serial log must be verified manually or via grep.
        required_markers=(
            "TSC calibrated"
            "tty:0: showing login prompt"
            "shell: stdin path = vfs-backed"
        )
        ;;
    l2_cluuterm_shell_input)
        # Proves the graphical-session shell's stdin round-trip via
        # POSIX read(0) over /dev/pts/<id>:
        #   - login authenticates root on VT4
        #   - cluuterm spawns /bin/shell with fd 0/1/2 bound to /dev/pts/0
        #   - shell starts with fd 0 VFS-backed (patch_vfs_stdio_endpoints worked)
        #   - shell receives and parses the typed line
        required_markers=(
            "TSC calibrated"
            "login: user authenticated"
            "cluuterm: /bin/shell spawned"
            "shell: stdin path = vfs-backed"
            "shell: read 4 bytes from fd 0"
            "shell: unsupported command"
        )
        ;;
    l2_envelope_home_propagated)
        # Proves HOME=/home/root is correctly propagated to the graphical-
        # session shell after login (Bug B regression guard).  The load-
        # bearing marker is shellrc sourcing from /home/root/.shellrc (not
        # //.shellrc — the pre-fix Bug B symptom).  No extra keystrokes
        # needed: the boot-time shellrc-sourcing log fires automatically
        # when the shell spawns.
        required_markers=(
            "TSC calibrated"
            "shell: stdin path = vfs-backed"
            "shellrc: sourcing /home/root/.shellrc"
        )
        ;;
    l2_cluuterm_ansi)
        # After login, run printf with a red SGR escape.  cluuterm's ANSI
        # parser fires an Event::SetAttr with the non-default fg colour and
        # emits a COM2 debug line with the ARGB hex value.
        required_markers=(
            "TSC calibrated"
            "cluuterm: /bin/shell spawned"
            "login: user authenticated"
            "cluuterm: ansi sgr fg=AA0000"
        )
        ;;
    l2_cluuterm_keymap)
        # After login, send an arrow key through the compositor.  cluuterm's
        # input handler calls encode_extended() which returns a CSI sequence
        # (0x1b first byte) and logs the event on COM2.
        required_markers=(
            "TSC calibrated"
            "cluuterm: /bin/shell spawned"
            "login: user authenticated"
            "cluuterm: input csi 1b"
        )
        ;;
    l2_cluuterm_exit)
        # After login, type `exit` to close the shell → PTS closes →
        # cluuterm calls shutdown() which logs on COM2 and sends WIN_DESTROY
        # to the compositor (compositor logs "window destroyed").
        required_markers=(
            "TSC calibrated"
            "cluuterm: /bin/shell spawned"
            "login: user authenticated"
            "cluuterm: shutdown"
            "compositor: window destroyed"
        )
        ;;
    l2_cluuterm_two_windows)
        # Press Ctrl+Alt+N to ask the compositor to spawn a second cluuterm.
        # The compositor logs the request and the new cluuterm instance emits
        # its own "start" marker (first instance's marker fires at boot).
        required_markers=(
            "TSC calibrated"
            "cluuterm: start"
            "compositor: spawn_demo: requested cluuterm"
        )
        ;;
    l2_cluuterm_raw_mode)
        # Run `edit` from the supervisor shell (legacy TTY path).  edit calls
        # tcsetattr(raw) which reaches the shared LineDiscipline via the legacy
        # tty daemon; LineDiscipline.set_mode() emits the COM2 marker.
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "line_discipline: mode=raw"
            "edit: starting up"
        )
        ;;
    l2_vt_legacy_preserved)
        # vtmgr boots at active_vt=0; press Ctrl+Alt+F5 to activate the
        # compositor (vtmgr: vt switch 0 -> 4 + compositor: VT activate),
        # then Ctrl+Alt+F1 to switch back to the legacy VT
        # (vtmgr: vt switch 4 -> 0 + compositor: VT deactivate),
        # then Ctrl+Alt+F5 back (compositor: VT activate again).
        required_markers=(
            "TSC calibrated"
            "vtmgr: vt switch 0 -> 4"
            "compositor: VT activate"
            "vtmgr: vt switch 4 -> 0"
            "compositor: VT deactivate"
        )
        ;;
    l2_timeserver_pushmode_tick)
        required_markers=(
            "TSC calibrated"
            "TIMETICK_PROBE: count=10"
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

# --- Streaming wait ---
# Poll the serial log up to RUN_WAIT seconds. Exit early when:
#   - all required_markers are present (success path), OR
#   - a fault / explicit failure pattern appears (abort path).
# Falls through to RUN_WAIT timeout if neither, leaving the existing
# missing-markers diagnostic to print the offending markers.
HARNESS_FAULT_PAT='PAGE_FAULT|GENERAL_PROTECTION|DOUBLE_FAULT|INVALID_OPCODE'
HARNESS_FAIL_PAT='\[FAIL\]|test FAILED|PANIC|panic'
echo "Waiting up to ${RUN_WAIT}s for workload markers (streaming)..."
HARNESS_WAIT_DEADLINE=$((SECONDS + RUN_WAIT))
HARNESS_WAIT_ELAPSED=0
while [ "$SECONDS" -lt "$HARNESS_WAIT_DEADLINE" ]; do
    if [ "$EXPECT_FAULT" -ne 1 ] \
        && grep -qiE "$HARNESS_FAULT_PAT" "$SERIAL_LOG" 2>/dev/null; then
        break
    fi
    if grep -qE "$HARNESS_FAIL_PAT" "$SERIAL_LOG" 2>/dev/null; then
        break
    fi
    if [ "${#required_markers[@]}" -gt 0 ]; then
        all_seen=1
        for marker in "${required_markers[@]}"; do
            if ! grep -Fq "$marker" "$SERIAL_LOG" 2>/dev/null; then
                all_seen=0
                break
            fi
        done
        if [ "$all_seen" -eq 1 ]; then
            HARNESS_WAIT_ELAPSED=$((SECONDS - (HARNESS_WAIT_DEADLINE - RUN_WAIT)))
            echo "All required markers present after ${HARNESS_WAIT_ELAPSED}s (streaming early-exit)"
            break
        fi
    fi
    sleep 0.5
done

# --- Capture and display serial output ---
echo ""
echo "=========================================="
echo "  COM2 Serial Output (debug log)"
echo "=========================================="
cat "$SERIAL_LOG"
echo ""
echo "=========================================="

# Check for faults (skip when EXPECT_FAULT=1, e.g. shell crash tests)
if [ "$EXPECT_FAULT" -ne 1 ] && grep -qiE "$HARNESS_FAULT_PAT" "$SERIAL_LOG"; then
    echo "*** FAULT DETECTED in serial output ***"
    grep -iE 'PAGE_FAULT|GENERAL_PROTECTION|DOUBLE_FAULT|INVALID_OPCODE|PF:|GPF:' "$SERIAL_LOG"
    exit 1
fi

# Check explicit test failures
if grep -qE "$HARNESS_FAIL_PAT" "$SERIAL_LOG"; then
    echo "*** TEST FAILURE MARKERS DETECTED ***"
    grep -nE "$HARNESS_FAIL_PAT" "$SERIAL_LOG" || true
    exit 1
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

parse_last_metric() {
    local marker="$1"
    awk -v marker="$marker" '
        $0 ~ marker {
            split($0, parts, "=");
            v = parts[length(parts)];
            gsub(/[^0-9-]/, "", v);
            if (v != "") {
                last = v;
            }
        }
        END {
            if (last != "") {
                print last
            }
        }
    ' "$SERIAL_LOG"
}

check_metric_limit() {
    local value="$1"
    local limit="$2"
    local name="$3"
    if [ -z "$limit" ]; then
        return 0
    fi
    if ! [[ "$limit" =~ ^-?[0-9]+$ ]]; then
        echo "ERROR: $name limit must be an integer, got '$limit'"
        exit 1
    fi
    if [ -z "$value" ]; then
        echo "MISSING: could not parse $name"
        echo "*** REQUIRED SUCCESS MARKERS MISSING ***"
        exit 1
    fi
    if [ "$value" -gt "$limit" ]; then
        echo "MISSING: $name exceeded limit (value=$value limit=$limit)"
        echo "*** REQUIRED SUCCESS MARKERS MISSING ***"
        exit 1
    fi
}

check_minimum_count() {
    local value="$1"
    local minimum="$2"
    local name="$3"
    if ! [[ "$minimum" =~ ^[0-9]+$ ]]; then
        echo "ERROR: $name minimum must be a non-negative integer, got '$minimum'"
        exit 1
    fi
    if [ -z "$value" ]; then
        echo "MISSING: could not parse $name"
        echo "*** REQUIRED SUCCESS MARKERS MISSING ***"
        exit 1
    fi
    if [ "$value" -lt "$minimum" ]; then
        echo "MISSING: $name below minimum (value=$value minimum=$minimum)"
        echo "*** REQUIRED SUCCESS MARKERS MISSING ***"
        exit 1
    fi
}

percentile_p95_from_values() {
    if [ "$#" -eq 0 ]; then
        return 0
    fi
    local count="$#"
    local rank=$(((95 * count + 99) / 100))
    if [ "$rank" -lt 1 ]; then
        rank=1
    fi
    printf '%s\n' "$@" | sort -n | sed -n "${rank}p"
}

if [[ "$MARKER_MODE" == p1_* ]]; then
    exit_count=$(grep -c "procmgr: exit cookie" "$SERIAL_LOG" || true)
    if [ "$exit_count" -lt "$MIN_EXIT_COOKIES" ]; then
        echo "MISSING: expected at least $MIN_EXIT_COOKIES exit cookies, got $exit_count"
        echo "*** REQUIRED SUCCESS MARKERS MISSING ***"
        exit 1
    fi
fi

if [ "$MARKER_MODE" = "m1_recv" ]; then
    exit_count=$(grep -c "procmgr: exit cookie" "$SERIAL_LOG" || true)
    if [ "$exit_count" -lt "$MIN_EXIT_COOKIES" ]; then
        echo "MISSING: expected at least $MIN_EXIT_COOKIES exit cookies, got $exit_count"
        echo "*** REQUIRED SUCCESS MARKERS MISSING ***"
        exit 1
    fi
fi

if [ "$MARKER_MODE" = "m2_token_audit" ]; then
    exit_count=$(grep -c "procmgr: exit cookie" "$SERIAL_LOG" || true)
    if [ "$exit_count" -lt "$MIN_EXIT_COOKIES" ]; then
        echo "MISSING: expected at least $MIN_EXIT_COOKIES exit cookies, got $exit_count"
        echo "*** REQUIRED SUCCESS MARKERS MISSING ***"
        exit 1
    fi

    extract_metric_value() {
        local marker="$1"
        awk -v marker="$marker" '
            $0 ~ marker {
                split($0, parts, "=");
                val = parts[length(parts)];
                gsub(/[^0-9]/, "", val);
                if (val != "") {
                    print val;
                    exit 0;
                }
            }
        ' "$SERIAL_LOG"
    }

    audit_next_seq="$(extract_metric_value "token_audit_next_seq=")"
    audit_stored="$(extract_metric_value "token_audit_stored=")"
    audit_dropped="$(extract_metric_value "token_audit_dropped=")"

    if [ -z "$audit_next_seq" ] || [ -z "$audit_stored" ] || [ -z "$audit_dropped" ]; then
        echo "MISSING: token audit telemetry metrics could not be parsed"
        echo "*** REQUIRED SUCCESS MARKERS MISSING ***"
        exit 1
    fi

    if [ "$audit_dropped" -ne 0 ]; then
        echo "MISSING: expected token_audit_dropped=0, got $audit_dropped"
        echo "*** REQUIRED SUCCESS MARKERS MISSING ***"
        exit 1
    fi

    if [ "$audit_stored" -lt 2 ]; then
        echo "MISSING: expected token_audit_stored>=2, got $audit_stored"
        echo "*** REQUIRED SUCCESS MARKERS MISSING ***"
        exit 1
    fi
fi

if [ "$MARKER_MODE" = "m2_leakdiag" ]; then
    exit_count=$(grep -c "procmgr: exit cookie" "$SERIAL_LOG" || true)
    if [ "$exit_count" -lt "$MIN_EXIT_COOKIES" ]; then
        echo "MISSING: expected at least $MIN_EXIT_COOKIES exit cookies, got $exit_count"
        echo "*** REQUIRED SUCCESS MARKERS MISSING ***"
        exit 1
    fi

    delta_samples=$(grep -c "resource delta:" "$SERIAL_LOG" || true)
    if [ "$delta_samples" -lt 1 ]; then
        echo "MISSING: expected at least one resource delta sample"
        echo "*** REQUIRED SUCCESS MARKERS MISSING ***"
        exit 1
    fi

    parse_last_delta() {
        local marker="$1"
        awk -v marker="$marker" '
            $0 ~ marker {
                split($0, parts, "=");
                v = parts[length(parts)];
                gsub(/[^0-9-]/, "", v);
                if (v != "") {
                    last = v;
                }
            }
            END {
                if (last != "") {
                    print last
                }
            }
        ' "$SERIAL_LOG"
    }

    last_delta_spaces="$(parse_last_delta "delta_spaces=")"
    last_delta_tokens="$(parse_last_delta "delta_tokens=")"
    last_delta_endpoints="$(parse_last_delta "delta_endpoints=")"
    last_delta_pmm="$(parse_last_delta "delta_pmm_used_frames=")"

    check_delta_limit() {
        local value="$1"
        local limit="$2"
        local name="$3"
        if [ -z "$limit" ]; then
            return 0
        fi
        if ! [[ "$limit" =~ ^-?[0-9]+$ ]]; then
            echo "ERROR: $name limit must be an integer, got '$limit'"
            exit 1
        fi
        if [ -z "$value" ]; then
            echo "MISSING: could not parse $name"
            echo "*** REQUIRED SUCCESS MARKERS MISSING ***"
            exit 1
        fi
        if [ "$value" -gt "$limit" ]; then
            echo "MISSING: $name exceeded limit (value=$value limit=$limit)"
            echo "*** REQUIRED SUCCESS MARKERS MISSING ***"
            exit 1
        fi
    }

    check_delta_limit "$last_delta_spaces" "$MAX_DELTA_SPACES" "delta_spaces"
    check_delta_limit "$last_delta_tokens" "$MAX_DELTA_TOKENS" "delta_tokens"
    check_delta_limit "$last_delta_endpoints" "$MAX_DELTA_ENDPOINTS" "delta_endpoints"
    check_delta_limit "$last_delta_pmm" "$MAX_DELTA_PMM_USED_FRAMES" "delta_pmm_used_frames"
fi

if [ "$MARKER_MODE" = "m5_fairness" ]; then
    exit_count=$(grep -c "procmgr: exit cookie" "$SERIAL_LOG" || true)
    if [ "$exit_count" -lt "$MIN_EXIT_COOKIES" ]; then
        echo "MISSING: expected at least $MIN_EXIT_COOKIES exit cookies, got $exit_count"
        echo "*** REQUIRED SUCCESS MARKERS MISSING ***"
        exit 1
    fi

    last_wait_p95_ms="$(parse_last_metric "ipc_wait_p95_ms=")"
    last_wait_p99_ms="$(parse_last_metric "ipc_wait_p99_ms=")"
    last_scan_avg_steps_x100="$(parse_last_metric "ipc_scan_avg_steps_x100=")"

    check_metric_limit "$last_wait_p95_ms" "$MAX_IPC_WAIT_P95_MS" "ipc_wait_p95_ms"
    check_metric_limit "$last_wait_p99_ms" "$MAX_IPC_WAIT_P99_MS" "ipc_wait_p99_ms"
    check_metric_limit "$last_scan_avg_steps_x100" "$MAX_IPC_SCAN_AVG_STEPS_X100" "ipc_scan_avg_steps_x100"
fi

if [ "$MARKER_MODE" = "m6_ipc_compact" ] || [ "$MARKER_MODE" = "m6_ipc_rendezvous" ]; then
    exit_count=$(grep -c "procmgr: exit cookie" "$SERIAL_LOG" || true)
    if [ "$exit_count" -lt "$MIN_EXIT_COOKIES" ]; then
        echo "MISSING: expected at least $MIN_EXIT_COOKIES exit cookies, got $exit_count"
        echo "*** REQUIRED SUCCESS MARKERS MISSING ***"
        exit 1
    fi

    last_wait_p95_ms="$(parse_last_metric "ipc_wait_p95_ms=")"
    last_wait_p99_ms="$(parse_last_metric "ipc_wait_p99_ms=")"
    last_scan_avg_steps_x100="$(parse_last_metric "ipc_scan_avg_steps_x100=")"
    last_queue_bytes_peak="$(parse_last_metric "ipc_queue_bytes_peak=")"
    last_queue_messages_peak="$(parse_last_metric "ipc_queue_messages_peak=")"

    check_metric_limit "$last_wait_p95_ms" "$MAX_IPC_WAIT_P95_MS" "ipc_wait_p95_ms"
    check_metric_limit "$last_wait_p99_ms" "$MAX_IPC_WAIT_P99_MS" "ipc_wait_p99_ms"
    check_metric_limit "$last_scan_avg_steps_x100" "$MAX_IPC_SCAN_AVG_STEPS_X100" "ipc_scan_avg_steps_x100"
    check_metric_limit "$last_queue_bytes_peak" "$MAX_IPC_QUEUE_BYTES_PEAK" "ipc_queue_bytes_peak"
    check_metric_limit "$last_queue_messages_peak" "$MAX_IPC_QUEUE_MESSAGES_PEAK" "ipc_queue_messages_peak"
fi

if [ "$MARKER_MODE" = "m6_ipc_rendezvous" ]; then
    direct_deliveries="$(parse_last_metric "ipc_direct_deliveries=")"
    if [ -z "$direct_deliveries" ]; then
        echo "MISSING: could not parse ipc_direct_deliveries"
        echo "*** REQUIRED SUCCESS MARKERS MISSING ***"
        exit 1
    fi
    if [ "$direct_deliveries" -lt 1 ]; then
        echo "MISSING: expected ipc_direct_deliveries>=1, got $direct_deliveries"
        echo "*** REQUIRED SUCCESS MARKERS MISSING ***"
        exit 1
    fi
fi

if [ "$MARKER_MODE" = "b_spawn_warm" ]; then
    mapfile -t noop_spawn_reply_dts < <(
        awk '
            match($0, /procmgr: spawn_trace seq=([0-9]+) stage=spawn_request/, m) {
                current_spawn_seq = m[1]
            }
            /procmgr: spawn path \/bin\/noop/ {
                if (current_spawn_seq != "") {
                    noop_seq[current_spawn_seq] = 1
                }
            }
            match($0, /procmgr: spawn_trace seq=([0-9]+) stage=reply_sent .* dt=([0-9]+)/, m) {
                seq = m[1]
                if (seq in noop_seq) {
                    print m[2]
                }
            }
        ' "$SERIAL_LOG"
    )

    mapfile -t noop_map_reply_dts < <(
        awk '
            /vfs: open '\''\/bin\/noop'\''/ {
                pending_noop_open = 1
                next
            }
            pending_noop_open && match($0, /vfs: open OK fd=([0-9]+)/, m) {
                noop_fd[m[1]] = 1
                pending_noop_open = 0
                next
            }
            match($0, /vfs: map_elf_trace fd=([0-9]+) stage=reply .* dt=([0-9]+)/, m) {
                fd = m[1]
                if (fd in noop_fd) {
                    print m[2]
                }
            }
        ' "$SERIAL_LOG"
    )

    noop_spawn_count="${#noop_spawn_reply_dts[@]}"
    noop_map_count="${#noop_map_reply_dts[@]}"
    check_minimum_count "$noop_spawn_count" "$MIN_NOOP_SPAWN_SAMPLES" "noop_spawn_reply_samples"
    check_minimum_count "$noop_map_count" "$MIN_NOOP_MAP_ELF_SAMPLES" "noop_map_elf_reply_samples"

    noop_spawn_p95="$(percentile_p95_from_values "${noop_spawn_reply_dts[@]}")"
    noop_map_p95="$(percentile_p95_from_values "${noop_map_reply_dts[@]}")"

    if [ -z "$noop_spawn_p95" ] || [ -z "$noop_map_p95" ]; then
        echo "MISSING: could not compute noop warm-cache p95 metrics"
        echo "*** REQUIRED SUCCESS MARKERS MISSING ***"
        exit 1
    fi

    echo "HARNESS noop_spawn_reply_samples=${noop_spawn_count}" >> "$SERIAL_LOG"
    echo "HARNESS noop_map_elf_reply_samples=${noop_map_count}" >> "$SERIAL_LOG"
    echo "HARNESS noop_spawn_reply_p95_cycles=${noop_spawn_p95}" >> "$SERIAL_LOG"
    echo "HARNESS noop_map_elf_reply_p95_cycles=${noop_map_p95}" >> "$SERIAL_LOG"

    check_metric_limit "$noop_spawn_p95" "$MAX_NOOP_SPAWN_REPLY_P95_CYCLES" "noop_spawn_reply_p95_cycles"
    check_metric_limit "$noop_map_p95" "$MAX_NOOP_MAP_ELF_REPLY_P95_CYCLES" "noop_map_elf_reply_p95_cycles"
fi

if [ "$MARKER_MODE" = "m3_mapfail" ]; then
    if grep -Fq "mapfail: FAIL" "$SERIAL_LOG"; then
        echo "MISSING: mapfail reported failure"
        echo "*** REQUIRED SUCCESS MARKERS MISSING ***"
        exit 1
    fi
fi

if [ "$MARKER_MODE" = "m3_mapcopyfail" ]; then
    if grep -Fq "mapcpfail: FAIL" "$SERIAL_LOG"; then
        echo "MISSING: mapcpfail reported failure"
        echo "*** REQUIRED SUCCESS MARKERS MISSING ***"
        exit 1
    fi
fi

if [ "$MARKER_MODE" = "m3_maperror" ]; then
    if grep -Fq "maperror: FAIL" "$SERIAL_LOG"; then
        echo "MISSING: maperror reported failure"
        echo "*** REQUIRED SUCCESS MARKERS MISSING ***"
        exit 1
    fi
fi

if [ "$MARKER_MODE" = "l2_ext2write" ]; then
    if grep -Fq "ext2write: FAIL" "$SERIAL_LOG"; then
        echo "MISSING: ext2write reported failure"
        echo "*** REQUIRED SUCCESS MARKERS MISSING ***"
        exit 1
    fi
fi

if [ "$MARKER_MODE" = "l2_ext2append" ]; then
    if grep -Fq "ext2append: FAIL" "$SERIAL_LOG"; then
        echo "MISSING: ext2append reported failure"
        echo "*** REQUIRED SUCCESS MARKERS MISSING ***"
        exit 1
    fi
fi

if [ "$MARKER_MODE" = "l2_ext2mutate" ]; then
    if grep -Fq "ext2mutate: FAIL" "$SERIAL_LOG"; then
        echo "MISSING: ext2mutate reported failure"
        echo "*** REQUIRED SUCCESS MARKERS MISSING ***"
        exit 1
    fi
fi

if [ "$MARKER_MODE" = "l2_ext2unlink" ]; then
    if grep -Fq "ext2unlink: FAIL" "$SERIAL_LOG"; then
        echo "MISSING: ext2unlink reported failure"
        echo "*** REQUIRED SUCCESS MARKERS MISSING ***"
        exit 1
    fi
fi

if [ "$MARKER_MODE" = "b_console_blit" ]; then
    # Extract cycles_per_full_screen from the serial log (-a: treat binary as text)
    blit_cycles="$(grep -ao 'BENCH_CONSOLE_BLIT: cycles_per_full_screen=[0-9]*' "$SERIAL_LOG" \
        | tail -1 \
        | sed 's/.*cycles_per_full_screen=//')"
    if [ -z "$blit_cycles" ]; then
        echo "MISSING: could not parse BENCH_CONSOLE_BLIT cycles_per_full_screen"
        echo "*** REQUIRED SUCCESS MARKERS MISSING ***"
        exit 1
    fi
    echo "HARNESS fb_blit_wc_cycles=${blit_cycles}" >> "$SERIAL_LOG"
    # Load ratchet limit: prefer env override, then perf_ratchet.json, then skip check
    ratchet_max="${MAX_FB_BLIT_WC_CYCLES:-}"
    if [ -z "$ratchet_max" ] && [ -f "scripts/perf_ratchet.json" ]; then
        ratchet_max="$(grep -ao '"fb_blit_wc_max_cycles":[[:space:]]*[0-9]*' scripts/perf_ratchet.json \
            | grep -ao '[0-9]*$' || true)"
    fi
    if [ -n "$ratchet_max" ]; then
        check_metric_limit "$blit_cycles" "$ratchet_max" "fb_blit_wc_cycles"
        echo "HARNESS fb_blit_wc_ratchet_max=${ratchet_max} actual=${blit_cycles} OK"
    else
        echo "HARNESS fb_blit_wc_cycles=${blit_cycles} (no ratchet limit set)"
    fi
fi

if [ "$MARKER_MODE" = "b_compositor_blit" ]; then
    comp_cycles="$(grep -ao 'BENCH_COMP_BLIT: cycles_per_frame=[0-9]*' "$SERIAL_LOG" \
        | tail -1 \
        | sed 's/.*cycles_per_frame=//')"
    if [ -z "$comp_cycles" ]; then
        echo "MISSING: could not parse BENCH_COMP_BLIT cycles_per_frame"
        echo "*** REQUIRED SUCCESS MARKERS MISSING ***"
        exit 1
    fi
    echo "HARNESS compositor_blit_cycles=${comp_cycles}" >> "$SERIAL_LOG"
    ratchet_max="$(grep -ao '"compositor_blit_max_cycles":[[:space:]]*[0-9]*' scripts/perf_ratchet.json \
        | grep -ao '[0-9]*$' || true)"
    if [ -n "$ratchet_max" ]; then
        check_metric_limit "$comp_cycles" "$ratchet_max" "compositor_blit_cycles"
        echo "HARNESS compositor_blit_ratchet_max=${ratchet_max} actual=${comp_cycles} OK"
    else
        echo "HARNESS compositor_blit_cycles=${comp_cycles} (no ratchet limit set)"
    fi
fi

echo "No faults detected and all required markers found."
exit 0
