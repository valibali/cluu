"""Built-in case catalog.

Importing this module registers all shipped cases via ``@cluu_case``.
The package ``__init__`` imports it, so both the CLI and pytest see
the same registrations.

Add new built-in cases here. Out-of-tree cases can be registered by
importing ``cluu_harness`` and calling ``registry.register(Case(...))``
or using the ``@cluu_case`` decorator from their own module.
"""

from __future__ import annotations

from cluu_harness.cases import cluu_case


@cluu_case(
    "l2_login",
    marker_mode="l2_login",
    description="interactive login → session-procmgr spawn",
    tags=["login", "session"],
)
class L2Login:
    pass


@cluu_case(
    "l2_ls",
    marker_mode="l2_ls",
    description="basic ls of /etc",
    tags=["shell", "ls"],
)
class L2Ls:
    pass


@cluu_case(
    "l2_cd",
    marker_mode="l2_cd",
    description="cd/pwd shell builtins",
    tags=["shell", "cd"],
)
class L2Cd:
    pass


@cluu_case(
    "l2_mkdir",
    marker_mode="l2_mkdir",
    description="mkdir + mkdir -p",
    tags=["shell", "mkdir"],
)
class L2Mkdir:
    pass


@cluu_case(
    "l2_cluuterm_login",
    marker_mode="l2_cluuterm_login",
    description="inject credentials → procmgr SESSION_CREATE",
    tags=["login", "cluuterm"],
)
class L2CluutermLogin:
    pass


@cluu_case(
    "l2_cluuterm_exit",
    marker_mode="l2_cluuterm_exit",
    description="exit → cluuterm shutdown + compositor window destroyed",
    tags=["cluuterm", "compositor"],
)
class L2CluutermExit:
    pass


@cluu_case(
    "l2_vt4_default",
    marker_mode="l2_vt4_default",
    description="boot → compositor pinned to VT4",
    tags=["compositor", "boot"],
)
class L2Vt4Default:
    pass


@cluu_case(
    "l2_dev_nodes",
    marker_mode="l2_dev_nodes",
    description="ls /dev regression — dynamic /dev enumeration",
    tags=["vfs", "dev"],
)
class L2DevNodes:
    pass


@cluu_case(
    "l2_poll_pipes",
    marker_mode="l2_poll_pipes",
    description="poll()/select() on pipes, TTYs, /dev pseudo-files",
    tags=["poll", "pipe", "c-program"],
)
class L2PollPipes:
    pass


@cluu_case(
    "l2_soak_test",
    marker_mode="l2_soak_test",
    description="pipeline soak — bounded memory + no orphans",
    tags=["soak", "pipeline", "stress"],
)
class L2SoakTest:
    pass


@cluu_case(
    "m1_recv",
    marker_mode="m1_recv",
    description="recv/wakeup churn checks",
    tags=["recv", "ipc"],
)
class M1Recv:
    pass


@cluu_case(
    "m5_fairness",
    marker_mode="m5_fairness",
    description="mixed-load fairness/latency telemetry SLO checks",
    tags=["ipc", "slo"],
)
class M5Fairness:
    pass


@cluu_case(
    "s_stress_churn",
    marker_mode="s_stress_churn",
    description="heavy load: spawn/stop/cont churn + interleaved job mix",
    tags=["stress", "spawn", "signal"],
)
class SStressChurn:
    pass





@cluu_case(
    "c_futex",
    marker_mode="c_futex",
    description="futex invoke wait/wake/timeout smoke",
    tags=["futex", "c-program"],
)
class CFutex:
    pass


@cluu_case(
    "l2_ext2write",
    marker_mode="l2_ext2write",
    description="end-to-end ext2 write smoke",
    tags=["ext2", "vfs"],
)
class L2Ext2Write:
    pass


@cluu_case(
    "errnoprobe",
    marker_mode="errnoprobe",
    description="per-thread errno isolation — two threads, distinct errno",
    tags=["pthread", "errno", "c-program"],
)
class ErrnoProbe:
    pass


@cluu_case(
    "stackprobe",
    marker_mode="stackprobe",
    description="pthread_attr_setstacksize honored — 256 KiB stack",
    tags=["pthread", "stack", "c-program"],
)
class StackProbe:
    pass


