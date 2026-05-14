# Plan 3: Pre-/post-login compositor swap (user-mode compositor)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** At login on VT4, procmgr kills the system-mode compositor (the one autostarted at boot to host the login modal) and spawns a fresh compositor under the user's envelope inside the session container. The user-mode compositor takes over VT4. On exit (logout), procmgr respawns the system compositor — handled in Plan 5.

**Architecture:** No new compositor binary — the same `/var/images/compositor/bin/compositor` runs in both modes. What differs is the VFS view + envelope env it inherits. System compositor's view comes from the autostart manifest path; user compositor's view comes from the SESSION_LOGIN's envelope. Procmgr orchestrates: on session_kind=1 login, send `THREAD_KILL` to the system compositor's main thread token, wait for its exit cookie, then spawn the user-mode compositor as the first child of the new session container.

**Tech Stack:** Rust, existing procmgr `thread_kill` + exit_endpoint, compositor manifest, harness.

**Depends on:** Plan 1 (Bug C closed), Plan 2 (envelope substitution).

---

## Pre-flight context

Files touched:

| File | Action |
|---|---|
| `userspace/procmgr/src/main.rs` | At SESSION_LOGIN session_kind=1: kill_system_compositor → wait for exit → spawn user compositor with envelope; record pid as session root |
| `userspace/compositor/src/main.rs` (or wherever the entry is) | Read SESSION-mode-flag from ProcessInfo params; in system mode host only the login modal area, in user mode render the full desktop chrome |
| `etc/autostart.toml` | Mark the boot compositor with a `session_mode=system` param |
| `containers/compositor/Cluufile` | Allow the `session_mode` param slot |
| `scripts/harness_*` | New marker: `l2_compositor_swap_login` verifies two different compositor pids before/after login |

Key existing primitives:
- `thread_kill(token)` via `libcluu::syscall::thread_kill` — procmgr already uses on container teardown (`grep -n thread_kill userspace/procmgr/src/main.rs`).
- Exit notifications via `procmgr.exit_endpoint` (see `main.rs:1619 poll_exit_notifications`).
- Compositor's main-thread token: held in procmgr's container record at `container_instances[<compositor_cid>].pid` → can be looked up against `pid_to_tid` and then to the thread_token via `pid_to_thread_token` (verify the exact map name during implementation).

---

## Task 1: Compositor session-mode flag

**Files:**
- Modify: `userspace/libcluu/src/boot.rs` — add `PARAM_SESSION_MODE` slot constant (just a usize index).
- Modify: `userspace/compositor/src/main.rs` — read it and gate behavior.

- [ ] **Step 1: Reserve a param slot**

In `userspace/libcluu/src/boot.rs`, find the existing `PARAM_*` constants (around lines 198-240). Pick the next unused index. Add:

```rust
/// Compositor session-mode discriminator.
///   0 = system (autostarted at boot, hosts login modal only)
///   1 = user   (spawned at SESSION_LOGIN under user envelope, full desktop)
/// Default 0 when absent.
pub const PARAM_SESSION_MODE: usize = 16;  // verify next-free index
```

- [ ] **Step 2: Compositor reads it at startup**

In `userspace/compositor/src/main.rs` (locate `pub extern "C" fn main` or `fn run`), early in init:

```rust
    let info = libcluu::boot::process_info();
    let session_mode = info.params[libcluu::boot::PARAM_SESSION_MODE] as u8;
    let _ = libcluu::debug_print(&alloc::format!(
        "compositor: session_mode={} ({})",
        session_mode,
        if session_mode == 1 { "user" } else { "system" },
    ));
```

Stash on the compositor state struct so render code can branch later (Task 4).

- [ ] **Step 3: Build + smoke**

```bash
cargo xtask build 2>&1 | tail -3
HARNESS_FORCE_BUILD=0 MARKER_MODE=l2_compositor_smoke bash scripts/harness_run.sh
grep "session_mode" /tmp/cluu-serial-com2.log
```

Expected: `compositor: session_mode=0 (system)` at boot.

- [ ] **Step 4: Commit**

```bash
git add userspace/libcluu/src/boot.rs userspace/compositor/src/main.rs
git commit -m "compositor: read PARAM_SESSION_MODE at startup"
```

---

## Task 2: Autostart manifests carry session_mode=0; Cluufile allows it

**Files:**
- Modify: `etc/autostart.toml`
- Modify: `containers/compositor/Cluufile`

- [ ] **Step 1: Add session_mode=0 in autostart entry for compositor**

In `etc/autostart.toml`, find the `[[service]] name = "compositor"` block. Add a `params` table entry:

```toml
[[service]]
name = "compositor"
manifest = "/var/images/compositor/manifest.toml"
restart = "always"
params = { session_mode = 0 }
```

