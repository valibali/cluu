# Session lifecycle — design

**Date:** 2026-05-18
**Status:** spec — pre-implementation
**Predecessor inventory:** `docs/superpowers/specs/2026-05-18-spawn-window-pty-inventory.md`
**Companion specs:**
- spec 1: `docs/superpowers/specs/2026-05-18-unified-spawn-protocol-design.md`
- spec 2: `docs/superpowers/specs/2026-05-18-terminal-pty-unification-design.md`

**Position in decomposition:** spec 3 of inventory §12.

## 1. Why

Today's session lifecycle is fragmented (inventory §7):

- procmgr holds `SessionEntry { user, profile, shell_cid, vt, stdin_endpoint, ... }`.
- VFS holds `PtsEntry { owner_tid, refcount }`.
- cluuterm holds local terminal state.

There are no back-pointers between these, no invariant that "session N
has pts X and cluuterm process Y and shell Z". Manual coupling in three
places.

The login flow currently does a system-compositor → user-compositor
swap (inventory §2): system compositor (pid 5, session_mode=0) lives
at boot; on SESSION_LOGIN, procmgr kills it and respawns a user
compositor (pid 8, session_mode=1) that re-registers identical
`compositor:*` service names. The procmgr handler waits up to 2 s for
`COMPOSITOR_READY_LABEL` — the longest-standing open
`feedback_no_timeouts` violation (inventory §9).

Multi-user (different users on VT1, VT2, VT3) is not supported as a
first-class concept. Text-VT login (getty-style) is not exercised in
the current code base.

This spec replaces the swap with a single persistent compositor.
Login becomes a regular compositor client. Session becomes a
procmgr-owned typed object addressed by IPC token, matching spec 1's
`ViewObject` cap pattern. Text-VT login (`/bin/getty`) uses the same
verb set as graphical-seat login — protocol-uniform across seat
surfaces.

## 2. Goals and non-goals

### Goals

1. One compositor for the lifetime of the seat. No system/user swap.
2. Login is a regular client (sessionless binary spawned by compositor
   at boot and on logout). Compositor draws login's window through
   the standard window-protocol verbs (spec 4 territory).
3. `SessionObject` is a procmgr-owned typed object addressed by IPC
   token. Cap-narrow-derive for distributing rights subsets.
4. Session leader designation is explicit via `SESSION_SET_LEADER`
   (set-once). Login or getty calls it after spawning the user's
   primary process.
5. Leader exit (clean exit, crash, or external kill) cascades:
   SIGHUP to session members, fanout `SESSION_ENDED` to subscribers,
   cleanup.
6. Multi-user. Three text-VT users and one graphical user can be
   logged in concurrently. Each session independent.
7. Delete the 2 s `COMPOSITOR_READY_LABEL` timeout — no swap, nothing
   to wait for.

### Non-goals

- User switching (e.g., "Switch User" / fast-user-switch on the
  graphical seat). Defer.
- Auth backend redesign (authd remains pure identity validator).
- Window protocol surface (spec 4).
- Per-user resource limits, cgroup-equivalents — out of scope.
- `/proc/sessions` (optional in step 11 of migration; full Unix-style
  `/proc` is `project_proc_unix_compliance` work).

## 3. Architecture

One persistent compositor (per seat). Login is a sessionless binary
spawned by compositor at boot and on logout. Compositor subscribes to
`SESSION_ENDED` for each session it sees; on event, closes the
session's windows and respawns login.

```
boot
 │
 ▼
compositor (one process, lives until shutdown)
 │  spawns first login window via procmgr::spawn
 ▼
login #1 ─► authd validates user "dave"
       │
       ├─► SESSION_CREATE { user_name: "dave", profile: {...} }
       │      ◄── { token, session_id: 7 }
       │
       ├─► SESSION_DERIVE_TOKEN { token, rights: Subscribe|Query }
       │      ◄── token_sub
       │   send token_sub to compositor via COMPOSITOR_SESSION_HANDOFF
       │   compositor calls SESSION_SUBSCRIBE on token_sub
       │
       ├─► procmgr::spawn(env_primary with session: Some(token))
       │      ◄── primary_pid
       │
       ├─► SESSION_SET_LEADER { token, leader_pid: primary_pid }
       │      ◄── ok
       │
       └─► exit cleanly
                    │
                    │  user works, then primary exits
                    ▼
                  procmgr: leader_pid exited → destroy_session(7)
                            ├─ SIGHUP to all members
                            ├─ fanout SESSION_ENDED { session_id: 7 }
                            └─ cleanup
                                  │
                                  ▼
                  compositor receives SESSION_ENDED 7
                            ├─ close session 7's windows
                            └─ spawn login #2 → cycle repeats
```

