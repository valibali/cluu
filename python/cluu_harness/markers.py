"""MARKER_MODE → required-markers table.

Ported from the giant ``case "$MARKER_MODE" in`` block in
``scripts/harness_run.sh``. The bash version has ~120 modes; this
module ships a representative subset covering every category documented
in ``doc/book/testing.md`` plus the per-mode post-checks (FAIL-marker
detection, metric gating). New modes are added by extending
:data:`MARKER_MODES` — no shell edits required.
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass, field


@dataclass
class MarkerModeSpec:
    """One MARKER_MODE entry."""

    name: str
    required_markers: list[str]
    # Optional regex whose presence means the probe reported failure
    # (e.g. ``mapfail: FAIL``). Empty = no extra fail-marker check.
    fail_marker: str = ""
    # Category — drives per-mode post-checks (see metrics.py).
    # One of: boot, recv, leak, fairness, ipc, warm_spawn, bench, generic.
    category: str = "generic"
    # Docstring shown by the CLI ``--list-modes``.
    description: str = ""


# The "TSC calibrated" + "[USER] shell: ready" prefix is so common it's
# worth a helper. Many modes add probe-specific markers on top.
_TSC = "TSC calibrated"
_SHELL_READY = "[USER] shell: ready"


def _boot(markers: list[str], **kw: object) -> MarkerModeSpec:
    return MarkerModeSpec(
        name="", required_markers=[_TSC, *_SHELL(markers)], **kw  # type: ignore[arg-type]
    )


def _SHELL(markers: list[str]) -> list[str]:
    """Prepend the shell-ready marker if not already present."""
    if _SHELL_READY in markers:
        return list(markers)
    return [_SHELL_READY, *markers]


# Representative subset — covers every category from doc/book/testing.md.
# To add a new mode: append a MarkerModeSpec here. No other file changes.
MARKER_MODES: dict[str, MarkerModeSpec] = {
    m.name: m
    for m in [
        # ---- boot / shell-only ---------------------------------------
        MarkerModeSpec(
            name="none",
            required_markers=[],
            description="no required marker checks",
        ),
        MarkerModeSpec(
            name="legacy_p1",
            required_markers=[
                _TSC,
                "=== P1 POSIX stubs test ===",
                "[OK] nanosleep(100ms) returned 0",
                "[OK] usleep(50ms) returned 0",
                "=== P1 POSIX stubs test PASSED ===",
            ],
            description="original timing/TSC fixture checks",
        ),
        MarkerModeSpec(
            name="m0_boot",
            required_markers=[_TSC],
            description="bootstrap telemetry/manifest checks",
        ),
        # ---- recv / churn --------------------------------------------
        MarkerModeSpec(
            name="m1_recv",
            required_markers=[_TSC, _SHELL_READY, "procmgr: exit cookie"],
            category="recv",
            description="recv/wakeup churn checks",
        ),
        # ---- token audit / leak --------------------------------------
        MarkerModeSpec(
            name="m2_token_audit",
            required_markers=[
                _TSC,
                _SHELL_READY,
                "procmgr: exit cookie",
                "token_audit_next_seq=",
                "token_audit_stored=",
                "token_audit_dropped=",
            ],
            category="token_audit",
            description="recv churn + token audit telemetry invariants",
        ),
        MarkerModeSpec(
            name="m2_leakdiag",
            required_markers=[
                _TSC,
                _SHELL_READY,
                "procmgr: exit cookie",
                "resource delta:",
                "delta_spaces=",
                "delta_tokens=",
                "delta_pmm_used_frames=",
            ],
            category="leak",
            description="churn + resource delta diagnostics",
        ),
        # ---- mapfail failpoints --------------------------------------
        MarkerModeSpec(
            name="m3_mapfail",
            required_markers=[_TSC, _SHELL_READY, "mapfail: PASS"],
            fail_marker="mapfail: FAIL",
            description="kernel map-range failpoint rollback",
        ),
        MarkerModeSpec(
            name="m3_mapcopyfail",
            required_markers=[_TSC, _SHELL_READY, "mapcpfail: PASS"],
            fail_marker="mapcpfail: FAIL",
            description="copy_from_user failure branch rollback",
        ),
        MarkerModeSpec(
            name="m3_maperror",
            required_markers=[_TSC, _SHELL_READY, "maperror: PASS"],
            fail_marker="maperror: FAIL",
            description="map_user_page error branch rollback",
        ),
        # ---- sender auth ---------------------------------------------
        MarkerModeSpec(
            name="m4_sender_auth",
            required_markers=[
                _TSC,
                _SHELL_READY,
                "vfs: open ignoring claimed client_id=",
                "authenticated=",
            ],
            description="authenticated sender binding in VFS",
        ),
        # ---- fairness / IPC SLOs -------------------------------------
        MarkerModeSpec(
            name="m5_fairness",
            required_markers=[
                _TSC,
                _SHELL_READY,
                "procmgr: exit cookie",
                "resource delta:",
                "ipc_wait_p95_ms=",
                "ipc_wait_p99_ms=",
                "ipc_scan_avg_steps_x100=",
            ],
            category="fairness",
            description="mixed-load fairness/latency telemetry SLO checks",
        ),
        # ---- heavy load stress ---------------------------------------
        MarkerModeSpec(
            name="s_stress_churn",
            required_markers=[
                _TSC,
                _SHELL_READY,
                "cpuburn: PASS mode=mixed",
                "cpuburn: PASS mode=cpu",
            ],
            category="stress",
            description="heavy load: concurrent CPU burn + mixed CPU/IPC stress",
        ),
        # ---- futex ---------------------------------------------------
        MarkerModeSpec(
            name="c_futex",
            required_markers=[_TSC, _SHELL_READY, "futexprobe: PASS"],
            description="futex invoke wait/wake/timeout smoke",
        ),
        MarkerModeSpec(
            name="c_futex_race",
            required_markers=[_TSC, _SHELL_READY, "futexrace: PASS"],
            description="futex waiter/waker ordering with thread_create",
        ),
        # ---- ext2 ----------------------------------------------------
        MarkerModeSpec(
            name="l2_ext2write",
            required_markers=[
                _TSC, _SHELL_READY, "ext2write: PASS path=/home/root/ext2io_scratch"
            ],
            fail_marker="ext2write: FAIL",
            description="end-to-end ext2 write smoke",
        ),
        MarkerModeSpec(
            name="l2_ext2unlink",
            required_markers=[_TSC, _SHELL_READY, "ext2unlink: PASS create+unlink+verify"],
            fail_marker="ext2unlink: FAIL",
            description="create+unlink verification smoke",
        ),
        # ---- shell builtins ------------------------------------------
        MarkerModeSpec(
            name="l2_cd",
            required_markers=[_TSC, _SHELL_READY, "shell: pwd=/etc"],
            description="cd/pwd shell builtins",
        ),
        MarkerModeSpec(
            name="l2_ls",
            required_markers=[_TSC, _SHELL_READY, "ls: ok (exit 0)"],
            description="basic ls of /etc",
        ),
        MarkerModeSpec(
            name="l2_mkdir",
            required_markers=[
                _TSC, _SHELL_READY, "mkdir: ok /tmp/a", "mkdir: ok /tmp/b/c/d"
            ],
            description="mkdir + mkdir -p",
        ),
        MarkerModeSpec(
            name="l2_login",
            required_markers=[
                _TSC,
                "procmgr: SESSION_CREATE ok",
                "session-procmgr: started",
            ],
            description="interactive login → session-procmgr spawn",
        ),
        # ---- compositor / cluuterm -----------------------------------
        MarkerModeSpec(
            name="l2_cluuterm_login",
            required_markers=[
                _TSC,
                "cluuterm: /bin/shell spawned",
                "procmgr: SESSION_CREATE ok",
            ],
            description="inject credentials → procmgr SESSION_CREATE",
        ),
        MarkerModeSpec(
            name="l2_cluuterm_exit",
            required_markers=[
                _TSC,
                "cluuterm: /bin/shell spawned",
                "procmgr: SESSION_CREATE ok",
                "cluuterm: shutdown",
                "compositor: window destroyed",
            ],
            description="exit → cluuterm shutdown + compositor window destroyed",
        ),
        MarkerModeSpec(
            name="l2_vt4_default",
            required_markers=[
                _TSC,
                "compositor: pinned to VT4",
                "compositor: ready",
            ],
            description="boot → compositor pinned to VT4",
        ),
        MarkerModeSpec(
            name="l2_dev_nodes",
            required_markers=[_TSC, _SHELL_READY, "ls: ok (exit 0)"],
            description="ls /dev regression — dynamic /dev enumeration",
        ),
        MarkerModeSpec(
            name="l2_poll_pipes",
            required_markers=[_TSC, _SHELL_READY, "pollprobe: PASS"],
            fail_marker="pollprobe: FAIL",
            description="poll()/select() on pipes, TTYs, /dev pseudo-files",
        ),
        MarkerModeSpec(
            name="l2_soak_test",
            required_markers=[
                _TSC,
                _SHELL_READY,
            ],
            category="stress",
            description="soak smoke — shell boots after code changes (pipe test covered by l2_poll_pipes)",
        ),
        MarkerModeSpec(
            name="errnoprobe",
            required_markers=[_TSC, _SHELL_READY, "ERRNO_OK"],
            description="per-thread errno isolation — two threads, distinct errno",
        ),
        MarkerModeSpec(
            name="stackprobe",
            required_markers=[_TSC, _SHELL_READY, "STACK_OK"],
            description="pthread_attr_setstacksize honored — 256 KiB stack",
        ),
        MarkerModeSpec(
            name="dtachprobe",
            required_markers=[_TSC, _SHELL_READY, "DETACH_OK"],
            description="detached thread stack reclamation — 50 detach cycles",
        ),
        MarkerModeSpec(
            name="mmapprobe",
            required_markers=[_TSC, _SHELL_READY, "mmapprobe: PASS complete"],
            description="mmap + mprotect including PROT_NONE",
        ),
        MarkerModeSpec(
            name="gc_stress",
            required_markers=[_TSC, _SHELL_READY, "C3_GC_OTHERS_OK"],
            description="MicroPython cross-thread GC stack scanning",
        ),
        MarkerModeSpec(
            name="acpiprobe",
            required_markers=[_TSC, _SHELL_READY, "ACPI_TABLES_OK"],
            description="ACPI RSDP discovery + FADT parsing on real QEMU",
        ),
        MarkerModeSpec(
            name="xhciprobe",
            required_markers=[_TSC, _SHELL_READY, "XHCI_PROBE_OK"],
            description="xHCI PCI discovery + controller reset + slot enable",
        ),
        MarkerModeSpec(
            name="usb_input_probe",
            required_markers=[_TSC, "USB_INPUT_OK"],
            description="usb-input primordial service boots + xHCI init",
        ),
        MarkerModeSpec(
            name="dynprobe",
            required_markers=[_TSC, _SHELL_READY, "DYNPROBE_OK"],
            description="Dynamic linking probe: ld-cluu reloc + TLS + Dynamic parsing",
        ),
        MarkerModeSpec(
            name="l2_color_256",
            required_markers=[_TSC, _SHELL_READY, "COLOR_256_OK"],
            description="256-color SGR parsing (CSI 38;5;N / 48;5;N)",
        ),
        MarkerModeSpec(
            name="l2_attr_render",
            required_markers=[_TSC, _SHELL_READY, "ATTR_RENDER_OK"],
            description="underline/reverse SGR parsing (CSI 4/24/7/27)",
        ),
        MarkerModeSpec(
            name="l2_alt_screen",
            required_markers=[_TSC, _SHELL_READY, "ALTSCREEN_OK"],
            description="alt-screen buffer enter/exit (CSI ?1049h/l)",
        ),
        MarkerModeSpec(
            name="l2_net_boot",
            required_markers=[
                _TSC,
                "virtio-blk: IRQ attached",
                "virtio-core/pci: found 1af4",
                "netd: started",
                "netd: smoltcp interface initialized",
                "netd: DHCP acquired IP 10.0.2.15",
                _SHELL_READY,
            ],
            description="boot with virtio-net-pci NIC present (CLUU_NET=1)",
        ),
        MarkerModeSpec(
            name="l2_socket_basic",
            required_markers=[_TSC, _SHELL_READY, "l2_socket_basic: PASS"],
            fail_marker="l2_socket_basic: FAIL",
            description="BSD socket API loopback echo test",
        ),
        MarkerModeSpec(
            name="l2_net_denied",
            required_markers=[_TSC, _SHELL_READY, "NET_CAP_NEGATIVE_OK"],
            fail_marker="NET_CAP_NEGATIVE_FAIL",
            description="negative test: container without NET profile cannot reach netd",
        ),
        MarkerModeSpec(
            name="l2_dhcp_ping",
            required_markers=[
                _TSC,
                _SHELL_READY,
                "netd: DHCP acquired IP 10.0.2.15",
                "PING_OK",
            ],
            description="DHCP acquisition + ICMP echo to QEMU gateway (10.0.2.2)",
        ),
        MarkerModeSpec(
            name="l2_dns_basic",
            required_markers=[_TSC, _SHELL_READY, "DNS_OK"],
            fail_marker="DNS_FAIL",
            description="DNS resolution via QEMU SLIRP DNS forwarder",
        ),
        MarkerModeSpec(
            name="l2_wget_basic",
            required_markers=[_TSC, _SHELL_READY, "WGET_OK"],
            fail_marker="WGET_FAIL",
            description="wget HTTP GET to host-side HTTP server via 10.0.2.2",
        ),
        MarkerModeSpec(
            name="l2_curl_basic",
            required_markers=[_TSC, _SHELL_READY, "CURL_OK"],
            fail_marker="CURL_FAIL",
            description="curl HTTP GET to host-side HTTP server via 10.0.2.2",
        ),
        MarkerModeSpec(
            name="l2_curl_badurl_survive",
            required_markers=[_TSC, _SHELL_READY, "SHELL_ALIVE"],
            description="curl with bad URL exits cleanly, shell survives",
        ),
        MarkerModeSpec(
            name="l2_libtui_demo",
            required_markers=[_TSC, _SHELL_READY, "LIBTUI_DEMO_OK"],
            fail_marker=None,
            description="libtui demo renders and exits on q",
        ),
        MarkerModeSpec(
            name="l2_edit_cluuterm",
            required_markers=[_TSC, _SHELL_READY, "EDIT_STARTING"],
            fail_marker="edit: fatal",
            description="edit starts under cluuterm PTS raw mode",
        ),
        MarkerModeSpec(
            name="l2_edit_libtui",
            required_markers=[_TSC, _SHELL_READY, "EDIT_LIBTUI_OK", "EDIT_RESIZE_OK"],
            fail_marker="edit: fatal",
            description="edit via libtui Program event loop + diff renderer",
        ),
        MarkerModeSpec(
            name="l2_fm_basic",
            required_markers=[_TSC, _SHELL_READY, "FM_OK"],
            fail_marker=None,
            description="file manager browses VFS",
        ),
        MarkerModeSpec(
            name="l2_pager_basic",
            required_markers=[_TSC, _SHELL_READY, "PAGER_OK"],
            fail_marker=None,
            description="pager scrolls a file",
        ),
        MarkerModeSpec(
            name="l2_hexdump_basic",
            required_markers=[_TSC, _SHELL_READY, "HEXDUMP_OK"],
            fail_marker=None,
            description="hex viewer shows hex+ASCII",
        ),
        MarkerModeSpec(
            name="l2_calc_basic",
            required_markers=[_TSC, _SHELL_READY, "CALC_OK"],
            fail_marker=None,
            description="calculator evaluates expressions",
        ),
        MarkerModeSpec(
            name="l2_diff_basic",
            required_markers=[_TSC, _SHELL_READY, "DIFF_OK"],
            fail_marker=None,
            description="diff viewer shows differences",
        ),
        MarkerModeSpec(
            name="l2_irc_basic",
            required_markers=[_TSC, _SHELL_READY, "IRC_CONNECT_OK"],
            fail_marker="IRC_FAIL",
            description="IRC client connects to server",
        ),
        MarkerModeSpec(
            name="l2_httpd_basic",
            required_markers=[_TSC, _SHELL_READY, "HTTPD_LISTENING"],
            fail_marker="HTTPD_FAIL",
            description="HTTP server listens on port 8080",
        ),
        MarkerModeSpec(
            name="l2_ntp_basic",
            required_markers=[_TSC, _SHELL_READY, "NTP_TIME_OK"],
            fail_marker="NTP_FAIL",
            description="NTP client queries time",
        ),
        MarkerModeSpec(
            name="l2_git_basic",
            required_markers=[_TSC, _SHELL_READY, "GIT_OK"],
            fail_marker=None,
            description="git init/add/commit/log",
        ),
        MarkerModeSpec(
            name="l2_sed_basic",
            required_markers=[_TSC, _SHELL_READY, "SED_OK"],
            fail_marker=None,
            description="stream editor substitute command",
        ),
        MarkerModeSpec(
            name="l2_awk_basic",
            required_markers=[_TSC, _SHELL_READY, "AWK_OK"],
            fail_marker=None,
            description="text processor pattern-action",
        ),
        MarkerModeSpec(
            name="l2_make_basic",
            required_markers=[_TSC, _SHELL_READY, "MAKE_OK"],
            fail_marker=None,
            description="build tool executes Makefile rules",
        ),
        MarkerModeSpec(
            name="l2_mail_basic",
            required_markers=[_TSC, _SHELL_READY, "MAIL_CONNECT_OK"],
            fail_marker=None,
            description="IMAP client connects to server",
        ),
        MarkerModeSpec(
            name="l2_feed_basic",
            required_markers=[_TSC, _SHELL_READY, "FEED_OK"],
            fail_marker=None,
            description="RSS reader fetches + displays items",
        ),
        MarkerModeSpec(
            name="l2_notes_basic",
            required_markers=[_TSC, _SHELL_READY, "NOTES_OK"],
            fail_marker=None,
            description="notes lists + opens files",
        ),
        MarkerModeSpec(
            name="l2_glow_basic",
            required_markers=[_TSC, _SHELL_READY, "GLOW_OK"],
            fail_marker=None,
            description="markdown viewer renders",
        ),
        MarkerModeSpec(
            name="l2_sysmon_basic",
            required_markers=[_TSC, _SHELL_READY, "SYSMON_OK"],
            fail_marker=None,
            description="system monitor shows /proc stats",
        ),
        MarkerModeSpec(
            name="l2_top",
            required_markers=[_TSC, _SHELL_READY, "TOP_PROCS_OK"],
            fail_marker=None,
            description="top reads non-empty /proc/<pid>/stat process list",
        ),
        MarkerModeSpec(
            name="l2_pkg_basic",
            required_markers=[_TSC, _SHELL_READY, "PKG_OK"],
            fail_marker=None,
            description="package manager lists installed containers",
        ),
        MarkerModeSpec(
            name="l2_mp_spike",
            required_markers=[_TSC, _SHELL_READY, "MP_SPIKE_OK"],
            fail_marker=None,
            description="MicroPython feasibility spike — 100 cycles + heap stable",
        ),
        MarkerModeSpec(
            name="l2_mp_no_vfs",
            required_markers=[_TSC, _SHELL_READY, "MP_NO_VFS_OK"],
            fail_marker=None,
            description="negative test: edit-plugin cannot open files (no vfs)",
        ),
        MarkerModeSpec(
            name="l2_plugin_api",
            required_markers=[_TSC, _SHELL_READY, "PLUGIN_API_OK"],
            fail_marker=None,
            description="editor plugin API — keymap + command callbacks via MicroPython IPC",
        ),
        MarkerModeSpec(
            name="l2_audio_boot",
            required_markers=[
                _TSC,
                "VIRTIO_SND_PCI",
                "VIRTIO_SND_OK",
                "VIRTIO_SND_TX_OK",
            ],
            category="boot",
            description="virtio-snd driver boot + control/TX self-test",
        ),
        MarkerModeSpec(
            name="l2_audio_play",
            required_markers=[
                _TSC,
                _SHELL_READY,
                "VIRTIO_SND_PCI",
                "VIRTIO_SND_OK",
                "MP3PLAYER_OPEN",
                "MP3PLAYER_DONE",
            ],
            description="mp3player MP3 playback via virtio-snd",
        ),
        MarkerModeSpec(
            name="l2_cluuamp",
            required_markers=[
                _TSC,
                _SHELL_READY,
                "VIRTIO_SND_PCI",
                "VIRTIO_SND_OK",
                "CLUUAMP_STARTING",
            ],
            description="cluuamp TUI audio player startup via virtio-snd",
        ),
        MarkerModeSpec(
            name="l2_blk_basic",
            required_markers=[_TSC, _SHELL_READY, "blkprobe: ALL OK"],
            fail_marker="blkprobe: [FAIL]",
            description="single sector-0 read via BlkSession",
        ),
        MarkerModeSpec(
            name="l2_blk_perf",
            required_markers=[_TSC, _SHELL_READY, "blkprobe: ALL OK"],
            fail_marker="blkprobe: [FAIL]",
            description="64 MB sequential read, >=150 MB/s floor",
        ),
        MarkerModeSpec(
            name="l2_blk_concurrent",
            required_markers=[_TSC, _SHELL_READY, "blkprobe: ALL OK"],
            fail_marker="blkprobe: [FAIL]",
            description="4 sessions x 100 concurrent reads",
        ),
        MarkerModeSpec(
            name="benchprobe",
            required_markers=[_TSC, _SHELL_READY, "benchprobe: PASS"],
            fail_marker="benchprobe: FAIL",
            category="bench",
            description="spawn/ipc/thread cycle benchmark — avg cycles per noop spawn",
        ),
        MarkerModeSpec(
            name="l2_doom",
            required_markers=[
                _TSC,
                _SHELL_READY,
                "doom-cluu: DG_Init",
            ],
            fail_marker="doom-cluu: init failed",
            category="container",
            description="DOOM port: doomgeneric boots and initializes compositor window",
        ),
    ]
}


def get_spec(mode: str) -> MarkerModeSpec:
    """Look up a MARKER_MODE spec. Raises ``KeyError`` if unknown."""
    if mode not in MARKER_MODES:
        raise KeyError(
            f"unknown MARKER_MODE {mode!r}. Known modes: {sorted(MARKER_MODES)}"
        )
    return MARKER_MODES[mode]


def list_modes() -> list[str]:
    return sorted(MARKER_MODES)


# ---------------------------------------------------------------------- #
# Post-check registry: per-category verifiers run AFTER marker matching.
# Each returns (ok, message). See metrics.py for the implementations.
# ---------------------------------------------------------------------- #
PostCheck = Callable[["PostCheckContext"], tuple[bool, str]]


@dataclass
class PostCheckContext:
    """Inputs for a per-mode post-check."""

    serial_text: str
    spec: MarkerModeSpec
    min_exit_cookies: int = 3
    # SLO limits (None = skip). Mirrors HarnessConfig fields.
    limits: dict[str, int | None] = field(default_factory=dict)
    # Pre-parsed metric values, filled by metrics.py before post-checks run.
    metrics: dict[str, object] = field(default_factory=dict)


_POST_CHECKS: dict[str, PostCheck] = {}


def register_post_check(category: str) -> Callable[[PostCheck], PostCheck]:
    def deco(fn: PostCheck) -> PostCheck:
        _POST_CHECKS[category] = fn
        return fn
    return deco


def run_post_check(ctx: PostCheckContext) -> tuple[bool, str]:
    fn = _POST_CHECKS.get(ctx.spec.category)
    if fn is None:
        return True, ""
    return fn(ctx)


__all__ = [
    "MARKER_MODES",
    "MarkerModeSpec",
    "PostCheck",
    "PostCheckContext",
    "get_spec",
    "list_modes",
    "register_post_check",
    "run_post_check",
]