@cluu_case(
    "dtachprobe",
    marker_mode="dtachprobe",
    description="detached thread stack reclamation — 50 detach cycles",
    tags=["pthread", "detach", "c-program"],
)
class DtachProbe:
    pass


@cluu_case(
    "mmapprobe",
    marker_mode="mmapprobe",
    description="mmap + mprotect including PROT_NONE",
    tags=["mmap", "mprotect", "c-program"],
)
class MmapProbe:
    pass


@cluu_case(
    "gc_stress",
    marker_mode="gc_stress",
    description="MicroPython cross-thread GC stack scanning",
    tags=["micropython", "gc", "thread"],
)
class GcStress:
    pass


@cluu_case(
    "acpiprobe",
    marker_mode="acpiprobe",
    description="ACPI RSDP discovery + FADT parsing on real QEMU",
    tags=["acpi", "discovery"],
)
class AcpiProbe:
    pass


@cluu_case(
    "xhciprobe",
    marker_mode="xhciprobe",
    description="xHCI PCI discovery + controller reset + slot enable",
    tags=["xhci", "usb"],
)
class XhciProbe:
    pass


@cluu_case(
    "usb_input_probe",
    marker_mode="usb_input_probe",
    description="usb-input primordial service boots + xHCI init",
    tags=["usb", "primordial"],
)
class UsbInputProbe:
    pass


@cluu_case(
    "dynprobe",
    marker_mode="dynprobe",
    description="Dynamic linking probe: ld-cluu reloc + TLS + Dynamic parsing",
    tags=["dynamic", "tls", "reloc"],
)
class DynProbe:
    pass


@cluu_case(
    "l2_color_256",
    marker_mode="l2_color_256",
    description="256-color SGR parsing (CSI 38;5;N / 48;5;N)",
    tags=["ansi", "sgr", "color"],
)
class L2Color256:
    pass


@cluu_case(
    "l2_attr_render",
    marker_mode="l2_attr_render",
    description="underline/reverse SGR parsing (CSI 4/24/7/27)",
    tags=["ansi", "sgr", "attr"],
)
class L2AttrRender:
    pass


@cluu_case(
    "l2_alt_screen",
    marker_mode="l2_alt_screen",
    description="alt-screen buffer enter/exit (CSI ?1049h/l)",
    tags=["ansi", "alt-screen"],
)
class L2AltScreen:
    pass


@cluu_case(
    "l2_net_boot",
    marker_mode="l2_net_boot",
    description="boot with virtio-net-pci NIC present (CLUU_NET=1)",
    tags=["net", "pci", "boot"],
)
class L2NetBoot:
    pass


@cluu_case(
    "l2_socket_basic",
    marker_mode="l2_socket_basic",
    description="BSD socket API loopback echo test",
    tags=["net", "socket"],
)
class L2SocketBasic:
    pass


@cluu_case(
    "l2_net_denied",
    marker_mode="l2_net_denied",
    description="negative test: container without NET cannot reach netd",
    tags=["net", "cap", "negative"],
)
class L2NetDenied:
    pass


@cluu_case(
    "l2_dhcp_ping",
    marker_mode="l2_dhcp_ping",
    description="DHCP acquisition + ICMP echo to QEMU gateway (10.0.2.2)",
    tags=["net", "dhcp", "icmp", "ping"],
)
class L2DhcpPing:
    pass


@cluu_case(
    "l2_dns_basic",
    marker_mode="l2_dns_basic",
    description="DNS resolution via QEMU SLIRP DNS forwarder (10.0.2.3)",
    tags=["net", "dns"],
)
class L2DnsBasic:
    pass


@cluu_case(
    "l2_wget_basic",
    marker_mode="l2_wget_basic",
    description="wget HTTP GET to host-side HTTP server via 10.0.2.2",
    tags=["net", "http", "wget"],
)
class L2WgetBasic:
    pass


@cluu_case(
    "l2_curl_basic",
    marker_mode="l2_curl_basic",
    description="curl HTTP GET to host-side HTTP server via 10.0.2.2",
    tags=["net", "http", "curl"],
)
class L2CurlBasic:
    pass