Text-VT seat path (`/bin/getty`) follows the same verb set; the only
differences are the user-interface surface (text input on `/dev/tty<n>`
instead of compositor window) and the absence of a compositor
subscriber.

**SOLID anchors:**

- Single-responsibility: authd = identity validation; login = UI +
  orchestration; compositor = windows; procmgr = process and session
  lifecycle; getty = text-VT login flavor of login.
- Open/closed: new login surface (e.g., SSH daemon) is a third caller
  of `SESSION_CREATE` — no protocol changes.
- Liskov: subscribers receive `SESSION_ENDED` identically regardless
  of why the session ended (clean logout, leader crash, explicit
  destroy).

**What dies:**

- `PROCMGR_SESSION_LOGIN_LABEL` (replaced by `SESSION_CREATE`).
- `kill_system_compositor()` / `space_destroy(compositor_pid)` swap
  dance.
- `session_mode` PARAM discriminator (0 = system, 1 = user) — no
  longer a distinction.
- 8 admin `force_unregister` calls on `compositor:*` services.
- `COMPOSITOR_READY_LABEL` constant and the 2 s wait for it.

## 4. SessionObject internals (procmgr-side)

```rust
pub struct SessionObject {
    pub id:           SessionId,
    pub user_name:    String,
    pub profile:      ProfileSpec,         // captured at CREATE
    pub creator_pid:  u32,                 // caller of SESSION_CREATE
    pub leader_pid:   Option<u32>,         // None until SESSION_SET_LEADER
    pub state:        SessionState,        // Live | Dying
    pub refcount:     u32,                 // outstanding token holders
    pub subscribers:  Vec<Subscriber>,     // SESSION_ENDED fanout list
    pub created_at:   u64,                 // boot-tick timestamp
}

pub enum SessionState { Live, Dying }

pub struct Subscriber {
    pub event_send_cap: TokenHandle,       // procmgr-derived IPC_SEND
    pub owner_pid:      u32,               // for cleanup on owner exit
}
```

Member tracking: derived on demand by walking `ProcessEntry.session_id`.
No redundant member list inside `SessionObject` — keeps procmgr
stateless per `feedback_procmgr_stateless`.

**Verb handlers (one site per session event):**

```rust
fn on_session_create(req: SessionCreateRequest, caller_pid: u32)
    -> Result<SessionCreateOk, SessionCreateErr>
{
    // caller manifest must declare RIGHT_SESSION_CREATE
    let session = SessionObject {
        id: next_session_id(), user_name: req.user_name,
        profile: req.profile,
        creator_pid: caller_pid, leader_pid: None,
        state: SessionState::Live, refcount: 1,
        subscribers: vec![], created_at: now_ticks(),
    };
    let token = mint_token(session.id,
        RIGHT_CONTROL | RIGHT_QUERY | RIGHT_SUBSCRIBE | RIGHT_JOIN);
    session_table.insert(session);
    Ok(SessionCreateOk { token, session_id: session.id })
}

fn on_session_set_leader(req: SessionSetLeaderRequest, caller_pid: u32)
    -> Result<(), SessionErr>
{
    let s = resolve_session(req.token, caller_pid, RIGHT_CONTROL)?;
    if s.leader_pid.is_some() { return Err(SessionErr::AlreadyHasLeader); }
    if process_entry(req.leader_pid).session_id != Some(s.id) {
        return Err(SessionErr::LeaderNotMember);
    }
    s.leader_pid = Some(req.leader_pid);
    Ok(())
}

fn on_process_exit(pid: u32, exit_code: i32) {
    for s in session_table.iter_mut() {
        if s.state != SessionState::Live { continue; }
        if s.leader_pid == Some(pid)       { destroy_session(s.id); }
        else if s.creator_pid == pid && s.leader_pid.is_none() {
            destroy_session(s.id);   // orphaned: creator died before
                                     //            SET_LEADER succeeded
        }
        // member-but-not-leader exit: no action; ProcessEntry tracking
        // suffices.
    }
}

fn destroy_session(session_id: SessionId) {
    let s = &mut session_table[session_id];
    s.state = SessionState::Dying;

    // SIGHUP to all session members (POSIX semantics):
    for proc in process_entries_in_session(session_id) {
        send_signal(proc.pid, SIGHUP);
    }

    // Fanout SESSION_ENDED:
    let event = SessionEndedEvent { session_id };
    for sub in &s.subscribers {
        ipc_send(sub.event_send_cap, SESSION_ENDED_LABEL,
                 &postcard(&event), &[]);
    }

    // Cleanup happens as members exit naturally (cap revocation on
    // tokens members held drops their session-bound resources).
    // When refcount reaches 0, session_table.gc() removes the entry.
}
```

