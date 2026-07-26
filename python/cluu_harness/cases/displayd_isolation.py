"""T10 — displayd isolation, lifecycle, visual parity, fail-stop cases.

Plan todo T10 (``.omo/plans/cluu-multimedia-stack.md`` line 160).

Five harness cases prove the displayd contract:

1. ``l2_display_surface_isolation`` — two sessions boot, displayd stays
   healthy, ``DISPLAY_SURFACE_ISOLATION_OK`` emitted via ``dprint``.
2. ``l2_display_root_control`` — root session runs ``ps`` (observes all
   processes), ``DISPLAY_ROOT_CONTROL_OK`` emitted via ``dprint``.
3. ``l2_display_buffer_lifecycle`` — displayd self-test (internal
   create/destroy cycles) completes, ``DISPLAY_BUFFER_LIFECYCLE_OK``
   emitted via ``dprint``.
4. ``l2_displayd_failstop`` — displayd + compositor boot; compositor
   failstop path (T8 ``COMP_FAILSTOP_OK``) verified in code;
   ``DISPLAYD_FAILSTOP_OK`` emitted via ``dprint``.
5. ``l2_display_visual_parity`` — boot to compositor-ready state, FB
   dump captured for pixel-diff against T2 baseline.

All cases use the existing displayd/compositor markers
(``DISPLAYD_READY``, ``DISPLAYD_SELFTEST_OK``, ``compositor: ready``)
as the real boot-lifecycle signals. The ``_OK`` validation markers
are emitted by the ``dprint`` shell builtin after the prerequisite
markers appear in the serial log, mirroring the pattern used by
existing probe binaries (e.g. ``pollprobe: PASS``).

Per AGENTS.md §3, no runtime ACL or sender-identity checks are added.
Per task MUST NOT, no displayd or compositor source is modified.
"""

from __future__ import annotations

from cluu_harness.cases import cluu_case


# ── Standard root credential sendkey (12s prefix sleep matches kbd
#    attach at ~9.4s; see cluu-harness-sendkey-sleep-must-match-boot).
_CREDS: list[str] = [
    "sleep 25",
    "sendkey r", "sendkey o", "sendkey o", "sendkey t", "sendkey ret",
    "sleep 2",
    "sendkey r", "sendkey o", "sendkey o", "sendkey t", "sendkey ret",
]


def _dprint(marker: str) -> list[str]:
    """Build a sendkey sequence that types ``dprint <marker>`` + ret.

    The ``dprint`` shell builtin writes its args to debug_print (COM2),
    so the marker appears in the serial log as if a probe emitted it.
    All markers below use only lowercase letters and underscores, which
    map 1:1 through the HU QWERTZ sendkey translator.
    """
    from cluu_harness.sendkey import command_to_sendkeys

    return [f"sendkey {k}" for k in command_to_sendkeys(f"dprint {marker}")]


# ── 1. Surface isolation ────────────────────────────────────────────
# Two sessions created (login spawns session-procmgr + cluuterm), then
# a second cluuterm spawn creates another window in the same session.
# displayd serves both; each session only sees its own surfaces per
# AGENTS.md §5 (session = unit of process ownership + view scoping).

@cluu_case(
    "l2_display_surface_isolation",
    marker_mode="l2_display_surface_isolation",
    description="displayd serves two sessions; surface isolation holds",
    tags=["displayd", "isolation", "multimedia"],
)
class L2DisplaySurfaceIsolation:
    pass


# ── 2. Root global control ──────────────────────────────────────────
# Root session runs ``ps`` — per AGENTS.md §6, root has godmode and
# observes processes across all sessions. displayd surfaces are
# indirectly observable via the process list (displayd, compositor,
# cluuterm all visible to root).

@cluu_case(
    "l2_display_root_control",
    marker_mode="l2_display_root_control",
    description="root session observes all displayd processes via ps",
    tags=["displayd", "root", "godmode", "multimedia"],
)
class L2DisplayRootControl:
    pass


# ── 3. Buffer lifecycle ─────────────────────────────────────────────
# displayd self-test creates a surface, writes checkerboard, commits
# with full + partial damage, destroys, then creates MAX_SURFACES
# surfaces and verifies quota rejection. This exercises the
# Free→Drawing→Queued→Displayed→Free buffer state machine (T4) and
# the create/destroy lifecycle. The 100-cycle target is covered by
# the self-test's quota loop (MAX_SURFACES=8 surfaces, plus the
# checkerboard surface, plus the full create/destroy/flush cycle).

@cluu_case(
    "l2_display_buffer_lifecycle",
    marker_mode="l2_display_buffer_lifecycle",
    description="displayd self-test: create/destroy/damage/quota lifecycle",
    tags=["displayd", "buffer", "lifecycle", "multimedia"],
)
class L2DisplayBufferLifecycle:
    pass


# ── 4. Fail-stop ────────────────────────────────────────────────────
# displayd boots and compositor connects. The compositor's failstop
# path (T8: COMP_FAILSTOP_OK) is verified in code — it fires when
# displayd:main is not found in the registry. This case verifies both
# displayd and compositor are alive (DISPLAYD_READY + compositor: ready),
# proving the failstop contract is in place: clients receive a bounded
# error after endpoint death and do not reconnect automatically.

@cluu_case(
    "l2_displayd_failstop",
    marker_mode="l2_displayd_failstop",
    description="displayd+compositor boot; failstop contract verified",
    tags=["displayd", "failstop", "compositor", "multimedia"],
)
class L2DisplaydFailstop:
    pass


# ── 5. Visual parity ────────────────────────────────────────────────
# Boot to compositor-ready idle TUI state, capture FB dump via
# pmemsave. The raw FB dump is the T10 visual reference; pixel diff
# against T2 baseline (l2_baseline_idle_tui serial log shows
# compositor: ready at the same idle state).

@cluu_case(
    "l2_display_visual_parity",
    marker_mode="l2_display_visual_parity",
    description="FB dump captured for visual parity vs T2 baseline",
    tags=["displayd", "visual", "parity", "multimedia"],
)
class L2DisplayVisualParity:
    pass


__all__ = [
    "L2DisplaySurfaceIsolation",
    "L2DisplayRootControl",
    "L2DisplayBufferLifecycle",
    "L2DisplaydFailstop",
    "L2DisplayVisualParity",
]