@cluu_case(
    "l2_curl_badurl_survive",
    marker_mode="l2_curl_badurl_survive",
    description="curl with bad URL exits cleanly, shell survives",
    tags=["net", "curl", "crash"],
)
class L2CurlBadurlSurvive:
    pass


@cluu_case(
    "l2_libtui_demo",
    marker_mode="l2_libtui_demo",
    description="libtui demo renders and exits on q",
    tags=["tui", "libtui"],
)
class L2LibtuiDemo:
    pass


@cluu_case(
    "l2_edit_cluuterm",
    marker_mode="l2_edit_cluuterm",
    description="edit works under cluuterm PTS raw mode",
    tags=["edit", "cluuterm", "tui"],
)
class L2EditCluuterm:
    pass


@cluu_case(
    "l2_edit_libtui",
    marker_mode="l2_edit_libtui",
    description="edit via libtui Program event loop + diff renderer",
    tags=["edit", "libtui", "tui"],
)
class L2EditLibtui:
    pass


@cluu_case("l2_fm_basic", marker_mode="l2_fm_basic", description="file manager browses VFS", tags=["tui", "fm"])
class L2FmBasic: pass

@cluu_case("l2_pager_basic", marker_mode="l2_pager_basic", description="pager scrolls a file", tags=["tui", "pager"])
class L2PagerBasic: pass

@cluu_case("l2_hexdump_basic", marker_mode="l2_hexdump_basic", description="hex viewer shows hex+ASCII", tags=["tui", "hexdump"])
class L2HexdumpBasic: pass

@cluu_case("l2_calc_basic", marker_mode="l2_calc_basic", description="calculator evaluates expressions", tags=["tui", "calc"])
class L2CalcBasic: pass

@cluu_case("l2_diff_basic", marker_mode="l2_diff_basic", description="diff viewer shows differences", tags=["tui", "diff"])
class L2DiffBasic: pass

@cluu_case("l2_irc_basic", marker_mode="l2_irc_basic", description="IRC client connects to server", tags=["net", "irc"])
class L2IrcBasic: pass

@cluu_case("l2_httpd_basic", marker_mode="l2_httpd_basic", description="HTTP server listens on port 8080", tags=["net", "httpd"])
class L2HttpdBasic: pass

@cluu_case("l2_ntp_basic", marker_mode="l2_ntp_basic", description="NTP client queries time", tags=["net", "ntp"])
class L2NtpBasic: pass

@cluu_case("l2_git_basic", marker_mode="l2_git_basic", description="git init/add/commit/log", tags=["dev", "git"])
class L2GitBasic: pass

@cluu_case("l2_sed_basic", marker_mode="l2_sed_basic", description="stream editor substitute command", tags=["dev", "sed"])
class L2SedBasic: pass

@cluu_case("l2_awk_basic", marker_mode="l2_awk_basic", description="text processor pattern-action", tags=["dev", "awk"])
class L2AwkBasic: pass

@cluu_case("l2_make_basic", marker_mode="l2_make_basic", description="build tool executes Makefile rules", tags=["dev", "make"])
class L2MakeBasic: pass

@cluu_case("l2_mail_basic", marker_mode="l2_mail_basic", description="IMAP client connects to server", tags=["net", "mail"])
class L2MailBasic: pass

@cluu_case("l2_feed_basic", marker_mode="l2_feed_basic", description="RSS reader fetches + displays items", tags=["net", "feed"])
class L2FeedBasic: pass

@cluu_case("l2_notes_basic", marker_mode="l2_notes_basic", description="notes lists + opens files", tags=["tui", "notes"])
class L2NotesBasic: pass

@cluu_case("l2_glow_basic", marker_mode="l2_glow_basic", description="markdown viewer renders", tags=["tui", "glow"])
class L2GlowBasic: pass

@cluu_case("l2_sysmon_basic", marker_mode="l2_sysmon_basic", description="system monitor shows /proc stats", tags=["sys", "sysmon"])
class L2SysmonBasic: pass

@cluu_case("l2_top", marker_mode="l2_top", description="top reads non-empty /proc/<pid>/stat process list", tags=["sys", "top", "procfs"])
class L2Top: pass