**Refcount discipline:**

- `SESSION_DERIVE_TOKEN` bumps refcount.
- `procmgr::spawn` with `envelope.session = Some(token)` does NOT bump
  again — the token is already held by the spawner; child's membership
  is tracked through `ProcessEntry.session_id`.
- Subscriber registration bumps refcount.
- Decrements happen on cap revocation via procmgr's existing
  cap-revocation hook (token holder exits → refcount drops).

## 5. Wire format

Per-call layout (every session verb):

```
words[0] = payload_len
words[1] = ABI_VERSION (= 1)
words[2..6] = 0 (reserved)
payload  = postcard::to_slice(&Request)   // or &Reply
```

**Label assignments:**

```rust
pub const PROCMGR_SESSION_CREATE_LABEL:        u32 = 82;
pub const PROCMGR_SESSION_DESTROY_LABEL:       u32 = 83;
pub const PROCMGR_SESSION_QUERY_LABEL:         u32 = 84;
pub const PROCMGR_SESSION_SUBSCRIBE_LABEL:     u32 = 85;
pub const PROCMGR_SESSION_DERIVE_TOKEN_LABEL:  u32 = 86;
pub const SESSION_ENDED_LABEL:                 u32 = 87;  // async event
pub const PROCMGR_SESSION_SET_LEADER_LABEL:    u32 = 88;
```

No conflict with spec 1 (80-81) or spec 2 (100-110).

**Rights bitmask:**

```rust
pub const RIGHT_SESSION_CONTROL:   u32 = 0x01;
pub const RIGHT_SESSION_QUERY:     u32 = 0x02;
pub const RIGHT_SESSION_SUBSCRIBE: u32 = 0x04;
pub const RIGHT_SESSION_JOIN:      u32 = 0x08;
```

`SESSION_CREATE` returns a token with all four bits. `SESSION_DERIVE_TOKEN`
narrows; the derived rights must be a strict subset of the holder's
rights — enforced inside procmgr.

**Caller-facing types:**

```rust
pub struct SessionCreateRequest {
    pub user_name: String,
    pub profile:   ProfileSpec,
}
pub struct ProfileSpec {
    pub home: String,
    pub initial_view: ViewSource,
    pub env: Vec<(String, String)>,
    pub umask: u32,
}
pub struct SessionCreateOk { pub token: TokenHandle, pub session_id: u32 }
pub type   SessionCreateReply = Result<SessionCreateOk, SessionCreateErr>;

pub struct SessionDestroyRequest { pub token: TokenHandle }
pub type   SessionDestroyReply   = Result<(), SessionErr>;

pub struct SessionQueryRequest { pub token: TokenHandle }
pub struct SessionQueryReply {
    pub session_id:   u32,
    pub user_name:    String,
    pub leader_pid:   Option<u32>,
    pub state:        SessionState,
    pub member_pids:  Vec<u32>,
}

pub struct SessionSubscribeRequest {
    pub token:      TokenHandle,
    pub event_send: TokenHandle,
}
pub type SessionSubscribeReply = Result<(), SessionErr>;

pub struct SessionDeriveRequest { pub token: TokenHandle, pub rights: u32 }
pub type   SessionDeriveReply   = Result<TokenHandle, SessionErr>;

pub struct SessionSetLeaderRequest { pub token: TokenHandle, pub leader_pid: u32 }
pub type   SessionSetLeaderReply   = Result<(), SessionErr>;

pub struct SessionEndedEvent { pub session_id: u32 }

pub enum SessionErr {
    InvalidToken,
    InsufficientRights,
    AlreadyDying,
    AlreadyHasLeader,
    LeaderNotMember,
    NotFound,
    Internal(u32),
}
```

