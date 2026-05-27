# Post-cap-refactor Backlog Synthesis

**Date:** 2026-05-27
**Author:** Balazs (with caveman-mode assistant)
**Scope:** sequence all open items left after the procmgr-cap-refactor branch landed (HEAD = `b43a848`) and the autologin removal closed (`6d2bf44`).
**Source of truth this plan reconciles:**
- `docs/superpowers/plans/2026-05-18-plan{1..4}-*.md` (the 5/18 quartet)
- `docs/superpowers/plans/2026-05-21-procmgr-cap-refactor.md` (Phase 14.3 deferral)
- `docs/superpowers/plans/2026-05-26-autologin-removal-harness-migration.md` (followups)
- `memory/MEMORY.md` (stranded debt entries)
- `docs/ROADMAP.md` Phase 5 (Network) — *next* phase after this backlog clears

## Ground rules

1. **Kernel freeze still active through ~2026-10-21.** No kernel commit lands without naming the userspace failure that forced it. ROADMAP §3.
2. **No new syscalls.** Every verb goes through existing IPC + tokens.
3. **No timeouts as deadlock guards.** Cap-revocation unblocks waiters.
4. **Commit per task.** `cargo xtask build` clean between tasks. No 3-day WIP on `develop`.
5. **Coverage gate (cargo xtask coverage-check) must remain green** post-cap-refactor (Phase 14.1).

## State as of 2026-05-27

| 5/18 quartet plan | State | Notes |
|---|---|---|
| Plan 1 (unified spawn protocol) | partial; bypass active | `handle_spawn_unified` short-circuits real `procmgr::spawn()` per commit `70a4418`. Login works; debt is the legacy code path that needs to die. |
| Plan 2 (terminal/PTY unification) | ~80% | Task 1 (pts labels), 2 (line discipline + termios), 3 (signal routing), 6 (POSIX termios shims), 9 (cluuterm registers pts), 11 (SIGWINCH on resize), 13 (acceptance markers) landed across 5/19–5/21. **Open:** task 8 (VFS per-session `/dev/pts/` overlay) and task 12 (dead-code purge of legacy TTY_* paths). |
| Plan 3 (session lifecycle) | DONE | 12 tasks, 15 commits, all acceptance probes green. SESSION_CREATE/DESTROY/HANDOFF + getty + 2 s timeout deletion all landed. |
| Plan 4 (window protocol) | NOT STARTED | No `cluu_wire::window` module. Compositor still on legacy `COMP_WIN_*`. 13 tasks, the biggest single piece in this plan. |
| procmgr-cap-refactor | LANDED 5/27 | 74-commit ff-merge. 8 `pm_*` probes green. Phase 14.3 (spawn-warm cap-possession ACL) explicitly deferred. |
| Autologin removal | LANDED 5/26 | All harness via interactive login. Followups open: `l2_cat_basic` marker missing, QEMU sendkey `0` → `o` under HU layout. |
| VFS view-cap delegation | LANDED 5/26 | Typed cap tag 0x09, op=23 `TokenDeriveScoped`. End-to-end smoke green. |

## Sequencing rationale

Order is **debt-aware** but **user-visible first**:

- **A. Autologin follow-ups** ship first — small (hours), surface test reliability that everything else depends on.
- **B. Plan 2 PTY closeout** next — closes the second of the quartet, unblocks any future MicroPython REPL termios work, lets us delete real legacy code (drift counter-move: deleting code is the highest-ROI cleanup during the freeze).
- **C. Plan 4 window protocol** is the keystone — without it, compositor menus (5/12 spec), TUI compositor (5/10 spec), `/dev/fb0` Unix surface (`project_dev_fb_unix`) all sit behind legacy `COMP_WIN_*`. Largest piece (~2 weeks).
- **D. Plan 1 spawn() bypass retire** is debt — defer until A/B/C land. Risk: while bypass is in place, every new spawn site is one more thing to fix later. Mitigation: forbid new callers from using the bypass path (lint via xtask grep gate).
- **E. Deferred items** stay deferred with explicit unblock triggers documented.

Phase 5 (Network) does **not** start until A/B/C/D all green. E items may or may not block — re-evaluate at the end of D.

