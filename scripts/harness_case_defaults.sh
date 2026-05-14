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
    SENDKEY_SEQUENCE_NOWAIT_DEFAULT="0"
    RUN_WAIT_DEFAULT=""
    if [ "$TEST_COMMAND" = "__AUTO__" ]; then
        case "$MARKER_MODE" in
            m3_mapfail) TEST_COMMAND="mapfail 12 4" ;;
            m3_mapcopyfail) TEST_COMMAND="mapcopyfail 4" ;;
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
                SHELL_AUTOSTART_CMD_DEFAULT="suspendprobe"
                ;;
            l2_argv)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="argvprobe hello world"
                ;;
            l2_vqprobe)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="vqprobe"
                ;;
            l2_blk_basic)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="blkprobe"
                ;;
            l2_blk_concurrent)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="blkprobe concurrent"
                ;;
            l2_blk_perf)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="blkprobe perf"
                ;;
            l2_blk_session_teardown)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="blkprobe leak"
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
            l2_path_symlink_resolve)
                TEST_COMMAND=""
                # Item #1 of open-work queue: /bin/ls is now a real ext2
                # symlink that resolves through VFS realpath instead of the
                # legacy strip_prefix("/bin/") hack. ls output goes to the
                # framebuffer, so harness_run.sh anchors on procmgr's
                # `container 'ls' started` debug print on COM2 instead.
                SHELL_AUTOSTART_CMD_DEFAULT="/bin/ls /"
                ;;
            l2_cd)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="cd /; cd etc; pwd"
                ;;
            l2_cd_inherit)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="cd /tmp; pwdprobe"
                ;;
            l2_ext2write)
                TEST_COMMAND="ext2io write"
                SHELL_AUTOSTART_CMD_DEFAULT="ext2io write"
                ;;
            l2_ext2append)
                TEST_COMMAND="ext2io append"
                SHELL_AUTOSTART_CMD_DEFAULT="ext2io append"
                ;;
            l2_ext2mutate)
                TEST_COMMAND="ext2io mutate"
                SHELL_AUTOSTART_CMD_DEFAULT="ext2io mutate"
                ;;
            l2_ext2unlink)
                TEST_COMMAND="ext2io unlink"
                SHELL_AUTOSTART_CMD_DEFAULT="ext2io unlink"
                ;;
            l2_owner_deny)
                TEST_COMMAND="ownerdeny"
                SHELL_AUTOSTART_CMD_DEFAULT="ownerdeny"
                ;;
            d7_container_storage)
                TEST_COMMAND="containerprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="containerprobe"
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
                SHELL_AUTOSTART_CMD_DEFAULT="sleepy"
                POST_SENDKEY_DEFAULT="ctrl-c"
                ;;
            l2_jobs)
                TEST_COMMAND="spawnbg sleepy"
                SHELL_AUTOSTART_CMD_DEFAULT="spawnbg sleepy"
                ;;
            l2_jobs_basic)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="sleep 30 & ; jobs"
                ;;
            l2_jobs_pipeline)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="echo abc | tr a-z A-Z"
                ;;
            l2_jobs_kill)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="sleep 30 & ; kill %1"
                ;;
            l2_alias_basic)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="alias ll='ls -l' ; alias ll"
                ;;
            l2_type_basic)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="type cd ; type ls ; type nope"
                ;;
            l2_help_basic)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="help"
                ;;
            l2_exit_status)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="false ; echo \$?"
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
                SHELL_AUTOSTART_CMD_DEFAULT="mkdir /tmp/a; mkdir -p /tmp/b/c/d"
                ;;
            l2_cp)
                TEST_COMMAND=""
                # Smoke test: spawn cp with no args. Verifies the binary
                # exists, the container view installs cleanly, and cp's
                # arg-parser fires. End-to-end file-copy is exercised
                # interactively (writing /tmp from shell-MemFs is a
                # separate VFS investigation — see follow-up task).
                SHELL_AUTOSTART_CMD_DEFAULT="cp"
                ;;
            l2_mv)
                TEST_COMMAND=""
                # Same smoke pattern as l2_cp until end-to-end /tmp file
                # creation is unblocked.
                SHELL_AUTOSTART_CMD_DEFAULT="mv"
                ;;
            l2_envelope_mounts)
                TEST_COMMAND=""
                # Root auto-logs in with supervisor envelope (/ rw), so we
                # drop into alice's *user* envelope via `su alice -c …` to
                # exercise the read-only /etc enforcement. The nested shell
                # runs the command, prints `touch: /etc/probefile:
                # PermissionDenied`, then exits.
                SHELL_AUTOSTART_CMD_DEFAULT="su alice -c touch /etc/probefile"
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
                SHELL_AUTOSTART_CMD_DEFAULT="cat /etc/motd"
                ;;
            l2_cluufile_mismatch)
                TEST_COMMAND=""
                # Mismatch path for UE13: the cfmismatch probe's Cluufile
                # demands MOUNT /etc readwrite, but alice's user envelope
                # provides /etc only as ro. Spawning from alice's nested
                # shell forces validation through pid_to_view and procmgr
                # must emit `cluufile mismatch` and reject with
                # PermissionDenied before main() runs.
                SHELL_AUTOSTART_CMD_DEFAULT="su alice -c cfmismatch"
                ;;
            l2_edit_smoke)
                TEST_COMMAND=""
                # Smoke: spawn edit (no args). Verifies the binary boots
                # into raw-mode input loop without crashing. Edit blocks
                # on stdin recv after `edit: starting up` — clean exit
                # via injected key is a follow-up case (post-T18 once
                # rendering exists; see harness_run.sh marker comment).
                SHELL_AUTOSTART_CMD_DEFAULT="edit"
                ;;
            l2_edit_insert)
                TEST_COMMAND=""
                # RED until an editprobe-style byte-injection helper exists. The
                # harness's KEYSTROKE_COMMANDS mechanism types whole lines + Enter,
                # so it can't drive INSERT mode (needs raw chars + Esc + :wq).
                # Manual interactive verification is the v1 acceptance path.
                SHELL_AUTOSTART_CMD_DEFAULT="edit /home/root/test.txt"
                ;;
            l2_edit_undo)
                TEST_COMMAND=""
                # Same RED status as l2_edit_insert.
                SHELL_AUTOSTART_CMD_DEFAULT="edit /home/root/undo.txt"
                ;;
            l2_edit_eacces)
                TEST_COMMAND=""
                # RED until byte-injection lands. Drops into alice (user envelope =
                # ro:/etc) and runs edit on /etc/motd. Without keystroke injection
                # for `iX:w`, the failing-write code path can't be exercised by the
                # harness; manual verification only.
                SHELL_AUTOSTART_CMD_DEFAULT="su alice -c edit /etc/motd"
                ;;
            l2_envelope_user)
                TEST_COMMAND=""
                # GREEN as of UE16: ENV trailer in CONTAINER_RUN propagates the
                # shell's envelope-resolved env to the child.
                SHELL_AUTOSTART_CMD_DEFAULT="su alice -c envprobe HOME USER PATH SHELL"
                ;;
            l2_export)
                TEST_COMMAND=""
                # UE15: `set X=v` is shell-local (child sees null); `export Y=v`
                # propagates via the ENV trailer so envprobe gets Y=exported.
                SHELL_AUTOSTART_CMD_DEFAULT="set X=local; export Y=exported; envprobe X Y"
                ;;
            l2_mount_private)
                TEST_COMMAND=""
                # Seed shell's /tmp, then spawn the probe. The probe should see an
                # empty /tmp because its Cluufile declares MOUNT /tmp private.
                SHELL_AUTOSTART_CMD_DEFAULT="mkdir /tmp/MOUNTPROBE_CANARY; mountprobe"
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
                SHELL_AUTOSTART_CMD_DEFAULT="mp -c \"open('/etc/motd').read()\""
                ;;
            l2_ls)
                TEST_COMMAND=""
                # Basic ls of /etc: verifies ls boots, VFS readdir works,
                # and at least one filename is printed.
                SHELL_AUTOSTART_CMD_DEFAULT="ls /etc"
                ;;
            l2_ls_long)
                TEST_COMMAND=""
                # Write a file then ls -l: verify mode string and filename appear.
                SHELL_AUTOSTART_CMD_DEFAULT="echo hello > /tmp/lf; ls -l /tmp/lf"
                ;;
            l2_ls_color)
                TEST_COMMAND=""
                # ls --color=always on /tmp: should emit ANSI escape prefix for dirs.
                SHELL_AUTOSTART_CMD_DEFAULT="mkdir -p /tmp/cd; ls --color=always /tmp"
                ;;
            l2_ls_recursive)
                TEST_COMMAND=""
                # Create nested dir, ls -R, verify sub-entries appear.
                SHELL_AUTOSTART_CMD_DEFAULT="mkdir -p /tmp/r/sub; touch /tmp/r/a; touch /tmp/r/sub/b; ls -R /tmp/r"
                ;;
            l2_rm)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="mkdir /tmp/rmtest; mkdir /tmp/rmtest/inner; rm -r /tmp/rmtest"
                ;;
            l2_shellrc)
                TEST_COMMAND=""
                # UE18+UE19+UE20: Verifies that /home/root/.shellrc was
                # sourced at session-shell startup. The rc file
                # overrides PATH via `export PATH=...`; if sourcing
                # worked, envprobe's child sees the overridden PATH
                # (instead of supervisor's envelope default
                # /sbin:/bin:/usr/sbin:/usr/bin).
                SHELL_AUTOSTART_CMD_DEFAULT="envprobe HOME PATH"
                ;;
            l2_waitpid)
                TEST_COMMAND="waitprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="waitprobe"
                ;;
            l2_mmap)
                TEST_COMMAND="mmapprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="mmapprobe"
                ;;
            a_poll)
                TEST_COMMAND="pollprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="pollprobe"
                ;;
            l2_poll_pipes)
                TEST_COMMAND="pollprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="pollprobe"
                ;;
            perf_benchprobe)
                TEST_COMMAND="benchprobe"
                SHELL_AUTOSTART_CMD_DEFAULT=""
                ;;
            b_spawn_perf)
                TEST_COMMAND="benchprobe spawnonly"
                SHELL_AUTOSTART_CMD_DEFAULT=""
                ;;
            b_spawn_warm)
                TEST_COMMAND="benchprobe spawnonly"
                SHELL_AUTOSTART_CMD_DEFAULT=""
                ;;
            c_futex)
                TEST_COMMAND="futexprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="futexprobe"
                ;;
            c_futex_race)
                TEST_COMMAND="futexrace"
                SHELL_AUTOSTART_CMD_DEFAULT="futexrace"
                ;;
            m6_ipc_compact)
                TEST_COMMAND="repeat 8 hello"
                ;;
            m6_ipc_rendezvous)
                TEST_COMMAND="repeat 8 hello"
                ;;
            m6_ring_io)
                TEST_COMMAND="echo ringio-marker"
                SHELL_AUTOSTART_CMD_DEFAULT="ringio"
                ;;
            p1_setjmp)
                TEST_COMMAND="setjmpprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="setjmpprobe"
                ;;
            p1_env)
                TEST_COMMAND="envprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="envprobe"
                ;;
            p1_stubs)
                TEST_COMMAND="stubsprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="stubsprobe"
                ;;
            p2_pipe)
                TEST_COMMAND="pipeprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="pipeprobe"
                ;;
            p2_spawn_pipe)
                TEST_COMMAND="spawnpipeprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="spawnpipeprobe"
                ;;
            p3_tls)
                TEST_COMMAND="tlsprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="tlsprobe"
                ;;
            p3_pthread)
                TEST_COMMAND="pthreadprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="pthreadprobe"
                ;;
            p4_dev)
                TEST_COMMAND="devprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="devprobe"
                ;;
            p4_framebuf)
                TEST_COMMAND="fbprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="fbprobe"
                ;;
            b_console_blit)
                TEST_COMMAND="console_blit_bench"
                SHELL_AUTOSTART_CMD_DEFAULT="console_blit_bench"
                ;;
            l2_devfb0)
                TEST_COMMAND="devfb0_probe"
                SHELL_AUTOSTART_CMD_DEFAULT="devfb0_probe"
                ;;
            p4_mmap)
                TEST_COMMAND="mmapprobe"
                SHELL_AUTOSTART_CMD_DEFAULT="mmapprobe"
                ;;
            l2_pipe_builtin)
                TEST_COMMAND=""
                # Phase 4 Plan B Stage 0: verify builtin | container works.
                # echo is an in-process builtin; cat is a container. The
                # builtin writes via PIPE_DATA_LABEL; cat reads and echoes.
                SHELL_AUTOSTART_CMD_DEFAULT="echo hello | cat"
                ;;
            l2_pipe_builtin_chain)
                TEST_COMMAND=""
                # Phase 4 Plan B Stage 0: verify builtin | container with
                # transformation. echo feeds tr which uppercases.
                SHELL_AUTOSTART_CMD_DEFAULT="echo abc | tr a-z A-Z"
                ;;
            l2_pipe_builtin_3stage)
                TEST_COMMAND=""
                # Phase 4 Plan B Stage 0: 3-stage pipeline where the first
                # stage is a shell builtin (echo) and stages 2-3 are
                # containers (cat|cat). Verifies builtin→pipe→container→pipe
                # chain: WriteSink::Pipe → first cat → second cat → TTY.
                SHELL_AUTOSTART_CMD_DEFAULT="echo hello | cat | cat"
                ;;
            l2_pipe_3stage)
                TEST_COMMAND=""
                # Phase 4 Plan E diagnostic: 3-stage cat|grep|head with
                # synthetic input. Writes 5 lines to /tmp/in.txt, then
                # pipelines cat → grep alpha → head -1. Expects "alpha"
                # and EXIT=0 on COM2. Distinct from l2_pipe_three (which
                # uses /etc/motd) to anchor on predictable synthetic data.
                SHELL_AUTOSTART_CMD_DEFAULT="echo -e 'alpha\nbeta\ngamma\nalpha\ndelta' > /tmp/in.txt; cat /tmp/in.txt | grep alpha | head -1; echo EXIT=\$?"
                ;;
            l2_pipe_basic)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="cat /etc/motd | head -3"
                ;;
            l2_pipe_env)
                TEST_COMMAND=""
                # Phase 4 Plan E Stage 2: verify env propagates into pipe
                # stages. echo is a shell builtin so it expands $PIPETEST
                # before spawn; wc -c is the spawned binary that inherits
                # the env from the pipeline spawn. "hello\n" = 6 bytes.
                # printenv not yet shipped (Plan B), so we exercise the
                # spawn-env path indirectly via wc character count.
                # Env propagation fix builds clean; targeted getenv-reading
                # test deferred to Plan B when printenv is available.
                SHELL_AUTOSTART_CMD_DEFAULT="export PIPETEST=hello; echo \$PIPETEST | wc -c"
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
                TEST_COMMAND="_shellcrash"
                ;;
            hr7_su_equal)
                TEST_COMMAND="sutest equal"
                SHELL_AUTOSTART_CMD_DEFAULT=""
                ;;
            m5_fairness) TEST_COMMAND="repeat 8 hello" ;;
            l2_cat_basic)
                TEST_COMMAND=""
                # GNU-close cat: -n numbers all output lines.
                # Uses /etc/motd as a stable file. Verifies flag parsing
                # and the debug marker on exit.
                SHELL_AUTOSTART_CMD_DEFAULT="cat -n /etc/motd"
                ;;
            l2_cp_recursive)
                TEST_COMMAND=""
                # cp -r: copy /etc to /tmp/etccopy, then ls the copy.
                # Verifies recursive directory copy via libcluu::cli.
                SHELL_AUTOSTART_CMD_DEFAULT="cp -r /etc /tmp/etccopy"
                ;;
            l2_head_bytes)
                TEST_COMMAND=""
                # head -c N: print first N bytes from /etc/motd.
                # Verifies -c flag and RequiredArg parsing via libcluu::cli.
                SHELL_AUTOSTART_CMD_DEFAULT="head -c 20 /etc/motd"
                ;;
            l2_wc_lines)
                TEST_COMMAND=""
                # wc -l: count newlines in /etc/motd.
                # Verifies -l flag and single-column output via libcluu::cli.
                SHELL_AUTOSTART_CMD_DEFAULT="wc -l /etc/motd"
                ;;
            l2_grep_recursive)
                TEST_COMMAND=""
                # grep -rn: recursive search for 'CLUU' under /etc.
                # Verifies -r and -n flags via libcluu::cli.
                SHELL_AUTOSTART_CMD_DEFAULT="grep -rn CLUU /etc"
                ;;
            l2_basename_basic)
                TEST_COMMAND=""
                # basename: strip directory from path.
                SHELL_AUTOSTART_CMD_DEFAULT="basename /etc/users.toml"
                EXPECTED_CONTAINS=("users.toml")
                ;;
            l2_dirname_basic)
                TEST_COMMAND=""
                # dirname: strip last component from path.
                SHELL_AUTOSTART_CMD_DEFAULT="dirname /etc/users.toml"
                EXPECTED_CONTAINS=("/etc")
                ;;
            l2_sleep_basic)
                TEST_COMMAND=""
                # sleep: delay then print done.
                SHELL_AUTOSTART_CMD_DEFAULT="sleep 1; echo done"
                EXPECTED_CONTAINS=("done")
                ;;
            l2_which_basic)
                TEST_COMMAND=""
                # which: find self in PATH. Each container's view maps
                # /bin → /var/images/<self>/bin, so `which <other>` won't
                # find binaries that don't ship with the which container.
                # `which which` always works because /bin/which is the
                # binary the container is running from.
                SHELL_AUTOSTART_CMD_DEFAULT="which which"
                EXPECTED_CONTAINS=("/bin/which")
                ;;
            l2_printf_basic)
                TEST_COMMAND=""
                # printf: format string substitution.
                SHELL_AUTOSTART_CMD_DEFAULT="printf '%s=%d\n' foo 42"
                EXPECTED_CONTAINS=("foo=42")
                ;;
            l2_date_basic)
                TEST_COMMAND=""
                # date: print current date — just check year "20xx" appears.
                SHELL_AUTOSTART_CMD_DEFAULT="date"
                EXPECTED_CONTAINS=("20")
                ;;
            l2_env_basic)
                TEST_COMMAND=""
                # env: print environment — check at least one KEY=VALUE line.
                SHELL_AUTOSTART_CMD_DEFAULT="env | head -1"
                EXPECTED_CONTAINS=("=")
                ;;
            l2_kill_basic)
                TEST_COMMAND=""
                # kill --help: verify binary builds and parses --help.
                SHELL_AUTOSTART_CMD_DEFAULT="kill --help"
                EXPECTED_CONTAINS=("Usage")
                ;;
            l2_sort_basic)
                TEST_COMMAND=""
                # sort: sort three lines lexicographically.
                SHELL_AUTOSTART_CMD_DEFAULT="printf 'c\nb\na\n' > /tmp/s.in; sort /tmp/s.in"
                EXPECTED_CONTAINS=("a" "b" "c")
                ;;
            l2_uniq_basic)
                TEST_COMMAND=""
                # uniq -c: prefix each line with occurrence count.
                SHELL_AUTOSTART_CMD_DEFAULT="printf 'a\na\nb\n' > /tmp/u.in; uniq -c /tmp/u.in"
                EXPECTED_CONTAINS=("2 a" "1 b")
                ;;
            l2_cut_basic)
                TEST_COMMAND=""
                # cut -d: -f2: extract second colon-delimited field.
                SHELL_AUTOSTART_CMD_DEFAULT="printf 'a:b:c\n' | cut -d: -f2"
                EXPECTED_CONTAINS=("b")
                ;;
            l2_tr_basic)
                TEST_COMMAND=""
                # tr a-z A-Z: uppercase ASCII letters.
                SHELL_AUTOSTART_CMD_DEFAULT="printf 'abc\n' | tr a-z A-Z"
                EXPECTED_CONTAINS=("ABC")
                ;;
            l2_stat_basic)
                TEST_COMMAND=""
                # stat: display file metadata for a freshly-touched file.
                SHELL_AUTOSTART_CMD_DEFAULT="touch /tmp/sf; stat /tmp/sf"
                EXPECTED_CONTAINS=("File:" "sf" "Size:")
                ;;
            l2_du_basic)
                TEST_COMMAND=""
                # du -s: summarize disk usage for /etc.
                SHELL_AUTOSTART_CMD_DEFAULT="du -s /etc"
                EXPECTED_CONTAINS=("/etc")
                ;;
            l2_find_basic)
                TEST_COMMAND=""
                # find -name: locate files by glob pattern.
                SHELL_AUTOSTART_CMD_DEFAULT="mkdir -p /tmp/f; touch /tmp/f/a.txt; find /tmp/f -name '*.txt'"
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
                # After login, run printf with a red SGR escape.  The harness
                # types the command after the shell prompt is ready.
                # printf '\033[31mred\033[0m'
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 5\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 2\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 3\nsendkey p\nsendkey r\nsendkey i\nsendkey n\nsendkey t\nsendkey f\nsendkey spc\nsendkey apostrophe\nsendkey backslash\nsendkey 0\nsendkey 3\nsendkey 3\nsendkey bracket_left\nsendkey 3\nsendkey 1\nsendkey m\nsendkey r\nsendkey e\nsendkey d\nsendkey backslash\nsendkey 0\nsendkey 3\nsendkey 3\nsendkey bracket_left\nsendkey 0\nsendkey m\nsendkey apostrophe\nsendkey ret'
                ;;
            l2_cluuterm_keymap)
                TEST_COMMAND=""
                # After login, press Up arrow.  The compositor forwards the
                # extended key to cluuterm which logs the CSI sequence.
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 5\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 2\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 3\nsendkey up'
                ;;
            l2_cluuterm_exit)
                TEST_COMMAND=""
                # After login, type `exit` to close the shell.
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 5\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 2\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 3\nsendkey e\nsendkey x\nsendkey i\nsendkey t\nsendkey ret'
                ;;
            l2_cluuterm_two_windows)
                TEST_COMMAND=""
                # Press Ctrl+Alt+N to ask compositor to spawn a second cluuterm.
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 3\nsendkey ctrl-alt-n'
                ;;
            l2_cluuterm_raw_mode)
                TEST_COMMAND=""
                # MicroPython calls tcsetattr(raw) for its REPL on stdin.
                # This reaches the legacy tty's LineDiscipline via TTY_CTL_LABEL,
                # which calls set_mode() and emits the raw-mode marker.
                # Use `mp -c ...` so the process exits and the shell can
                # observe line_discipline: mode=canonical on restore, but we
                # only require the initial raw-mode switch.
                SHELL_AUTOSTART_CMD_DEFAULT="micropython"
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
                # Two compdemos autostart (second entry in autostart.toml); the
                # harness injects Alt+Tab via SENDKEY_SEQUENCE_DEFAULT to trigger
                # focus_next and emit "compositor: focus -> ".
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 3\nsendkey alt-tab'
                ;;
            l2_compositor_destroy)
                TEST_COMMAND=""
                # compositor + compdemo autostart; harness injects Ctrl+Alt+N
                # (spawn second) then Ctrl+Alt+Q (close-request → WIN_DESTROY).
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 3\nsendkey ctrl-alt-q'
                ;;
            l2_compositor_legacy_vt)
                TEST_COMMAND=""
                # Switch to compositor VT (Ctrl+Alt+F5), then back (Ctrl+Alt+F1).
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 3\nsendkey ctrl-alt-f5\nsleep 3\nsendkey ctrl-alt-f1'
                ;;
            b_compositor_blit)
                TEST_COMMAND=""
                # Switch to compositor VT so tick_frame runs; bench fires after 100 frames.
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 3\nsendkey ctrl-alt-f5'
                ;;
            l2_timeserver_pushmode_tick)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="timetick_probe"
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
                SHELL_AUTOSTART_CMD_DEFAULT="minimal"
                ;;
            *) TEST_COMMAND="hello" ;;
        esac
    fi
}