**libcluu wrapper:**

```rust
pub fn create(req: SessionCreateRequest)        -> Result<SessionCreateOk, SessionCreateErr>;
pub fn destroy(token: TokenHandle)              -> Result<(), SessionErr>;
pub fn query(token: TokenHandle)                -> Result<SessionQueryReply, SessionErr>;
pub fn subscribe(token: TokenHandle,
                 event_send: TokenHandle)       -> Result<(), SessionErr>;
pub fn derive_token(token: TokenHandle,
                    rights: u32)                -> Result<TokenHandle, SessionErr>;
pub fn set_leader(token: TokenHandle,
                  leader_pid: u32)              -> Result<(), SessionErr>;
```

**Error semantics:**

Every reply is `Result<_, SessionErr>`. No timeouts. Service death
surfaced via cap revocation:

- Procmgr crashes → kernel revokes its endpoints → all callers'
  pending session-verb calls return `EBADTOKEN` → libcluu translates
  to `SessionErr::Internal(EPROCMGR_DEAD)`.
- Subscriber dies → `event_send_cap` revoked → procmgr's send on event
  fails silently; procmgr removes the subscriber on its next
  `on_process_exit` hook for the subscriber's owner_pid.

## 6. Compositor lifecycle + login window

**Compositor process: one for the seat's lifetime.**

Spawned at boot via autostart. Lives until shutdown. Cluufile declares
`RESTART always` — crash recovery respawns the same process; session
state in the compositor is lost (acceptable; matches forced-logout).

**No `session_mode` discriminator.** PARAM `session_mode=0|1`
(system vs user compositor) deleted. There is "the compositor"; one
mode.

**Compositor's per-window state:**

