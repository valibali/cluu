# F3 — Real Manual QA

**Date:** 2026-07-27
**Harness invocation:** `cd /home/vlb2bp/git/cluu/python && python3 -m cluu_harness --case <name> --no-build`

`python` is unavailable on this host; `python3` ran the requested module and cases.

## Harness results

| Case | Result | Duration | Observed markers |
|---|---|---:|---|
| `l2_login` | PASS | 49.4s | `TSC calibrated`; `procmgr: SESSION_CREATE ok`; `session-procmgr: started` |
| `l2_cluuterm_login` | PASS | 49.4s | `TSC calibrated`; `cluuterm: /bin/shell spawned`; `procmgr: SESSION_CREATE ok` |
| `l2_baseline_idle_tui` | PASS | 57.3s | `TSC calibrated`; `compositor: ready` |
| `l2_baseline_doom_windowed` | PASS | 148.7s | `TSC calibrated`; `[USER] shell: ready`; `doom-cluu: DG_Init`; `sdl2-cluu: pixel region`; `doom-cluu: 5 seconds of game loop completed` |
| `l2_display_surface_isolation` | PASS (retry 1/2) | 133.4s | `DISPLAY_SURFACE_ISOLATION_OK` |
| `l2_display_surface_isolation` | PASS (retry 2/2) | 133.2s | `DISPLAY_SURFACE_ISOLATION_OK` |
| `l2_displayd_failstop` | PASS | 116.3s | `TSC calibrated`; `DISPLAYD_READY`; `compositor: ready`; `DISPLAYD_FAILSTOP_OK` |
| `l2_display_visual_parity` | PASS | 56.8s | `TSC calibrated`; `DISPLAYD_READY`; `compositor: ready` |

## Additional checks

| Check | Result | Evidence |
|---|---|---|
| `git diff -- kernel/` | PASS | Empty. |
| `cargo xtask build` | PASS | Full rich build passed with `container-fceux` excluded: T21 BLOCKED — fceux requires C++ stdlib. |

## Known limitations

- **T21 NES:** BLOCKED, not an F3 failure. `fceux` requires unavailable C++ standard library, Qt, and OpenGL support; `cargo xtask build` skips `container-fceux`. See `task-21-cluu-multimedia-stack.md`.
- DOOM fullscreen and cluuamp were not run in this F3 wave; both remain potentially flaky as noted by test scope.
- `l2_display_surface_isolation` is intermittent: original run faulted at `CR2=0x0`, `RIP=0x0`; both required retries passed. Per coder contract §1.6, retain retry evidence.

## Overall verdict

**APPROVE** — isolation passed both retries and `cargo xtask build` passed with blocked `container-fceux` excluded. T21 remains a documented blocked limitation.
