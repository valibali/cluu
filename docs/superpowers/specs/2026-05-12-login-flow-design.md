# Login Flow Redesign — Spec

**Date:** 2026-05-12
**Status:** Draft, pre-plan
**Owners:** kernel-team

## 1. Goal

Replace tty-based autologin with a compositor-native, modal `/bin/login` flow.
After boot the user sees a fullscreen login window on the compositor VT;
successful auth spawns a cluuterm window holding a shell. Raw VT consoles
(VT0–VT3) remain reachable via Ctrl-Alt-F1..F4 for diagnostics and each show a
banner + interactive `login:` prompt with no autologin.

Auth validation moves out of procmgr into a new primordial service, `authd`,
that consumes `tpmd` for hashing and owns `/etc/users.toml`. Procmgr keeps
session ownership; it RPCs authd for credential checks.

## 2. Non-goals

- No GUI widget toolkit. Compositor stays TUI/cell-grid; pixel windows are a
  later concern (mid-term, for image viewing).
- No PAM, no nsswitch, no pluggable auth backends. authd is a thin service.
- No persistent session resume across reboot.
- No remote login (ssh/telnet).

## 3. Target boot/flow

```
init
  └─ primordial in order: registry, timeserver, tpmd, authd, procmgr, vfs, virtio-blk
procmgr reads /etc/autostart.toml
  └─ spawn: console, vtmgr, kbd, compositor, getty(VT0..VT3)
vtmgr active_vt = DEFAULT_COMPOSITOR_VT (4) unconditionally
compositor up on VT4
  └─ procmgr autostart spawns /bin/login as compositor client
     └─ /bin/login WIN_REGISTER(fullscreen=true, modal=true)
     └─ cell-grid render: ASCII chrome + "login:" + "password:" fields
     └─ user submits → PROCMGR_SESSION_LOGIN_LABEL → procmgr
        └─ procmgr → authd AUTH_VERIFY → ok / fail
        ok:
          ├─ procmgr builds session view from envelope (existing path)
          ├─ procmgr spawns cluuterm bound to session
          ├─ cluuterm WIN_REGISTER on compositor, runs /bin/shell inside
          └─ /bin/login process exits; compositor destroys its window
        fail:
          └─ procmgr returns errno; /bin/login re-prompts (retry forever)

shell `exit`
  └─ shell PROC_EXIT → cluuterm sees pts EOF → cluuterm exits
  └─ procmgr session teardown → respawn /bin/login on compositor
```

VT0–VT3 in parallel:

```
procmgr autostart: getty@vt0, getty@vt1, getty@vt2, getty@vt3
  each getty:
    ├─ register stdin/stdout with tty:N
    ├─ print sysinfo banner (uname, build, uptime)
    ├─ prompt "login: ", "password: "
    ├─ send PROCMGR_SESSION_LOGIN_LABEL via same procmgr handler
    └─ on success: exec /bin/shell into same tty:N
       on shell exit: getty restarts → banner + login: again
```

## 4. Subsystems & changes

### 4.1 vtmgr — boot VT race fix

**File:** `userspace/vtmgr/src/context.rs`

- `active_vt: 0` → `active_vt: DEFAULT_COMPOSITOR_VT` at construction.
- Drop `boot_switch_pending` race: compositor's pin message can still
  override `compositor_vt`, but the active VT no longer waits on it.
- Side effect: fb starts blank on VT4 until compositor draws; acceptable.
- Ctrl-Alt-Fn handling unchanged; user can still flip to VT0..VT3.

### 4.2 procmgr/tty — rip autologin

**Files:**
- `userspace/procmgr/src/main.rs` (drop `try_auto_login`,
  `auto_login_done`, both call sites at ~1777, ~1791)
- `userspace/tty/src/context.rs` (drop `auto_login_pending`, init at :127,
  consumer at :181, clears at :463 + :477, SESSION_LOGIN send at :420)
- Existing `SHELL_AUTOSTART_CMD` env hook stays for harness tests
  (CLUU_SHELL_AUTOSTART_CMD path), but no longer fires via auto-login.
  Harness should drive cluuterm/login directly instead. (Confirm with
  smokes that depend on it; update or skip.)

### 4.3 authd — new primordial service

**Files (new):**
- `userspace/authd/Cargo.toml`
- `userspace/authd/src/main.rs`
- `userspace/authd/src/users.rs` (parser + lookup)
- `sys/boot.manifest` (add authd between tpmd and procmgr)
- `xtask/src/main.rs` (build authd in primordial list)

**IPC labels (new):**
- `AUTHD_VERIFY_LABEL` — payload: `username\0password\0`. Reply:
  `words[0] = errno (0=ok)`, `words[1] = uid`, `words[2..] = reserved`.
- `AUTHD_USER_LOOKUP_LABEL` — payload: `username\0`. Reply: profile name,
  home path, shell path inlined as payload. Used by procmgr to resolve
  envelope without re-parsing users.toml.

Service contract:
- Reads `/etc/users.toml` at boot, caches in memory.
- For password verify: parses `$sha256$<salt>$<hash>`, calls tpmd for
  SHA256 over `salt || password`, constant-time compare.