Each window has `session_id: Option<u32>`. Compositor sets this at
window creation by reading the registering client's session via
procmgr `SESSION_QUERY` (or by the window-protocol verb in spec 4
threading the client's session token).

Sessionless windows (login itself, future system status bar) have
`session_id = None` and persist across all session lifecycles.

**Login window lifecycle (graphical seat):**

```
boot: compositor up
  └─ compositor spawns /bin/login (sessionless; RIGHT_SESSION_CREATE)
     login window draws via window-protocol
     user types creds
     login → authd: AUTH_VALIDATE { user, password }
     authd → login: { ok, user_id }
     login → procmgr: SESSION_CREATE { user_name, profile }
        ◄── { token, session_id }
     login → procmgr: SESSION_DERIVE_TOKEN { token, Subscribe|Query }
        ◄── token_sub
     login → compositor (over compositor:control):
        COMPOSITOR_SESSION_HANDOFF { session_id, token_sub }
        compositor calls SESSION_SUBSCRIBE(token_sub, compositor_send_cap)
     login → procmgr::spawn(env_primary with session: Some(token))
        ◄── primary_pid
     login → procmgr: SESSION_SET_LEADER { token, primary_pid }
     login exits cleanly (releases token; window closes naturally)

primary process exits
  procmgr: leader exit → destroy_session → fanout SESSION_ENDED
  compositor receives SESSION_ENDED { session_id }
     ├─ close all windows where window.session_id == session_id
     └─ spawn /bin/login again
  cycle repeats
```

**New compositor:control verb:**

```rust
pub const COMPOSITOR_SESSION_HANDOFF_LABEL: u32 = 200;
// payload: { session_id: u32, token_sub: TokenHandle }
// caller: login (or getty, if a future getty wants a window-based
//         post-login indicator)
// effect: compositor records (session_id, token_sub) and calls
//         procmgr SESSION_SUBSCRIBE(token_sub, compositor_event_send).
```

**Login binary's manifest (`/var/images/login/manifest.toml`):**

```toml
ENTRYPOINT  /bin/login
RESTART     never
SESSIONLESS allow
RIGHTS      RIGHT_SESSION_CREATE \
            RIGHT_SPAWN \
            RIGHT_AUTH_VALIDATE \
            COMPOSITOR_HANDOFF
MOUNT       /home/
```

**Compositor death case:**

If compositor crashes, manifest `RESTART always` respawns. In-memory
session-window table is lost. Fresh compositor spawns fresh login.
Previously-running cluuterms talk to a now-revoked compositor endpoint
→ `EBADTOKEN` → cluuterm exits → cascade. Effective forced-logout for
all users — acceptable failure mode for spec 3.

## 7. Text-VT login flow (getty-style)

**Goal:** `/dev/tty1..3` text-VTs are valid login surfaces. Multi-user
concurrent — different VTs may host different users at the same time.

**Boot-time wiring (`/etc/autostart.toml`):**

```toml
[[autostart]]
image = "getty"
args  = ["/dev/tty1"]

[[autostart]]
image = "getty"
args  = ["/dev/tty2"]

[[autostart]]
image = "getty"
args  = ["/dev/tty3"]
```

Each getty is sessionless, holds `RIGHT_SESSION_CREATE`, bound to a
specific `/dev/tty<n>`.

**Per-VT flow:**

```
getty /dev/tty1
  ├ open("/dev/tty1") for stdin/stdout/stderr via FdInherit-of-self
  ├ print "cluu login: "
  ├ read username from /dev/tty1
  ├ disable ECHO via tcsetattr (PTS_SET_TERMIOS); print "password: "
  ├ read password from /dev/tty1
  ├ re-enable ECHO
  ├ authd → AUTH_VALIDATE { user, password }   ◄── { ok, user_id }
  ├ procmgr → SESSION_CREATE { user_name, profile_text }
  │     ◄── { token, session_id }
  ├ procmgr::spawn(env_user_shell_on_tty1 with session: Some(token))
  │     - envelope.image: profile_text.initial_image (e.g., "shell")
  │     - envelope.fd_inherit: stdin/stdout/stderr bound to /dev/tty1
  │     - envelope.env: TERM=vt100, HOME=/home/<user>, USER=<user>
  │   ◄── shell_pid
  ├ procmgr → SESSION_SET_LEADER { token, leader_pid: shell_pid }
  └ getty exits cleanly
```

When the shell exits:

```
procmgr: leader exit → destroy_session → cascade SIGHUP
no subscribers (no compositor for text-VT path) → no fanout
autostart's getty: RESTART always → procmgr respawns getty
getty prompt returns; next user can log in
```

**Multi-user invariant:**

- VT1, VT2, VT3 may each have a different user logged in concurrently.
- VT0 (compositor / graphical) may have yet another user.
- Each VT's session is independent: separate `session_id`, separate
  token, separate `/dev/pts/` overlay (spec 2 §9), separate `/home`
  view derive.
- User switching = Ctrl+Alt+F<n> (existing kbd VT-switch binding) →
  vtmgr changes active VT → input routes per existing input-routing
  design (`project_input_routing_design`). Spec 3 doesn't redesign
  it.

**Compositor-vs-getty: no protocol difference.**

Both call `SESSION_CREATE` / `SESSION_SET_LEADER`. Differences are
environmental — graphical primary vs text shell; compositor subscribes
via `COMPOSITOR_SESSION_HANDOFF`, getty does not subscribe (no
window manager to notify).

**Recovery / single-user mode:**

Boot with `single_user` kernel arg → init's primordial seed includes
a special `single_user_shell` envelope instead of normal autostart.
Sessionless shell with full privileges declared in its Cluufile. Spec
3 doesn't define the kernel arg flow (kernel work is separate) — the
session machinery accommodates a sessionless shell trivially.

**Background-VT lifecycle:**

A session on a background text-VT is fully alive — its processes run;
they just don't see input. Switching to a background VT does NOT
destroy its session. Switching back continues the session where it
left off.

## 8. Removing the 2 s COMPOSITOR_READY timeout

```rust
// today: procmgr/src/main.rs:2587
fn handle_session_login(...) {
    kill_system_compositor();
    spawn_user_compositor();
    let ready = wait_label_with_timeout(COMPOSITOR_READY_LABEL, 2_000_ms)?;
    if !ready { return Err(...); }
    spawn_cluuterm(...);
}
```

After spec 3: no `kill_system_compositor`, no `spawn_user_compositor`,
no `COMPOSITOR_READY_LABEL` wait. The whole handler is gone.

`PROCMGR_SESSION_LOGIN_LABEL` is deleted. Replaced by `SESSION_CREATE`
+ `procmgr::spawn` + `SESSION_SET_LEADER`, none of which wait for
anyone. `compositor:control` `COMPOSITOR_SESSION_HANDOFF` is a normal
synchronous IPC — blocks normally if compositor is busy; cap-revocation
on compositor death surfaces concretely.

**Deletions:**

| Deleted | What it was |
|---|---|
| `PROCMGR_SESSION_LOGIN_LABEL` | the killing-and-respawning wire verb |
| `kill_system_compositor()` | the kill-pid-5 helper |
| `system_compositor_pid` global | the swap state |
| `spawn_user_compositor()` | the user-mode compositor launcher |
| `session_mode` PARAM | the 0/1 discriminator passed via spawn PARAM |
| 8 admin `force_unregister` calls | unregistering `compositor:*` before respawn |
| `COMPOSITOR_READY_LABEL` | the ready signal nobody needs |
| `wait_label_with_timeout` use here | the 2 s timer |

## 9. Migration plan

Depends on spec 1's steps 1-4 + step 9 (SESSION_LOGIN internal spawns
flipped to `procmgr::spawn`). Compatible with spec 2 but doesn't
require it landed first.

