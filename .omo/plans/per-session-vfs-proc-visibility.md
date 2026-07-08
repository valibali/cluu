# per-session-vfs-proc-visibility - Work Plan

## TL;DR (For humans)

**What you'll get:** Every login session gets its own VFS process that can only see its own session's processes, filesystem, and threads. The shell and micropython spawned inside a cluuterm appear in `top` as children of cluuterm. Sessions cannot see each other's processes, files, or PIDs. A root user (holding a system-scope capability) sees all sessions with a session column in `top`. No runtime ACL anywhere — visibility is a declarative property of threads (session_id + system_scope), enforced by the kernel at enumerate time, and procmgr/VFS authority is per-session by construction.

**Why this approach:** Today a single global VFS uses its own session-0 token to enumerate all threads and routes all `/proc/<tid>/stat` queries to root-procmgr, which only knows about processes it spawned itself — so session-procmgr's children (shell, micropython) are invisible. Fixing this by adding caller-session-checking logic in VFS would be a runtime ACL. Instead, we migrate to per-session VFS (each VFS runs inside its session, kernel enumerate is session-scoped naturally) and wire session-procmgr's existing-but-dead ProcQuery handler. A new devmgr service grants block-region caps at session creation so each session-VFS owns its own filesystem. Root gets a `system_scope` thread property (declarative, set at spawn from the cap profile) that makes kernel enumerate return all threads, and root-VFS routes stat queries through the existing ProcQueryAll fan-out.