- Empty-password user → accept immediately, log warning.
- No write API. SIGHUP-equivalent (admin IPC) to re-read is future work.

### 4.4 procmgr — SESSION_LOGIN handler refactor

**File:** `userspace/procmgr/src/main.rs:1933` (existing handler)

- Replace inline `etc/users.toml` parse + password check with single RPC:
  `authd_call(AUTHD_VERIFY_LABEL, username, password) -> Result<uid>`.
- On Ok: existing session-creation path runs (envelope lookup, view
  build, spawn shell or cluuterm depending on caller — see §4.5).
- On Err: reply with errno, no state change.
- Cluuterm-spawn integration: SESSION_LOGIN handler is now polymorphic
  on caller. Caller (login binary) declares "compositor session" vs
  "tty session" via reply-payload flag in initial request. Compositor
  session → procmgr spawns cluuterm bound to new session; tty session
  → existing behaviour (exec shell in tty).

  Add field to PROCMGR_SESSION_LOGIN_LABEL payload: leading byte
  `session_kind` (0 = tty, 1 = compositor). All existing callers send 0.

### 4.5 /bin/login — compositor-native rewrite

**File:** `userspace/login/src/main.rs` (rewrite, keep cargo metadata)

Today's binary is text-mode over fd 0/1. New version:

- Drop `_read`/`_write`. No tty dependency.
- WIN_REGISTER w/ compositor: fullscreen=true, modal=true, focus=locked
  (compositor refuses focus loss while modal up — new compositor flag).
- Cell-grid SHM: same primitive cluuterm uses (`5af8b96` /
  `14282b0` commits). Initial size = full VT4 cell dims; redraw on
  WIN_RESIZE.
- Render: centered box w/ "CLUU login", username field (echoed),
  password field (masked), Tab toggles field focus, Enter submits.
- Input: compositor INPUT_FORWARD (keyboard) events; reuse cluuterm's
  keymap decoder. Mouse-click focus deferred until compositor
  sub-project C lands.
- Auth: `PROCMGR_SESSION_LOGIN_LABEL` w/ `session_kind=1`.
- Success: process exits; procmgr does the rest. Compositor destroys
  modal on WIN_UNREGISTER. Procmgr separately tells compositor "a
  cluuterm window is incoming for session N".
- Failure: re-prompt in place. Retry forever, brief "login incorrect"
  flash for ~1s.

### 4.6 compositor — scope additions

**Reference:** `docs/superpowers/specs/2026-05-10-tui-compositor-design.md` is the base
spec. Focus management, window move (Super+Arrow), window resize
(Super+Shift+Arrow), focused-vs-unfocused chrome, status bar, and
INPUT_FORWARD (keyboard) are already specified there. Mouse support +
pointer overlay + click-to-focus + drag-to-move/resize are listed as
**sub-project C** in §14, gated on Phase 5 raw-input.

**Delta this spec adds** on top of the base spec:

| Capability               | Notes                                                            |
|--------------------------|------------------------------------------------------------------|
| Modal / fullscreen flag  | New on `WIN_REGISTER`. Compositor enforces: modal window keeps focus until destroyed; other clients' `WIN_REGISTER` accepted but queued, no input until modal dismissed. |
| WIN_RESIZE app notify    | New compositor→client message. Existing spec resizes via hotkey but doesn't notify app of new dims; needed so /bin/login and cluuterm can re-allocate SHM. |
| Text cursor              | New. Single text-cell cursor in focused window's interior, software-drawn at `(cursor_x, cursor_y)` from `WindowShm` header (already exists). Compositor toggles blink on 500 ms timer. |
| Mouse (deferred until C) | Spec calls for mouse; honoured by mouse-driven path in sub-project C. v1 login modal usable with keyboard only (Tab switches field, Enter submits). |

Scope decisions:
- Cell-grid only (consistent with base spec).
- Modal-lock is compositor-enforced (security: a rogue client cannot
  steal focus during login).
- Mouse delivery deferred to sub-project C; login modal works without
  mouse via Tab/Enter.

### 4.7 getty — VT0–VT3 raw console

**Files (new):**
- `userspace/getty/Cargo.toml`
- `userspace/getty/src/main.rs`
- `etc/autostart.toml` (add 4 getty instances w/ `VT_INDEX` arg)
- procmgr autostart entries to pass VT index per instance

Responsibilities:
- argv: `getty <vt_index>`.
- Registers stdin/stdout with `tty:N`.
- Prints sysinfo banner (call `uname` syscall, read `/proc/uptime` once
  it exists; for v1 print hardcoded build string + boot time).
- Prompts `login:` then `password:` (text-mode, echo + mask).
- Sends `PROCMGR_SESSION_LOGIN_LABEL` w/ `session_kind=0`.
- On success: procmgr execs shell in this tty (existing tty session
  path).
- On shell exit: getty restarts (procmgr `RestartPolicy::Always`),
  prints banner + prompt again.