1. **`cluu_proto::session` module.** Verb labels (82-88), request /
   reply types, `SessionObject` shape, `RIGHT_SESSION_*` bitflags,
   `SessionErr` enum, `SessionEndedEvent`. `SESSIONLESS` / `SESSION_LEADER`
   directives for Cluufile parser. No call-site changes. Build clean.

2. **Procmgr-internal `SessionObject` table + verb handlers.**
   `session_table: BTreeMap<SessionId, SessionObject>`. Verb handlers:
   `handle_session_create`, `handle_session_set_leader`,
   `handle_session_destroy`, `handle_session_query`,
   `handle_session_subscribe`, `handle_session_derive_token`. Wire to
   existing `on_process_exit` hook for leader / creator-orphan
   triggers. `procmgr::spawn` checks `envelope.session` against
   `SessionObject.state == Live`.

3. **libcluu `session` module.** Wrappers around the verbs.

4. **Login binary rewrite.** Replace today's `PROCMGR_SESSION_LOGIN_LABEL`
   send with the new flow: `SESSION_CREATE` →
   `SESSION_DERIVE_TOKEN` → `COMPOSITOR_SESSION_HANDOFF` →
   `procmgr::spawn(env_primary, session: Some(token))` →
   `SESSION_SET_LEADER` → exit. Login manifest:
   `RIGHT_SESSION_CREATE`, `SESSIONLESS allow`, `RESTART never`.

5. **Compositor: subscribe + handoff support.** Implement
   `COMPOSITOR_SESSION_HANDOFF` verb handler on `compositor:control`.
   On handoff: call `SESSION_SUBSCRIBE`. Main recv loop arm for
   `SESSION_ENDED`: close session's windows, spawn fresh `/bin/login`.

6. **Compositor: drop swap dance.** Delete `kill_system_compositor`,
   `spawn_user_compositor`, `session_mode` PARAM, system-vs-user
   compositor branches. Compositor's autostart entry: `RESTART always`.

7. **Compositor's boot-time login spawn.** At compositor startup,
   after framebuffer / input init, spawn `/bin/login` via
   `procmgr::spawn(env_login, session: None)`. Same helper used for
   "spawn login on SESSION_ENDED".

8. **Delete `PROCMGR_SESSION_LOGIN_LABEL` machinery.** The label
   constant, `handle_session_login`, `COMPOSITOR_READY_LABEL`, the
   2 s `wait_label_with_timeout` site, 8 admin `force_unregister`
   calls, any helper that depended on `system_compositor_pid`.

