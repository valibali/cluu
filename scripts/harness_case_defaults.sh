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