**What it will NOT do:** No async VFS (deferred — sync per-session VFS is fine for CLUU's concurrency level). No per-session service registry (deferred — known encapsulation hole, flagged for later). No subtree-scoping of a shared writable filesystem (each session gets its own writable block region). No shared writable state across sessions (would need a separate cap-gated service). No pipes or redirection (out of scope, separate Phase 1 roadmap item). No kernel file/inode understanding (kernel stays thin: threads, session_id, block caps).

**Effort:** Large — cross-cutting migration touching kernel, 4 userspace services, and top.
**Risk:** Medium-High — session creation is a critical path; per-session VFS spawn must not regress login. Mitigated by phased delivery: Phase 1 fixes the bug pragmatically without the full migration, Phase 2-3 do the architecture.
**Decisions to sanity-check:** (1) `system_scope` as a kernel thread property (not a CapProfile bit — it's a kernel visibility scope, same category as session_id); (2) devmgr as a new userspace service (not extending root-procmgr); (3) per-session PIDs with root top showing a session column; (4) root-VFS gets caps to all session block regions for debugging (not per-session proxy RPC); (5) session-VFS spawned alongside session-procmgr at session creation by root-procmgr.

Your next move: approve, or run a high-accuracy review. Full execution detail follows below.

---

> TL;DR (machine): Large cross-cutting migration, Medium-High risk, 3 phases / 16 todos — kernel system_scope + devmgr + per-session VFS + session-procmgr ProcQuery wiring + top display.

## Scope

### Must have

**Phase 1 — Pragmatic /proc fix (fixes the bug, no architecture change):**
- Session-procmgr `ProcQuery` handler wired into `dispatch.rs` (currently dead code — `PROCMGR_PROC_QUERY_LABEL` falls through to `BadLabel`).
- Root-procmgr `handle_proc_query` forwards to `self.session_pmgr_endpoints` on `tid_to_pid` miss (mirror the `PROCMGR_PG_SIGNAL_LABEL` fan-out pattern at main.rs:3199-3206).
- Session-procmgr `QUERY_STAT` emits real `pcid` from `child_table`'s stored parent pid, instead of hard-coded `0`.
- Session-procmgr `QUERY_STAT` emits real `cid` (the child's own session-scoped container id), instead of hard-coded `0`.

**Phase 2 — Per-session VFS migration:**
- New `devmgr` userspace service: owns all hardware block devices, grants block-region caps to sessions at creation. Registered as `devmgr:main` in the service registry.
- Kernel: new `system_scope: bool` property on threads (set via `thread_set_system_scope` at spawn, alongside `thread_set_session`). `enumerate_live_tids_in_session` extended: `caller_session_id == 0 || t.session_id == caller_session_id || caller_system_scope`.
- Root-procmgr `handle_session_create` spawns a session-VFS alongside session-procmgr. Session-VFS gets: session-scoped block-region caps (from devmgr), session-procmgr's endpoint token, `session_id = X`, no `system_scope`.
- Session-VFS: a thin VFS instance (reuse existing `vfs` crate code) that mounts `/proc` backed by session-X-procmgr, and mounts its block regions as `/`, `/tmp`, etc. Registers as `session-vfs:main:{sid}` in the service registry.
- Session-VFS `/proc` readdir: `thread_enumerate(own token)` → session-X TIDs only (kernel enforces).
- Session-VFS `/proc/<tid>/stat`: IPC to session-X-procmgr's `ProcQuery` handler (wired in Phase 1).
- Session-procmgr registers `PROCMGR_PROC_QUERY_LABEL` in its dispatch and answers from `child_table`.
- Root-VFS (session 0, the current VFS code): keeps serving system services. Its `/proc` readdir already sees all threads (session 0). Its `/proc/<tid>/stat` for session-0 TIDs goes to root-procmgr (existing path). For non-session-0 TIDs, root-VFS with `system_scope` routes through ProcQueryAll fan-out.
- Root-procmgr `handle_proc_query` on `tid_to_pid` miss: if caller has `SYSTEM_PROC_QUERY_CAP_ID`, fan out via ProcQueryAll (already wired in `proc_query_all.rs`). If caller lacks the cap, return NotFound (no cross-session leak).
- Retire: remove the single-global-VFS routing path for non-system sessions. The current `registry::subscribe_output("procmgr", "spawn")` in VFS main.rs:352 becomes root-VFS-only behavior. Session processes no longer use the global VFS.

**Phase 3 — Top display + per-session PIDs:**
- Session-procmgr assigns PIDs in a per-session namespace (pid_base from `SessionEnvelope.pid_base`, already computed at root-procmgr/main.rs:4118-4119). PIDs are not globally unique.
- `top` (session-scoped): shows local PIDs, no session column. Already works after Phase 2 (reads session-VFS `/proc`).
- `top` (root, `system_scope` held): adds a session column. Root-VFS `/proc/<tid>/stat` via ProcQueryAll returns `(session_id, pid)` so top can display both.
- `ProcQueryAll` wire format extended to carry `session_id` alongside existing ProcInfo fields.

### Must NOT have (guardrails, anti-slop, scope boundaries)

- Do NOT add async to VFS in this migration. Sync only. Design VFS-internal APIs so async could be slotted later (don't bake "this call always blocks caller until completion" into protocols in ways that prevent pipelining).
- Do NOT add per-session service registry. The global `registry::lookup_service` is a known encapsulation hole — flag it in code comments, do not fix it here.
- Do NOT add kernel file/inode understanding. Kernel stays thin: threads, session_id, system_scope, block caps, IPC. VFS does all FS tree-walking.
- Do NOT add subtree-scoping of a shared writable filesystem. Each session gets its own writable block region. A read-only shared base region is granted as a cap.
- Do NOT add runtime ACL checks (session_id comparison in application code, path-based access control, identity-based filtering). Visibility is enforced by kernel scope (enumerate) and cap holdings (IPC endpoints). Application code never inspects "who is the caller" to decide visibility.
- Do NOT add shared writable state across sessions. If needed later, design a separate cap-gated service — not a shared FS path.
- Do NOT revert the existing session_id kernel property or change its semantics. session_id stays as-is; system_scope is added alongside it.
- Do NOT make `system_scope` a CapProfile bit. CapProfile is a userspace concept translated to kernel tokens at spawn. system_scope is a kernel thread property, same category as session_id. Procmgr sets it from the profile, but the kernel stores and checks it.
- Do NOT use `as any` / `@ts-ignore` / type suppression (Rust: no `unsafe` beyond existing FFI, no `unwrap` on IPC results in hot paths).
- Do NOT break the existing boot path. Session 0 (init, root-procmgr, root-VFS, compositor, login) must keep working throughout all phases. Phase 1 is pure addition; Phase 2 adds session-VFS without removing the global VFS until session-VFS is proven.

## Verification strategy

> Zero human intervention - all verification is agent-executed.
- Test decision: **harness integration** for end-to-end (login → cluuterm → shell → top shows shell); **rustc --test** for pure-logic (ProcQuery wire format, pid namespace arithmetic, system_scope filter). Framework: `scripts/harness_repeat.sh` for integration; `rustc --edition 2021 --test` for no_std unit tests (matches README §Tests pattern).
- Evidence: `.omo/evidence/task-<N>-per-session-vfs.<ext>` (`.txt` for test stdout, `.png` for QEMU screenshots).
- Critical regression test: after each phase, `scripts/harness_repeat.sh l2_login 3` must still pass (login is the critical path).
- Phase 1 specific: start `top`, start a second cluuterm, verify shell + micropython appear in top's process list as children of cluuterm.
- Phase 2 specific: from session X, `ls /proc` shows only session-X TIDs. From root, `ls /proc` shows all TIDs. Session X `cat /proc/<session-Y-tid>/stat` fails (NotFound or not visible).
- Phase 3 specific: root `top` shows session column; session `top` does not.

## Execution strategy

### Phased delivery (phases are sequential, todos within a phase are parallelizable)

**Phase 1 (3 tasks — pragmatic /proc fix, no architecture change):** T1, T2, T3
**Phase 2 (9 tasks — per-session VFS migration, depends on Phase 1):** T4, T5, T6, T7, T8, T9, T10, T11, T12
**Phase 3 (4 tasks — top display + per-session PIDs, depends on Phase 2):** T13, T14, T15, T16

### Dependency matrix
| Todo | Depends on | Blocks | Can parallelize with |
| --- | --- | --- | --- |
| T1 (session-procmgr ProcQuery dispatch) | — | T2 | T3 |
| T2 (root-procmgr proc_query forwarding) | T1 | T11 | T3 |
| T3 (session-procmgr real cid/pcid) | — | T13 | T1, T2 |
| T4 (kernel system_scope) | — | T9, T11 | T5, T6, T7 |
| T5 (devmgr service skeleton) | — | T8 | T4, T6, T7 |
| T6 (session-VFS crate) | — | T8 | T4, T5, T7 |
| T7 (ProcQueryAll session_id wire) | — | T12, T15 | T4, T5, T6 |
| T8 (devmgr block-region grants + session-VFS spawn wiring) | T5, T6 | T9 | T4, T7 |
| T9 (root-procmgr session-VFS spawn in handle_session_create) | T4, T8 | T12 | T7, T10 |
| T10 (root-procmgr ProcQueryAll cap-gate on miss) | T2 | T12 | T7, T8, T9 |
| T11 (root-VFS system_scope enumerate + ProcQueryAll stat) | T4, T2 | T12 | T7, T8, T9, T10 |
| T12 (retire global VFS routing for non-system sessions) | T9, T10, T11 | T16 | T13, T14, T15 |
| T13 (session-procmgr per-session PID namespace) | T3 | T15 | T14 |
| T14 (top: session-scoped display) | — | T16 | T13, T15 |
| T15 (top: root system_scope display + session column) | T7, T13 | T16 | T14 |
| T16 (harness: end-to-end verification) | T12, T14, T15 | — | — |

## Todos

> Implementation + Test = ONE todo. Never separate.

- [ ] 1. session-procmgr: wire ProcQuery handler into dispatch
  What to do / Must NOT do: Add a `PROCMGR_PROC_QUERY_LABEL` arm to `session-procmgr/src/dispatch.rs` (currently falls through to `_ => Err(HandlerError::BadLabel)` at line 121). The handler already exists in `session-procmgr/src/proc_query.rs` (struct `ProcQuery` with `const LABEL = PROCMGR_PROC_QUERY_LABEL`, answers from `child_table` by `child_tid == target_tid`). Wire it the same way other handlers are wired in the dispatch match. Must NOT change the ProcQuery handler logic itself — it already works, it's just not dispatched. Must NOT add a new label constant — `PROCMGR_PROC_QUERY_LABEL` already exists in cluu_wire. **Reply wire format (KB `cluu-session-procmgr-send-reply-word-layout`):** session-procmgr has its own `send_reply` that was historically buggy (word-only vs payload branching — `words[0]` clobbered by payload_len for empty-payload replies). Verify the ProcQuery handler uses the correct reply path: if the reply carries a payload (stat content), `words[0] = payload_len`, data in `words[1..5]`; if word-only, data in `words[0..5]`. KB convention: "Don't reimplement send_reply in each procmgr — share one implementation." If session-procmgr's send_reply is still a separate implementation, audit it for the empty-payload bug before relying on it for ProcQuery replies.
  Parallelization: Phase 1 | Blocked by: — | Blocks: T2
  References: `session-procmgr/src/dispatch.rs:56-123` (dispatch match, add arm before the `_` fallthrough), `session-procmgr/src/proc_query.rs:33-65` (ProcQuery handler, already implements the logic), `session-procmgr/src/proc_query.rs:101-104` (the hard-coded cid=0/pcid=0 that T3 will fix). KB convention: `cluu-session-procmgr-send-reply-word-layout` (reply wire format: branch on payload emptiness before writing words; session-procmgr had a buggy duplicate send_reply). Explore agent bg_f294e330 confirmed this is dead code.
  Acceptance criteria: `cargo build -p session-procmgr` succeeds. Unit test: construct a SessionState with a child in child_table, send PROCMGR_PROC_QUERY_LABEL with QUERY_STAT and the child's TID, assert reply contains the child's stat fields (non-empty name, correct heap_pages). Verify `words[0]` is correct for both payload and word-only replies (KB `cluu-session-procmgr-send-reply-word-layout`). `rustc --edition 2021 --test` on a test module that exercises the dispatch arm.

- [ ] 2. root-procmgr: forward proc_query to session-procmgrs on tid_to_pid miss
  What to do / Must NOT do: In `root-procmgr/src/main.rs::handle_proc_query` (line 3248), at the `tid_to_pid` miss branch (line 3274-3278), instead of immediately returning NotFound, iterate `self.session_pmgr_endpoints` and forward the `PROCMGR_PROC_QUERY_LABEL` to each session-procmgr. Return the first successful reply. If all return NotFound, then return NotFound. Mirror the existing `PROCMGR_PG_SIGNAL_LABEL` fan-out pattern at lines 3199-3206. Must NOT add a cap check here — the cap gate is on the caller's ability to reach root-procmgr at all (IPC endpoint). Must NOT forward if the TID IS in tid_to_pid (fast path for root-procmgr's own children). Must NOT change the stat format — session-procmgr already returns the same format.
  Parallelization: Phase 1 | Blocked by: T1 | Blocks: T11
  References: `root-procmgr/src/main.rs:3248-3325` (handle_proc_query), `root-procmgr/src/main.rs:3271-3282` (tid_to_pid lookup + NotFound), `root-procmgr/src/main.rs:3199-3206` (PG_SIGNAL fan-out pattern to copy), `root-procmgr/src/main.rs:1973-1974` (session_pmgr_endpoints population). Explore agents bg_1164ef47 and bg_1d0055c3 confirmed no forwarding exists today.
  Acceptance criteria: `cargo build -p root-procmgr` succeeds. Integration: after T1+T2, `top` shows shell and micropython as children of cluuterm. Harness: `scripts/harness_repeat.sh l2_login 3` still passes (no regression).

- [ ] 3. session-procmgr: emit real cid and pcid in QUERY_STAT
  What to do / Must NOT do: In `session-procmgr/src/proc_query.rs::ProcQuery::handle` (around line 101-104), replace the hard-coded `cid=0 pcid=0` with real values. `pcid` = the parent process's PID (session-procmgr knows the parent because the spawn IPC arrives from the parent's thread — store parent_pid in `child_table`'s `ChildState` at spawn time in `spawn.rs:86-98`). `cid` = the child's own session-scoped container id (assign incrementally per session, or derive from pid). The stat format string at line ~104 becomes: `format!("{} ({}) {} {} {} {} {} {} {} {}\n", pid, name, state_char, cpu_ticks, heap_pages, other_pages, ppid, sid, cid, pcid)`. Must NOT use 0 for pcid if the parent is known — that orphans the child in top's tree. Must NOT use a global cid — cid is session-scoped.
  Parallelization: Phase 1 | Blocked by: — | Blocks: T13
  References: `session-procmgr/src/proc_query.rs:54-65` (QUERY_STAT handler), `session-procmgr/src/proc_query.rs:101-104` (hard-coded cid=0/pcid=0 with comment "session children appear as root-level in top"), `session-procmgr/src/spawn.rs:86-98` (child_table insert — add parent_pid to ChildState), `session-procmgr/src/child_table.rs:7-25` (ChildState struct — add parent_pid field), `top/src/main.rs:375-408` (parse_stat_line reads parts[6]=cid, parts[7]=pcid). Explore agent bg_1164ef47 confirmed the hard-coded zeros.
  Acceptance criteria: `cargo build -p session-procmgr` succeeds. Integration: `top` shows shell nested under cluuterm in the tree (not as a root-level entry). `parse_stat_line` extracts non-zero cid and pcid for session children.

- [ ] 4. kernel: add system_scope thread property + enumerate filter
  What to do / Must NOT do: Add a `system_scope: bool` field to the kernel's `Thread` struct (in `kernel/src/sched/thread_manager.rs` or wherever Thread is defined). Add a `thread_set_system_scope(thread_tok, bool)` syscall wrapper in `libcluu/src/syscall.rs` (alongside `thread_set_session` at line 1198). Extend `enumerate_live_tids_in_session` (thread_manager.rs:353-361) filter: `caller_session_id == 0 || t.session_id == caller_session_id || caller_system_scope`. The caller's system_scope is read from the caller's Thread struct. **system_scope is NOT a replacement for `session_id == 0`** — KB `cluu-session-scoped-thread-enumeration` documents that `session_id == 0` already means "root/system scope, sees all threads." system_scope is a *new, additional* property for a different case: a privileged thread (e.g. root user) living in a *non-zero* session X who should still see all threads. The three filter clauses cover three distinct cases: (1) `caller_session_id == 0` — boot services in session 0 (unchanged), (2) `t.session_id == caller_session_id` — normal session-scoped visibility (unchanged), (3) `caller_system_scope` — privileged cross-session visibility (new). **Timing (KB `cluu-thread-set-session-timing`):** `thread_set_system_scope` MUST be called after `thread_create` (suspended) and before `thread_resume` — same window as `thread_set_session`. Setting it after resume creates a race where the thread runs without system_scope and sees a wrong view. Setting it before create is impossible (no thread). Must NOT make system_scope a CapProfile bit — it's a kernel thread property, same category as session_id. Procmgr reads the cap profile and sets the kernel property at spawn, but the kernel stores and enforces it. Must NOT add a runtime check in application code — the kernel enforces it in enumerate. Must NOT change the existing `caller_session_id == 0` short-circuit.
  Parallelization: Phase 2 | Blocked by: — | Blocks: T9, T11
  References: `kernel/src/sched/thread_manager.rs:353-361` (enumerate_live_tids_in_session filter), `libcluu/src/syscall.rs:1184-1196` (thread_enumerate wrapper), `libcluu/src/syscall.rs:1198-1204` (thread_set_session — mirror this pattern for thread_set_system_scope). Kernel handler: `kernel/src/syscall/handlers.rs:3450-3507` (invoke_thread_enumerate — resolve caller's system_scope alongside session_id). KB conventions: `cluu-session-scoped-thread-enumeration` (the pattern this extends — session_id==0 is the existing system-scope escape hatch), `cluu-thread-set-session-timing` (timing: set between create and resume, same window).
  Acceptance criteria: `cargo build -p kernel` and `cargo build -p libcluu` succeed. Unit test: create threads with session_id=X and session_id=Y, enumerate from X without system_scope → only X threads. Enumerate from X with system_scope → all threads. `rustc --edition 2021 --test` on the filter logic. Audit: `rg "thread_resume" userspace/*procmgr*/` — every spawn path that sets system_scope must call it before resume (KB `cluu-thread-set-session-timing` audit guidance).

- [ ] 5. devmgr: new userspace service skeleton
  What to do / Must NOT do: Create `userspace/devmgr/` with a Cargo crate (mirror the structure of `userspace/vfs/` or `userspace/session-procmgr/`). The devmgr owns all hardware block devices (starts with the virtio-block device or ramdisk that VFS currently uses). At startup, devmgr probes hardware, registers as `devmgr:main` in the service registry. Exposes an IPC interface: `DEVMGR_GRANT_REGION_LABEL` — takes (session_id, region_type [base_ro|writable], size_hint) and returns a block-cap token scoped to a region. Must NOT put FS logic in devmgr — it only grants block regions, doesn't understand inodes/paths. Must NOT make devmgr session-scoped — it runs in session 0 (system service). Must NOT add a new kernel syscall for block caps — use the existing token derivation mechanism (token_derive_scoped, same as root-procmgr uses for VfsViewManager at main.rs:4156).
  Parallelization: Phase 2 | Blocked by: — | Blocks: T8
  References: `userspace/vfs/src/main.rs` (VFS startup pattern — devmgr mirrors this), `userspace/root-procmgr/src/main.rs:4156-4171` (token_derive_scoped pattern for sub-minting scoped caps), `userspace/init/src/wiring.rs` (where devmgr would be launched as a boot service, alongside VFS), `userspace/libcluu/src/cap.rs:16` (DEVICE cap bit — devmgr holds this).
  Acceptance criteria: `cargo build -p devmgr` succeeds. devmgr registers `devmgr:main` in the service registry at boot. Boot still succeeds (`scripts/harness_repeat.sh l2_login 1`).

- [ ] 6. session-VFS: new crate (thin VFS instance per session)
  What to do / Must NOT do: Create `userspace/session-vfs/` as a Cargo crate. Reuse the existing `vfs` crate's procfs backend and mount logic — session-VFS is a thinner instantiation of the same VFS code, configured at startup with: (a) session-procmgr endpoint token (for /proc stat), (b) block-region caps (for / and /tmp), (c) session_id from envelope. Mounts `/proc` backed by session-procmgr. Mounts block regions as `/`, `/tmp`. Registers as `session-vfs:main:{sid}` in the service registry. Must NOT duplicate the VFS code — share the `vfs` crate or extract shared modules. Must NOT hardcode root-procmgr's endpoint — session-VFS only knows its session-procmgr. Must NOT add session-checking logic — session-VFS IS its session, the kernel enforces scope. **Stack discipline (KB `cluu-vfs-readdir-stack-buffer`):** no_std, small stacks (CHILD_STACK_PAGES = 32 per `session-procmgr/elf_spawn.rs`). Never put >512 bytes on the stack. The existing `procfs.rs:253` has `let mut reply_buf = [0u8; 4096]` on the stack — the exact anti-pattern the KB warns about. Session-VFS must heap-allocate ALL IPC reply buffers (`Vec::with_capacity` + `resize`, not `[0u8; N]`). Audit the shared VFS code for stack-allocated buffers before reusing it.
  Parallelization: Phase 2 | Blocked by: — | Blocks: T8
  References: `userspace/vfs/src/main.rs` (VFS startup, mount logic, ProcfsBackend construction at line 352), `userspace/vfs/src/procfs.rs` (procfs backend — reuse, parameterize the procmgr_endpoint), `userspace/vfs/src/procfs.rs:253` (existing 4KB stack buffer — heap-allocate before reuse), `userspace/vfs/src/mount.rs` (mount table). KB convention: `cluu-vfs-readdir-stack-buffer` (no_std stack discipline: >512 bytes → heap; VfsClient::readdir had a 4KB stack frame that caused silent stack overflow in top). The key change: ProcfsBackend's `procmgr_endpoint` is set to the SESSION-procmgr's endpoint, not root-procmgr's.
  Acceptance criteria: `cargo build -p session-vfs` succeeds. Session-VFS can be spawned with a session envelope, mounts /proc, and `ls /proc` shows only session-scoped TIDs. No stack buffer >512 bytes in the new crate (grep for `[0u8;` and `[0u32;` — any array >512 bytes must be heap-allocated).

- [ ] 7. ProcQueryAll: extend wire format to carry session_id
  What to do / Must NOT do: In `root-procmgr/src/proc_query_all.rs` and `session-procmgr/src/proc_query_local.rs`, extend the `ProcInfo` struct (or equivalent wire type in cluu_wire) to carry `session_id: u32` alongside existing fields (pid, name, state, cpu_ticks, heap_pages, etc.). The aggregator (ProcQueryAll) already fans out to session-procmgrs and concatenates results — each session-procmgr's reply now includes its session_id. Must NOT change the per-session ProcQuery wire format (that's unchanged). Must NOT break existing ProcQueryAll callers — add session_id as a new field, existing fields stay in the same positions. **IPC payload limit (KB `cluu-stat-path-readdir-exceeds-ipc-message-max`):** `IPC_MESSAGE_MAX = 4096` bytes. ProcQueryAll aggregates across ALL sessions — with many processes the concatenated reply can exceed 4096. The existing ProcQueryAll may already handle this (verify); if not, implement chunking or a streaming protocol. Do NOT silently discard `reply_with_payload` errors on overflow (KB `cluu-stat-path-readdir-exceeds-ipc-message-max`: blkdev did `let _ =` on a failed reply and the caller deadlocked forever). Must NOT let the aggregator block forever on an oversized reply — log and send an error reply on overflow, same fix pattern as the KB gotcha.
  Parallelization: Phase 2 | Blocked by: — | Blocks: T12, T15
  References: `root-procmgr/src/proc_query_all.rs` (ProcQueryAll aggregator), `session-procmgr/src/proc_query_local.rs` (per-session child list, SESSION_PROCMGR_PROC_QUERY_LOCAL_LABEL), `userspace/cluu_wire/src/` (wire types — add session_id to ProcInfo). KB convention: `cluu-stat-path-readdir-exceeds-ipc-message-max` (IPC_MESSAGE_MAX=4096; oversized readdir reply deadlocked shell; never `let _ =` on reply_with_payload — log and send error reply).
  Acceptance criteria: `cargo build` succeeds for root-procmgr, session-procmgr, cluu_wire. ProcQueryAll reply includes session_id per entry. Verify: with >50 processes across >2 sessions, ProcQueryAll reply does not exceed IPC_MESSAGE_MAX or handles overflow gracefully (no deadlock, no silent drop).

- [ ] 8. devmgr: block-region grant API + session-VFS spawn wiring
  What to do / Must NOT do: Implement `DEVMGR_GRANT_REGION_LABEL` in devmgr: takes (session_id, region_type), derives a scoped block cap from the device's root block cap via `token_derive_scoped`, returns the cap token. Two region types: `BASE_RO` (read-only system region, same blocks for all sessions) and `WRITABLE` (per-session writable region, allocated from a pool or partition table). Wire root-procmgr's `handle_session_create` to call devmgr's grant API for the new session, then pass the granted block caps to session-VFS via the spawn envelope. Must NOT allocate overlapping writable regions for different sessions. Must NOT grant writable access to the base region.
  Parallelization: Phase 2 | Blocked by: T5, T6 | Blocks: T9
  References: `userspace/root-procmgr/src/main.rs:6157-6280` (handle_session_create — add devmgr grant + session-VFS spawn), `userspace/root-procmgr/src/main.rs:4110-4227` (spawn_session_procmgr_for — mirror this pattern for session-VFS), `userspace/root-procmgr/src/main.rs:4156-4171` (token_derive_scoped pattern).
  Acceptance criteria: Session creation grants a base-ro cap and a writable cap to the session-VFS. Session-VFS can read the base region and read/write its writable region. Session-VFS cannot read another session's writable region (kernel enforces block-cap bounds).

- [ ] 9. root-procmgr: spawn session-VFS in handle_session_create
  What to do / Must NOT do: In `root-procmgr/src/main.rs::handle_session_create` (line 6265, after `self.spawn_session_procmgr_for(session_id, ...)`), add a call to `self.spawn_session_vfs_for(session_id, &req.user_name, block_caps)` — a new method mirroring `spawn_session_procmgr_for` (line 4110). Session-VFS gets: session_id=X, no system_scope, session-procmgr's endpoint token, block-region caps from devmgr. Must NOT give session-VFS system_scope. Must NOT give session-VFS root-procmgr's endpoint. Must NOT spawn session-VFS before session-procmgr (session-VFS needs session-procmgr's endpoint at startup).
  Parallelization: Phase 2 | Blocked by: T4, T8 | Blocks: T12
  References: `root-procmgr/src/main.rs:6265` (insert spawn_session_vfs_for call after spawn_session_procmgr_for), `root-procmgr/src/main.rs:4110-4227` (spawn_session_procmgr_for — template for spawn_session_vfs_for).
  Acceptance criteria: Login creates both session-procmgr and session-VFS. Session-VFS registers `session-vfs:main:{sid}`. `ls /proc` from inside the session shows only session TIDs.

- [ ] 10. root-procmgr: ProcQueryAll fan-out on proc_query miss (no ACL — endpoint reachability is the gate)
  What to do / Must NOT do: In `root-procmgr/src/main.rs::handle_proc_query`, on `tid_to_pid` miss (the forwarding path from T2), fan out via ProcQueryAll (already wired in proc_query_all.rs) to all session-procmgrs. Return the first successful reply; if all return NotFound, return NotFound. **Must NOT add a cap check inside handle_proc_query** — KB convention `cluu-procmgr-proc-query-acl-gatekeeper-mistake` explicitly removed ACL from proc_query: "Visibility is enforced at readdir time (kernel). If you can discover a TID via readdir, you can read its stat — by design." The protection in the per-session-VFS architecture is **endpoint reachability**: session-VFS doesn't hold root-procmgr's IPC token, so it physically cannot call handle_proc_query at all. Only root-VFS (session 0, holds the token) can reach root-procmgr. No application-level check needed — the cap is "do you have the IPC endpoint token," which is a kernel-capability check, not a runtime ACL. Must NOT synthesize rows from partial data on TID miss — return NotFound if all fan-out targets miss (per KB `cluu-procmgr-proc-query-acl-gatekeeper-mistake` Bug 2: "TID ≠ PID confusion"). Must NOT change the stat format.
  Parallelization: Phase 2 | Blocked by: T2 | Blocks: T12
  References: `root-procmgr/src/main.rs:3248-3325` (handle_proc_query), `root-procmgr/src/proc_query_all.rs` (ProcQueryAll aggregator — already wired, already fans out), `root-procmgr/src/main.rs:3199-3206` (PG_SIGNAL fan-out — the proven forwarding pattern, KB `cluu-pg-signal-not-forwarded-to-session-procmgr`). KB conventions: `cluu-procmgr-proc-query-acl-gatekeeper-mistake` (no ACL in proc_query — kernel enumerate is the gatekeeper), `cluu-pg-signal-not-forwarded-to-session-procmgr` (forwarding pattern, unguarded).
  Acceptance criteria: Root-VFS querying any TID gets the stat (local via tid_to_pid, or cross-session via ProcQueryAll fan-out). Session-VFS cannot reach root-procmgr's proc_query endpoint at all (no IPC token — verify by attempting the call and confirming it fails at the IPC layer, not inside handle_proc_query).

- [ ] 11. root-VFS: system_scope enumerate + ProcQueryAll stat routing
  What to do / Must NOT do: Root-VFS (the existing VFS code, running in session 0) gets `system_scope = true` at spawn (set by init or root-procmgr, from the SUPERVISOR cap profile). Root-VFS `/proc` readdir: `thread_enumerate(own token)` → with system_scope, kernel returns ALL threads (all sessions). Root-VFS `/proc/<tid>/stat`: for session-0 TIDs, existing path (root-procmgr tid_to_pid). For non-session-0 TIDs, root-procmgr's ProcQueryAll fan-out (T10). Root-VFS must determine which path to take — but must NOT do this by checking the TID's session (that would be a runtime ACL). Instead, root-procmgr handles the routing internally: tid_to_pid hit → local, miss → ProcQueryAll (T10 already handles this). So root-VFS just calls root-procmgr for all stats; root-procmgr decides local vs. fan-out. Must NOT add session-checking in VFS.
  Parallelization: Phase 2 | Blocked by: T4, T2 | Blocks: T12
  References: `userspace/vfs/src/procfs.rs:310-315` (query_tid_list — with system_scope, enumerate sees all), `userspace/vfs/src/procfs.rs:253-276` (query_procmgr — unchanged, still calls root-procmgr), `userspace/vfs/src/main.rs:352` (procmgr_endpoint — unchanged for root-VFS, still root-procmgr). `userspace/init/src/wiring.rs` (set system_scope on root-VFS thread at spawn).
  Acceptance criteria: Root-VFS `ls /proc` shows all sessions' TIDs. Root-VFS `cat /proc/<any-tid>/stat` returns the stat (via root-procmgr → tid_to_pid or ProcQueryAll).

- [ ] 12. Retire global VFS routing for non-system sessions
  What to do / Must NOT do: After session-VFS is proven (T9), update session-procmgr's spawn path (elf_spawn.rs) to set the child's VFS endpoint to session-VFS (not the global VFS). Children spawned inside a session use session-VFS for all VFS IPC. The global VFS (root-VFS) only serves session-0 processes. Must NOT remove root-VFS — it still serves system services. Must NOT break session-0 processes — they still use root-VFS. Must NOT change the VFS IPC protocol — same protocol, different endpoint.
  Parallelization: Phase 2 | Blocked by: T9, T10, T11 | Blocks: T16
  References: `session-procmgr/src/elf_spawn.rs:405-445` (VFS_SET_VIEW for child — set to session-VFS endpoint), `session-procmgr/src/elf_spawn.rs:452` (thread_set_session — already sets session, now also ensure VFS endpoint is session-VFS). `userspace/vfs/src/main.rs:352` (root-VFS endpoint — still used by session-0 processes).
  Acceptance criteria: Session processes' VFS IPC goes to session-VFS. Session-0 processes' VFS IPC goes to root-VFS. `scripts/harness_repeat.sh l2_login 3` passes.

- [ ] 13. session-procmgr: per-session PID namespace
  What to do / Must NOT do: Session-procmgr already computes `pid_base` from the session_id at root-procmgr/main.rs:4118-4119 (`pid_base = (session_id & 0xFF) << LOCAL_BITS`). Ensure session-procmgr assigns PIDs within its namespace (pid_base..pid_base + (1 << LOCAL_BITS) - 1). PIDs are NOT globally unique — they're unique within a session. Root's cross-session view uses (session_id, pid) tuples. Must NOT change the pid_base computation. Must NOT make PIDs globally unique (that would leak session identity into the PID). Must NOT change the per-session ProcQuery stat format — PID is already per-session.
  Parallelization: Phase 3 | Blocked by: T3 | Blocks: T15
  References: `root-procmgr/src/main.rs:4118-4119` (pid_base computation), `procmgr-common/src/pid.rs` (LOCAL_BITS constant, PID namespace arithmetic), `session-procmgr/src/spawn.rs` (where PIDs are assigned to children).
  Acceptance criteria: Two sessions can have the same PID (e.g., both have PID 1 for their session leader). Root's ProcQueryAll distinguishes them by session_id.

- [ ] 14. top: session-scoped display (local PIDs, no session column)
  What to do / Must NOT do: `top` running inside a session already reads session-VFS `/proc` after Phase 2. Verify that the display works correctly: local PIDs, session-scoped tree (cluuterm → shell → micropython). No session column needed — top only sees one session. Must NOT add session column to session-scoped top. Must NOT add cross-session logic to top — top just reads /proc, the scope is enforced by the kernel + session-VFS.
  Parallelization: Phase 3 | Blocked by: — | Blocks: T16
  References: `userspace/top/src/main.rs:324-373` (read_all_proc_stats — reads /proc via VFS, unchanged), `userspace/top/src/main.rs:375-408` (parse_stat_line — unchanged).
  Acceptance criteria: Session-scoped `top` shows cluuterm, shell, micropython in a tree. Does NOT show system services (vfs, compositor, login).

- [ ] 15. top: root system_scope display + session column
  What to do / Must NOT do: When `top` runs with `system_scope` (root user), it reads root-VFS `/proc` which shows all sessions. Extend `top`'s display to add a `SID` column showing the session_id from ProcQueryAll's extended wire format (T7). The column goes between `PCID` and `NAME` (or at the end — match the existing layout). Root top shows all processes across all sessions, each labeled with its session. Must NOT add the session column for non-root top (top doesn't know its scope — it just reads /proc and if /proc only shows one session, there's no session_id to display). Actually: top can detect system_scope by checking if /proc contains TIDs from multiple sessions (if ProcQueryAll returns session_id, top shows the column when >1 session_id appears). Must NOT hardcode a "root mode" flag.
  Parallelization: Phase 3 | Blocked by: T7, T13 | Blocks: T16
  References: `userspace/top/src/main.rs:181-230` (frame rendering — add SID column), `userspace/top/src/main.rs:375-408` (parse_stat_line — parse session_id from extended stat format), `userspace/cluu_wire/src/` (ProcInfo with session_id from T7).
  Acceptance criteria: Root `top` shows all sessions with a SID column. Session `top` does not show a SID column. PIDs may repeat across sessions but (SID, PID) is unique.

- [ ] 16. Harness: end-to-end verification
  What to do / Must NOT do: Add a harness case `l2_per_session_vfs` (or extend an existing login test) that: (1) logs in, starts top, verifies cluuterm + shell appear; (2) starts a second cluuterm, verifies shell in that cluuterm appears; (3) starts micropython, verifies it appears as child of shell; (4) from root, verifies all sessions visible with session column. Run `scripts/harness_repeat.sh l2_per_session_vfs 3`. Must NOT delete or modify existing harness cases. Must NOT skip the regression suite — run `scripts/harness_suite.sh` or at minimum `l2_login` x3 after all changes.
  Parallelization: Phase 3 | Blocked by: T12, T14, T15 | Blocks: —
  References: `scripts/harness_repeat.sh` (harness runner), `scripts/harness_suite.sh` (full suite).
  Acceptance criteria: `l2_per_session_vfs` passes 3/3. `l2_login` passes 3/3 (no regression). Full suite: no new failures beyond the 3 known tracked failures (#78, #70, #39).

---

## Execution status (2026-07-03)

### Phase 1 — SHIPPED (commits `fea9ca17`)

T1, T2, T3 complete. The reported bug (shell/micropython invisible in top) is fixed:
- session-procmgr's dead `ProcQuery` handler is wired into dispatch + `lib.rs`
- root-procmgr forwards proc_query to session-procmgrs on `tid_to_pid` miss
- session-procmgr emits real `cid`/`pcid` so top nests children under their parent

Verified: `l2_login` 3/3, `pm_proc_query_all_cap` 3/3, `l2_jobs_basic` 1/1. Pre-existing failures (`l2_jobs`, `l2_fg`, `l2_bare_cmd`, `l2_cluufile_match`) confirmed failing on previous commit — not regressions.

### Phase 2 — PARTIAL (commit `9e2c0380`)

T4 complete: kernel `system_scope` thread property + three-clause enumerate filter + `ThreadSetSystemScope` syscall. Infrastructure for future root-user cross-session visibility.

T7 already done: `ProcQueryAllReply` already carries `(u8 sid, ProcInfo)` tuples — wire format was already correct.

T10 already done by T2: root-procmgr forwarding on `tid_to_pid` miss IS the fan-out (unguarded, endpoint reachability is the gate per KB convention).

T11 no-op: root-VFS runs in session 0, which already grants system scope via the first filter clause (`caller_session_id == 0`). `system_scope` is for future root users in non-zero sessions.

### Phase 2 — DEFERRED (requires kernel block-range cap type)

T5, T6, T8, T9, T12 deferred. Blocker discovered during T5 implementation:

`token_derive_scoped` (libcluu/src/syscall.rs:1074) only works for `VfsViewManager` tokens — it scopes VFS mount paths (ROOT, DEV), not block device ranges. There is no "block-range cap" type in the kernel. The plan's assumption that devmgr would use `token_derive_scoped` for block regions was wrong.

Without devmgr, per-session VFS can't deliver FS isolation — it would only buy `/proc` isolation at the cost of a full 5000-line VFS process (128MB cache, grant buffers, ring pool) per session. That's poor value. The actual bug is already fixed by Phase 1.

**Deferred milestone:** kernel block-range cap type → devmgr service → per-session VFS with real FS isolation. This is a kernel extension and should be its own plan. The kernel is supposed to be near-freeze, so this needs explicit scheduling.

### Phase 3 — DEFERRED (depends on Phase 2)

T13, T14, T15, T16 deferred. Per-session PIDs and top display changes depend on the per-session VFS migration.

---

## Knowledge-base references (consulted during planning)

The following notes from `~/agentic-knowledge/` were read before writing this plan and directly shaped the todos above. Future executors must re-read them before implementation.

| KB note | Type | How it shaped this plan |
|---|---|---|
| `patterns/cluu/cluu-session-scoped-thread-enumeration` | pattern | T4: `session_id == 0` already means system scope. `system_scope` is an *additional* property for privileged non-session-0 threads, not a replacement. Three-clause filter. |
| `gotchas/cluu/cluu-procmgr-proc-query-acl-gatekeeper-mistake` | gotcha | T10: **removed the cap-gate from handle_proc_query.** ACL was deliberately removed from proc_query — kernel enumerate is the gatekeeper. Per-session-VFS protection is endpoint reachability (no IPC token = no call), not an application-level check. |
| `gotchas/cluu/cluu-pg-signal-not-forwarded-to-session-procmgr` | gotcha | T2, T10: the unguarded forwarding pattern (`session_pmgr_endpoints` fan-out on local miss). proc_query forwarding mirrors this proven pattern. |
| `gotchas/cluu/cluu-thread-set-session-timing` | gotcha | T4, T9: `thread_set_system_scope` must be called between `thread_create` (suspended) and `thread_resume` — same window as `thread_set_session`. Setting after resume = race / privilege escalation. Audit all spawn call sites. |
| `gotchas/cluu/cluu-session-procmgr-send-reply-word-layout` | gotcha | T1: session-procmgr has its own `send_reply` that was historically buggy (word-only vs payload branching). Verify ProcQuery replies use the correct wire format. Convention: don't reimplement send_reply per procmgr. |
| `gotchas/cluu/cluu-vfs-readdir-stack-buffer` | gotcha | T6: no_std, small stacks. Never >512 bytes on stack. Existing `procfs.rs:253` has a 4KB stack buffer — heap-allocate before reuse in session-VFS. |
| `gotchas/cluu/cluu-stat-path-readdir-exceeds-ipc-message-max` | gotcha | T7: IPC_MESSAGE_MAX = 4096. ProcQueryAll fan-out can exceed this with many processes. Never `let _ =` on reply_with_payload — log and send error reply on overflow. |

### KB write obligation (post-implementation)

Per the AGENT-CONTRACT in `~/agentic-knowledge/_meta/AGENT-CONTRACT.md`, after this plan is implemented and verified, write or update:
- **decision note** `decisions/cluu/cluu-per-session-vfs-architecture.md` — why per-session VFS + devmgr + system_scope was chosen over proxy-VFS / kernel-file-caps / shared-FS-subtree-scoping. Record the seL4/Fuchsia comparison and the session_id-as-kernel-property pragmatic exception.
- **pattern note** `patterns/cluu/cluu-per-session-vfs-isolation.md` — the per-session VFS + devmgr block-cap + kernel-enumerate-scope pattern, parallel to the existing `cluu-session-scoped-thread-enumeration` pattern note.
- **gotcha note** (if any new gotchas are discovered during implementation) — follow the existing CLUU gotcha format.
