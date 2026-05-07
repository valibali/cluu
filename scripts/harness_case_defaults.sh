# Shared MARKER_MODE → default derivation for the harness scripts.
#
# This file is sourced by both scripts/harness_run.sh (which actually uses the
# derived TEST_COMMAND / SHELL_AUTOSTART_CMD_DEFAULT / POST_SENDKEY_DEFAULT) and
# scripts/harness_suite.sh (which uses SHELL_AUTOSTART_CMD_DEFAULT to decide
# whether a case can skip its rebuild).
#
# The function sets three variables in the caller's shell:
#   TEST_COMMAND                 — auto-filled if the caller still holds "__AUTO__"
#   SHELL_AUTOSTART_CMD_DEFAULT  — fallback for CLUU_SHELL_AUTOSTART_CMD
#   POST_SENDKEY_DEFAULT         — fallback for POST_SENDKEY
#
# Inputs: MARKER_MODE, TEST_COMMAND.

harness_derive_marker_defaults() {
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
            kernel_suspended_thread)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="spawn suspendprobe"
                ;;
            l2_argv)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="spawn argvprobe hello world"
                ;;
            l2_vqprobe)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="spawn vqprobe"
                ;;
            l2_bare_cmd)
                TEST_COMMAND=""
                # UE17: PATH-based bare-command resolution. No `spawn` prefix —
                # the shell falls through from the builtin lookup to PATH-based
                # dispatch and runs /var/images/cat. We anchor on the procmgr
                # debug-print marker (`procmgr: container 'cat' started`)
                # because /etc/motd's contents go to TTY/stdout, not to COM2.
                SHELL_AUTOSTART_CMD_DEFAULT="cat /etc/motd"
                ;;
            l2_cd)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="cd /; cd etc; pwd"
                ;;
            l2_cd_inherit)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="cd /tmp; spawn pwdprobe"
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
            d7_container_storage)
                TEST_COMMAND="spawn containerprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="spawn containerprobe"
                ;;
            e13_container_run)
                TEST_COMMAND="container run hello"
                ;;
            f8_nested_container_run)
                TEST_COMMAND="container run nestprobe"
                ;;
            f9_escalation)
                TEST_COMMAND="container run escalateprobe"
                ;;
            f10_view_passthrough)
                TEST_COMMAND="container run viewprobe"
                ;;
            f11_deny_inherit)
                TEST_COMMAND="container run denyprobe"
                ;;
            f12_cascade_cleanup)
                TEST_COMMAND="container run cascadeprobe"
                ;;
            f13_detach_survive)
                TEST_COMMAND="container run detachprobe"
                ;;
            g7_vt_container)
                TEST_COMMAND=""
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
            l2_mkdir)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="spawn mkdir /tmp/a; spawn mkdir -p /tmp/b/c/d"
                ;;
            l2_cp)
                TEST_COMMAND=""
                # Smoke test: spawn cp with no args. Verifies the binary
                # exists, the container view installs cleanly, and cp's
                # arg-parser fires. End-to-end file-copy is exercised
                # interactively (writing /tmp from shell-MemFs is a
                # separate VFS investigation — see follow-up task).
                SHELL_AUTOSTART_CMD_DEFAULT="spawn cp"
                ;;
            l2_mv)
                TEST_COMMAND=""
                # Same smoke pattern as l2_cp until end-to-end /tmp file
                # creation is unblocked.
                SHELL_AUTOSTART_CMD_DEFAULT="spawn mv"
                ;;
            l2_envelope_mounts)
                TEST_COMMAND=""
                # Root auto-logs in with supervisor envelope (/ rw), so we
                # drop into alice's *user* envelope via `su alice -c …` to
                # exercise the read-only /etc enforcement. The nested shell
                # runs the command, prints `touch: /etc/probefile:
                # PermissionDenied`, then exits.
                SHELL_AUTOSTART_CMD_DEFAULT="su alice -c spawn touch /etc/probefile"
                ;;
            l2_cluufile_match)
                TEST_COMMAND=""
                # Happy path for UE13's strict Cluufile validation: the
                # cat container has no MOUNT directives, so any caller view
                # is acceptable. The supervisor shell spawns /bin/cat to
                # read /etc/motd, demonstrating that validation is
                # permissive when the Cluufile makes no demands. Using
                # `spawn cat …` (not bare `cat …`) because the shell's
                # parser dispatches plain command words only to builtins;
                # `spawn` is the explicit binary-launch builtin.
                SHELL_AUTOSTART_CMD_DEFAULT="spawn cat /etc/motd"
                ;;
            l2_cluufile_mismatch)
                TEST_COMMAND=""
                # Mismatch path for UE13: the cfmismatch probe's Cluufile
                # demands MOUNT /etc readwrite, but alice's user envelope
                # provides /etc only as ro. Spawning from alice's nested
                # shell forces validation through pid_to_view and procmgr
                # must emit `cluufile mismatch` and reject with
                # PermissionDenied before main() runs.
                SHELL_AUTOSTART_CMD_DEFAULT="su alice -c spawn cfmismatch"
                ;;
            l2_edit_smoke)
                TEST_COMMAND=""
                # Smoke: spawn edit (no args). Verifies the binary boots
                # into raw-mode input loop without crashing. Edit blocks
                # on stdin recv after `edit: starting up` — clean exit
                # via injected key is a follow-up case (post-T18 once
                # rendering exists; see harness_run.sh marker comment).
                SHELL_AUTOSTART_CMD_DEFAULT="spawn edit"
                ;;
            l2_edit_insert)
                TEST_COMMAND=""
                # RED until an editprobe-style byte-injection helper exists. The
                # harness's KEYSTROKE_COMMANDS mechanism types whole lines + Enter,
                # so it can't drive INSERT mode (needs raw chars + Esc + :wq).
                # Manual interactive verification is the v1 acceptance path.
                SHELL_AUTOSTART_CMD_DEFAULT="spawn edit /home/root/test.txt"
                ;;
            l2_edit_undo)
                TEST_COMMAND=""
                # Same RED status as l2_edit_insert.
                SHELL_AUTOSTART_CMD_DEFAULT="spawn edit /home/root/undo.txt"
                ;;
            l2_edit_eacces)
                TEST_COMMAND=""
                # RED until byte-injection lands. Drops into alice (user envelope =
                # ro:/etc) and runs edit on /etc/motd. Without keystroke injection
                # for `iX:w`, the failing-write code path can't be exercised by the
                # harness; manual verification only.
                SHELL_AUTOSTART_CMD_DEFAULT="su alice -c spawn edit /etc/motd"
                ;;
            l2_envelope_user)
                TEST_COMMAND=""
                # GREEN as of UE16: ENV trailer in CONTAINER_RUN propagates the
                # shell's envelope-resolved env to the child.
                SHELL_AUTOSTART_CMD_DEFAULT="su alice -c spawn envprobe HOME USER PATH SHELL"
                ;;
            l2_export)
                TEST_COMMAND=""
                # UE15: `set X=v` is shell-local (child sees null); `export Y=v`
                # propagates via the ENV trailer so envprobe gets Y=exported.
                SHELL_AUTOSTART_CMD_DEFAULT="set X=local; export Y=exported; spawn envprobe X Y"
                ;;
            l2_mount_private)
                TEST_COMMAND=""
                # Seed shell's /tmp, then spawn the probe. The probe should see an
                # empty /tmp because its Cluufile declares MOUNT /tmp private.
                SHELL_AUTOSTART_CMD_DEFAULT="spawn mkdir /tmp/MOUNTPROBE_CANARY; spawn mountprobe"
                ;;
            l2_mp_etc)
                TEST_COMMAND=""
                # MicroPython opens /etc/motd through libcluu's POSIX shim, which
                # in turn goes through VFS. Success proves: (a) the mp container
                # composes correctly with the supervisor envelope, (b) /etc is
                # reachable via the inherited view, (c) mp's POSIX VFS layer is
                # functional end-to-end. Marker: `micropython: exit 0` (added in
                # UE22 as the one permanent debug_print mp emits on exit).
                #
                # The python source is double-quoted because cluu_lang's
                # parser treats `(` `)` as subshell delimiters in bare
                # words. Inside double quotes parens are plain text. The
                # python source itself uses single quotes for the path,
                # so the outer double quotes nest cleanly.
                SHELL_AUTOSTART_CMD_DEFAULT="spawn mp -c \"open('/etc/motd').read()\""
                ;;
            l2_rm)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="spawn mkdir /tmp/rmtest; spawn mkdir /tmp/rmtest/inner; spawn rm -r /tmp/rmtest"
                ;;
            l2_shellrc)
                TEST_COMMAND=""
                # UE18+UE19+UE20: Verifies that /home/root/.shellrc was
                # sourced at session-shell startup. The rc file
                # overrides PATH via `export PATH=...`; if sourcing
                # worked, envprobe's child sees the overridden PATH
                # (instead of supervisor's envelope default
                # /sbin:/bin:/usr/sbin:/usr/bin).
                SHELL_AUTOSTART_CMD_DEFAULT="spawn envprobe HOME PATH"
                ;;
            l2_waitpid)
                TEST_COMMAND="spawn waitprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="spawn waitprobe"
                ;;
            l2_mmap)
                TEST_COMMAND="spawn mmapprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="spawn mmapprobe"
                ;;
            a_poll)
                TEST_COMMAND="spawn pollprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="spawn pollprobe"
                ;;
            perf_benchprobe)
                TEST_COMMAND="spawn benchprobe"
                SHELL_AUTOSTART_CMD_DEFAULT=""
                ;;
            b_spawn_perf)
                TEST_COMMAND="spawn benchprobe spawnonly"
                SHELL_AUTOSTART_CMD_DEFAULT=""
                ;;
            b_spawn_warm)
                TEST_COMMAND="spawn benchprobe spawnonly"
                SHELL_AUTOSTART_CMD_DEFAULT=""
                ;;
            c_futex)
                TEST_COMMAND="spawn futexprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="spawn futexprobe"
                ;;
            c_futex_race)
                TEST_COMMAND="spawn futexrace"
                SHELL_AUTOSTART_CMD_DEFAULT="spawn futexrace"
                ;;
            m6_ipc_compact)
                TEST_COMMAND="repeat 8 spawn hello"
                ;;
            m6_ipc_rendezvous)
                TEST_COMMAND="repeat 8 spawn hello"
                ;;
            m6_ring_io)
                TEST_COMMAND="echo ringio-marker"
                SHELL_AUTOSTART_CMD_DEFAULT="ringio"
                ;;
            p1_setjmp)
                TEST_COMMAND="spawn setjmpprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="spawn setjmpprobe"
                ;;
            p1_env)
                TEST_COMMAND="spawn envprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="spawn envprobe"
                ;;
            p1_stubs)
                TEST_COMMAND="spawn stubsprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="spawn stubsprobe"
                ;;
            p2_pipe)
                TEST_COMMAND="spawn pipeprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="spawn pipeprobe"
                ;;
            p2_spawn_pipe)
                TEST_COMMAND="spawn spawnpipeprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="spawn spawnpipeprobe"
                ;;
            p3_tls)
                TEST_COMMAND="spawn tlsprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="spawn tlsprobe"
                ;;
            p3_pthread)
                TEST_COMMAND="spawn pthreadprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="spawn pthreadprobe"
                ;;
            p4_dev)
                TEST_COMMAND="spawn devprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="spawn devprobe"
                ;;
            p4_framebuf)
                TEST_COMMAND="spawn fbprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="spawn fbprobe"
                ;;
            p4_mmap)
                TEST_COMMAND="spawn mmapprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="spawn mmapprobe"
                ;;
            l2_pipe_basic)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="cat /etc/motd | head -3"
                ;;
            l2_pipe_three)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="cat /etc/motd | grep CLUU | head -1"
                ;;
            l2_redir_stdout_file)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="cat /etc/motd > /tmp/motdcopy; cat /tmp/motdcopy | head -1"
                ;;
            l2_tab_complete)
                TEST_COMMAND=""
                # Type "cat /etc/m" then TAB: TTY completes to "cat /etc/motd ".
                # Press Enter: shell runs cat /etc/motd and emits motd content.
                KEYSTROKE_COMMANDS=$'cat /etc/m\t'
                ;;
            perf_typing_storm)
                # Inject 500 chars at KEY_DELAY=0 (as fast as QEMU monitor +
                # bash can issue). After typing stops, the harness idles for
                # RUN_WAIT seconds. Diagnostics in IRQ/kbd/TTY/console emit
                # rate counts per layer; success = none of the layers go
                # silent for >5s after the last keystroke.
                TEST_COMMAND=""
                KEYSTROKE_COMMANDS="$(printf 'a%.0s' {1..500})"
                ;;
            hr6_shell_crash)
                TEST_COMMAND="shellcrash"
                ;;
            hr7_su_equal)
                TEST_COMMAND="suequaltest"
                SHELL_AUTOSTART_CMD_DEFAULT=""
                ;;
            m5_fairness) TEST_COMMAND="repeat 8 spawn hello" ;;
            legacy_p1)
                TEST_COMMAND="spawn minimal"
                SHELL_AUTOSTART_CMD_DEFAULT="spawn minimal"
                ;;
            *) TEST_COMMAND="spawn hello" ;;
        esac
    fi
}
