# Plan 5: Logout teardown + pre-login compositor respawn

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the VT4 graphical session lifecycle. When the user-mode compositor exits (clean logout, crash, or user-invoked `exit`), procmgr tears down the entire session container — killing every child (cluuterm, apps, app children) — then respawns the system-mode compositor so the next user can log in. Mirror this on VT0-3: when the shell exits, tty service shows the login prompt again.

**Architecture:** Hook into procmgr's existing exit-cookie handler. When a process exits and its container_id matches a registered session, walk `container_children[session_cid]` in reverse-dependency order, send THREAD_KILL to each, reap their exit cookies, drop session_table entry, then respawn the appropriate stand-in (system compositor for VT4, login prompt for VT0-3).

**Tech Stack:** Rust, existing exit notification flow, harness.

**Depends on:** Plans 1 + 2 + 3 (+ Plan 4 if you want to also test menu-spawned-child teardown).

---

## Pre-flight context

| File | Action |
|---|---|
| `userspace/procmgr/src/main.rs` | Extend exit handler to detect session-root exit; new helper `teardown_session_container`; new helper `respawn_pre_login_compositor` |
| `userspace/procmgr/src/main.rs` | Detect text VT shell exit; respawn tty login prompt (currently tty service does this implicitly because no one tears down the shell — verify the new path doesn't break it) |
| `scripts/harness_*` | Markers `l2_logout_graphical` and `l2_logout_text` |

Key existing functions:
- `poll_exit_notifications` at line 1619 — handles incoming `PROCMGR_EXIT_LABEL` messages.
- `should_restart_container` + `handle_restart_exit` — existing restart-policy machinery.
- Plan 3's `kill_system_compositor` and the auto-start mechanism (re-spawnable manifest entry).

---

## Task 1: Detect session-root exit

**Files:**
- Modify: `userspace/procmgr/src/main.rs` — `poll_exit_notifications` and the cookie→action branch around line 1680.

- [ ] **Step 1: After the existing restart-check, add a session-root branch**

Find the code that processes an exit cookie (after `if self.should_restart_container(cookie, exit_code) { ... }`). Add right after:

```rust
        // Check whether the exiting pid is a session root (compositor for
        // graphical, shell for text). If so, tear down the whole session
        // container and respawn the stand-in (system compositor or login
        // prompt).
        let exiting_pid = self.cookie_to_pid.get(&cookie).copied();
        if let Some(pid) = exiting_pid {
            let session_root_match = self.session_table
                .iter()
                .find(|(_, e)| e.pid == pid)
                .map(|(cid, e)| (*cid, e.vt_index, e.profile))
                .clone();
            if let Some((session_cid, vt_index, _profile)) = session_root_match {
                let _ = debug_print(&format!(
                    "procmgr: session root pid={} exited, tearing down session_cid={} vt={}",
                    pid, session_cid, vt_index
                ));
                self.teardown_session_container(session_cid);
                if vt_index >= 4 {
                    // VT4 (or future graphical VTs): respawn the system compositor.
                    self.respawn_pre_login_compositor();
                } else {
                    // VT0-3: tty service still owns the VT; just notify it that
                    // the session ended so it can redraw "login:".
                    self.notify_tty_session_end(vt_index);
                }
                self.session_table.remove(&session_cid);
                return Ok(());
            }
        }
```

- [ ] **Step 2: Add `teardown_session_container`**

On `impl ProcessManager`:

```rust
    /// Kill every child of the given session container, in reverse-spawn
    /// order, waiting briefly for each exit cookie. Drops view + envelope
    /// state for the container last.
    fn teardown_session_container(&mut self, session_cid: u64) {
        let children = self.container_children.remove(&session_cid).unwrap_or_default();
        for &child_cid in children.iter().rev() {
            let pid = match self.container_instances.get(&child_cid).map(|c| c.pid) {
                Some(p) => p,
                None => continue,
            };
            let thread_token = match self.pid_to_thread_token.get(&pid).copied() {
                Some(t) => t,
                None => continue,
            };
            let _ = debug_print(&format!(
                "procmgr: teardown_session: killing child_cid={} pid={}", child_cid, pid
            ));
            let _ = libcluu::syscall::thread_kill(thread_token);
        }
        // Drain exit cookies for up to 2 s; non-reaped children are leaked
        // (deliberately conservative — better than blocking forever).
        let deadline = self.clock_sample() + 2_000;
        while self.clock_sample() < deadline {
            // poll_exit_notifications already processes whatever lands
            let _ = self.poll_exit_notifications();
            if self.container_children.get(&session_cid).map(|v| v.is_empty()).unwrap_or(true) {
                break;
            }
        }
        self.container_instances.remove(&session_cid);
        // VFS view state for the container is dropped by VFS when no caller
        // remains; trigger an explicit cleanup IPC.
        let _ = send_vfs_container_cleanup(self.vfs_endpoint, session_cid, 1);
    }
```

- [ ] **Step 3: Add `respawn_pre_login_compositor`**

```rust
    /// Re-spawn the boot-style system compositor on VT4 after logout. Mirror
    /// of the autostart path but invoked on demand.
    fn respawn_pre_login_compositor(&mut self) {
        let (argv_payload, argc) = build_argv_payload(&["compositor"]);
        let param_overrides: &[(usize, u64)] = &[
            (libcluu::boot::PARAM_SESSION_MODE, 0),
        ];
        match self.spawn_service_with_env(
            "/var/images/compositor/bin/compositor",
            DEFAULT_PRIORITY,
            &argv_payload,
            argc,
            &[],     // default env
            0,
            0,       // owner_tid 0 → use defaults
            self.next_spawn_seq(),
            self.clock_sample(),
            &[],
            CapProfile::System,
            0,
            0,
            param_overrides,
            None,
            &[],
            &[],
            THREAD_CREATE_START_SUSPENDED,
        ) {
            Ok((thread_token, _cookie, pid, _)) => {
                let _ = debug_print(&format!(
                    "procmgr: respawned system compositor pid={}", pid
                ));
                let cid = self.next_container_id();
                self.pid_to_container_id.insert(pid, cid);
                self.install_view_and_run(thread_token, &ViewMountList::system_default(), CapProfile::System, cid);
                self.container_instances.insert(cid, ContainerInstance {
                    name: String::from("compositor"),
                    instance_name: self.next_instance_name(cid, "compositor"),
                    session_id: 0,
                    container_id: cid,
                    parent_container_id: 0,
                    pid,
                    image_path: String::from("/var/images/compositor/bin/compositor"),
                    mapped_pages: (SERVICE_STACK_SIZE / PAGE_SIZE + 1) as u32,
                    restart_policy: RestartPolicy::Always,
                    restart_count: 0,
                    last_exit_code: 0,
                    restart_attempt_start: 0,
                    quota: QuotaSpec::default(),
                    live_processes: 1,
                });
            }
            Err(e) => {
                let _ = debug_print(&format!(
                    "procmgr: respawn system compositor FAILED: {:?}", e
                ));
            }
        }
    }
```

(`ViewMountList::system_default()` may need to be added — what the autostart path uses. Look for the existing system-default view, often `build_view_from_envelope(...)` with a system profile or a hard-coded `ViewMountList::new()`.)

- [ ] **Step 4: Add `notify_tty_session_end`**

```rust
    /// Tell the tty service on `vt_index` that the session ended so it can
    /// redraw the "login:" prompt and accept new credentials.
    fn notify_tty_session_end(&mut self, vt_index: usize) {
        let ep = self.tty_endpoints[vt_index];
        if ep == 0 { return; }
        // Reuse the existing TTY_REGISTER_LABEL with zero foreground endpoint
        // to mean "session ended"; tty's wire_shell_stdin / configure_foreground
        // path was retired in Plan 1, so we need a small additional label
        // dedicated to "session ended" — TODO: define TTY_SESSION_ENDED_LABEL.
        // For now, send a no-op message and let tty fall back to login prompt
        // via its existing login state machine on shell-stdin endpoint
        // disappearing.
        let msg = Message::new(libcluu::ipc::TTY_REGISTER_LABEL, [0, 0, 0, 0, 0, 0], 1);
        let _ = libcluu::ipc::send(ep, &msg, libcluu::types::IpcFlags::empty());
    }
```

(Plan 1 removed wire_shell_stdin; this notification might need a new label to make tty redraw its prompt. Mark with a comment so the implementer adds `TTY_SESSION_ENDED_LABEL` if needed.)

- [ ] **Step 5: Build + smoke**

```bash
cargo xtask build 2>&1 | tail -5
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_cluuterm_login bash scripts/harness_run.sh
```

Login still works; logout marker tested next.

- [ ] **Step 6: Commit**

```bash
git add userspace/procmgr/src/main.rs
git commit -m "procmgr: session-root exit triggers teardown + stand-in respawn"
```

---

## Task 2: Harness marker — graphical logout

**Files:**
- Modify: `scripts/harness_case_defaults.sh` + `scripts/harness_run.sh`

- [ ] **Step 1: Add marker**

In `scripts/harness_case_defaults.sh`:

```bash
            l2_logout_graphical)
                TEST_COMMAND=""
                # Login, then close the cluuterm window via ctrl-alt-q (which
                # sends COMP_CLOSE_REQUEST), then close the compositor's only
                # window (= itself) — the compositor should exit on
                # last-window-destroyed, triggering session teardown.
                # Marker: "respawned system compositor" trace fires.
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 5\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 1\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 3\nsendkey ctrl-alt-q'
                ;;
```

In `scripts/harness_run.sh`:

```bash
    l2_logout_graphical)
        required_markers=(
            "TSC calibrated"
            "compositor: session_mode=1 (user)"
            "session root pid=.* exited"
            "teardown_session: killing"
            "respawned system compositor"
            "compositor: session_mode=0 (system)"
        )
        ;;
```

(The regex-style marker `session root pid=.* exited` requires the harness's marker matcher to support partial-line substrings — verify by looking at existing markers; most are literal substrings, which would match the prefix `session root pid=` too.)

- [ ] **Step 2: Run**

```bash
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_logout_graphical bash scripts/harness_run.sh
```

Expected: full chain fires. If `respawned system compositor` doesn't appear, debug by looking at exit cookie flow.

- [ ] **Step 3: Commit**

```bash
git add scripts/harness_case_defaults.sh scripts/harness_run.sh
git commit -m "harness: l2_logout_graphical verifies session teardown + respawn"
```

---

## Task 3: Compositor exits on logout signal

**Files:**
- Modify: `userspace/compositor/src/main.rs` — when the last window is destroyed (or a system menu "Logout" entry is activated), the compositor exits cleanly.

- [ ] **Step 1: Locate the last-window-destroyed branch**

Search compositor source for `window destroyed` (a trace already fires). After that trace, if windows count == 0 AND session_mode == 1, exit:

```rust
    if self.windows.is_empty() && self.session_mode == 1 {
        debug_print("compositor: last window closed in user session, exiting");
        return;  // main loop terminates → process exits → procmgr reaps
    }
```

(System compositor must NOT exit on empty windows — the login modal IS its window, but if dismissed mid-login it should keep running and respawn the modal.)

- [ ] **Step 2: Build + smoke**

```bash
cargo xtask build 2>&1 | tail -3
HARNESS_FORCE_BUILD=0 MARKER_MODE=l2_logout_graphical bash scripts/harness_run.sh
```

- [ ] **Step 3: Commit**

```bash
git add userspace/compositor/src/main.rs
git commit -m "compositor: exit user-mode on last window destroyed"
```

---

## Task 4: VT0-3 text logout — shell exit triggers tty login prompt

**Files:**
- Modify: `userspace/tty/src/main.rs` and/or `userspace/tty/src/context.rs` — detect that the shell's view/session is gone and redraw login prompt.

- [ ] **Step 1: When tty receives `TTY_SESSION_ENDED_LABEL` (or the no-op from Task 1 Step 4), redraw prompt**

Either:
- Define `TTY_SESSION_ENDED_LABEL` in libcluu/ipc.rs (next free value), and handle it in tty's recv loop: clear line discipline state, set mode back to `TtyMode::Login(LoginState::Username)`, redraw `login:` via `forward_to_console`.
- OR: tty service detects the absence of pending reads + a flag set by procmgr via the no-op label.

Prefer the explicit label. Add to libcluu and the handler.

```rust
        TTY_SESSION_ENDED_LABEL => {
            ctx.mode = TtyMode::Login(LoginState::Username);
            ctx.input_queue.clear();
            ctx.pending_reads.clear();
            ctx.write_to_console(b"\r\nlogin: ");
        }
```

- [ ] **Step 2: Procmgr's `notify_tty_session_end` switches to the new label**

Replace the placeholder `TTY_REGISTER_LABEL` with `TTY_SESSION_ENDED_LABEL`:

```rust
        let msg = Message::new(libcluu::ipc::TTY_SESSION_ENDED_LABEL, [0; 6], 0);
        let _ = libcluu::ipc::send(ep, &msg, libcluu::types::IpcFlags::empty());
```

- [ ] **Step 3: Harness marker `l2_logout_text`**

In `scripts/harness_case_defaults.sh`:

```bash
            l2_logout_text)
                TEST_COMMAND=""
                # VT0 login then `exit` → shell exits → tty redraws login:
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 3\nsendkey ctrl-alt-f1\nsleep 1\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 1\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 2\nsendkey e\nsendkey x\nsendkey i\nsendkey t\nsendkey ret'
                ;;
```

In `scripts/harness_run.sh`:

```bash
    l2_logout_text)
        required_markers=(
            "TSC calibrated"
            "tty:0: showing login prompt"
            "session root pid=.* exited"
            "tty:0: showing login prompt"   # second occurrence after logout
        )
        ;;
```

(Marker matcher must allow duplicate substring counts — verify; if not, change second marker to a distinct trace like `tty: session ended`.)

- [ ] **Step 4: Run**

```bash
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_logout_text bash scripts/harness_run.sh
```

- [ ] **Step 5: Commit**

```bash
git add userspace/tty/src/main.rs userspace/tty/src/context.rs userspace/libcluu/src/ipc.rs userspace/procmgr/src/main.rs scripts/harness_case_defaults.sh scripts/harness_run.sh
git commit -m "tty+procmgr: text VT logout redraws login prompt"
```

---

## Task 5: Final regression sweep

**Files:**
- None modified; just verify.

- [ ] **Step 1: Run every marker**

```bash
for m in l2_text_shell_input l2_cluuterm_shell_input l2_envelope_dev_filter l2_envelope_home_propagated l2_compositor_swap_login l2_compositor_menu_cluuterm l2_spawn_session_reject l2_logout_graphical l2_logout_text l2_cluuterm_login; do
    echo "=== $m ==="
    HARNESS_FORCE_BUILD=0 MARKER_MODE="$m" bash scripts/harness_run.sh 2>&1 | tail -3
done
```

All markers must pass. Any failure → debug + fix before declaring this plan done.

- [ ] **Step 2: Memory updates**

Add `/home/vlb2bp/.claude/projects/-home-vlb2bp-git-cluu/memory/project_session_teardown_2026_05_14.md`:

```markdown
---
name: session-teardown-respawn-2026-05-14
description: "Procmgr tears down session container on root-process exit, kills all session children, respawns system compositor on VT4 / tty login prompt on VT0-3."
metadata:
  type: project
---

The session-root pid's exit (user compositor for VT4; shell for VT0-3)
triggers `teardown_session_container`: kill every child in reverse-spawn
order with thread_kill + reap exit cookies within 2s. Then respawn the
stand-in: system-mode compositor on VT4 (PARAM_SESSION_MODE=0) or
TTY_SESSION_ENDED_LABEL to the tty service on VT0-3 (redraws "login:").

**Why:** Closes the lifecycle the spec demanded: nothing user-touched
persists past logout, and the system returns to a clean pre-login state
ready for the next login.

**How to apply:** New session-attached services should register themselves
in `container_children[session_cid]` so they're killed on logout. The
teardown helper doesn't try to be clever — it kills children in reverse
order without recursion; if a child has descendants, expect them to be
reaped via the kernel's process-death cascade.
```

Append index entry to MEMORY.md.

Edit `project_loginCC_session_2026_05_13.md`: mark T9 (logout respawn) closed with the commit hash from Task 1.

- [ ] **Step 3: Final commit**

No code changes; just memory.

---

## Self-review checklist

- All 10 markers (across plans 1-5) green on a fresh build.
- Compositor binary: exactly two PIDs per VT4 login/logout cycle (system → user → system → ...).
- Procmgr exit handler doesn't leak `container_instances` entries — verify with a debug-mode counter.
- MEMORY.md reflects T9 closure + new session-teardown memory.
