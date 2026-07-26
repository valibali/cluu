# T3 — Multimedia Architecture Contract Audit Report

**Date:** 2026-07-26
**Task:** Correct and freeze multimedia architecture contracts (Plan todo T3,
`.omo/plans/cluu-multimedia-stack.md` line 104).
**Files modified:**
- `docs/superpowers/specs/2026-07-26-multimedia-architecture-design.md`
- `docs/superpowers/plans/2026-07-26-multimedia-coder-contract.md`

## What changed and why

The prior contracts carried several claims that were not measurement-grounded and a few
that were structurally wrong for CLUU's capability/session model. T3 corrects those,
binds the adopted defaults from `.omo/drafts/multimedia-architecture.md`, and cites the T2
baseline (`.omo/evidence/task-2-cluu-multimedia-stack.md`) for every performance assertion.

### Spec (`2026-07-26-multimedia-architecture-design.md`)

| Section | Prior claim | Correction | Reason |
|---|---|---|---|
| Header status | "Approved (design); implementation plan pending" | "Proposed (measurement-grounded)" | Claims were not yet backed by measurement; T2 baseline now exists |
| §1 Problem | "Expected budget for a 320x200-class game is 1-2%" | Removed absolute target; gates are relative to T2 baseline | T2 measured vCPU 4-5% steady-state; a fixed percentage is not defensible before the refactor |
| §1.1 Goals | "fullscreen costs zero" | "fullscreen promoted to direct scanout opportunistically; zero guest copies is not guaranteed" | Direct scanout depends on backend + exact-size; not a promise |
| §2.6 virtio-gpu | "TRANSFER_TO_HOST_2D becomes a no-op - zero guest copies" (blob guaranteed) | Blob is an optional host capability; classic 2D path is the baseline | VIRTIO_GPU_F_RESOURCE_BLOB is not universally available; classic 2D must work without it |
| §3.2 Copy-pass | Fullscreen = 0 passes; "genuinely zero guest copies" | Fullscreen = 1 composite or 0 when promoted (opportunistic) | Same as above |
| §3.3 Surface protocol | "Double-buffered, client-allocated" | "Double-buffered, server-owned" (displayd allocates/maps, retains lifecycle) | Server controls layout/lifetime; safe queued/displayed ownership |
| §3.3 Surface protocol | "present blocks until buffer_release" | "present = nonblocking commit; acquire = blocking (blocks when no FREE buffer)" | Correct double-buffer lifecycle; displayd never waits on clients |
| §3.3 Surface protocol | (no authority model) | Per-session display:client/display:wm endpoints; per-surface buffer tokens; no numeric-ID authority | Preserves CLUU session + capability invariants (AGENTS.md S3, S5) |
| §3.5 Audio | "Target 1024 bytes... fall back to 2048" | "Initial 2048 bytes; measured 1024-byte experiment in audiod phase" | 2048 is the committed default; 1024 is an experiment, not a target |
| §3.6 SDL2 | "this is the entire job"; "Five files buy the real SDL2 API surface" | File count is T14's scope; exact SDL revision pinned in T14 | Scope includes platform config, libc/thread gaps, patch series - not fixed at five |
| §3.6 SDL2 | "sdl2-shim is deleted" (immediate) | "frozen (bug fixes only), deleted in T19 after stock doomgeneric_sdl.c validates" | Transitional shim needed during port validation |
| §4 Phase 0 | "four baselines" (descriptive) | References T2 harness cases by name: l2_baseline_idle_tui, l2_baseline_quiet_shell, l2_baseline_doom_windowed, l2_baseline_doom_fullscreen | T2 already produced the -display none baseline |
| §5 Sequencing | "Upstream SDL2 + five CLUU backend files; sdl2-shim deleted" | SDL port (revision in T14); shim frozen then deleted in T19 | Same as §3.6 |
| §6 Risks | "Blocking present and deadlock"; "5.8 ms periods... fallback 2048" | "Blocking acquire and deadlock"; "initial 2048, 1024 is the experiment" | Matches new presentation + audio semantics |
| §7 Decisions | Old table with blocking present, client-allocated, five files, 2D+blob | New table with all T3 binding decisions + correction log | Single source of truth for frozen decisions |

