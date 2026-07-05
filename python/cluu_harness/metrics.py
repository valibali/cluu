"""Metric extraction + SLO post-checks.

Ports the awk/grep metric parsers and the per-mode limit checks from
``scripts/harness_run.sh``. Everything here operates on the captured
serial text after the marker wait completes — the wait itself is
event-driven and lives in ``serial_stream.py``.

Categories registered via :func:`markers.register_post_check`:

* ``recv`` — minimum exit-cookie count
* ``token_audit`` — ``token_audit_dropped=0``, ``stored>=2``
* ``leak`` — resource-delta limits (spaces/tokens/endpoints/pmm)
* ``fairness`` — IPC wait p95/p99 + scan-average limits
* ``warm_spawn`` — noop spawn/map_elf sample counts + p95 cycle limits
"""

from __future__ import annotations

import re
from collections.abc import Iterable

from cluu_harness.markers import PostCheckContext, register_post_check

# ---------------------------------------------------------------------- #
# Regex helpers
# ---------------------------------------------------------------------- #

def _last_int(text: str, marker: str) -> int | None:
    """Return the last integer that appears after ``marker`` in the log."""
    pattern = re.escape(marker) + r"(-?\d+)"
    matches = re.findall(pattern, text)
    if not matches:
        return None
    return int(matches[-1])


def _first_int(text: str, marker: str) -> int | None:
    pattern = re.escape(marker) + r"(-?\d+)"
    m = re.search(pattern, text)
    return int(m.group(1)) if m else None


def _count_occurrences(text: str, substring: str) -> int:
    return text.count(substring)