---

## A. Autologin follow-ups

**Goal:** clear the two known regressions surfaced by `6d2bf44` so the harness matrix is trustworthy again.

**A.1 — `l2_cat_basic` marker emission.**
Marker missing post-migration. Likely cause: interactive login flow doesn't inject the same env/cwd as the autostart path; cat container can't find or print the source it used to. Bisect against `747fb00` to confirm.
*Verification:* `MARKER_MODE=l2_cat_basic bash scripts/harness_run.sh`, grep `l2_cat_basic: pass` in `serial.log`.

**A.2 — QEMU sendkey `0` → `o` under HU layout.**
HU QWERTZ kbd map sends `o` when harness expects `0`. Look at `scripts/harness/type_ascii_command` (already touched in `768c98a`) — `0` likely missing from the punctuation/digit fixup table. Either fix the map or wrap harness to switch to US layout for the duration of test input.
*Verification:* harness with a marker that types `0` (e.g. an `echo 0` smoke) prints `0`, not `o`.

**Acceptance:** both fixed, harness matrix green. **Estimated cost:** 1 day.

---

## B. Plan 2 PTY closeout

**Goal:** finish `docs/superpowers/plans/2026-05-18-plan2-terminal-pty-unification.md`. Two tasks left.

**B.1 — Plan 2 Task 8: VFS per-session `/dev/pts/` overlay.**
Spec: `docs/superpowers/specs/2026-05-18-terminal-pty-unification-design.md` §"Mount layout".
Current state: pts fds are inherited via FD VFS trailer (`4037c2d`) but `/dev/pts/<id>` paths are not enumerable through VFS in the child's view. Need a VFS overlay that synthesizes the per-session pts dir on `readdir(/dev/pts)` and resolves `/dev/pts/<id>` to the right pts token.
- Verify VFS-side dispatch with cap-delegated view (`365dfa0`) is what we ride on top of.
- Acceptance: `ls /dev/pts` in shell shows the live ptys for the session; nothing from other sessions.
- New probe: `l2_pts_listing`.

**B.2 — Plan 2 Task 12: dead-code purge.**
Legacy `TTY_*` labels and dual-protocol guards. Inventory:
- `userspace/libcluu/src/ipc.rs:215` — `COMP_WIN_DESTROY_LABEL = 93` (window, not PTY — leave for Plan 4).
- Old TTY_* labels in cluu_wire / tty service: enumerate via `grep -rn 'TTY_[A-Z_]*_LABEL' userspace/`. Anything not referenced after Plan 2 finishes is dead.
- Cluuterm dual-protocol guards (`if uses_pts_protocol`): can collapse since every consumer now does PTS_*.
- Acceptance: build green; `cargo xtask check-cap-purity` (added in Phase 14.1 of cap-refactor) stays green; no new orphan labels.

**Acceptance:** both tasks done, `l2_pts_listing` green, dead code gone. **Estimated cost:** 2 days.

---

## C. Plan 4 window protocol

**Goal:** execute `docs/superpowers/plans/2026-05-18-plan4-window-protocol.md` in full (13 tasks).
**Handoff doc:** `docs/superpowers/plans/2026-05-19-plan4-handoff.md` — written for an external implementer; honor it.

