# Login-as-Compositor-Client Migration

> **For agentic workers:** Steps use checkbox (`- [ ]`) syntax.

**Goal:** Execute spec §4.5 + §4.4 — make `/bin/login` a compositor client (not cluuterm's child). Cluuterm spawned only post-login by procmgr, bound to authenticated user session. Identity propagates from /bin/login through procmgr SESSION_LOGIN to cluuterm's shell.

**Architecture (per spec):**
```
boot → procmgr autostart: compositor + /bin/login (no cluuterm)
     → /bin/login registers compositor window (cell-grid SHM)
     → renders login modal, recv INPUT_FORWARD for keystrokes
     → on submit: PROCMGR_SESSION_LOGIN_LABEL (session_kind=1) + user/pass
     → procmgr validates against /etc/users.toml
     → ok → procmgr builds user envelope/view, spawns cluuterm BOUND TO SESSION
     → cluuterm WIN_REGISTER under user identity, runs /bin/shell
     → /bin/login exits → compositor destroys login window
fail → /bin/login redraws modal with "incorrect" message
shell exit → cluuterm exits → procmgr respawns /bin/login → loop
```

**Tech Stack:** Rust (procmgr, compositor, cluuterm, login, libcluu).

**Parent spec:** `docs/superpowers/specs/2026-05-12-login-flow-design.md` §4.4 + §4.5.

**Supersedes:** the cluuterm-spawns-/bin/login transitional path. After this lands, /bin/login is no longer cluuterm's child. Removes the architectural inversion where a system service owned the auth flow.

---

## Task 1: drop cluuterm from autostart

**File:** `etc/autostart.toml`

- [ ] **Step 1:** Remove `cluuterm` from autostart list. Add `login` (or `/bin/login`) — whichever the autostart parser expects.
- [ ] **Step 2:** Build + boot. Expected: `procmgr: autostart 'login'` fires; cluuterm not spawned at boot.
- [ ] **Step 3:** Commit `etc/autostart: replace cluuterm with /bin/login at boot`.

---

## Task 2: /bin/login compositor scaffolding

**Files:** `userspace/login/src/main.rs` (major rewrite), `userspace/login/Cargo.toml` (deps).

Currently /bin/login is text-mode on fd 0/1. Rewrite to be a compositor client like cluuterm.

- [ ] **Step 1:** WIN_REGISTER with compositor:client output. Get win_id + frame_token.
- [ ] **Step 2:** space_map_range the SHM frame at a known VA. Initialize WindowShm header.
- [ ] **Step 3:** Render initial login modal: centered "CLUU login", "username: __", "password: __", cursor on username field.
- [ ] **Step 4:** Send WIN_DAMAGE to compositor.
- [ ] **Step 5:** debug_print `login: window registered`.
- [ ] **Step 6:** Commit `login: scaffold compositor client (WIN_REGISTER + initial modal)`.

---

## Task 3: /bin/login INPUT_FORWARD handling

**File:** `userspace/login/src/main.rs`.

- [ ] **Step 1:** Set up recv loop on login's recv endpoint.
- [ ] **Step 2:** Handle COMP_INPUT_FORWARD_LABEL: extract ASCII, scancode, modifiers.
- [ ] **Step 3:** Maintain field state: which field has focus (username vs password). Tab toggles. Enter submits.
- [ ] **Step 4:** On printable char: append to active field's buffer. Render. WIN_DAMAGE.
- [ ] **Step 5:** On backspace: pop. Render. WIN_DAMAGE.
- [ ] **Step 6:** On Enter in username: move focus to password.
- [ ] **Step 7:** On Enter in password: submit (Task 4).
- [ ] **Step 8:** Commit `login: handle INPUT_FORWARD, field focus, Tab/Enter/Backspace`.

---

## Task 4: /bin/login SESSION_LOGIN submit

**File:** `userspace/login/src/main.rs`.

- [ ] **Step 1:** On Enter-in-password: build PROCMGR_SESSION_LOGIN_LABEL message with `session_kind=1` byte + username\0 + password\0 payload.
- [ ] **Step 2:** ipc::call to procmgr:spawn endpoint with the message.
- [ ] **Step 3:** Reply errno == 0 → debug_print `login: user authenticated`, exit 0. Compositor's PROC_EXIT teardown destroys window.
- [ ] **Step 4:** Reply errno != 0 → flash "login incorrect" in modal, clear fields, focus username. Loop.
- [ ] **Step 5:** Commit `login: submit credentials via PROCMGR_SESSION_LOGIN session_kind=1`.

---

## Task 5: PROCMGR_SESSION_LOGIN payload — session_kind byte

**Files:** `userspace/libcluu/src/ipc.rs` (label payload spec), `userspace/procmgr/src/main.rs` (handler at line ~1933).

- [ ] **Step 1:** Spec the wire: payload first byte = `session_kind` (0 = tty, 1 = compositor). Rest = `username\0password\0`.
- [ ] **Step 2:** procmgr's SESSION_LOGIN handler: read first byte. If 0, run existing tty session path. If 1, run NEW compositor session path (Task 6).
- [ ] **Step 3:** Document in libcluu/ipc.rs label comment.
- [ ] **Step 4:** Commit `procmgr: SESSION_LOGIN payload gains session_kind byte`.

---

## Task 6: procmgr — spawn cluuterm bound to session

**File:** `userspace/procmgr/src/main.rs` (SESSION_LOGIN handler, session_kind=1 branch).

This is the heart of the migration. On successful auth with session_kind=1:

- [ ] **Step 1:** Validate credentials (authd path or inline against users.toml — keep existing inline for v1).
- [ ] **Step 2:** Resolve user envelope from `etc/envelopes.toml` for the user's profile.
- [ ] **Step 3:** Build the per-session view from the envelope.
- [ ] **Step 4:** Allocate a new session_id (`next_container_id`).
- [ ] **Step 5:** Spawn cluuterm with `THREAD_CREATE_START_SUSPENDED`, attach view + profile + session.
- [ ] **Step 6:** install_view_and_run.
- [ ] **Step 7:** Reply to /bin/login: words[0]=0 (ok). No fds in reply — login doesn't need cluuterm's stdin.
- [ ] **Step 8:** debug_print `procmgr: session_kind=1 spawned cluuterm pid=N`.
- [ ] **Step 9:** Commit `procmgr: SESSION_LOGIN session_kind=1 spawns cluuterm under user envelope`.

---

## Task 7: cluuterm — drop /bin/login spawn; accept session-bound startup

**File:** `userspace/cluuterm/src/main.rs`.

- [ ] **Step 1:** Remove `spawn_login_with_pts` and its call site. Cluuterm no longer spawns /bin/login.
- [ ] **Step 2:** Add: cluuterm now spawns `/bin/shell` as its child via posix_spawn with fd 0/1/2 wired to its pts.
- [ ] **Step 3:** Cluuterm's user envelope is set by procmgr at spawn — cluuterm doesn't need to know which user; the kernel/VFS view enforces.
- [ ] **Step 4:** WIN_REGISTER + PTS_REGISTER paths unchanged.
- [ ] **Step 5:** Commit `cluuterm: spawn /bin/shell (drop /bin/login spawn path)`.

---

## Task 8: compositor — accept multiple windows; login modal vs shell window

**File:** `userspace/compositor/src/main.rs`, `userspace/compositor/src/window_mgr.rs`.

- [ ] **Step 1:** Verify compositor handles 2 windows (login + cluuterm) correctly. Today's hotkey path (Alt+Tab) works.
- [ ] **Step 2:** When /bin/login process exits, compositor's exit-notify handler destroys its window. cluuterm window becomes the only/focused one.
- [ ] **Step 3:** On cluuterm exit (shell `exit` → cluuterm `term.run()` returns → cluuterm exits), procmgr should respawn /bin/login. **Procmgr exit-monitor responsibility.**
- [ ] **Step 4:** Commit `compositor: multi-window login + cluuterm coexistence`.

---

## Task 9: procmgr — respawn /bin/login on cluuterm exit

**File:** `userspace/procmgr/src/main.rs`.

- [ ] **Step 1:** When a session's cluuterm exits (PROC_EXIT_LABEL), procmgr destroys session + respawns /bin/login as compositor client (same as boot autostart did).
- [ ] **Step 2:** Commit `procmgr: respawn /bin/login on session cluuterm exit`.

---

## Task 10: revert the bandaids

**Files:**
- `userspace/libcluu/src/fs/client.rs` (revert close timeout `e1ed2e2`) — optional, kept for defense-in-depth. Decide per code review.
- `userspace/vfs/src/main.rs` (PTS write `0513d0c`) — KEEP. Fire-and-forget is correct semantics for PTS-write regardless of architecture.

- [ ] **Step 1:** Audit each bandaid commit. Decide: revert (no longer needed) vs keep (defense-in-depth).
- [ ] **Step 2:** Single commit with rationale.

---

## Task 11: visual smoke

- [ ] **Step 1:** Boot. Confirm login modal renders on compositor at boot.
- [ ] **Step 2:** Type `root` + Enter + empty password + Enter → expect cluuterm window to appear, /bin/shell prompt visible inside.
- [ ] **Step 3:** Type `bad_user` + Enter + Enter → expect "login incorrect" on modal, fields cleared.
- [ ] **Step 4:** In cluuterm shell, type `exit` → expect cluuterm window closes, /bin/login modal returns.
- [ ] **Step 5:** Document outcomes; update spec §4.5 status.

---

## Task 12: spec status + memory

- [ ] **Step 1:** `docs/superpowers/specs/2026-05-12-login-flow-design.md` §4.5: mark **Status: done in plan 2026-05-13-login-as-compositor-client (commits AAAAA..ZZZZZ)**.
- [ ] **Step 2:** New memory `project_login_compositor_client_2026_05_13.md`. Index in MEMORY.md.
- [ ] **Step 3:** Commit `docs/spec: login-as-compositor-client marked done`.

---

## Self-review

- Big plan: 12 tasks across procmgr, compositor, cluuterm, login, libcluu.
- Drops cluuterm from autostart — first-class user-visible behavior change.
- Identity flows: /bin/login → procmgr SESSION_LOGIN → user envelope → cluuterm → shell.
- No more cluuterm-as-login-host.
- Bandaids may be reverted in Task 10.
- All commits on develop. No --no-verify, no --amend.