(If the existing format uses a different shape — list of `(key, value)` pairs — match that. Run `grep -A5 compositor etc/autostart.toml` to see.)

- [ ] **Step 2: Declare the param in Cluufile**

In `containers/compositor/Cluufile` (or whatever the compositor's manifest source is), add the parameter:

```
PARAM session_mode default=0
```

(Match the existing PARAM directive syntax — check another container's Cluufile that uses params.)

- [ ] **Step 3: Build + smoke + verify trace**

```bash
cargo xtask build 2>&1 | tail -3
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_compositor_smoke bash scripts/harness_run.sh
grep "session_mode" /tmp/cluu-serial-com2.log
```

Expected: still `session_mode=0`, no regression.

- [ ] **Step 4: Commit**

```bash
git add etc/autostart.toml containers/compositor/Cluufile
git commit -m "compositor: autostart carries session_mode=0"
```

---

## Task 3: Procmgr kills system compositor at session_kind=1 login

**Files:**
- Modify: `userspace/procmgr/src/main.rs` — locate `handle_session_login` session_kind=1 branch (around line 2110). Add the kill step before the user-compositor spawn.

- [ ] **Step 1: Failing test idea (manual): boot → login on VT4 → verify system compositor pid is gone**

This is checked at the end via harness (Task 6). For now, focus on the implementation.

- [ ] **Step 2: Add a helper to find + kill the system compositor**

In `impl ProcessManager`, near the existing teardown helpers:

```rust
    /// Find the autostarted (system-mode) compositor's thread token and kill
    /// it. Blocks until the exit cookie is received via exit_endpoint, so the
    /// caller can spawn the user-mode compositor without VT4 contention.
    ///
    /// Returns Ok(()) when the system compositor is gone or wasn't running;
    /// Err otherwise.
    fn kill_system_compositor(&mut self) -> Result<()> {
        // Identify system compositor by image_path + container's session_id == 0
        // (system containers have no session attached).
        let system_compositor_cid = self.container_instances
            .iter()
            .find(|(_, c)| {
                c.image_path == "/var/images/compositor/bin/compositor"
                    && c.session_id == 0
            })
            .map(|(cid, _)| *cid);

        let cid = match system_compositor_cid {
            Some(c) => c,
            None => {
                let _ = debug_print("procmgr: kill_system_compositor: not running");
                return Ok(());
            }
        };

        let container = self.container_instances.get(&cid).unwrap();
        let pid = container.pid;
        let thread_token = match self.pid_to_thread_token.get(&pid) {
            Some(&t) => t,
            None => {
                let _ = debug_print(&format!(
                    "procmgr: kill_system_compositor: no thread_token for pid={}", pid
                ));
                return Err(Error::NotFound);
            }
        };
        let exit_cookie = container.exit_cookie;
        drop(container);

        let _ = debug_print(&format!(
            "procmgr: killing system compositor pid={} cookie={}", pid, exit_cookie
        ));
        if let Err(e) = libcluu::syscall::thread_kill(thread_token) {
            let _ = debug_print(&format!("procmgr: thread_kill failed: {:?}", e));
            return Err(e);
        }

        // Drain exit notifications until we see this cookie. Bound the wait.
        let deadline = self.clock_sample() + 2_000;  // 2 s
        loop {
            self.poll_exit_notifications()?;
            if !self.exit_table.contains_key(&exit_cookie) {
                let _ = debug_print("procmgr: system compositor reaped");
                return Ok(());
            }
            if self.clock_sample() >= deadline {
                let _ = debug_print("procmgr: system compositor reap TIMEOUT");
                return Err(Error::Timeout);
            }
        }
    }
```

(Field names — `pid_to_thread_token`, `container.exit_cookie` — verify exact names during implementation; adjust if different.)

- [ ] **Step 3: Call it at session_kind=1 login**

Locate the session_kind=1 spawn call in `handle_session_login` (around line 2232 where it calls `spawn_service_with_env` for cluuterm). Insert before it:

```rust
            if let Err(e) = self.kill_system_compositor() {
                let _ = debug_print(&format!(
                    "procmgr: SESSION_LOGIN kind=1 abort: kill_system_compositor {:?}", e
                ));
                reply_msg.words[0] = e.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }
```

- [ ] **Step 4: Build + smoke**

```bash
cargo xtask build 2>&1 | tail -3
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_cluuterm_login bash scripts/harness_run.sh
grep -E "killing system compositor|system compositor reaped" /tmp/cluu-serial-com2.log
```

Expected: both traces fire during login flow. VT4 may briefly show a black framebuffer between the kill and the next task's user compositor spawn — that's OK for now.

- [ ] **Step 5: Commit**

```bash
git add userspace/procmgr/src/main.rs
git commit -m "procmgr: kill system compositor at SESSION_LOGIN kind=1"
```

---

## Task 4: Spawn user compositor under session envelope

**Files:**
- Modify: `userspace/procmgr/src/main.rs` — same session_kind=1 branch.

- [ ] **Step 1: Spawn the user compositor first, then cluuterm**

Restructure the session_kind=1 path so the order is:
1. Auth + envelope resolve (existing).
2. `kill_system_compositor()` (from Task 3).
3. Create `session_cid`.
4. Spawn compositor with `session_mode=1` param, view = resolved user view, env = resolved user env. Record its pid as session root.
5. After compositor's `ready` trace appears (or after a fixed delay; better: wait for compositor to publish its endpoints), spawn cluuterm as a sibling under the same session_cid.

For Step 4 specifically, add this code right after `kill_system_compositor`:

```rust
            let session_cid = self.next_container_id();
            // Spawn user-mode compositor as the session root.
            let (comp_argv_payload, comp_argc) = build_argv_payload(&["compositor"]);
            let param_overrides: &[(usize, u64)] = &[
                (libcluu::boot::PARAM_SESSION_MODE, 1),
            ];
            let comp_spawn = self.spawn_service_with_env(
                "/var/images/compositor/bin/compositor",
                DEFAULT_PRIORITY,
                &comp_argv_payload,
                comp_argc,
                &user_env,
                user_envc,
                self.procmgr_main_tid()?,  // from Plan 1 Task 2
                self.next_spawn_seq(),
                self.clock_sample(),
                &[],                       // no FDAC for compositor
                profile,
                0,
                0,
                param_overrides,
                Some(&view_mounts),
                &[],
                &[],
                THREAD_CREATE_START_SUSPENDED,
            );
            let (comp_thread_token, comp_cookie, comp_pid, _) = match comp_spawn {
                Ok(t) => t,
                Err(e) => {
                    let _ = debug_print(&format!(
                        "procmgr: SESSION_LOGIN kind=1 compositor spawn failed: {:?}", e
                    ));
                    reply_msg.words[0] = e.to_errno() as usize;
                    if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                    return Ok(());
                }
            };
            self.pid_to_container_id.insert(comp_pid, session_cid);
            self.install_view_and_run(comp_thread_token, &view_mounts, profile, session_cid);
            self.session_table.insert(session_cid, SessionEntry {
                container_id: session_cid,
                shell_cid: 0,   // not yet — cluuterm follows
                pid: comp_pid,
                username: username.clone(),
                profile,
                vt_index: 4,
                stdin_endpoint: 0, // compositor has no stdin endpoint
            });
            // Record the compositor instance.
            self.container_instances.insert(session_cid, ContainerInstance {
                name: String::from("compositor"),
                instance_name: self.next_instance_name(session_cid, "compositor"),
                session_id: session_cid,
                container_id: session_cid,
                parent_container_id: 0,
                pid: comp_pid,
                image_path: String::from("/var/images/compositor/bin/compositor"),
                mapped_pages: (SERVICE_STACK_SIZE / PAGE_SIZE + 1) as u32,
                restart_policy: RestartPolicy::Never,
                restart_count: 0,
                last_exit_code: 0,
                restart_attempt_start: 0,
                quota: QuotaSpec::default(),
                live_processes: 1,
            });
            let _ = debug_print(&format!(
                "procmgr: user compositor spawned pid={} session_cid={}", comp_pid, session_cid
            ));
            let _ = comp_cookie;  // wired into the exit table by spawn_service_with_env
```

- [ ] **Step 2: Defer cluuterm spawn — make it the next sibling**

Keep the existing cluuterm spawn after the compositor block. Adjust its container_id assignment to use `session_cid` as `parent_container_id` and a new sub-container id for cluuterm itself. The exact diff:

Find the line `let session_cid = self.next_container_id();` further down in the kind=1 path (it's currently in the cluuterm spawn block) and DELETE it — we already created session_cid above. Use it directly for cluuterm's parent_container_id.

- [ ] **Step 3: Build + smoke**

```bash
cargo xtask build 2>&1 | tail -3
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_cluuterm_login bash scripts/harness_run.sh
grep -E "user compositor spawned|cluuterm: start|session_mode=1" /tmp/cluu-serial-com2.log
```

Expected:
- `procmgr: user compositor spawned pid=N session_cid=M`
- `compositor: session_mode=1 (user)`
- `cluuterm: start` shortly after.

- [ ] **Step 4: Commit**

```bash
git add userspace/procmgr/src/main.rs
git commit -m "procmgr: spawn user-mode compositor under session container"
```

---

## Task 5: Wait for compositor readiness before cluuterm spawn

**Files:**
- Modify: `userspace/procmgr/src/main.rs` — between compositor spawn and cluuterm spawn in session_kind=1.

- [ ] **Step 1: Subscribe to compositor's "ready" registry event**

The system compositor publishes `compositor:client` via the registry (existing code). The user compositor will do the same. Procmgr can subscribe + wait synchronously:

```rust
            // Wait up to 2 s for the user compositor to publish its endpoints.
            let deadline = self.clock_sample() + 2_000;
            let mut comp_client_ep: usize = 0;
            while self.clock_sample() < deadline {
                if let Ok(ep) = registry::subscribe_output("compositor", "client") {
                    if ep != 0 { comp_client_ep = ep; break; }
                }
                let _ = libcluu::syscall::yield_cpu();
            }
            if comp_client_ep == 0 {
                let _ = debug_print(
                    "procmgr: user compositor failed to publish 'client' endpoint within 2s"
                );
                // Best-effort: continue anyway; cluuterm will retry registry lookup
            }
```

- [ ] **Step 2: Build + smoke**

```bash
cargo xtask build 2>&1 | tail -3
HARNESS_FORCE_BUILD=0 MARKER_MODE=l2_cluuterm_login bash scripts/harness_run.sh
grep -E "compositor:client|user compositor|cluuterm: start" /tmp/cluu-serial-com2.log
```

- [ ] **Step 3: Commit**

```bash
git add userspace/procmgr/src/main.rs
git commit -m "procmgr: wait for user compositor 'client' endpoint before cluuterm spawn"
```

---

## Task 6: Harness marker — verify compositor swap

**Files:**
- Modify: `scripts/harness_case_defaults.sh` + `scripts/harness_run.sh`

- [ ] **Step 1: Add marker**

In `scripts/harness_case_defaults.sh`:

```bash
            l2_compositor_swap_login)
                TEST_COMMAND=""
                # Use the same credentials as l2_cluuterm_login. Two
                # distinct `compositor: session_mode=...` traces must
                # appear: first session_mode=0 at boot, then
                # session_mode=1 after login. Plus killing/respawn traces.
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 5\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 1\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 3'
                ;;
```

In `scripts/harness_run.sh`:

```bash
    l2_compositor_swap_login)
        required_markers=(
            "TSC calibrated"
            "compositor: session_mode=0 (system)"
            "killing system compositor"
            "system compositor reaped"
            "compositor: session_mode=1 (user)"
            "user compositor spawned"
            "cluuterm: start"
        )
        ;;
```

- [ ] **Step 2: Run**

```bash
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_compositor_swap_login bash scripts/harness_run.sh
```

Expected: all required markers fire in order.

- [ ] **Step 3: Commit harness**

```bash
git add scripts/harness_case_defaults.sh scripts/harness_run.sh
git commit -m "harness: l2_compositor_swap_login verifies VT4 compositor handoff"
```

---

## Task 7: Memory updates

**Files:**
- Add: `/home/vlb2bp/.claude/projects/-home-vlb2bp-git-cluu/memory/project_compositor_swap_2026_05_14.md`
- Edit: `MEMORY.md` index.

- [ ] **Step 1: Write memory**

```markdown
---
name: compositor-swap-system-to-user-2026-05-14
description: "System-mode compositor hosts login modal; killed at SESSION_LOGIN kind=1; user-mode compositor spawns under user envelope inside session container."
metadata:
  type: project
---

After plan 2026-05-14-plan3-compositor-swap, VT4 graphical sessions
have two compositor lifetimes: the boot-autostarted system-mode
compositor (`session_mode=0`) hosts only the login modal, then procmgr
sends THREAD_KILL on session_kind=1 login and spawns a fresh user-mode
compositor (`session_mode=1`) under the user envelope as the root of
the session container.

**Why:** Spec target — every user-facing process in a graphical session
runs under the user envelope. The pre-login compositor is the only
graphical service that gets system-mode rights, and only to render the
login modal.

**How to apply:** Don't add ad-hoc state to the system compositor —
anything the user touches must move into the user compositor or a
sibling app. If you need a feature in both, gate it on
`PARAM_SESSION_MODE`. Logout respawn of the system compositor is handled
by Plan 5.
```

Append to MEMORY.md.

- [ ] **Step 2: Edit `project_loginCC_session_2026_05_13.md`** — note the compositor swap as the answer to the "logout flow" T9 item.

---

## Self-review checklist

- `l2_compositor_swap_login` and `l2_cluuterm_login` both green.
- Other markers (`l2_text_shell_input`, `l2_cluuterm_shell_input`, etc.) unchanged.
- Two distinct compositor pids in `/tmp/cluu-serial-com2.log` for a single login.
- Memory MEMORY.md reflects the new architecture.