**Pre-flight:**
- Confirm `cluu_wire::window` module does not already exist (it doesn't, verified 5/27).
- Tag `plan4-start` before first commit (per handoff): `git tag plan4-start HEAD`.

**Task chunking:** group of 4, build green between groups:
- Group 1 (wire + libcluu surface): Tasks 1, 2 — `cluu_wire::window` types/labels + `libcluu::window` wrappers + `SurfaceBufferPool`.
- Group 2 (compositor state machine): Tasks 3, 4, 5 — `Surface` typed object, per-client async event endpoint, dispatch arms for 9 verbs.
- Group 3 (render + input): Tasks 6, 7 — per-frame callback render loop + focus tracking + keymap.
- Group 4 (clients flip): Tasks 8, 9 — cluuterm + login flip to `libcluu::window`.
- Group 5 (cleanup + tests): Tasks 10, 11, 12, 13 — session-aware cleanup, cap-revocation force-destroy, dead-code delete, acceptance probes.

**Special attention:**
- Task 10 (`SESSION_ENDED` consumer) lands the long-deferred compositor reaction to session cascade-destroy. Plan 3 already broadcasts; just consume here.
- Task 12 deletes `COMP_WIN_*` and global `compositor:input` — coordinate with input routing redesign (`project_input_routing_design`).
- Probes (Task 12 of Plan 4) integrate with existing `pm_*` probe harness.

**Acceptance:** login flow + cluuterm both render via WIN_* protocol; boot smoke (`bash scripts/harness_run.sh`) + visual smoke (`bash scripts/fb_dump.sh`) both green. **Estimated cost:** 8–10 days.

---

## D. Spawn-path unification + bypass retire

**Goal:** collapse the **three** live spawn label paths down to one, and delete `handle_spawn_unified`'s short-circuit (`70a4418`); route every spawn through the real `procmgr::spawn()` hooks defined in `docs/superpowers/plans/2026-05-18-plan1-unified-spawn-protocol.md` Tasks 8–10.

**Why now:** with cap-refactor landed the dispatcher is no longer `procmgr` but `root-procmgr` + `session-procmgr`. The bypass lives in `root-procmgr`. The real `spawn()` hooks need to land **once**, with the cap-broker plumbing already in place from cap-refactor — that's friendlier than wiring them while the procmgr split was in flight.

**Current live labels (verified 2026-05-27):**
- `PROCMGR_SPAWN_LABEL = 2` (`userspace/libs/procmgr-common/src/labels.rs:10`) — legacy historic-carry-over path. Per `memory/project_container_run_posix_spawn_unify.md` this bypasses the manifest. Was the source of the HOME-env Bug B.
- `PROCMGR_CONTAINER_RUN_LABEL = 24` (`userspace/libcluu/src/ipc.rs:116`) — manifest-driven container-run, correct semantics.
- `SESSION_PROCMGR_SPAWN_LABEL = 0xB000` (`userspace/libs/procmgr-common/src/labels.rs:37`) — post-cap-refactor session-scoped spawn; what login + cluuterm currently use.

**Target end state:** one client-facing spawn verb (the unified envelope from Plan 1) dispatched to root-procmgr for primordial seeds + session-procmgr for everything else. `PROCMGR_SPAWN_LABEL` and `PROCMGR_CONTAINER_RUN_LABEL` both retired.

**Steps:**
1. **Audit.** Three greps and mark every call site:
   - `grep -rn 'PROCMGR_SPAWN_LABEL\b' userspace/`
   - `grep -rn 'PROCMGR_CONTAINER_RUN_LABEL' userspace/`
   - `grep -rn 'handle_spawn_unified' userspace/root-procmgr userspace/session-procmgr`
2. **Resolver shim.** Land libcluu `posix_spawn(path, argv, envp)` → image-name resolver per `memory/project_container_run_posix_spawn_unify.md` step 1. Symlink-target parse, no other behavior change yet.
3. **Land Plan 1 Tasks 8 + 9** (real `spawn()` and `PROCMGR_SPAWN_UNIFIED_LABEL` dispatch) inside the new procmgr crate split. Treat the spec sections as advisory — re-read with the post-refactor lens.
4. **Add `cargo xtask check-cap-purity` lint rule** forbidding new references to the bypass + the legacy labels once step 3 lands. Removes the temptation to add new callers.
5. **Cutover one caller at a time:** login → cluuterm → shell → primordial seed paths → in-process utility spawns (echo, ls, etc.). Build green per cutover; harness matrix green per group.
6. **Delete** `PROCMGR_SPAWN_LABEL` handler, `PROCMGR_CONTAINER_RUN_LABEL` handler, `handle_spawn_unified` bypass, the comments noting them.
7. **Coverage check:** `cargo xtask coverage-check` ≥ 95 % line + branch for both procmgr crates.

**Risk:** Plan 1 Tasks 8/9 specs are pre-cap-refactor. Re-read them with the post-refactor lens; some token-passing surfaces will be different. If a spec section is materially wrong, append a dated update — do not rewrite (ROADMAP §4 Pattern 3).

**Acceptance:** zero call sites for the bypass + legacy labels; cap-purity gate green; coverage gate green. **Estimated cost:** 5–7 days (raised from 4–5 to absorb the dual-path unify scope).

---

## E. Deferred (with unblock triggers)

These do **not** belong in this plan's critical path. Listed here so they don't get re-discovered and re-debated:

- **Phase 14.3 spawn-warm cap-possession ACL** (procmgr-cap-refactor deferral). *Unblock:* when a real performance pain shows up in a userspace test, not before. Tracked in `memory/project_kill_acl_session_mismatch_2026_05_27.md`.
- **Cluuterm ANSI parser extraction (5/11 Q3).** *Unblock:* the day MicroPython or another full-screen TUI hits a parser-spec gap that the in-line state machine can't represent. Tracked in `memory/project_cluuterm_next_session.md`.
- **`/proc` Unix-style + `/dev/fb0` surface.** *Unblock:* when `ps` or any TUI app needs them. The infra for `/dev/fb0` lands cleanest after Plan 4 because the compositor owns the fb today. Tracked in `memory/project_proc_unix_compliance.md` and `memory/project_dev_fb_unix.md`.
- **TUI compositor design (5/10 spec).** *Unblock:* same — after Plan 4 establishes the window protocol that the compositor would draw into. Multi-window comes next; the surface protocol comes first.
- **Compositor menus + Cluufile APP directive (5/12 spec).** *Unblock:* after Plan 4 + TUI compositor. The APP directive landed (`34db0d3`); the UI side hasn't.
- **Phase 4 Plan D TODOs** (`& ;` separator, Ctrl-Z input injection harness, SIGTTIN wire). *Unblock:* when a shell smoke surfaces them. Tracked in `memory/project_phase4_plan_d_todos.md`.
- **Live restart-loop probe + procmgr-internal crash cascade probe + PROC_QUERY_ALL privileged path probe** (cap-refactor coverage gaps, see `docs/superpowers/specs/PROCMGR_CAP_REFACTOR_COVERAGE.md`). *Unblock:* before the *next* cap-refactor revision, not before merge.
- **LoginCC Bug A — banner `???` in login modal.** Unicode → cp437 table gap in compositor render pipeline. Tracked in `memory/project_loginCC_session_2026_05_13.md`. *Unblock:* fold into Plan 4 Task 7 (focus/keymap/glyph) — extend the cp437 fallback table; ≤ 1 h once Plan 4 is in motion.
- **MAP_SHARE_PHYS cache-invalidation re-enable.** Five reset sites in `userspace/vfs/src/main.rs` are no-ops behind `invalidate_cache_after_mutation` until refcount-aware invalidation lands. 32 MiB cache > sum of v1 binaries, so functionally benign. Tracked in `memory/project_map_share_phys_uaf.md`. *Unblock:* when the cache approaches saturation OR a future MAP_SHARE_PHYS perf re-enable lands.
- **VFS split (root-vfs + session-vfs), mirror of procmgr cap-refactor.** Today one VFS process handles persistent mounts (ext2/initrd/memfs/procfs), per-session `/dev/pts/`, view-cap minting + deriving, FD inheritance, cache. Same single recv loop, no fault isolation, root-mint authority muddied with session-derive authority. Split would give: root-vfs owns disk cache + persistent mounts + view-cap mint; session-vfs owns `/dev/pts/<id>` overlay + `/tmp` + view-cap derive + PTS proxy. **Doesn't fix the blocking-RPC class on its own** — session-vfs is still single-threaded; async-park (entry above) is the actual fix for that. Split is an isolation + cap-scope lever. *Costs:* cross-vfs routing for client opens (which mount → which server?), view-cap delegation re-design, cache grants across server boundary. *Unblock trigger:* Plan 2 task 8 (`/dev/pts/` overlay) lands and reveals the natural fault line, OR multi-tenant deployment (>1 concurrent session) where fault isolation matters, OR cap-purity audit finds root-mint authority leaking into session paths. Don't pre-build — procmgr split had a forcing function (cap-refactor + Phase 14); VFS doesn't yet.
- **VFS proxy must not block — async-park required.** `handle_pts_set_pgrp_proxy` (and any future VFS→peer proxy) uses synchronous `ipc_call` to cluuterm. While VFS waits, its single-threaded recv loop is frozen for *every other client* (registry GRANT_REQUEST forwards, readdir, open, etc.). The c31ed93 → 4ccb5ec ls regression was a direct hit on this. **Today the cluuterm reply is fast enough that nothing else surfaces — but every new VFS proxy verb (POLL, GET/SET_TERMIOS, GET/SET_WINSIZE, FLUSH all have `Pts` handlers but no dispatch arms yet) is a latent stall.** *Fix shape:* park the caller's `reply_token` keyed by (cluuterm_ep, pts_id, in-flight tag), `ipc_send` (not `ipc_call`) the forward, route cluuterm's reply through VFS's main recv loop and ipc_reply to the parked token. Same pattern as `PTS_READ`/`PTS_READ_DELIVER` already uses. *Unblock:* the next VFS proxy verb that ships, OR the next reported VFS stall traced to a slow cluuterm — whichever first. Don't pre-build the async runtime speculatively; do it when the second blocking proxy lands.
- **Registry no re-Grant on entry replace.** Force-unregister + re-register under the same name does NOT notify already-granted subscribers. Tracked in `memory/project_registry_no_regrant_on_replace.md`. Every service that subscribes to a name owned by a replaceable process must trigger its own re-Grant (today: vtmgr `handle_pin_vt`). *Unblock:* when a third such consumer appears (then it's worth a registry change, not a per-consumer one).
- **`l2_owner_deny` test redesign.** Pre-existing broken since mount-policy (`/tmp` is MemFs, mode-blind). Tracked in `memory/project_l2_owner_deny_flaky.md`. *Unblock:* either point it at an ext2-backed PERSISTENT dir (smaller fix) or extend MemFs with POSIX mode bits (larger payoff).
- **etc/envelopes.toml `+rw:/proc` admin mod.** Memory entry `project_envelopes_admin_proc_uncommitted.md` flagged a working-tree mod that no longer exists (`git ls-files -m` clean as of 2026-05-27). **Action:** drop the memory entry next time auto-memory is touched; nothing else to do here.
- **Compositor runtime PTE corruption 5/15 (RSV bit on 0x400000-0x5fffff during login → cluuterm handoff).** Tracked in `memory/project_compositor_runtime_pte_corruption_2026_05_15.md`. Almost certainly closed by the frame-typing redesign (`memory/project_frame_typing_redesign_landed_2026_05_18.md`: "Compositor survives login teardown"). *Unblock action:* on the next boot of the post-cap-refactor build, watch for any PF in compositor's text segment. If three clean boots pass, mark the debt closed and delete the memory entry.

## After D: Phase 5 entry conditions

Before opening `kernel/src/virtio_net*` or anything Phase-5-shaped:
- A, B, C, D all DONE.
- `cargo xtask check-cap-purity` green.
- `cargo xtask coverage-check` green for procmgr crates.
- Harness matrix green end-to-end via login flow only (no autostart shortcuts).
- ROADMAP §3 freeze still in effect; first Phase 5 commit must name the userspace network testcase that forced it.

## Estimated total

A (1d) + B (2d) + C (8–10d) + D (5–7d) = **16–20 working days**. Phase 5 (Network) starts after.

## Open questions for the author

1. **D before C?** If the spec-1 bypass starts paying interest fast (new spawn sites slipping in), retire it first. Lint gate in A keeps the cost low; preserve the option.
2. **Plan 4 keymap source.** Task 10 spec says `/etc/keymap/us.toml` or embedded. We've been bitten by HU layout in QEMU (A.2). Recommend embed-default + optional override — fewer moving pieces.
3. **Should Phase 5 still be Network?** A year of "almost ready" patterns in MicroPython suggests a TUI/`/dev/fb0`/compositor-menus mini-phase between this backlog and Network would yield more user-visible motion. ROADMAP says no — but the ROADMAP edit clause allows phase reorder if the criteria are obviously wrong, not just hard. Defer this question until C is done.