9. **Getty binary.** Create `userspace/getty/`. Small main: open
   `/dev/tty<n>`, read user/pass, call authd, `SESSION_CREATE`,
   spawn user-shell, `SESSION_SET_LEADER`, exit. Manifest:
   `RIGHT_SESSION_CREATE`, `SESSIONLESS allow`, `RESTART always`.
   Add three autostart entries (`getty /dev/tty1..3`).

10. **Session-aware windows (compositor side).** Window registration
    gains a `session_token` field (or compositor reads client's
    session via procmgr `SESSION_QUERY` after authenticating the
    client). Compositor records `window.session_id`. On
    `SESSION_ENDED`, walks windows and closes matching ones.

11. **`/proc/sessions` (optional).** Procmgr exposes session list as
    /proc node. Each session listed with id, user, leader_pid, state,
    created_at. Defer if `/proc` Unix-style work isn't ready.

12. **Verify.** Acceptance criteria pass.

**Step interlock risk (4-7):** between login's flip (step 4) and
compositor's subscribe support (step 5) there is a window where login
sends `COMPOSITOR_SESSION_HANDOFF` but compositor doesn't handle it
yet. Mitigate: land step 5 first (handler accepts the call, calls
SESSION_SUBSCRIBE), then step 4. Each step ends with harness green.

**Per-step gate:** `bash scripts/harness_run.sh` reaches `compositor:
ready` and an interactive `shell: ready` after the login cycle.

## 10. Acceptance criteria

### Build

- `cargo xtask build` clean.
- `cargo clippy --workspace --all-targets -- -D warnings` clean.

### Grep zero-hit proofs

- `git grep PROCMGR_SESSION_LOGIN_LABEL`
- `git grep COMPOSITOR_READY_LABEL`
- `git grep kill_system_compositor`
- `git grep spawn_user_compositor`
- `git grep "system_compositor_pid"`
- `git grep "session_mode"` (the PARAM discriminator)
- `git grep "wait.*COMPOSITOR_READY"`

### Grep one-match proofs

- `git grep "PROCMGR_SESSION_CREATE_LABEL.*= 82"` → one in
  `cluu_proto::session`.
- `git grep "PROCMGR_SESSION_SET_LEADER_LABEL.*= 88"` → one.
- `git grep "fn handle_session_create" userspace/procmgr/` → one.
- `git grep "RIGHT_SESSION_CREATE" userspace/login/` → present.
- `git grep "RIGHT_SESSION_CREATE" userspace/getty/` → present.

### Functional smoke — graphical seat

- Boot reaches `compositor: ready` and a `/bin/login` window appears.
- Interactive root/root login → cluuterm window with shell prompt.
- `exit` in shell → cluuterm closes → fresh login window appears.
- Repeat 3 times in one boot — no leak, no kernel warnings, no
  PMM AUDIT.
- External kill of cluuterm → procmgr cascades; SIGHUP to session
  members; SESSION_ENDED fires; compositor spawns fresh login.

### Functional smoke — text-VT seat

- Boot, switch to VT1 (Ctrl+Alt+F2) → getty prompt `cluu login:`.
- Type user/password → user shell starts on /dev/tty1.
- `exit` → getty respawns; prompt returns.
- VT1 user A + VT2 user B simultaneously: each session independent.

### No-timeout proof

`grep -rn "wait.*timeout\|recv_with_timeout\|call_with_timeout"
userspace/procmgr/src/main.rs` returns same set as pre-spec-3 (no new
entries; the 2 s `COMPOSITOR_READY` site is gone).

### Cap-discipline markers

- `l3_session_create_requires_right`: binary without
  `RIGHT_SESSION_CREATE` calls SESSION_CREATE → `PermissionDenied`.
- `l3_session_destroy_requires_control`: token without
  `RIGHT_SESSION_CONTROL` attempts destroy → `InsufficientRights`.
- `l3_session_set_leader_twice`: second call → `AlreadyHasLeader`.
- `l3_session_set_leader_not_member`: pid not in session →
  `LeaderNotMember`.
- `l3_session_derive_widening_denied`: derive asks for a right not
  held → `InsufficientRights`.
- `l3_session_creator_death_destroys_unfilled`: caller exits before
  SET_LEADER → session destroyed.