@cluu_case("l2_pkg_basic", marker_mode="l2_pkg_basic", description="package manager lists installed containers", tags=["sys", "pkg"])
class L2PkgBasic: pass

@cluu_case("l2_mp_spike", marker_mode="l2_mp_spike", description="MicroPython feasibility spike — 100 cycles + heap stable", tags=["edit", "micropython"])
class L2MpSpike: pass

@cluu_case("l2_mp_no_vfs", marker_mode="l2_mp_no_vfs", description="negative test: edit-plugin cannot open files (no vfs)", tags=["edit", "micropython", "security"])
class L2MpNoVfs: pass


@cluu_case("l2_plugin_api", marker_mode="l2_plugin_api", description="editor plugin API — keymap + command callbacks via MicroPython IPC", tags=["edit", "plugin", "micropython"])
class L2PluginApi: pass


@cluu_case("l2_audio_boot", marker_mode="l2_audio_boot", description="virtio-snd driver boot + control/TX self-test", tags=["audio", "virtio-snd"])
class L2AudioBoot: pass


@cluu_case("l2_audio_play", marker_mode="l2_audio_play", description="mp3player raw-PCM playback via virtio-snd", tags=["audio", "virtio-snd", "mp3player"])
class L2AudioPlay: pass

@cluu_case("l2_cluuamp", marker_mode="l2_cluuamp", description="cluuamp TUI audio player startup", tags=["audio", "virtio-snd", "tui", "cluuamp"])
class L2Cluuamp: pass


@cluu_case("l2_blk_basic", marker_mode="l2_blk_basic", description="single sector-0 read via BlkSession", tags=["storage", "virtio-blk", "blkprobe"])
class L2BlkBasic: pass


@cluu_case("l2_blk_perf", marker_mode="l2_blk_perf", description="64 MB sequential read, >=150 MB/s floor", tags=["storage", "virtio-blk", "blkprobe", "perf"])
class L2BlkPerf: pass


@cluu_case("l2_blk_concurrent", marker_mode="l2_blk_concurrent", description="4 sessions x 100 concurrent reads", tags=["storage", "virtio-blk", "blkprobe"])
class L2BlkConcurrent: pass


@cluu_case("benchprobe", marker_mode="benchprobe", description="spawn/ipc/thread cycle benchmark — avg cycles per noop spawn", tags=["bench", "spawn", "perf"])
class Benchprobe: pass

@cluu_case("l2_doom", marker_mode="l2_doom", description="DOOM port: doomgeneric boots and initializes compositor window", tags=["doom", "container", "compositor", "audio"])
class L2Doom: pass


__all__ = [
    "Benchprobe",
    "CFutex",
    "DtachProbe",
    "ErrnoProbe",
    "GcStress",
    "AcpiProbe",
    "XhciProbe",
    "UsbInputProbe",
    "DynProbe",
    "L2AltScreen",
    "L2AttrRender",
    "L2Cd",
    "L2CluutermExit",
    "L2CluutermLogin",
    "L2Color256",
    "L2DhcpPing",
    "L2DevNodes",
    "L2Ext2Write",
    "L2Login",
    "L2Ls",
    "L2Mkdir",
    "L2NetBoot",
    "L2NetDenied",
    "L2PollPipes",
    "L2SocketBasic",
    "L2Vt4Default",
    "L2DnsBasic",
    "L2WgetBasic",
    "L2CurlBasic",
    "L2LibtuiDemo",
    "L2EditCluuterm",
    "L2EditLibtui",
    "L2FmBasic",
    "L2PagerBasic",
    "L2HexdumpBasic",
    "L2CalcBasic",
    "L2DiffBasic",
    "L2IrcBasic",
    "L2HttpdBasic",
    "L2NtpBasic",
    "L2GitBasic",
    "L2SedBasic",
    "L2AwkBasic",
    "L2MakeBasic",
    "L2MailBasic",
    "L2FeedBasic",
    "L2NotesBasic",
    "L2GlowBasic",
    "L2SysmonBasic",
    "L2PkgBasic",
    "L2PluginApi",
    "M1Recv",
    "M5Fairness",
    "MmapProbe",
    "StackProbe",
    "L2Doom",
]