This roughly equals today's `try_auto_login` minus auto and plus banner.

### 4.8 exit / logout

Today: shell `exit` → process exits → procmgr PROC_EXIT → no session
teardown for tty sessions; for compositor sessions, teardown must:

1. cluuterm sees pts EOF (shell gone) → cluuterm exits.
2. procmgr observes cluuterm PROC_EXIT → looks up session by pid →
   destroys session entry, releases container, drops fdac grants.
3. procmgr re-spawns `/bin/login` on compositor (autostart-equivalent
   trigger).
4. Compositor destroys cluuterm window on WIN_UNREGISTER, accepts new
   /bin/login window as modal.

For tty sessions (VT0–VT3) the equivalent path is: shell exits → getty
restarts → banner + login. No compositor involvement.

## 5. IPC summary

| Label                              | Direction       | Status        |
|------------------------------------|-----------------|---------------|
| `PROCMGR_SESSION_LOGIN_LABEL = 30` | login→procmgr   | extended payload (session_kind byte) |
| `AUTHD_VERIFY_LABEL`               | procmgr→authd   | new           |
| `AUTHD_USER_LOOKUP_LABEL`          | procmgr→authd   | new (optional v1) |
| `VTMGR_PIN_VT_LABEL`               | compositor→vtmgr| unchanged     |
| `WIN_REGISTER` (compositor)        | client→comp     | add fullscreen+modal flags |
| `WIN_RESIZE`                       | comp→client     | new           |
| `WIN_FOCUS` / `WIN_BLUR`           | comp→client     | new           |
| `INPUT_FORWARD`                    | comp→client     | unchanged for v1; mouse extension under sub-project C |

## 6. Failure / retry UX

- Wrong creds: flash "login incorrect" 1s, clear fields, focus username.
- Empty username: stay on field.
- procmgr→authd RPC fail: show "auth service unavailable", retry every
  2s, no input accepted.
- Compositor crash during login: out of scope for v1 (init panics).

## 7. Test plan

L1 unit:
- authd users.toml parser
- authd password verify (empty, hash match, hash mismatch)
- procmgr SESSION_LOGIN dispatch (session_kind switch)

L2 smoke (harness):
- `l2_login_modal_renders`: boot, expect compositor up + login modal
  marker.
- `l2_login_bad_password`: drive INPUT_FORWARD w/ wrong creds,
  expect "login incorrect" marker, no shell.
- `l2_login_good_password`: empty-pw root, expect cluuterm window
  + shell prompt marker.
- `l2_getty_vt0_banner`: switch to VT0, expect sysinfo + login: marker.
- `l2_getty_vt0_login`: drive VT0 login, expect shell prompt on VT0.
- `l2_logout_respawn_login`: in compositor session, send `exit`,
  expect login modal re-appearing marker.

Existing `l2_cluuterm_login` marker (pending from session 2026-05-12)
gets superseded by `l2_login_modal_renders` + `l2_login_good_password`.

## 8. Implementation order (one plan per chunk)

Each chunk lands independently w/ green harness before next:

1. **vtmgr boot-VT fix** (smallest, derisks compositor visibility).
2. **autologin rip** (procmgr + tty; harness smokes update).
3. **getty** (replaces autologin user-visibly on VT0–VT3; uses today's
   text-mode login IPC path — no compositor work needed).
4. **authd skeleton + procmgr refactor** (move auth out of procmgr;
   getty unchanged, just routes through authd).
5. **compositor scope additions** (focus, modal, mouse, resize, move).
6. **/bin/login compositor rewrite** (depends on 5).
7. **Logout flow** (compositor session teardown + respawn).

Each becomes a separate `docs/superpowers/plans/2026-05-…-*.md`.

## 9. Open questions

- Compositor modal lock: is "modal" enforced compositor-side (refuse
  focus change while modal up) or by convention only? Recommend
  compositor-side enforcement for security (otherwise a rogue client
  could WIN_REGISTER pre-login and grab focus). v1: enforce.
- Mouse coordinate space: cell-aligned vs pixel? Recommend pixel
  delivered, cell-rounded by client helper. Lets pixel windows
  (future) reuse same event.
- VT0–VT3 sysinfo banner content: minimum = build hash + uptime;
  desired = build, kernel ver, free mem, primordial health. Defer
  rich banner until /proc fleshed out.
- authd write API: out of scope for v1; users.toml is read-only at
  runtime. `passwd` builtin / binary is a separate spec.

## 10. References

- `docs/ARCHITECTURE.md` §5 — userspace service map.
- `docs/ROADMAP.md` — phase guards.
- `docs/superpowers/specs/2026-05-10-tui-compositor-design.md` — base
  compositor design (cell-grid, INPUT_FORWARD).
- `docs/superpowers/specs/2026-05-11-cluuterm-design.md` — cluuterm
  rendering primitives (reused by /bin/login).
- Commits: `4fc8861` (cluuterm autostart on VT4), `484bfb6` (default
  active VT 4 — partial), `332a49f` (compositor VT pin),
  `5155f4c` (cluuterm spawns /bin/login).
