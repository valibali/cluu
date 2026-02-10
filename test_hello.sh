#!/bin/bash
# Automated CLUU test harness: build, launch QEMU, type test command(s), capture serial output
set -e

PROJECT_ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$PROJECT_ROOT"

SERIAL_LOG="${SERIAL_LOG:-/tmp/cluu-serial-com2.log}"
MONITOR_SOCK="${MONITOR_SOCK:-/tmp/cluu-qemu-monitor.sock}"
OVMF="${OVMF:-/usr/share/ovmf/OVMF.fd}"
IMG="${IMG:-$PROJECT_ROOT/target/cluu.img}"
USER_DISK="${USER_DISK:-$PROJECT_ROOT/target/userdisk.img}"
QEMU_GDB="${QEMU_GDB:-0}"
QEMU_EXTRA_ARGS="${QEMU_EXTRA_ARGS:-}"
QEMU_PID=""
BOOT_WAIT="${BOOT_WAIT:-8}"
SHELL_READY_WAIT="${SHELL_READY_WAIT:-25}"
RUN_WAIT="${RUN_WAIT:-5}"
POST_SENDKEY="${POST_SENDKEY:-}"
POST_SENDKEY_DELAY="${POST_SENDKEY_DELAY:-1}"
# Preserve explicit empty TEST_COMMAND; only auto-fill when it is truly unset.
if [ -z "${TEST_COMMAND+x}" ]; then
    TEST_COMMAND="__AUTO__"
fi
TEST_COMMAND_REPEAT="${TEST_COMMAND_REPEAT:-1}"
COMMAND_GAP="${COMMAND_GAP:-1}"
KEY_DELAY="${KEY_DELAY:-0.05}"
MARKER_MODE="${MARKER_MODE:-legacy_p1}"
SHELL_AUTOSTART_CMD_DEFAULT=""
POST_SENDKEY_DEFAULT=""
if [ "$TEST_COMMAND" = "__AUTO__" ]; then
    case "$MARKER_MODE" in
        m3_mapfail) TEST_COMMAND="mapfail 12 4" ;;
        m3_mapcopyfail) TEST_COMMAND="mapcpfail 4" ;;
        m3_maperror) TEST_COMMAND="maperror 3" ;;
        m4_deny_paths)
            TEST_COMMAND="killdeny 2 9"
            SHELL_AUTOSTART_CMD_DEFAULT="killdeny 2 9"
            ;;
        m4_registry_deny_paths)
            TEST_COMMAND="regdeny"
            SHELL_AUTOSTART_CMD_DEFAULT="regdeny"
            ;;
        l2_ext2write)
            TEST_COMMAND="ext2write"
            SHELL_AUTOSTART_CMD_DEFAULT="ext2write"
            ;;
        l2_ext2append)
            TEST_COMMAND="ext2append"
            SHELL_AUTOSTART_CMD_DEFAULT="ext2append"
            ;;
        l2_ext2mutate)
            TEST_COMMAND="ext2mutate"
            SHELL_AUTOSTART_CMD_DEFAULT="ext2mutate"
            ;;
        l2_ext2unlink)
            TEST_COMMAND="ext2unlink"
            SHELL_AUTOSTART_CMD_DEFAULT="ext2unlink"
            ;;
        l2_owner_deny)
            TEST_COMMAND="ext2ownerdeny"
            SHELL_AUTOSTART_CMD_DEFAULT="ext2ownerdeny"
            ;;
        l2_sigint)
            TEST_COMMAND="spawn sleepy"
            SHELL_AUTOSTART_CMD_DEFAULT="spawn sleepy"
            POST_SENDKEY_DEFAULT="ctrl-c"
            ;;
        l2_jobs)
            TEST_COMMAND="spawnbg sleepy"
            SHELL_AUTOSTART_CMD_DEFAULT="spawnbg sleepy"
            ;;
        l2_fg)
            TEST_COMMAND="fg"
            SHELL_AUTOSTART_CMD_DEFAULT="spawnbg sleepy"
            ;;
        l2_stop)
            TEST_COMMAND="stop"
            SHELL_AUTOSTART_CMD_DEFAULT="spawnbg sleepy"
            ;;
        l2_jobchurn)
            TEST_COMMAND="jobchurn 3"
            SHELL_AUTOSTART_CMD_DEFAULT=""
            ;;
        l2_jobchurn_heavy)
            TEST_COMMAND="jobchurn 8"
            SHELL_AUTOSTART_CMD_DEFAULT=""
            ;;
        l2_jobmix)
            TEST_COMMAND="jobmix"
            SHELL_AUTOSTART_CMD_DEFAULT=""
            ;;
        l2_waitpid)
            TEST_COMMAND="spawn waitprobe"
            SHELL_AUTOSTART_CMD_DEFAULT="spawn waitprobe"
            ;;
        l2_mmap)
            TEST_COMMAND="spawn mmapprobe"
            SHELL_AUTOSTART_CMD_DEFAULT="spawn mmapprobe"
            ;;
        m5_fairness) TEST_COMMAND="repeat 8 spawn hello" ;;
        *) TEST_COMMAND="spawn hello" ;;
    esac
fi

if [ -n "$SHELL_AUTOSTART_CMD_DEFAULT" ] && [ -z "${CLUU_SHELL_AUTOSTART_CMD:-}" ]; then
    export CLUU_SHELL_AUTOSTART_CMD="$SHELL_AUTOSTART_CMD_DEFAULT"