def _percentile_p95(values: Iterable[int]) -> int | None:
    vals = sorted(values)
    if not vals:
        return None
    n = len(vals)
    rank = max(1, (95 * n + 99) // 100)
    return vals[rank - 1]


# ---------------------------------------------------------------------- #
# Per-category post-checks
# ---------------------------------------------------------------------- #

@register_post_check("recv")
def _recv_check(ctx: PostCheckContext) -> tuple[bool, str]:
    exit_cookies = _count_occurrences(ctx.serial_text, "procmgr: exit cookie")
    ctx.metrics["exit_cookies"] = exit_cookies
    if exit_cookies < ctx.min_exit_cookies:
        return False, (
            f"expected at least {ctx.min_exit_cookies} exit cookies, "
            f"got {exit_cookies}"
        )
    return True, ""


@register_post_check("token_audit")
def _token_audit_check(ctx: PostCheckContext) -> tuple[bool, str]:
    # The recv exit-cookie check still applies.
    ok, msg = _recv_check(ctx)
    if not ok:
        return False, msg
    next_seq = _first_int(ctx.serial_text, "token_audit_next_seq=")
    stored = _first_int(ctx.serial_text, "token_audit_stored=")
    dropped = _first_int(ctx.serial_text, "token_audit_dropped=")
    if next_seq is None or stored is None or dropped is None:
        return False, "token audit telemetry metrics could not be parsed"
    ctx.metrics.update(
        {"token_audit_next_seq": next_seq, "token_audit_stored": stored,
         "token_audit_dropped": dropped}
    )
    if dropped != 0:
        return False, f"expected token_audit_dropped=0, got {dropped}"
    if stored < 2:
        return False, f"expected token_audit_stored>=2, got {stored}"
    return True, ""


@register_post_check("leak")
def _leak_check(ctx: PostCheckContext) -> tuple[bool, str]:
    ok, msg = _recv_check(ctx)
    if not ok:
        return False, msg
    delta_samples = _count_occurrences(ctx.serial_text, "resource delta:")
    ctx.metrics["delta_samples"] = delta_samples
    if delta_samples < 1:
        return False, "expected at least one resource delta sample"
    deltas = {
        "delta_spaces": _last_int(ctx.serial_text, "delta_spaces="),
        "delta_tokens": _last_int(ctx.serial_text, "delta_tokens="),
        "delta_endpoints": _last_int(ctx.serial_text, "delta_endpoints="),
        "delta_pmm_used_frames": _last_int(ctx.serial_text, "delta_pmm_used_frames="),
    }
    ctx.metrics.update(deltas)
    for name, val in deltas.items():
        limit = ctx.limits.get("MAX_" + name.upper())
        if limit is None or val is None:
            if val is None and limit is not None:
                return False, f"could not parse {name}"
            continue
        if val > limit:
            return False, f"{name} exceeded limit (value={val} limit={limit})"
    return True, ""


@register_post_check("fairness")
def _fairness_check(ctx: PostCheckContext) -> tuple[bool, str]:
    ok, msg = _recv_check(ctx)
    if not ok:
        return False, msg
    metrics = {
        "ipc_wait_p95_ms": _last_int(ctx.serial_text, "ipc_wait_p95_ms="),
        "ipc_wait_p99_ms": _last_int(ctx.serial_text, "ipc_wait_p99_ms="),
        "ipc_scan_avg_steps_x100": _last_int(
            ctx.serial_text, "ipc_scan_avg_steps_x100="
        ),
    }
    ctx.metrics.update(metrics)
    for name, val in metrics.items():
        limit = ctx.limits.get("MAX_" + name.upper())
        if limit is None:
            continue
        if val is None:
            return False, f"could not parse {name}"
        if val > limit:
            return False, f"{name} exceeded limit (value={val} limit={limit})"
    return True, ""


@register_post_check("warm_spawn")
def _warm_spawn_check(ctx: PostCheckContext) -> tuple[bool, str]:
    """Parse noop spawn_trace + map_elf_trace dt= samples, check p95."""
    text = ctx.serial_text
    # Spawn: link spawn_request seq → /bin/noop → reply_sent dt=...
    spawn_seq_to_noop: set[str] = set()
    current_seq: str | None = None
    spawn_reply_dts: list[int] = []
    for line in text.splitlines():
        m = re.search(r"spawn_trace seq=(\d+) stage=spawn_request", line)
        if m:
            current_seq = m.group(1)
            continue
        if "procmgr: spawn path /bin/noop" in line and current_seq is not None:
            spawn_seq_to_noop.add(current_seq)
        m = re.search(
            r"spawn_trace seq=(\d+) stage=reply_sent .* dt=(\d+)", line
        )
        if m and m.group(1) in spawn_seq_to_noop:
            spawn_reply_dts.append(int(m.group(2)))

    # map_elf: link /bin/noop open → fd → map_elf_trace fd=... stage=reply dt=...
    pending_noop_open = False
    noop_fds: set[str] = set()
    map_reply_dts: list[int] = []
    for line in text.splitlines():
        if "vfs: open '/bin/noop'" in line:
            pending_noop_open = True
            continue
        if pending_noop_open:
            m = re.search(r"vfs: open OK fd=(\d+)", line)
            if m:
                noop_fds.add(m.group(1))
                pending_noop_open = False
        m = re.search(
            r"vfs: map_elf_trace fd=(\d+) stage=reply .* dt=(\d+)", line
        )
        if m and m.group(1) in noop_fds:
            map_reply_dts.append(int(m.group(2)))

    spawn_p95 = _percentile_p95(spawn_reply_dts)
    map_p95 = _percentile_p95(map_reply_dts)
    ctx.metrics.update(
        {
            "noop_spawn_reply_samples": len(spawn_reply_dts),
            "noop_map_elf_reply_samples": len(map_reply_dts),
            "noop_spawn_reply_p95_cycles": spawn_p95,
            "noop_map_elf_reply_p95_cycles": map_p95,
        }
    )

    min_spawn = ctx.limits.get("MIN_NOOP_SPAWN_SAMPLES", 8)
    min_map = ctx.limits.get("MIN_NOOP_MAP_ELF_SAMPLES", 8)
    if len(spawn_reply_dts) < min_spawn:
        return False, (
            f"noop_spawn_reply_samples below minimum "
            f"(value={len(spawn_reply_dts)} minimum={min_spawn})"
        )
    if len(map_reply_dts) < min_map:
        return False, (
            f"noop_map_elf_reply_samples below minimum "
            f"(value={len(map_reply_dts)} minimum={min_map})"
        )
    if spawn_p95 is None or map_p95 is None:
        return False, "could not compute noop warm-cache p95 metrics"

    max_spawn = ctx.limits.get("MAX_NOOP_SPAWN_REPLY_P95_CYCLES")
    max_map = ctx.limits.get("MAX_NOOP_MAP_ELF_REPLY_P95_CYCLES")
    if max_spawn is not None and spawn_p95 > max_spawn:
        return False, (
            f"noop_spawn_reply_p95_cycles exceeded limit "
            f"(value={spawn_p95} limit={max_spawn})"
        )
    if max_map is not None and map_p95 > max_map:
        return False, (
            f"noop_map_elf_reply_p95_cycles exceeded limit "
            f"(value={map_p95} limit={max_map})"
        )
    return True, ""


# ---------------------------------------------------------------------- #
# Public surface
# ---------------------------------------------------------------------- #

def extract_fail_marker(text: str, fail_marker: str) -> str | None:
    """Return the first line containing ``fail_marker``, or None."""
    if not fail_marker:
        return None
    for line in text.splitlines():
        if fail_marker in line:
            return line
    return None


__all__ = ["extract_fail_marker"]
