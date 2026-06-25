# Shared MARKER_MODE → default derivation for the harness scripts.
#
# This file is sourced by scripts/harness_run.sh.
#
# The function sets these variables in the caller's shell:
#   TEST_COMMAND                 — auto-filled if the caller still holds "__AUTO__"
#   POST_SENDKEY_DEFAULT         — fallback for POST_SENDKEY
#   SENDKEY_SEQUENCE_DEFAULT     — fallback for SENDKEY_SEQUENCE (login creds)
#   SENDKEY_SEQUENCE_NOWAIT_DEFAULT, RUN_WAIT_DEFAULT
#
# Inputs: MARKER_MODE, TEST_COMMAND.

harness_derive_marker_defaults() {
    POST_SENDKEY_DEFAULT=""
    SENDKEY_SEQUENCE_NOWAIT_DEFAULT="0"
    RUN_WAIT_DEFAULT=""
    # Standard root/root credentials sendkey sequence for cases that drive
    # the interactive login flow. Each case that uses this MUST also set
    # SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1" and RUN_WAIT_DEFAULT to at least 45.
    CREDS_SENDKEY_ROOT=$'sleep 5\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 2\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret'
    if [ "$TEST_COMMAND" = "__AUTO__" ]; then
        case "$MARKER_MODE" in
            m3_mapfail) TEST_COMMAND="mapfail 12 4" ;;
            m3_mapcopyfail) TEST_COMMAND="mapcopyfail 4" ;;
            m3_maperror) TEST_COMMAND="maperror 3" ;;
            m4_deny_paths)
                TEST_COMMAND="killdeny 2 9"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            m4_registry_deny_paths)
                TEST_COMMAND="regdeny"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            kernel_suspended_thread)
                TEST_COMMAND="suspendprobe"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_argv)
                TEST_COMMAND="argvprobe hello world"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_vqprobe)
                TEST_COMMAND="vqprobe"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_blk_basic)
                TEST_COMMAND="blkprobe"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_blk_concurrent)
                TEST_COMMAND="blkprobe concurrent"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_blk_perf)
                TEST_COMMAND="blkprobe perf"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_blk_session_teardown)
                TEST_COMMAND="blkprobe leak"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_bare_cmd)
                # UE17: PATH-based bare-command resolution. No `spawn` prefix —
                # the shell falls through from the builtin lookup to PATH-based
                # dispatch and runs /var/images/cat. We anchor on the procmgr
                # debug-print marker (`procmgr: container 'cat' started`)
                # because /etc/motd's contents go to TTY/stdout, not to COM2.
                TEST_COMMAND="cat /etc/motd"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_path_symlink_resolve)
                # Item #1 of open-work queue: /bin/ls is now a real ext2
                # symlink that resolves through VFS realpath instead of the
                # legacy strip_prefix("/bin/") hack. ls output goes to the
                # framebuffer, so harness_run.sh anchors on procmgr's
                # `container 'ls' started` debug print on COM2 instead.
                TEST_COMMAND="/bin/ls /"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_cd)
                TEST_COMMAND="cd /; cd etc; pwd"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_cd_inherit)
                TEST_COMMAND="cd /tmp; pwdprobe"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_ext2write)
                TEST_COMMAND="ext2io write"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_ext2append)
                TEST_COMMAND="ext2io append"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_ext2mutate)
                TEST_COMMAND="ext2io mutate"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_ext2unlink)
                TEST_COMMAND="ext2io unlink"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_owner_deny)
                TEST_COMMAND="ownerdeny"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            d7_container_storage)
                TEST_COMMAND="containerprobe"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
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
                TEST_COMMAND="sleepy"
                POST_SENDKEY_DEFAULT="ctrl-c"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_jobs)
                TEST_COMMAND="spawnbg sleepy"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_jobs_basic)
                TEST_COMMAND="sleep 30 & ; jobs"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_jobs_pipeline)
                TEST_COMMAND="echo abc | tr a-z A-Z"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_jobs_kill)
                TEST_COMMAND="sleep 30 & ; kill %1"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_alias_basic)
                TEST_COMMAND="alias ll=ls ; alias ll"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_type_basic)
                TEST_COMMAND="type cd ; type ls ; type nope"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_help_basic)
                TEST_COMMAND="help"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_exit_status)
                TEST_COMMAND="false ; echo \$?"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_ctrl_d_eof)
                # ^D at bare prompt → line discipline VEOF → cluuterm DeliverEof
                # → shell read(0) returns 0 → shell exits cleanly.
                TEST_COMMAND=""
                POST_SENDKEY_DEFAULT="ctrl-d"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_fg)
                TEST_COMMAND="spawnbg sleepy ; fg"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_stop)
                TEST_COMMAND="spawnbg sleepy ; stop"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_jobchurn)
                TEST_COMMAND="jobchurn 3"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_jobchurn_heavy)
                TEST_COMMAND="jobchurn 8"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_jobmix)
                TEST_COMMAND="jobmix"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_mkdir)
                TEST_COMMAND="mkdir /tmp/a; mkdir -p /tmp/b/c/d"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_cp)
                # Smoke test: spawn cp with no args. Verifies the binary
                # exists, the container view installs cleanly, and cp's
                # arg-parser fires. End-to-end file-copy is exercised
                # interactively (writing /tmp from shell-MemFs is a
                # separate VFS investigation — see follow-up task).
                TEST_COMMAND="cp"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_mv)
                # Same smoke pattern as l2_cp until end-to-end /tmp file
                # creation is unblocked.
                TEST_COMMAND="mv"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_envelope_mounts)
                # Root auto-logs in with supervisor envelope (/ rw), so we
                # drop into alice's *user* envelope via `su alice -c …` to
                # exercise the read-only /etc enforcement. The nested shell
                # runs the command, prints `touch: /etc/probefile:
                # PermissionDenied`, then exits.
                TEST_COMMAND="su alice -c touch /etc/probefile"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_cluufile_match)
                # Happy path for UE13's strict Cluufile validation: the
                # cat container has no MOUNT directives, so any caller view
                # is acceptable. The supervisor shell spawns /bin/cat to
                # read /etc/motd, demonstrating that validation is
                # permissive when the Cluufile makes no demands. Using
                # `spawn cat …` (not bare `cat …`) because the shell's
                # parser dispatches plain command words only to builtins;
                # `spawn` is the explicit binary-launch builtin.
                TEST_COMMAND="cat /etc/motd"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_cluufile_mismatch)
                # Mismatch path for UE13: the cfmismatch probe's Cluufile
                # demands MOUNT /etc readwrite, but alice's user envelope
                # provides /etc only as ro. Spawning from alice's nested
                # shell forces validation through pid_to_view and procmgr
                # must emit `cluufile mismatch` and reject with
                # PermissionDenied before main() runs.
                TEST_COMMAND="su alice -c cfmismatch"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_edit_smoke)
                # Smoke: spawn edit (no args). Verifies the binary boots
                # into raw-mode input loop without crashing. Edit blocks
                # on stdin recv after `edit: starting up` — clean exit
                # via injected key is a follow-up case (post-T18 once
                # rendering exists; see harness_run.sh marker comment).
                TEST_COMMAND="edit"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_edit_insert)
                # RED until an editprobe-style byte-injection helper exists. The
                # harness's KEYSTROKE_COMMANDS mechanism types whole lines + Enter,
                # so it can't drive INSERT mode (needs raw chars + Esc + :wq).
                # Manual interactive verification is the v1 acceptance path.
                TEST_COMMAND="edit /home/root/test.txt"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_edit_undo)
                # Same RED status as l2_edit_insert.
                TEST_COMMAND="edit /home/root/undo.txt"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_edit_eacces)
                # RED until byte-injection lands. Drops into alice (user envelope =
                # ro:/etc) and runs edit on /etc/motd. Without keystroke injection
                # for `iX:w`, the failing-write code path can't be exercised by the
                # harness; manual verification only.
                TEST_COMMAND="su alice -c edit /etc/motd"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_envelope_user)
                # GREEN as of UE16: ENV trailer in CONTAINER_RUN propagates the
                # shell's envelope-resolved env to the child.
                TEST_COMMAND="su alice -c envprobe HOME USER PATH SHELL"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_export)
                # UE15: `set X=v` is shell-local (child sees null); `export Y=v`
                # propagates via the ENV trailer so envprobe gets Y=exported.
                TEST_COMMAND="set X=local; export Y=exported; envprobe X Y"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_mount_private)
                # Seed shell's /tmp, then spawn the probe. The probe should see an
                # empty /tmp because its Cluufile declares MOUNT /tmp private.
                TEST_COMMAND="mkdir /tmp/MOUNTPROBE_CANARY; mountprobe"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_pts_listing)
                # Plan 2 Task 8: shell-spawned child can readdir /dev/pts and
                # see the cluuterm pts hosting its session. PASS if count >= 1.
                TEST_COMMAND="l2_pts_listing"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_mp_etc)
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
                TEST_COMMAND="mp -c \"open('/etc/motd').read()\""
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_ls)
                # Basic ls of /etc: verifies ls boots, VFS readdir works,
                # and at least one filename is printed.
                TEST_COMMAND="ls /etc"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_ls_long)
                # Write a file then ls -l: verify mode string and filename appear.
                TEST_COMMAND="echo hello > /tmp/lf; ls -l /tmp/lf"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_ls_color)
                # ls --color=always on /tmp: should emit ANSI escape prefix for dirs.
                TEST_COMMAND="mkdir -p /tmp/cd; ls --color=always /tmp"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_ls_recursive)
                # Create nested dir, ls -R, verify sub-entries appear.
                TEST_COMMAND="mkdir -p /tmp/r/sub; touch /tmp/r/a; touch /tmp/r/sub/b; ls -R /tmp/r"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_rm)
                TEST_COMMAND="mkdir /tmp/rmtest; mkdir /tmp/rmtest/inner; rm -r /tmp/rmtest"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_shellrc)
                # UE18+UE19+UE20: Verifies that /home/root/.shellrc was
                # sourced at session-shell startup. The rc file
                # overrides PATH via `export PATH=...`; if sourcing
                # worked, envprobe's child sees the overridden PATH
                # (instead of supervisor's envelope default
                # /sbin:/bin:/usr/sbin:/usr/bin).
                TEST_COMMAND="envprobe HOME PATH"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_waitpid)
                TEST_COMMAND="waitprobe"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_mmap)
                TEST_COMMAND="mmapprobe"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            a_poll)
                TEST_COMMAND="pollprobe"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_poll_pipes)
                TEST_COMMAND="pollprobe"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            perf_benchprobe)
                TEST_COMMAND="benchprobe"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            b_spawn_perf)
                TEST_COMMAND="benchprobe spawnonly"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            b_spawn_warm)
                TEST_COMMAND="benchprobe spawnonly"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            c_futex)
                TEST_COMMAND="futexprobe"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            c_futex_race)
                TEST_COMMAND="futexrace"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            m6_ipc_compact)
                TEST_COMMAND="repeat 8 hello"
                ;;
            m6_ipc_rendezvous)
                TEST_COMMAND="repeat 8 hello"
                ;;
            m6_ring_io)
                TEST_COMMAND="ringio ; echo ringio-marker"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            p1_setjmp)
                TEST_COMMAND="setjmpprobe"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            p1_env)
                TEST_COMMAND="envprobe"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            p1_stubs)
                TEST_COMMAND="stubsprobe"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            p2_pipe)
                TEST_COMMAND="pipeprobe"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            p2_spawn_pipe)
                TEST_COMMAND="spawnpipeprobe"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            p3_tls)
                TEST_COMMAND="tlsprobe"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            p3_pthread)
                TEST_COMMAND="pthreadprobe"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            p4_dev)
                TEST_COMMAND="devprobe"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            p4_framebuf)
                TEST_COMMAND="fbprobe"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            b_console_blit)
                TEST_COMMAND="console_blit_bench"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_devfb0)
                TEST_COMMAND="devfb0_probe"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            p4_mmap)
                TEST_COMMAND="mmapprobe"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_pipe_builtin)
                # Phase 4 Plan B Stage 0: verify builtin | container works.
                # echo is an in-process builtin; cat is a container. The
                # builtin writes via PIPE_DATA_LABEL; cat reads and echoes.
                TEST_COMMAND="echo hello | cat"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_pipe_builtin_chain)
                # Phase 4 Plan B Stage 0: verify builtin | container with
                # transformation. echo feeds tr which uppercases.
                TEST_COMMAND="echo abc | tr a-z A-Z"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_pipe_builtin_3stage)
                # Phase 4 Plan B Stage 0: 3-stage pipeline where the first
                # stage is a shell builtin (echo) and stages 2-3 are
                # containers (cat|cat). Verifies builtin→pipe→container→pipe
                # chain: WriteSink::Pipe → first cat → second cat → TTY.
                TEST_COMMAND="echo hello | cat | cat"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_pipe_3stage)
                # Phase 4 Plan E diagnostic: 3-stage cat|grep|head with
                # synthetic input. Writes 5 lines to /tmp/in.txt, then
                # pipelines cat → grep alpha → head -1. Expects "alpha"
                # and EXIT=0 on COM2. Distinct from l2_pipe_three (which
                # uses /etc/motd) to anchor on predictable synthetic data.
                TEST_COMMAND="echo -e 'alpha\nbeta\ngamma\nalpha\ndelta' > /tmp/in.txt; cat /tmp/in.txt | grep alpha | head -1; echo EXIT=\$?"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_pipe_basic)
                TEST_COMMAND="cat /etc/motd | head -3"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_pipe_env)
                # Phase 4 Plan E Stage 2: verify env propagates into pipe
                # stages. echo is a shell builtin so it expands $PIPETEST
                # before spawn; wc -c is the spawned binary that inherits
                # the env from the pipeline spawn. "hello\n" = 6 bytes.
                # printenv not yet shipped (Plan B), so we exercise the
                # spawn-env path indirectly via wc character count.
                # Env propagation fix builds clean; targeted getenv-reading
                # test deferred to Plan B when printenv is available.
                TEST_COMMAND="export PIPETEST=hello; echo \$PIPETEST | wc -c"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_pipe_three)
                TEST_COMMAND="cat /etc/motd | grep CLUU | head -1"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_redir_stdout_file)
                TEST_COMMAND="cat /etc/motd > /tmp/motdcopy; cat /tmp/motdcopy | head -1"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
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
                TEST_COMMAND="_shellcrash"
                ;;
            hr7_su_equal)
                TEST_COMMAND="sutest equal"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            m5_fairness) TEST_COMMAND="repeat 8 hello" ;;
            l2_cat_basic)
                # GNU-close cat: -n numbers all output lines.
                # Uses /etc/motd as a stable file. Verifies flag parsing
                # and the debug marker on exit.
                TEST_COMMAND="cat -n /etc/motd"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_cp_recursive)
                # cp -r: copy /etc to /tmp/etccopy, then ls the copy.
                # Verifies recursive directory copy via libcluu::cli.
                TEST_COMMAND="cp -r /etc /tmp/etccopy"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_head_bytes)
                # head -c N: print first N bytes from /etc/motd.
                # Verifies -c flag and RequiredArg parsing via libcluu::cli.
                TEST_COMMAND="head -c 20 /etc/motd"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_wc_lines)
                # wc -l: count newlines in /etc/motd.
                # Verifies -l flag and single-column output via libcluu::cli.
                TEST_COMMAND="wc -l /etc/motd"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_grep_recursive)
                # grep -rn: recursive search for 'CLUU' under /etc.
                # Verifies -r and -n flags via libcluu::cli.
                TEST_COMMAND="grep -rn CLUU /etc"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_basename_basic)
                # basename: strip directory from path.
                TEST_COMMAND="basename /etc/users.toml"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                EXPECTED_CONTAINS=("users.toml")
                ;;
            l2_dirname_basic)
                # dirname: strip last component from path.
                TEST_COMMAND="dirname /etc/users.toml"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                EXPECTED_CONTAINS=("/etc")
                ;;
            l2_sleep_basic)
                # sleep: delay then print done.
                TEST_COMMAND="sleep 1; echo done"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                EXPECTED_CONTAINS=("done")
                ;;
            l2_which_basic)
                # which: find self in PATH. Each container's view maps
                # /bin → /var/images/<self>/bin, so `which <other>` won't
                # find binaries that don't ship with the which container.
                # `which which` always works because /bin/which is the
                # binary the container is running from.
                TEST_COMMAND="which which"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                EXPECTED_CONTAINS=("/bin/which")
                ;;
            l2_printf_basic)
                # printf: format string substitution.
                TEST_COMMAND="printf '%s=%d\n' foo 42"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                EXPECTED_CONTAINS=("foo=42")
                ;;
            l2_date_basic)
                # date: print current date — just check year "20xx" appears.
                TEST_COMMAND="date"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                EXPECTED_CONTAINS=("20")
                ;;
            l2_env_basic)
                # env: print environment — check at least one KEY=VALUE line.
                TEST_COMMAND="env | head -1"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                EXPECTED_CONTAINS=("=")
                ;;
            l2_kill_basic)
                # kill --help: verify binary builds and parses --help.
                TEST_COMMAND="kill --help"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                EXPECTED_CONTAINS=("Usage")
                ;;
            l2_sort_basic)
                # sort: sort three lines lexicographically.
                TEST_COMMAND="printf 'c\nb\na\n' > /tmp/s.in; sort /tmp/s.in"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                EXPECTED_CONTAINS=("a" "b" "c")
                ;;
            l2_uniq_basic)
                # uniq -c: prefix each line with occurrence count.
                TEST_COMMAND="printf 'a\na\nb\n' > /tmp/u.in; uniq -c /tmp/u.in"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                EXPECTED_CONTAINS=("2 a" "1 b")
                ;;
            l2_cut_basic)
                # cut -d: -f2: extract second colon-delimited field.
                TEST_COMMAND="printf 'a:b:c\n' | cut -d: -f2"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                EXPECTED_CONTAINS=("b")
                ;;
            l2_tr_basic)
                # tr a-z A-Z: uppercase ASCII letters.
                TEST_COMMAND="printf 'abc\n' | tr a-z A-Z"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                EXPECTED_CONTAINS=("ABC")
                ;;
            l2_stat_basic)
                # stat: display file metadata for a freshly-touched file.
                TEST_COMMAND="touch /tmp/sf; stat /tmp/sf"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                EXPECTED_CONTAINS=("File:" "sf" "Size:")
                ;;
            l2_du_basic)
                # du -s: summarize disk usage for /etc.
                TEST_COMMAND="du -s /etc"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                EXPECTED_CONTAINS=("/etc")
                ;;
            l2_find_basic)
                # find -name: locate files by glob pattern.
                TEST_COMMAND="mkdir -p /tmp/f; touch /tmp/f/a.txt; find /tmp/f -name '*.txt'"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                EXPECTED_CONTAINS=("/tmp/f/a.txt")
                ;;
            l2_vt4_default)
                TEST_COMMAND=""
                # Pure boot-time marker: compositor is pinned to VT4 at boot
                # (Task 20). No keyboard input needed.
                ;;
            l2_cluuterm_smoke)
                TEST_COMMAND=""
                # autostart.toml boots cluuterm at VT4; all markers fire at
                # boot without any keyboard input.
                ;;
            l2_login)
                TEST_COMMAND=""
                # Inject credentials to trigger SESSION_CREATE + session-procmgr spawn.
                # Same sendkey sequence as l2_cluuterm_login: root/root on VT4.
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 5\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 2\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret'
                ;;
            l2_cluuterm_login)
                TEST_COMMAND=""
                # After boot, inject credentials into the login modal.
                # The login modal spawns BEFORE any shell, so `shell: ready`
                # cannot gate keystroke injection — fire keys unconditionally.
                #   sleep 5: compositor + login modal ready by ~5s.
                #   sleep 2: password field appears after username Enter.
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 5\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 2\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret'
                ;;
            l2_compositor_swap_login)
                TEST_COMMAND=""
                # Same credentials as l2_cluuterm_login. Boot, log in
                # via the compositor login modal, observe both
                # session_mode traces.
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 5\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 2\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret'
                ;;
            l2_envelope_home_propagated)
                TEST_COMMAND=""
                # After graphical login (VT4 cluuterm session), shell prints
                # /home/root upon `echo $HOME`. Marker is the literal
                # `vfs: open '/home/root/.shellrc'` from shellrc loading
                # (proves HOME was populated by procmgr envelope substitution
                # AND propagated through posix_spawn env trailer to the shell).
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 5\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 2\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret'
                ;;
            l2_cluuterm_ansi)
                TEST_COMMAND=""
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 5\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 2\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 3\nsendkey p\nsendkey r\nsendkey i\nsendkey n\nsendkey t\nsendkey f\nsendkey spc\nsendkey apostrophe\nsendkey backslash\nsendkey 0\nsendkey 3\nsendkey 3\nsendkey bracket_left\nsendkey 3\nsendkey 1\nsendkey m\nsendkey r\nsendkey e\nsendkey d\nsendkey backslash\nsendkey 0\nsendkey 3\nsendkey 3\nsendkey bracket_left\nsendkey 0\nsendkey m\nsendkey apostrophe\nsendkey ret'
                ;;
            l2_cluuterm_keymap)
                TEST_COMMAND=""
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 5\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 2\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 3\nsendkey up'
                ;;
            l2_cluuterm_exit)
                TEST_COMMAND=""
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 5\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 2\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 3\nsendkey e\nsendkey x\nsendkey i\nsendkey t\nsendkey ret'
                ;;
            l2_cluuterm_two_windows)
                TEST_COMMAND=""
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 5\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 2\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 3\nsendkey ctrl-alt-n'
                ;;
            l2_cluuterm_raw_mode)
                # MicroPython calls tcsetattr(raw) for its REPL on stdin.
                # This reaches the legacy tty's LineDiscipline via TTY_CTL_LABEL,
                # which calls set_mode() and emits the raw-mode marker.
                # Use `mp -c ...` so the process exits and the shell can
                # observe line_discipline: mode=canonical on restore, but we
                # only require the initial raw-mode switch.
                TEST_COMMAND="micropython"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_vt_legacy_preserved)
                TEST_COMMAND=""
                # vtmgr boots at active_vt=0 regardless of compositor pin.
                # First switch TO compositor VT4 (ctrl-alt-f5), then back to
                # legacy VT0 (ctrl-alt-f1), confirming full round-trip.
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 3\nsendkey ctrl-alt-f5\nsleep 3\nsendkey ctrl-alt-f1'
                ;;
            l2_compositor_smoke)
                TEST_COMMAND=""
                # No TEST_COMMAND needed — compositor + compdemo autostart from
                # etc/autostart.toml at boot; markers fire without shell command.
                ;;
            l2_compositor_focus)
                TEST_COMMAND=""
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 5\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 2\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 3\nsendkey ctrl-alt-n\nsleep 3\nsendkey alt-tab'
                ;;
            l2_compositor_destroy)
                TEST_COMMAND=""
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 5\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 2\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 3\nsendkey ctrl-alt-q'
                ;;
            l2_compositor_legacy_vt)
                TEST_COMMAND=""
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="30"
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 8\nsendkey ctrl-alt-f1\nsleep 3\nsendkey ctrl-alt-f5'
                ;;
            b_compositor_blit)
                TEST_COMMAND=""
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 8\nsendkey ctrl-alt-f5'
                ;;
            l2_timeserver_pushmode_tick)
                TEST_COMMAND="timetick_probe"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            l2_text_shell_input)
                TEST_COMMAND=""
                # VT0 text login flow: switch to VT0, log in as root,
                # then type `xyz\n` (an unknown command) so shell emits
                # `shell: read 4 bytes from fd 0` + `shell: unsupported command`
                # debug_prints — both serial-visible. The previous marker
                # design tried to read shell stdout via the COM2 mirror,
                # but tty/console writes only reach the framebuffer.
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 12\nsendkey ctrl-alt-f1\nsleep 1\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 1\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 4\nsendkey x\nsendkey y\nsendkey z\nsendkey ret'
                ;;
            l2_envelope_dev_filter)
                TEST_COMMAND=""
                # After VT0 text login, list /dev. Expect tty0 visible,
                # tty1/tty2/tty3 NOT visible. Marker is the literal
                # output of the shell builtin `ls` listing /dev contents
                # (forwarded to the console via stdout writes to /dev/tty0).
                # Sequence: open VT0 (Ctrl+Alt+F1), root/root login, `ls /dev`.
                # NOTE: '/' maps to shift-6 on the HU (QWERTZ) keyboard layout
                # that the QEMU harness uses (see type_ascii_command '/' case).
                # sleep 12: match l2_text_shell_input timing so the text login
                # prompt is stable before we start injecting credentials.
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 12\nsendkey ctrl-alt-f1\nsleep 1\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 1\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 4\nsendkey l\nsendkey s\nsendkey spc\nsendkey shift-6\nsendkey d\nsendkey e\nsendkey v\nsendkey ret'
                ;;
            l2_cluuterm_shell_input)
                TEST_COMMAND=""
                # Default VT is 4 (compositor). Type root/root in the login
                # modal, wait for cluuterm to take over and spawn /bin/shell,
                # then type `xyz\n` so shell emits `shell: read 4 bytes`
                # + `shell: unsupported command` debug_prints. Proves the
                # cluuterm pts -> shell read(0) round-trip works.
                #
                # SENDKEY_SEQUENCE_NOWAIT_DEFAULT=1: the login modal spawns
                # BEFORE any shell, so `shell: ready` cannot gate keystroke
                # injection — we must fire keys unconditionally. The sleep
                # values inside the sequence handle the timing:
                #   sleep 5: compositor + login modal are ready by ~5s.
                #   sleep 2: password field appears after username Enter.
                #   sleep 3: cluuterm + shell start up after auth (~3s).
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 5\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 2\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 8\nsendkey x\nsendkey y\nsendkey z\nsendkey ret'
                ;;
            legacy_p1)
                TEST_COMMAND="minimal"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            pm_vfs_view_scope)
                TEST_COMMAND="container run pm_vfs_view_scope"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            pm_pid_layout)
                TEST_COMMAND="container run pm_pid_layout"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            pm_session_id_recycle)
                TEST_COMMAND="container run pm_session_id_recycle"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            pm_cross_session_no_leak)
                TEST_COMMAND="container run pm_cross_session_no_leak"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            pm_cap_revoke_stale)
                TEST_COMMAND="container run pm_cap_revoke_stale"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            pm_proc_query_all_cap)
                TEST_COMMAND="container run pm_proc_query_all_cap"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            pm_service_restart)
                TEST_COMMAND="container run pm_service_restart"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            pm_session_crash_cascade)
                TEST_COMMAND="container run pm_session_crash_cascade"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            pm_bootstrap_two_pmgr)
                TEST_COMMAND="container run pm_bootstrap_two_pmgr"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
            *) TEST_COMMAND="hello" ;;
        esac
    fi
}