fi
if [ -n "$POST_SENDKEY_DEFAULT" ] && [ -z "$POST_SENDKEY" ]; then
    POST_SENDKEY="$POST_SENDKEY_DEFAULT"
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
qemu_args=(
    -bios "$OVMF"
    -m 512M
    -accel kvm
    -cpu host
    -drive "file=$IMG,format=raw,if=ide,index=0"
    -drive "file=$USER_DISK,format=raw,if=none,id=userblk"
    -device virtio-blk-pci,drive=userblk
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

wait_for_shell_ready() {
    local deadline=$((SECONDS + SHELL_READY_WAIT))
    while [ "$SECONDS" -lt "$deadline" ]; do
        if grep -Fq "[USER] shell: ready" "$SERIAL_LOG"; then
            return 0
        fi
        sleep 1
    done
    return 1
}

if [ -n "$TEST_COMMAND" ]; then
    echo "Waiting up to ${SHELL_READY_WAIT}s for shell readiness marker..."
    if ! wait_for_shell_ready; then
        echo "ERROR: shell readiness marker not observed before command injection"
        exit 1
    fi
fi

# --- Step 5: Type test command(s) via QEMU monitor ---
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

if ! [[ "$TEST_COMMAND_REPEAT" =~ ^[0-9]+$ ]] || [ "$TEST_COMMAND_REPEAT" -lt 1 ]; then
    echo "ERROR: TEST_COMMAND_REPEAT must be a positive integer"
    exit 1
fi

for ((i = 1; i <= TEST_COMMAND_REPEAT; i++)); do
    echo "Sending command ${i}/${TEST_COMMAND_REPEAT}: '$TEST_COMMAND'"
    type_ascii_command "$TEST_COMMAND"
    if [ "$i" -lt "$TEST_COMMAND_REPEAT" ]; then
        sleep "$COMMAND_GAP"
    fi
done

if [ -n "$POST_SENDKEY" ]; then
    sleep "$POST_SENDKEY_DELAY"
    echo "Sending post key: '$POST_SENDKEY'"
    send_key "$POST_SENDKEY"
fi

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
# - m1_recv: recv/wakeup churn checks
# - m2_token_audit: recv churn + token audit telemetry invariants
# - m2_leakdiag: churn + resource delta diagnostics
# - m3_mapfail: kernel map-range failpoint rollback validation via shell builtin
# - m3_mapcopyfail: copy_from_user failure branch rollback validation via shell builtin (`mapcpfail`)
# - m3_maperror: map_user_page error branch rollback validation via shell builtin
# - m4_sender_auth: authenticated sender binding in VFS (ignore caller-supplied client_id)
# - m4_registry_sender_auth: authenticated sender binding in registry subscribe/register flows
# - m4_notify_lifecycle: sender notify bindings are reclaimed after child lifecycle ends
# - m4_deny_paths: explicit sender-auth denial path regressions (PermissionDenied flows)
# - m4_registry_deny_paths: explicit registry ownership denial path regressions
# - l2_ext2write: end-to-end ext2 write smoke test via shell builtin
# - l2_ext2append: append-past-EOF ext2 smoke test via shell builtin
# - l2_ext2mutate: mkdir/rename/rmdir ext2 metadata mutation smoke test
# - l2_ext2unlink: create+unlink verification smoke test
# - l2_owner_deny: explicit non-owner mutation denial with second spawned client
# - l2_sigint: foreground spawn interrupted by Ctrl-C (minimal SIGINT path)
# - l2_jobs: background spawn + async reap notification (`SIGCHLD`-style baseline)
# - l2_fg: background spawn promoted to foreground wait path via `fg`
# - l2_stop: background job transitions to stopped state via `stop`
# - l2_jobchurn: repeated stop/resume/foreground cycles with telemetry signal counters
# - l2_jobchurn_heavy: higher-volume jobchurn loop for transition stability
# - l2_jobmix: deterministic two-job stop/bg/fg-style interleaving stress
# - l2_waitpid: libc wait queue + `WNOHANG` behavior via userspace probe
# - l2_mmap: mmap/munmap reuse + strict mprotect region validation
# - m5_fairness: mixed-load fairness/latency telemetry SLO checks
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
    l2_ext2write)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "ext2write: PASS path=/bin/hello"
        )
        ;;
    l2_ext2append)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "ext2append: PASS path=/bin/hello"
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
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "jobchurn: PASS iterations=8"
            "procmgr: signal 19 pid"
            "procmgr: signal 18 pid"
            "thread_suspend_success="
            "thread_resume_success="
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
                if (getline > 0) {
                    gsub(/[^0-9]/, "", $0);
                    if ($0 != "") {
                        print $0;
                        exit 0;
                    }
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
                if (getline > 0) {
                    v = $0
                    gsub(/[^0-9-]/, "", v)
                    if (v != "") {
                        last = v
                    }
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

    parse_last_metric() {
        local marker="$1"
        awk -v marker="$marker" '
            $0 ~ marker {
                if (getline > 0) {
                    v = $0
                    gsub(/[^0-9-]/, "", v)
                    if (v != "") {
                        last = v
                    }
                }
            }
            END {
                if (last != "") {
                    print last
                }
            }
        ' "$SERIAL_LOG"
    }

    last_wait_p95_ms="$(parse_last_metric "ipc_wait_p95_ms=")"
    last_wait_p99_ms="$(parse_last_metric "ipc_wait_p99_ms=")"
    last_scan_avg_steps_x100="$(parse_last_metric "ipc_scan_avg_steps_x100=")"

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

    check_metric_limit "$last_wait_p95_ms" "$MAX_IPC_WAIT_P95_MS" "ipc_wait_p95_ms"
    check_metric_limit "$last_wait_p99_ms" "$MAX_IPC_WAIT_P99_MS" "ipc_wait_p99_ms"
    check_metric_limit "$last_scan_avg_steps_x100" "$MAX_IPC_SCAN_AVG_STEPS_X100" "ipc_scan_avg_steps_x100"
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

echo "No faults detected and all required markers found."
exit 0