### Contract (`2026-07-26-multimedia-coder-contract.md`)

| Section | Prior claim | Correction | Reason |
|---|---|---|---|
| Header status | "binding" | "binding and measurement-grounded" | Performance gates are relative to T2, not absolute |
| §1.1 Surfaces | "SHM frames allocated with InvokeOp::FrameAllocate" (client allocates) | "displayd allocates the backing frames (server-owned); clients map via token" | Server-owned double buffers |
| §1.5 Capability | Two endpoints (display:client / display:wm) | Added per-session delivery + per-surface buffer tokens; no numeric-ID authority | AGENTS.md S3/S5 compliance |
| §1.6 QA commands | "Visual smoke: bash scripts/fb_dump.sh" | Replaced with T2 harness cases; noted fb_dump.sh does not exist | scripts/fb_dump.sh does not exist on disk |
| §3.6 Pacing | "Pacing comes from blocking on buffer_release" | "Pacing comes from blocking on surface_acquire; surface_present is nonblocking" | Matches spec S3.3 |
| §5.2 Visual QA | "run bash scripts/fb_dump.sh and look at the PNG" | "boot and capture framebuffer or use harness visual-marker; fb_dump.sh does not exist" | Same nonexistent-script fix |
| §8 (new) | (did not exist) | Binding decisions table mirroring spec S7 | Single source of truth; contract-spec alignment |

## Forbidden-phrase verification

Grep for stale claims across both files (2026-07-26 post-edit):

```
grep -rn -i 'present blocks\|TRANSFER_TO_HOST.*no-op\|1-2%\|1–2%\|zero-copy\|five SDL files\|Five files buy\|client-allocated\|genuinely zero\|costs zero\|this is the entire job\|blocks until.*buffer_release' \
  docs/superpowers/specs/2026-07-26-multimedia-architecture-design.md \
  docs/superpowers/plans/2026-07-26-multimedia-coder-contract.md
```

Result: no matches. Both files clean.

The strings `scripts/fb_dump.sh` and `scripts/harness_run.sh` appear only in explicit
"does not exist" / "was retired and deleted" context, not as commands to run.

## T2 evidence cited

Performance assertions in both documents now reference
`.omo/evidence/task-2-cluu-multimedia-stack.md`:

- Steady-state vCPU: 4-5% across all four harness states (idle TUI, quiet shell, DOOM
  windowed, DOOM fullscreen) under `-display none`.
- DOOM frame cadence: 3.6-4.3 fps (windowed 4.3, fullscreen 3.6).
- SHIM_UPDATE: 62-87M TSC cycles per frame (windowed 62M, fullscreen 87M).
- DOOM_FRAME: 144-169M TSC cycles (windowed 144M, fullscreen 169M).
- Harness cases: `l2_baseline_idle_tui`, `l2_baseline_quiet_shell`,
  `l2_baseline_doom_windowed`, `l2_baseline_doom_fullscreen` (registered in
  `python/cluu_harness/catalog.py`).

## Nonexistent QA commands replaced

- `scripts/fb_dump.sh` — does not exist on disk (verified: `ls scripts/*.sh` shows no
  fb_dump.sh). Replaced with harness baseline cases and QEMU monitor `screendump`
  guidance.
- `scripts/harness_run.sh` — does not exist on disk. The contract already noted it was
  retired; updated to "retired and deleted" and pointed to `python -m cluu_harness`.

## Spec-contract alignment

Both documents now agree on:
- displayd created now as sole hardware owner
- Server-owned double buffers
- present = nonblocking commit; acquire = blocking
- Per-session/per-surface capabilities, no runtime ACL
- Classic virtio-gpu 2D only; blobs/direct-scanout opportunistic
- 2048-byte initial audio periods; 1024-byte experiment
- SDL revision in T14; transitional shim frozen then deleted in T19
- Performance gates relative to T2, not absolute
- T2 harness cases as the QA baseline

No contradictions identified in self-audit. GPT-5.6 Sol contract audit to be dispatched
separately by the orchestrator.
