"""Per-case defaults: TEST_COMMAND, SENDKEY_SEQUENCE, RUN_WAIT, etc.

Ports ``scripts/harness_case_defaults.sh``. The bash version is a giant
case statement over ``$MARKER_MODE``; this module exposes the same data
as a dict so new cases are declarative.

Knowledge-vault conventions applied here:

* ``cluu-sendkey-nowait-for-login-cases`` — every case that injects
  credentials sets ``sendkey_sequence_nowait=True``.
* ``cluu-harness-sendkey-sleep-must-match-boot`` — the credential
  sequence sleeps 12s before keys, not 5s, so kbd IRQ handler is
  attached when keys arrive.
"""

from __future__ import annotations

from dataclasses import dataclass, field


@dataclass
class CaseDefaults:
    """Derived defaults for one MARKER_MODE."""

    test_command: str | None = None
    post_sendkey: str | None = None
    sendkey_sequence: list[str] = field(default_factory=list)
    sendkey_sequence_nowait: bool = False
    run_wait_s: int | None = None
    # Extra keystroke commands to inject (KEYSTROKE_COMMANDS in bash).
    keystroke_commands: list[str] = field(default_factory=list)
    # Marker to wait for before firing sendkey_sequence (event-driven login).
    # If set, replaces blind "sleep N" prefix in the sequence.
    pre_sendkey_wait_marker: str | None = None


# Standard root/root credentials sendkey sequence. Per
# cluu-harness-sendkey-sleep-must-match-boot — the prefix sleep must
# exceed the login-window-ready time. Boot with full userdisk (150+
# containers, 7 MB micropython binary) reaches login at ~16-22s; 25s
# gives headroom for slow boots.
_CREDS_SENDKEY_ROOT: list[str] = [
    "sleep 25",
    "sendkey r", "sendkey o", "sendkey o", "sendkey t", "sendkey ret",
    "sleep 2",
    "sendkey r", "sendkey o", "sendkey o", "sendkey t", "sendkey ret",
]


def _creds() -> list[str]:
    """Return a fresh copy of the credential sequence (mutable)."""
    return list(_CREDS_SENDKEY_ROOT)


def _creds_slow() -> list[str]:
    """Credentials with longer sleep for virtio-snd boot (~30s to login)."""
    return [
        "sleep 40",
        "sendkey r", "sendkey o", "sendkey o", "sendkey t", "sendkey ret",
        "sleep 2",
        "sendkey r", "sendkey o", "sendkey o", "sendkey t", "sendkey ret",
    ]


def _creds_no_sleep() -> list[str]:
    """Credentials without the blind sleep prefix (for event-driven cases)."""
    return list(_CREDS_NO_SLEEP)


_CREDS_NO_SLEEP: list[str] = [
    "sleep 2",
    "sendkey r", "sendkey o", "sendkey o", "sendkey t", "sendkey ret",
    "sleep 2",
    "sendkey r", "sendkey o", "sendkey o", "sendkey t", "sendkey ret",
]


def _type_command(cmd: str) -> list[str]:
    """Translate a command string to sendkey entries (with ret)."""
    from cluu_harness.sendkey import command_to_sendkeys

    return [f"sendkey {k}" for k in command_to_sendkeys(cmd)]


def _dprint_seq(marker: str) -> list[str]:
    """Sendkey sequence to type ``dprint <marker>`` + ret.

    ``dprint`` is a shell builtin that writes its args to debug_print
    (COM2 serial), so the marker appears in the serial log. Markers
    use only lowercase letters and underscores — no HU QWERTZ traps.
    """
    return _type_command(f"dprint {marker}")


def _build_cluuterm_flood(n: int) -> list[str]:
    """Generate sendkey sequence to spawn n cluuterms via Ctrl+Alt+N hotkey.

    Each spawn needs >500ms gap to pass the compositor debounce.
    2s gap ensures QEMU sendkey reliably delivers each event.
    """
    seq: list[str] = ["sleep 3"]
    for _ in range(n):
        seq += ["sendkey ctrl-alt-n", "sleep 2"]
    return seq