### Leader-death cascade markers

- `l3_leader_exit_sighup_members`: spawn a member alongside the
  leader; kill the leader; member receives SIGHUP.
- `l3_subscriber_receives_session_ended`: subscriber registered;
  leader exits; receives `SESSION_ENDED { session_id }`.
- `l3_member_exit_no_destroy`: non-leader member exits; session
  remains Live.

### Multi-user markers

- `l3_multiuser_concurrent_vts`: log in user A on VT1, user B on
  VT2, user C on graphical (VT0); all three sessions Live; queries
  on each return correct user / leader.
- `l3_logout_one_vt_others_continue`: logout VT2; VT1 and VT0
  sessions remain Live.

### Login-as-window markers

- `l3_login_is_a_window`: at boot, list compositor's windows; verify
  one window owned by `/bin/login` with `session_id = None`.
- `l3_login_respawns_on_logout`: log in then logout; compositor's
  window list now has a fresh login window with a different pid.
- `l3_no_compositor_swap`: capture compositor pid before login;
  after login, pid unchanged.

### Performance gate

- Time from "user presses Enter on password" to "shell prompt
  visible": under 200 ms p99.
- Cycle "login → exit → fresh login": under 100 ms additional
  latency on the exit-to-fresh transition.

### Documentation

- File at
  `docs/superpowers/specs/2026-05-18-session-lifecycle-design.md`.
- Cross-referenced from `docs/ROADMAP.md` and
  `docs/CURRENT_PHASE.md`.
- Linked from spec 1 (`session: Option<TokenHandle>` field) and
  spec 2 (`/dev/pts/` per-session overlay).

### Spec 1 / 2 dependency

- Verb labels 82-88 do not conflict with spec 1 (80-81) or spec 2
  (100-110).
- `envelope.session = Some(token)` semantics from spec 1 §9 used
  unchanged.
- `RIGHT_SESSION_CREATE` enforced via Cluufile manifest declaration
  mechanism from spec 1.

## 11. Open follow-ups (out of spec 3)

- User-switching / fast-user-switch on graphical seat (close existing
  session windows but keep session alive; show login; switch back
  later).
- Leader migration (`SESSION_PROMOTE_MEMBER_TO_LEADER`) for recovery
  cases — set-once policy of spec 3 may need to relax.
- `/proc/sessions` real file (depends on `/proc` Unix-style work).
- `loginctl`-style CLI tool listing/inspecting sessions.
- Audit log fanout (additional subscriber kind for security-event
  logging on session create / destroy).
- Per-session resource limits / cgroup-equivalents.

## 12. Related memory

- `[[no-timeouts]]` — closes the 2 s `COMPOSITOR_READY` violation.
- `[[unified-process-model-decision-2026-05-18]]` — procmgr as sole
  owner extends to sessions.
- `[[procmgr-stateless]]` — member tracking derived from
  `ProcessEntry.session_id`; no redundant member list in
  `SessionObject`.
- `[[vfs-view-caps-monotone]]` — `SESSION_DERIVE_TOKEN` rights narrow,
  never widen, matching view-derive discipline.
- `[[login-flow-redesign]]` — predecessor sketch; this spec is the
  formal landing.
- `[[compositor-swap-2026-05-15]]` — the swap pattern that gets
  deleted.
- `[[input-routing-design]]` — VT-switching kept as-is; spec 3 builds
  on it.

## 13. Related committed work

- `1a8c218` docs(spawn-window-pty): inventory of current pipeline.
- `da8da75` libcluu/registry: drop 2 s subscribe timeout — same
  family as the `COMPOSITOR_READY` removal here.
- `a597e09` procmgr: derive notify_endpoint into own token table —
  same cap-derive pattern this spec uses for session tokens.
- `860f996` procmgr: derive parent_stdin_send from original stdin
  endpoint — same pattern.

## 14. Related specs

- Spec 1: unified spawn protocol (this spec consumes its
  `envelope.session` field and procmgr-side spawn machinery).
- Spec 2: terminal + PTY unification (per-session `/dev/pts/`
  overlay; getty uses unified PTS_* verbs against `/dev/tty<n>`).
- Spec 4: window protocol formalization (window-to-session
  threading is formalized there).