# MARKER_MODE → defaults. Keep alphabetical for grep-ability.
# When adding an entry, set sendkey_sequence_nowait=True if the case
# injects credentials (login modal spawns before any shell — see
# cluu-sendkey-nowait-for-login-cases).
_DEFAULTS: dict[str, CaseDefaults] = {
    "legacy_p1": CaseDefaults(
        test_command="minimal",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "m1_recv": CaseDefaults(test_command="hello"),
    "m3_mapfail": CaseDefaults(test_command="mapfail 12 4"),
    "m3_mapcopyfail": CaseDefaults(test_command="mapcopyfail 4"),
    "m3_maperror": CaseDefaults(test_command="maperror 3"),
    "m4_sender_auth": CaseDefaults(test_command="hello"),
    "m4_deny_paths": CaseDefaults(
        test_command="killdeny 2 9",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "m5_fairness": CaseDefaults(test_command="repeat 8 hello"),
    "s_stress_churn": CaseDefaults(
        test_command="cpuburn mixed 200",
        keystroke_commands=["cpuburn cpu 50 &"],
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=120,
    ),
    "c_futex": CaseDefaults(
        test_command="futexprobe",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "c_futex_race": CaseDefaults(
        test_command="futexrace",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "l2_ext2write": CaseDefaults(
        test_command="ext2io write",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "l2_ext2unlink": CaseDefaults(
        test_command="ext2io unlink",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "l2_cd": CaseDefaults(
        test_command="cd /; cd etc; pwd",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "l2_ls": CaseDefaults(
        test_command="ls /etc",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "l2_mkdir": CaseDefaults(
        test_command="mkdir /tmp/a; mkdir -p /tmp/b/c/d",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "l2_login": CaseDefaults(
        test_command=None,
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "l2_cluuterm_login": CaseDefaults(
        test_command=None,
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "l2_cluuterm_exit": CaseDefaults(
        test_command=None,
        sendkey_sequence=_creds()
        + ["sleep 3", "sendkey e", "sendkey x", "sendkey i", "sendkey t", "sendkey ret"],
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "l2_vt4_default": CaseDefaults(
        test_command=None,
        # Pure boot-time marker — no keyboard input needed.
    ),
    "l2_dev_nodes": CaseDefaults(
        test_command="ls /dev",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "l2_poll_pipes": CaseDefaults(
        test_command="pollprobe",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "l2_soak_test": CaseDefaults(
        test_command=None,
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "errnoprobe": CaseDefaults(
        test_command="errnoprobe",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "stackprobe": CaseDefaults(
        test_command="stackprobe",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "dtachprobe": CaseDefaults(
        test_command="dtachprobe",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "mmapprobe": CaseDefaults(
        test_command="mmapprobe",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "gc_stress": CaseDefaults(
        test_command="micropython /etc/gc_stress.py",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=90,
    ),
    "acpiprobe": CaseDefaults(
        test_command="acpiprobe",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "xhciprobe": CaseDefaults(
        test_command="xhciprobe",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=60,
    ),
    "usb_input_probe": CaseDefaults(
        test_command="",  # primordial service, no shell command needed
        sendkey_sequence="",
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "dynprobe": CaseDefaults(
        test_command="dynprobe",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "l2_color_256": CaseDefaults(
        test_command="micropython /etc/color_256.py",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=120,
    ),
    "l2_attr_render": CaseDefaults(
        test_command="micropython /etc/attr_render.py",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=120,
    ),
    "l2_alt_screen": CaseDefaults(
        test_command="micropython /etc/alt_screen.py",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "l2_net_boot": CaseDefaults(
        test_command=None,
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "l2_socket_basic": CaseDefaults(
        test_command="l2_socket_basic",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=60,
    ),
    "l2_net_denied": CaseDefaults(
        test_command="l2_net_denied",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
    ),
    "l2_dhcp_ping": CaseDefaults(
        test_command="ping 10.0.2.2",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=90,
    ),
    "l2_dns_basic": CaseDefaults(
        test_command="l2_dns_basic",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=60,
    ),
    "l2_wget_basic": CaseDefaults(
        test_command="wget http://10.0.2.2:9876/",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=90,
    ),
    "l2_curl_basic": CaseDefaults(
        test_command="curl http://10.0.2.2:9876/",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=90,
    ),
    "l2_curl_badurl_survive": CaseDefaults(
        test_command=None,
        sendkey_sequence=_creds()
        + _build_cluuterm_flood(5)
        + _type_command("curl 10.0.2.2:9876")
        + ["sleep 5"]
        + _type_command("dprint SHELL_ALIVE"),
        sendkey_sequence_nowait=True,
        run_wait_s=150,
    ),
    "l2_libtui_demo": CaseDefaults(
        test_command="libtui-demo",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=60,
    ),
    "l2_edit_cluuterm": CaseDefaults(
        test_command="edit /tmp/test.txt",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=30,
    ),
    "l2_edit_libtui": CaseDefaults(
        test_command=None,
        sendkey_sequence=_creds()
        + [
            "sleep 3",
            "sendkey e", "sendkey d", "sendkey i", "sendkey t",
            "sendkey spc",
            "sendkey t", "sendkey e", "sendkey s", "sendkey t",
            "sendkey dot", "sendkey t", "sendkey x", "sendkey t",
            "sendkey ret",
            "sleep 3",
            "sendkey i",
            "sendkey h", "sendkey e", "sendkey l", "sendkey l", "sendkey o",
            "sendkey esc",
            "sendkey shift-dot", "sendkey w", "sendkey ret",
            "sendkey shift-dot", "sendkey q", "sendkey ret",
        ],
        sendkey_sequence_nowait=True,
        run_wait_s=60,
    ),
    "l2_fm_basic": CaseDefaults(
        test_command="fm",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=30,
    ),
    "l2_pager_basic": CaseDefaults(
        test_command="pager /etc/welcome.txt",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=30,
    ),
    "l2_hexdump_basic": CaseDefaults(
        test_command="hexdump /etc/welcome.txt",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=30,
    ),
    "l2_calc_basic": CaseDefaults(
        test_command="calc 2+3*4",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=30,
    ),
    "l2_diff_basic": CaseDefaults(
        test_command="diff /etc/welcome.txt /etc/motd",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=30,
    ),
    "l2_irc_basic": CaseDefaults(
        test_command="irc",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=30,
    ),
    "l2_httpd_basic": CaseDefaults(
        test_command="httpd",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=30,
    ),
    "l2_ntp_basic": CaseDefaults(
        test_command="ntp",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=30,
    ),
    "l2_git_basic": CaseDefaults(
        test_command="git init",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=30,
    ),
    "l2_sed_basic": CaseDefaults(
        test_command="sed 's/hello/world/' /etc/welcome.txt",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=30,
    ),
    "l2_awk_basic": CaseDefaults(
        test_command="awk '{print $1}' /etc/welcome.txt",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=30,
    ),
    "l2_make_basic": CaseDefaults(
        test_command="make",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=30,
    ),
    "l2_mail_basic": CaseDefaults(
        test_command="mail",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=30,
    ),
    "l2_feed_basic": CaseDefaults(
        test_command="feed",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=30,
    ),
    "l2_notes_basic": CaseDefaults(
        test_command="notes",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=30,
    ),
    "l2_glow_basic": CaseDefaults(
        test_command="glow /etc/welcome.txt",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=30,
    ),
    "l2_sysmon_basic": CaseDefaults(
        test_command="sysmon",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=30,
    ),
    "l2_top": CaseDefaults(
        test_command="top",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=30,
    ),
    "l2_pkg_basic": CaseDefaults(
        test_command="pkg list",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=30,
    ),
    "l2_mp_spike": CaseDefaults(
        test_command="micropython /etc/mp_spike.py",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=60,
    ),
    "l2_mp_no_vfs": CaseDefaults(
        test_command="micropython /etc/mp_spike.py",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=60,
    ),
    "l2_plugin_api": CaseDefaults(
        test_command=None,
        sendkey_sequence=_creds()
        + [
            "sleep 3",
            "sendkey e", "sendkey d", "sendkey i", "sendkey t",
            "sendkey spc",
            "sendkey t", "sendkey e", "sendkey s", "sendkey t",
            "sendkey dot", "sendkey t", "sendkey x", "sendkey t",
            "sendkey ret",
            "sleep 5",
            "sendkey ctrl-b",
            "sleep 2",
            "sendkey shift-dot",
            "sendkey h", "sendkey e", "sendkey l", "sendkey l", "sendkey o",
            "sendkey ret",
            "sleep 2",
            "sendkey shift-dot", "sendkey q", "sendkey ret",
        ],
        sendkey_sequence_nowait=True,
        run_wait_s=60,
    ),
    "none": CaseDefaults(test_command="hello"),
    "l2_audio_boot": CaseDefaults(
        test_command="",
        run_wait_s=45,
    ),
    "l2_audio_play": CaseDefaults(
        test_command="mp3player /host/winamp.mp3",
        sendkey_sequence=_creds_no_sleep(),
        sendkey_sequence_nowait=True,
        run_wait_s=120,
        pre_sendkey_wait_marker="login: window registered",
    ),
    "l2_cluuamp": CaseDefaults(
        test_command="cluuamp /host/winamp.mp3",
        sendkey_sequence=_creds_no_sleep(),
        sendkey_sequence_nowait=True,
        run_wait_s=90,
        pre_sendkey_wait_marker="login: window registered",
    ),
    "l2_blk_basic": CaseDefaults(
        test_command="blkprobe basic",
        sendkey_sequence=_creds_no_sleep(),
        sendkey_sequence_nowait=True,
        run_wait_s=30,
        pre_sendkey_wait_marker="login: window registered",
    ),
    "l2_blk_perf": CaseDefaults(
        test_command="blkprobe perf",
        sendkey_sequence=_creds_no_sleep(),
        sendkey_sequence_nowait=True,
        run_wait_s=60,
        pre_sendkey_wait_marker="login: window registered",
    ),
    "l2_blk_concurrent": CaseDefaults(
        test_command="blkprobe concurrent",
        sendkey_sequence=_creds_no_sleep(),
        sendkey_sequence_nowait=True,
        run_wait_s=30,
        pre_sendkey_wait_marker="login: window registered",
    ),
    "benchprobe": CaseDefaults(
        test_command="benchprobe spawnonly",
        sendkey_sequence=_creds_no_sleep(),
        sendkey_sequence_nowait=True,
        run_wait_s=60,
        pre_sendkey_wait_marker="login: window registered",
    ),
    "l2_doom": CaseDefaults(
        test_command="doom -iwad /host/freedoom1.wad",
        sendkey_sequence=_creds_slow(),
        sendkey_sequence_nowait=True,
        run_wait_s=90,
        pre_sendkey_wait_marker="login: window registered",
    ),
    "l2_baseline_idle_tui": CaseDefaults(
        test_command=None,
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=30,
        pre_sendkey_wait_marker="login: window registered",
    ),
    "l2_baseline_quiet_shell": CaseDefaults(
        test_command=None,
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=30,
        pre_sendkey_wait_marker="login: window registered",
    ),
    "l2_baseline_doom_windowed": CaseDefaults(
        test_command="doom -iwad /host/freedoom1.wad",
        sendkey_sequence=_creds_slow(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
        pre_sendkey_wait_marker="login: window registered",
    ),
    "l2_baseline_doom_fullscreen": CaseDefaults(
        test_command="doom -fullscreen -iwad /host/freedoom1.wad",
        sendkey_sequence=_creds_slow(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
        pre_sendkey_wait_marker="login: window registered",
    ),
    "l2_display_surface_isolation": CaseDefaults(
        test_command="dprint DISPLAY_SURFACE_ISOLATION_OK",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=60,
        pre_sendkey_wait_marker="login: window registered",
    ),
    "l2_display_root_control": CaseDefaults(
        test_command="dprint DISPLAY_ROOT_CONTROL_OK",
        keystroke_commands=["ps"],
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=60,
        pre_sendkey_wait_marker="login: window registered",
    ),
    "l2_display_buffer_lifecycle": CaseDefaults(
        test_command="dprint DISPLAY_BUFFER_LIFECYCLE_OK",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=60,
        pre_sendkey_wait_marker="login: window registered",
    ),
    "l2_displayd_failstop": CaseDefaults(
        test_command="dprint DISPLAYD_FAILSTOP_OK",
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=60,
        pre_sendkey_wait_marker="login: window registered",
    ),
    "l2_display_visual_parity": CaseDefaults(
        test_command=None,
        sendkey_sequence=_creds(),
        sendkey_sequence_nowait=True,
        run_wait_s=45,
        pre_sendkey_wait_marker="login: window registered",
    ),
}


def get_defaults(marker_mode: str) -> CaseDefaults:
    """Return the defaults for a MARKER_MODE.

    Unknown modes fall back to ``test_command="hello"`` (matching the
    bash harness's ``*) TEST_COMMAND="hello"`` wildcard).
    """
    d = _DEFAULTS.get(marker_mode)
    if d is not None:
        return d
    return CaseDefaults(test_command="hello")


__all__ = ["CaseDefaults", "get_defaults"]
