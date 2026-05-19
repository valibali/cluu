# Session Lifecycle Implementation Plan

> **For agentic workers:** Self-contained for handoff (target: deepseek v4 pro). Each step: exact paths + complete code + verification commands. Frequent commits. Steps use checkbox (`- [ ]`).

**Goal:** Replace the system/user compositor swap with one persistent compositor. Login = sessionless client respawned on logout. `SessionObject` becomes a procmgr-owned typed object addressed by IPC token (cap-narrow-derive). Session leader = explicit `SESSION_SET_LEADER` (set-once). Leader exit cascades SIGHUP + `SESSION_ENDED` fanout. Multi-user concurrent VTs supported via getty on `/dev/tty1..3`. Delete the 2 s `COMPOSITOR_READY_LABEL` wait.

**Architecture:** New `cluu_proto::session` module (labels 82-88, rights bitmask). Procmgr-internal `SessionObject` table + handler functions. Login binary rewritten: post-auth flow = `SESSION_CREATE` → `SESSION_DERIVE_TOKEN` → `COMPOSITOR_SESSION_HANDOFF` → `procmgr::spawn` → `SESSION_SET_LEADER` → exit. Compositor: subscribe to `SESSION_ENDED`, close session windows + respawn login. New getty binary for text-VT. `PROCMGR_SESSION_LOGIN_LABEL` machinery deleted.

**Tech Stack:** Rust 2021, postcard 1.x, bitflags 2.4, `cluu_proto` (plan 1).

**Reference spec:** `docs/superpowers/specs/2026-05-18-session-lifecycle-design.md`.

**Prerequisites:**
- Plan 1 tasks 1-4 (cluu_proto crate + libcluu/procmgr re-export).
- Plan 1 task 9 (PROCMGR_SPAWN_UNIFIED handler).
- Plan 1 task 15 (SESSION_LOGIN internal spawns flipped) — recommended but not strict; plan 3 finishes the transition.

Plan 3 can land before or in parallel with plan 2; no hard dependency.

---

## File Structure

### New files

- `userspace/cluu_proto/src/session.rs` — labels 82-88, request/reply types, `RIGHT_SESSION_*` bitmask, `SessionErr`, `SessionEndedEvent`.
- `userspace/procmgr/src/session_table.rs` — `SessionObject` table + verb handlers + rollback helpers.
- `userspace/getty/Cargo.toml` + `userspace/getty/src/main.rs` — text-VT login.
- `userspace/probes/l3_*` (multiple) — acceptance markers.

### Modified files

- `userspace/cluu_proto/src/lib.rs` — declare `session` module.
- `userspace/libcluu/src/session.rs` (NEW) — wrappers around the verbs.
- `userspace/libcluu/src/lib.rs` — pub use new module.
- `userspace/procmgr/src/main.rs` — IPC dispatch arms for 82-88; delete SESSION_LOGIN swap; spawn login at boot; delete `kill_system_compositor` / `spawn_user_compositor` / `session_mode` PARAM / 8 admin force-unregisters / `COMPOSITOR_READY_LABEL` wait.
- `userspace/procmgr/src/lib.rs` — declare `session_table` module.
- `userspace/procmgr/src/spawn.rs` — `procmgr::spawn` consults `SessionObject` for `envelope.session`.
- `userspace/login/src/main.rs` — rewrite post-auth flow.
- `userspace/compositor/src/main.rs` — subscribe + handoff handler.
- `userspace/compositor/src/protocol.rs` — `COMPOSITOR_SESSION_HANDOFF_LABEL = 200`.
- `userspace/compositor/src/state.rs` — per-window `session_id: Option<u32>`.
- `userspace/libcluu/src/ipc.rs` — delete `COMPOSITOR_READY_LABEL` const; delete `PROCMGR_SESSION_LOGIN_LABEL`.
- `Cargo.toml` — add `userspace/getty` member.
- `/etc/autostart.toml` (in tree at `var/images/.../etc/` or `etc/`) — add 3 getty entries.
- `/var/images/login/manifest.toml` — declares `RIGHT_SESSION_CREATE`, `SESSIONLESS allow`, `RESTART never`.
- `/var/images/getty/manifest.toml` — same; `RESTART always`.

---

## Build / verify cheat sheet

- Build: `cargo xtask build`.
- Lint: `cargo clippy --workspace --all-targets -- -D warnings`.
- Single crate: `cargo build -p <crate>` (`cluu_proto`, `procmgr`, `login`, `compositor`, `getty`).
- Boot smoke: `bash scripts/harness_run.sh` (expect `compositor: ready`).
- Marker: `HARNESS_FORCE_BUILD=1 MARKER_MODE=<m> bash scripts/harness_run.sh; grep "<m>:" serial.log`.

---

## Task 1: `cluu_proto::session` module

**Files:**
- Create: `userspace/cluu_proto/src/session.rs`
- Modify: `userspace/cluu_proto/src/lib.rs`

- [ ] **Step 1: Declare module in lib.rs**

Add to `userspace/cluu_proto/src/lib.rs`:

```rust
pub mod session;
```

- [ ] **Step 2: Write `userspace/cluu_proto/src/session.rs`**

```rust
//! Session lifecycle protocol — see spec 3.

use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::TokenHandle;
use crate::spawn::ViewSource;

// ----- Verb labels -----

pub const PROCMGR_SESSION_CREATE_LABEL:        u32 = 82;
pub const PROCMGR_SESSION_DESTROY_LABEL:       u32 = 83;
pub const PROCMGR_SESSION_QUERY_LABEL:         u32 = 84;
pub const PROCMGR_SESSION_SUBSCRIBE_LABEL:     u32 = 85;
pub const PROCMGR_SESSION_DERIVE_TOKEN_LABEL:  u32 = 86;
pub const SESSION_ENDED_LABEL:                 u32 = 87;   // async event
pub const PROCMGR_SESSION_SET_LEADER_LABEL:    u32 = 88;

// Compositor:control verb (for the login → compositor handoff).
pub const COMPOSITOR_SESSION_HANDOFF_LABEL:    u32 = 200;

// ----- Rights bitmask -----

pub const RIGHT_SESSION_CONTROL:   u32 = 0x01;
pub const RIGHT_SESSION_QUERY:     u32 = 0x02;
pub const RIGHT_SESSION_SUBSCRIBE: u32 = 0x04;
pub const RIGHT_SESSION_JOIN:      u32 = 0x08;

pub const RIGHT_SESSION_ALL: u32 = RIGHT_SESSION_CONTROL
                                 | RIGHT_SESSION_QUERY
                                 | RIGHT_SESSION_SUBSCRIBE
                                 | RIGHT_SESSION_JOIN;

// ----- Errors -----

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SessionErr {
    InvalidToken,
    InsufficientRights,
    AlreadyDying,
    AlreadyHasLeader,
    LeaderNotMember,
    NotFound,
    Internal(u32),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SessionCreateErr {
    PermissionDenied,
    InvalidProfile,
    Internal(u32),
}

// ----- Requests / replies -----

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProfileSpec {
    pub home: String,
    pub initial_view: ViewSource,
    pub env: Vec<(String, String)>,
    pub umask: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionCreateRequest {
    pub user_name: String,
    pub profile:   ProfileSpec,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionCreateOk {
    pub token:      TokenHandle,
    pub session_id: u32,
}
pub type SessionCreateReply = Result<SessionCreateOk, SessionCreateErr>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionDestroyRequest { pub token: TokenHandle }
pub type   SessionDestroyReply   = Result<(), SessionErr>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionQueryRequest  { pub token: TokenHandle }

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SessionState { Live, Dying }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionQueryReply {
    pub session_id:  u32,
    pub user_name:   String,
    pub leader_pid:  Option<u32>,
    pub state:       SessionState,
    pub member_pids: Vec<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionSubscribeRequest {
    pub token:      TokenHandle,
    pub event_send: TokenHandle,
}
pub type SessionSubscribeReply = Result<(), SessionErr>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionDeriveRequest { pub token: TokenHandle, pub rights: u32 }
pub type   SessionDeriveReply   = Result<TokenHandle, SessionErr>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionSetLeaderRequest { pub token: TokenHandle, pub leader_pid: u32 }
pub type   SessionSetLeaderReply   = Result<(), SessionErr>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionEndedEvent { pub session_id: u32 }

// ----- Compositor handoff -----

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompositorSessionHandoffRequest {
    pub session_id: u32,
    pub token_sub:  TokenHandle,
}
pub type CompositorSessionHandoffReply = Result<(), SessionErr>;
```

- [ ] **Step 3: Add round-trip tests at the bottom**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::spawn::ViewSource;
    use alloc::vec;

    fn sample_profile() -> ProfileSpec {
        ProfileSpec {
            home: String::from("/home/dave"),
            initial_view: ViewSource::Derive(0xC0FFEE),
            env: vec![(String::from("USER"), String::from("dave"))],
            umask: 0o022,
        }
    }

    #[test]
    fn create_request_roundtrip() {
        let req = SessionCreateRequest {
            user_name: String::from("dave"),
            profile: sample_profile(),
        };
        let bytes = postcard::to_allocvec(&req).expect("ser");
        let decoded: SessionCreateRequest = postcard::from_bytes(&bytes).expect("deser");
        assert_eq!(decoded.user_name, "dave");
        assert_eq!(decoded.profile.home, "/home/dave");
    }

    #[test]
    fn query_reply_roundtrip() {
        let r = SessionQueryReply {
            session_id: 7,
            user_name: String::from("dave"),
            leader_pid: Some(42),
            state: SessionState::Live,
            member_pids: vec![42, 43, 44],
        };
        let bytes = postcard::to_allocvec(&r).expect("ser");
        let decoded: SessionQueryReply = postcard::from_bytes(&bytes).expect("deser");
        assert_eq!(decoded.session_id, 7);
        assert_eq!(decoded.leader_pid, Some(42));
        assert_eq!(decoded.member_pids.len(), 3);
        assert_eq!(decoded.state, SessionState::Live);
    }

    #[test]
    fn session_ended_event_roundtrip() {
        let e = SessionEndedEvent { session_id: 99 };
        let bytes = postcard::to_allocvec(&e).expect("ser");
        let decoded: SessionEndedEvent = postcard::from_bytes(&bytes).expect("deser");
        assert_eq!(decoded.session_id, 99);
    }

    #[test]
    fn rights_subset_check() {
        let full = RIGHT_SESSION_ALL;
        let qonly = RIGHT_SESSION_QUERY;
        assert_eq!(qonly & full, qonly); // subset
        assert_ne!(full & qonly, full);  // not equal (qonly is narrower)
    }
}
```

- [ ] **Step 4: Build + test**

```
cd /home/vlb2bp/git/cluu
cargo build -p cluu_proto
cargo test -p cluu_proto --features host-test
```

Expected: builds clean; 4 new tests pass (+ tests from plan 1/2).

- [ ] **Step 5: Commit**

```bash
git add userspace/cluu_proto/src/lib.rs userspace/cluu_proto/src/session.rs
git commit -m "feat(cluu_proto): session module — labels 82-88, rights, types"
```

---

## Task 2: `libcluu::session` wrapper

**Files:**
- Create: `userspace/libcluu/src/session.rs`
- Modify: `userspace/libcluu/src/lib.rs`

- [ ] **Step 1: Write `userspace/libcluu/src/session.rs`**

```rust
//! Client-side wrappers around the procmgr session verbs.

use cluu_proto::ABI_VERSION;
use cluu_proto::TokenHandle;
use cluu_proto::session::*;

fn build_words(payload_len: usize) -> [u64; 6] {
    let mut w = [0u64; 6];
    w[0] = payload_len as u64;
    w[1] = ABI_VERSION as u64;
    w
}

fn call_procmgr_postcard<Req, Rep>(label: u32, request: Req) -> Result<Rep, SessionErr>
where
    Req: serde::Serialize,
    Rep: for<'de> serde::Deserialize<'de>,
{
    let payload = postcard::to_allocvec(&request).map_err(|_| SessionErr::Internal(0xE_SER))?;
    let words = build_words(payload.len());
    let reply = crate::ipc::call_procmgr(label, words, &payload)
        .map_err(|_| SessionErr::Internal(0xE_PROCMGR_DEAD))?;
    let result: Rep = postcard::from_bytes(&reply.payload)
        .map_err(|_| SessionErr::Internal(0xE_DESER))?;
    Ok(result)
}

pub fn create(req: SessionCreateRequest) -> Result<SessionCreateOk, SessionCreateErr> {
    let payload = postcard::to_allocvec(&req).map_err(|_| SessionCreateErr::Internal(0xE_SER))?;
    let words = build_words(payload.len());
    let reply = crate::ipc::call_procmgr(PROCMGR_SESSION_CREATE_LABEL, words, &payload)
        .map_err(|_| SessionCreateErr::Internal(0xE_PROCMGR_DEAD))?;
    let result: SessionCreateReply = postcard::from_bytes(&reply.payload)
        .map_err(|_| SessionCreateErr::Internal(0xE_DESER))?;
    result
}

pub fn destroy(token: TokenHandle) -> Result<(), SessionErr> {
    let reply: SessionDestroyReply = call_procmgr_postcard(
        PROCMGR_SESSION_DESTROY_LABEL,
        SessionDestroyRequest { token },
    )?;
    reply
}

pub fn query(token: TokenHandle) -> Result<SessionQueryReply, SessionErr> {
    call_procmgr_postcard(
        PROCMGR_SESSION_QUERY_LABEL,
        SessionQueryRequest { token },
    )
}

pub fn subscribe(token: TokenHandle, event_send: TokenHandle) -> Result<(), SessionErr> {
    let reply: SessionSubscribeReply = call_procmgr_postcard(
        PROCMGR_SESSION_SUBSCRIBE_LABEL,
        SessionSubscribeRequest { token, event_send },
    )?;
    reply
}

pub fn derive_token(token: TokenHandle, rights: u32) -> Result<TokenHandle, SessionErr> {
    let reply: SessionDeriveReply = call_procmgr_postcard(
        PROCMGR_SESSION_DERIVE_TOKEN_LABEL,
        SessionDeriveRequest { token, rights },
    )?;
    reply
}

pub fn set_leader(token: TokenHandle, leader_pid: u32) -> Result<(), SessionErr> {
    let reply: SessionSetLeaderReply = call_procmgr_postcard(
        PROCMGR_SESSION_SET_LEADER_LABEL,
        SessionSetLeaderRequest { token, leader_pid },
    )?;
    reply
}
```

- [ ] **Step 2: Add module to libcluu**

In `userspace/libcluu/src/lib.rs`:

```rust
pub mod session;
```

- [ ] **Step 3: Build**

```
cd /home/vlb2bp/git/cluu
cargo build -p libcluu
```

Expected: clean. Adapt `crate::ipc::call_procmgr` to actual signature if different.

- [ ] **Step 4: Commit**

```bash
git add userspace/libcluu/src/session.rs userspace/libcluu/src/lib.rs
git commit -m "feat(libcluu): session client wrapper"
```

---

## Task 3: `SessionObject` table + handlers

**Files:**
- Create: `userspace/procmgr/src/session_table.rs`
- Modify: `userspace/procmgr/src/lib.rs`

- [ ] **Step 1: Write the table module**

Create `userspace/procmgr/src/session_table.rs`:

```rust
//! Procmgr-owned SessionObject table.
//!
//! See spec 3 §4. Every session is a typed object addressed by IPC token.
//! Rights bitmask controls per-token capability.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

use cluu_proto::session::{ProfileSpec, RIGHT_SESSION_ALL, SessionErr, SessionState};
use cluu_proto::TokenHandle;

pub type SessionId = u32;

#[derive(Clone, Debug)]
pub struct SessionObject {
    pub id:           SessionId,
    pub user_name:    String,
    pub profile:      ProfileSpec,
    pub creator_pid:  u32,
    pub leader_pid:   Option<u32>,
    pub state:        SessionState,
    pub refcount:     u32,
    pub subscribers:  Vec<Subscriber>,
    pub created_at:   u64,
}

#[derive(Clone, Debug)]
pub struct Subscriber {
    pub event_send_cap: TokenHandle,
    pub owner_pid:      u32,
}

/// Per-token state: which session, what rights, who owns it.
#[derive(Clone, Debug)]
pub struct SessionTokenEntry {
    pub session_id: SessionId,
    pub rights:     u32,
    pub owner_pid:  u32,
}

pub struct SessionTable {
    inner: Mutex<SessionTableInner>,
}

struct SessionTableInner {
    next_session_id: SessionId,
    next_token: u64,
    sessions: BTreeMap<SessionId, SessionObject>,
    /// Token → (session_id, rights, owner_pid)
    tokens: BTreeMap<TokenHandle, SessionTokenEntry>,
}

impl SessionTable {
    pub const fn new() -> Self {
        Self {
            inner: Mutex::new(SessionTableInner {
                next_session_id: 1,
                next_token: 0xC0DE_0000_0000_0001,
                sessions: BTreeMap::new(),
                tokens: BTreeMap::new(),
            }),
        }
    }

    pub fn create(&self, user_name: String, profile: ProfileSpec, creator_pid: u32, now_ticks: u64)
        -> (SessionId, TokenHandle)
    {
        let mut g = self.inner.lock();
        let id = g.next_session_id;
        g.next_session_id = g.next_session_id.wrapping_add(1);
        let session = SessionObject {
            id, user_name, profile, creator_pid,
            leader_pid: None, state: SessionState::Live,
            refcount: 1, subscribers: Vec::new(),
            created_at: now_ticks,
        };
        g.sessions.insert(id, session);
        let token = g.next_token;
        g.next_token = g.next_token.wrapping_add(1);
        g.tokens.insert(token, SessionTokenEntry {
            session_id: id, rights: RIGHT_SESSION_ALL, owner_pid: creator_pid,
        });
        (id, token)
    }

    pub fn resolve(&self, token: TokenHandle, caller_pid: u32, required_rights: u32)
        -> Result<(SessionId, u32 /* rights */), SessionErr>
    {
        let g = self.inner.lock();
        let entry = g.tokens.get(&token).ok_or(SessionErr::InvalidToken)?;
        if entry.owner_pid != caller_pid {
            return Err(SessionErr::InvalidToken);
        }
        if (entry.rights & required_rights) != required_rights {
            return Err(SessionErr::InsufficientRights);
        }
        Ok((entry.session_id, entry.rights))
    }

    pub fn derive_token(
        &self, parent_token: TokenHandle, caller_pid: u32, requested_rights: u32,
        recipient_pid: u32,
    ) -> Result<TokenHandle, SessionErr> {
        let mut g = self.inner.lock();
        let entry = g.tokens.get(&parent_token).ok_or(SessionErr::InvalidToken)?.clone();
        if entry.owner_pid != caller_pid {
            return Err(SessionErr::InvalidToken);
        }
        if (entry.rights & requested_rights) != requested_rights {
            return Err(SessionErr::InsufficientRights);
        }
        let new_token = g.next_token;
        g.next_token = g.next_token.wrapping_add(1);
        g.tokens.insert(new_token, SessionTokenEntry {
            session_id: entry.session_id,
            rights:     requested_rights,
            owner_pid:  recipient_pid,
        });
        // Bump session refcount.
        if let Some(s) = g.sessions.get_mut(&entry.session_id) {
            s.refcount = s.refcount.saturating_add(1);
        }
        Ok(new_token)
    }

    pub fn set_leader(&self, token: TokenHandle, caller_pid: u32, leader_pid: u32,
                      check_member: impl Fn(u32, SessionId) -> bool)
        -> Result<(), SessionErr>
    {
        // First resolve.
        let session_id = {
            let g = self.inner.lock();
            let entry = g.tokens.get(&token).ok_or(SessionErr::InvalidToken)?;
            if entry.owner_pid != caller_pid {
                return Err(SessionErr::InvalidToken);
            }
            if (entry.rights & cluu_proto::session::RIGHT_SESSION_CONTROL) == 0 {
                return Err(SessionErr::InsufficientRights);
            }
            entry.session_id
        };
        if !check_member(leader_pid, session_id) {
            return Err(SessionErr::LeaderNotMember);
        }
        let mut g = self.inner.lock();
        let session = g.sessions.get_mut(&session_id).ok_or(SessionErr::NotFound)?;
        if session.leader_pid.is_some() {
            return Err(SessionErr::AlreadyHasLeader);
        }
        session.leader_pid = Some(leader_pid);
        Ok(())
    }

    pub fn subscribe(&self, token: TokenHandle, caller_pid: u32, event_send: TokenHandle)
        -> Result<(), SessionErr>
    {
        let session_id = {
            let g = self.inner.lock();
            let entry = g.tokens.get(&token).ok_or(SessionErr::InvalidToken)?;
            if entry.owner_pid != caller_pid {
                return Err(SessionErr::InvalidToken);
            }
            if (entry.rights & cluu_proto::session::RIGHT_SESSION_SUBSCRIBE) == 0 {
                return Err(SessionErr::InsufficientRights);
            }
            entry.session_id
        };
        let mut g = self.inner.lock();
        let session = g.sessions.get_mut(&session_id).ok_or(SessionErr::NotFound)?;
        session.subscribers.push(Subscriber { event_send_cap: event_send, owner_pid: caller_pid });
        session.refcount = session.refcount.saturating_add(1);
        Ok(())
    }

    pub fn snapshot(&self, session_id: SessionId) -> Option<SessionObject> {
        self.inner.lock().sessions.get(&session_id).cloned()
    }

    pub fn mark_dying(&self, session_id: SessionId) -> Option<Vec<Subscriber>> {
        let mut g = self.inner.lock();
        let session = g.sessions.get_mut(&session_id)?;
        if session.state == SessionState::Dying {
            return Some(Vec::new());
        }
        session.state = SessionState::Dying;
        Some(session.subscribers.clone())
    }

    pub fn remove_if_unref(&self, session_id: SessionId) {
        let mut g = self.inner.lock();
        if let Some(s) = g.sessions.get(&session_id) {
            if s.refcount == 0 && s.state == SessionState::Dying {
                g.sessions.remove(&session_id);
            }
        }
    }

    /// On token-owner exit: drop all tokens owned by the dying pid and
    /// decrement the corresponding sessions' refcounts.
    pub fn on_pid_exit(&self, dead_pid: u32) -> Vec<SessionId> {
        let mut g = self.inner.lock();
        let dead_tokens: Vec<TokenHandle> = g.tokens.iter()
            .filter_map(|(t, e)| if e.owner_pid == dead_pid { Some(*t) } else { None })
            .collect();
        let mut affected = Vec::new();
        for t in dead_tokens {
            if let Some(entry) = g.tokens.remove(&t) {
                if let Some(s) = g.sessions.get_mut(&entry.session_id) {
                    s.refcount = s.refcount.saturating_sub(1);
                }
                affected.push(entry.session_id);
            }
        }
        affected
    }
}

pub static SESSION_TABLE: SessionTable = SessionTable::new();
```

- [ ] **Step 2: Declare module**

In `userspace/procmgr/src/lib.rs`:

```rust
pub mod session_table;
```

- [ ] **Step 3: Build**

```
cd /home/vlb2bp/git/cluu
cargo build -p procmgr
```

Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add userspace/procmgr/src/lib.rs userspace/procmgr/src/session_table.rs
git commit -m "feat(procmgr): SessionObject table + cap-derive primitives"
```

---

## Task 4: Procmgr verb dispatch arms

**Files:**
- Modify: `userspace/procmgr/src/main.rs`

- [ ] **Step 1: Locate the dispatch site**

```
cd /home/vlb2bp/git/cluu
grep -n "msg.tag.label ==" userspace/procmgr/src/main.rs | head -20
```

Identify the receive loop in main.

- [ ] **Step 2: Add five dispatch arms**

Add (in the same dispatch block):

```rust
if msg.tag.label == cluu_proto::session::PROCMGR_SESSION_CREATE_LABEL {
    return self.handle_session_create(msg, payload, sender_tid);
}
if msg.tag.label == cluu_proto::session::PROCMGR_SESSION_SET_LEADER_LABEL {
    return self.handle_session_set_leader(msg, payload, sender_tid);
}
if msg.tag.label == cluu_proto::session::PROCMGR_SESSION_QUERY_LABEL {
    return self.handle_session_query(msg, payload, sender_tid);
}
if msg.tag.label == cluu_proto::session::PROCMGR_SESSION_SUBSCRIBE_LABEL {
    return self.handle_session_subscribe(msg, payload, sender_tid);
}
if msg.tag.label == cluu_proto::session::PROCMGR_SESSION_DERIVE_TOKEN_LABEL {
    return self.handle_session_derive(msg, payload, sender_tid);
}
if msg.tag.label == cluu_proto::session::PROCMGR_SESSION_DESTROY_LABEL {
    return self.handle_session_destroy(msg, payload, sender_tid);
}
```

- [ ] **Step 3: Add each handler method**

Inside the same `impl` block (anchor placement next to existing `handle_*` methods):

```rust
fn handle_session_create(&mut self, msg: Message, payload: &[u8], sender_tid: TidLike) -> ReplyResult {
    use cluu_proto::session::*;
    use cluu_proto::ABI_VERSION;

    if msg.tag.words[1] != ABI_VERSION {
        return self.send_session_reply::<SessionCreateReply>(
            msg.tag.reply_id, PROCMGR_SESSION_CREATE_LABEL,
            Err(SessionCreateErr::Internal(0xE_BADABI)));
    }
    let req: SessionCreateRequest = match postcard::from_bytes(payload) {
        Ok(r) => r,
        Err(_) => return self.send_session_reply::<SessionCreateReply>(
            msg.tag.reply_id, PROCMGR_SESSION_CREATE_LABEL,
            Err(SessionCreateErr::Internal(0xE_BADENV))),
    };
    let caller_pid = self.tid_to_pid(sender_tid).unwrap_or(0);

    // Manifest right check.
    if !self.caller_has_right(caller_pid, "RIGHT_SESSION_CREATE") {
        return self.send_session_reply::<SessionCreateReply>(
            msg.tag.reply_id, PROCMGR_SESSION_CREATE_LABEL,
            Err(SessionCreateErr::PermissionDenied));
    }

    let now = self.now_ticks();
    let (session_id, token) = crate::session_table::SESSION_TABLE.create(
        req.user_name, req.profile, caller_pid, now);

    // Inform the existing process-exit-hook to drop this session if the
    // creator exits before SET_LEADER fires.
    self.session_creators.insert(caller_pid, session_id);

    self.send_session_reply::<SessionCreateReply>(
        msg.tag.reply_id, PROCMGR_SESSION_CREATE_LABEL,
        Ok(SessionCreateOk { token, session_id }))
}

fn handle_session_set_leader(&mut self, msg: Message, payload: &[u8], sender_tid: TidLike) -> ReplyResult {
    use cluu_proto::session::*;
    use cluu_proto::ABI_VERSION;

    if msg.tag.words[1] != ABI_VERSION {
        return self.send_session_reply::<SessionSetLeaderReply>(
            msg.tag.reply_id, PROCMGR_SESSION_SET_LEADER_LABEL,
            Err(SessionErr::Internal(0xE_BADABI)));
    }
    let req: SessionSetLeaderRequest = match postcard::from_bytes(payload) {
        Ok(r) => r,
        Err(_) => return self.send_session_reply::<SessionSetLeaderReply>(
            msg.tag.reply_id, PROCMGR_SESSION_SET_LEADER_LABEL,
            Err(SessionErr::Internal(0xE_BADENV))),
    };
    let caller_pid = self.tid_to_pid(sender_tid).unwrap_or(0);

    // Wire the check_member closure: ProcessEntry.session_id == session_id?
    let result = crate::session_table::SESSION_TABLE.set_leader(
        req.token, caller_pid, req.leader_pid,
        |pid, sid| self.process_session_id(pid) == Some(sid));

    self.send_session_reply::<SessionSetLeaderReply>(
        msg.tag.reply_id, PROCMGR_SESSION_SET_LEADER_LABEL, result)
}

fn handle_session_query(&mut self, msg: Message, payload: &[u8], sender_tid: TidLike) -> ReplyResult {
    use cluu_proto::session::*;
    use cluu_proto::ABI_VERSION;
    let _ = ABI_VERSION;
    let req: SessionQueryRequest = match postcard::from_bytes(payload) {
        Ok(r) => r,
        Err(_) => return self.send_session_reply::<Result<SessionQueryReply, SessionErr>>(
            msg.tag.reply_id, PROCMGR_SESSION_QUERY_LABEL,
            Err(SessionErr::Internal(0xE_BADENV))),
    };
    let caller_pid = self.tid_to_pid(sender_tid).unwrap_or(0);
    let resolved = crate::session_table::SESSION_TABLE.resolve(
        req.token, caller_pid, RIGHT_SESSION_QUERY);
    let result: Result<SessionQueryReply, SessionErr> = match resolved {
        Err(e) => Err(e),
        Ok((sid, _)) => {
            match crate::session_table::SESSION_TABLE.snapshot(sid) {
                None => Err(SessionErr::NotFound),
                Some(s) => Ok(SessionQueryReply {
                    session_id: s.id,
                    user_name: s.user_name.clone(),
                    leader_pid: s.leader_pid,
                    state: s.state,
                    member_pids: self.members_of_session(sid),
                }),
            }
        }
    };
    self.send_session_reply::<Result<SessionQueryReply, SessionErr>>(
        msg.tag.reply_id, PROCMGR_SESSION_QUERY_LABEL, result)
}

fn handle_session_subscribe(&mut self, msg: Message, payload: &[u8], sender_tid: TidLike) -> ReplyResult {
    use cluu_proto::session::*;
    let req: SessionSubscribeRequest = match postcard::from_bytes(payload) {
        Ok(r) => r,
        Err(_) => return self.send_session_reply::<SessionSubscribeReply>(
            msg.tag.reply_id, PROCMGR_SESSION_SUBSCRIBE_LABEL,
            Err(SessionErr::Internal(0xE_BADENV))),
    };
    let caller_pid = self.tid_to_pid(sender_tid).unwrap_or(0);

    // Derive a procmgr-owned send-cap on caller's event endpoint.
    let derived = match self.derive_send_cap_for_event(req.event_send, caller_pid) {
        Some(d) => d,
        None => return self.send_session_reply::<SessionSubscribeReply>(
            msg.tag.reply_id, PROCMGR_SESSION_SUBSCRIBE_LABEL,
            Err(SessionErr::InvalidToken)),
    };

    let result = crate::session_table::SESSION_TABLE.subscribe(
        req.token, caller_pid, derived);
    self.send_session_reply::<SessionSubscribeReply>(
        msg.tag.reply_id, PROCMGR_SESSION_SUBSCRIBE_LABEL, result)
}

fn handle_session_derive(&mut self, msg: Message, payload: &[u8], sender_tid: TidLike) -> ReplyResult {
    use cluu_proto::session::*;
    let req: SessionDeriveRequest = match postcard::from_bytes(payload) {
        Ok(r) => r,
        Err(_) => return self.send_session_reply::<SessionDeriveReply>(
            msg.tag.reply_id, PROCMGR_SESSION_DERIVE_TOKEN_LABEL,
            Err(SessionErr::Internal(0xE_BADENV))),
    };
    let caller_pid = self.tid_to_pid(sender_tid).unwrap_or(0);
    // The new token's recipient: for now, same as caller (caller_pid distributes
    // out-of-band). A future ergonomic step is to take an explicit recipient_pid.
    let result = crate::session_table::SESSION_TABLE.derive_token(
        req.token, caller_pid, req.rights, caller_pid);
    self.send_session_reply::<SessionDeriveReply>(
        msg.tag.reply_id, PROCMGR_SESSION_DERIVE_TOKEN_LABEL, result)
}

fn handle_session_destroy(&mut self, msg: Message, payload: &[u8], sender_tid: TidLike) -> ReplyResult {
    use cluu_proto::session::*;
    let req: SessionDestroyRequest = match postcard::from_bytes(payload) {
        Ok(r) => r,
        Err(_) => return self.send_session_reply::<SessionDestroyReply>(
            msg.tag.reply_id, PROCMGR_SESSION_DESTROY_LABEL,
            Err(SessionErr::Internal(0xE_BADENV))),
    };
    let caller_pid = self.tid_to_pid(sender_tid).unwrap_or(0);
    let resolved = crate::session_table::SESSION_TABLE.resolve(
        req.token, caller_pid, RIGHT_SESSION_CONTROL);
    match resolved {
        Err(e) => self.send_session_reply::<SessionDestroyReply>(
            msg.tag.reply_id, PROCMGR_SESSION_DESTROY_LABEL, Err(e)),
        Ok((sid, _)) => {
            self.destroy_session(sid);
            self.send_session_reply::<SessionDestroyReply>(
                msg.tag.reply_id, PROCMGR_SESSION_DESTROY_LABEL, Ok(()))
        }
    }
}

/// Shared helper for serializing session replies.
fn send_session_reply<R: serde::Serialize>(&mut self, reply_id: u64, label: u32, value: R) -> ReplyResult {
    let bytes = postcard::to_allocvec(&value).expect("postcard serialize");
    let mut words = [0u64; 6];
    words[0] = bytes.len() as u64;
    words[1] = cluu_proto::ABI_VERSION as u64;
    self.send_reply(reply_id, label, words, &bytes)
}
```

The engineer wires placeholder helpers (`caller_has_right`, `now_ticks`, `session_creators`, `process_session_id`, `members_of_session`, `derive_send_cap_for_event`, `destroy_session`) to existing or new procmgr internals.

- [ ] **Step 4: Add the destroy + cascade helper**

```rust
fn destroy_session(&mut self, sid: u32) {
    let subscribers = match crate::session_table::SESSION_TABLE.mark_dying(sid) {
        None => return,
        Some(s) => s,
    };
    // Walk members in our own table; SIGHUP each.
    for pid in self.members_of_session(sid) {
        const SIGHUP: u32 = 1;
        self.send_signal(pid, SIGHUP);
    }
    // Fanout SESSION_ENDED to subscribers.
    let event = cluu_proto::session::SessionEndedEvent { session_id: sid };
    let bytes = postcard::to_allocvec(&event).expect("ser");
    for sub in subscribers {
        let mut words = [0u64; 6];
        words[0] = bytes.len() as u64;
        words[1] = cluu_proto::ABI_VERSION as u64;
        let _ = self.send_via_token(sub.event_send_cap,
            cluu_proto::session::SESSION_ENDED_LABEL, words, &bytes);
    }
    // GC happens later when refcount drops to 0 (on token-owner exits).
}
```

- [ ] **Step 5: Wire on_process_exit to cover leader / creator-orphan / token-owner**

Locate the existing process-exit hook (likely `fn on_process_exit` or similar):

```rust
fn on_process_exit(&mut self, dead_pid: u32, _exit_code: i32) {
    // 1. Decrement session refcounts for any tokens this pid held.
    let affected = crate::session_table::SESSION_TABLE.on_pid_exit(dead_pid);

    // 2. If pid is a session leader → destroy that session.
    let leader_sessions: alloc::vec::Vec<u32> = affected.iter().copied()
        .filter(|&sid| {
            crate::session_table::SESSION_TABLE.snapshot(sid)
                .map(|s| s.leader_pid == Some(dead_pid))
                .unwrap_or(false)
        })
        .collect();
    for sid in leader_sessions {
        self.destroy_session(sid);
    }

    // 3. If pid is creator of a session that has NO leader yet → destroy it.
    if let Some(sid) = self.session_creators.remove(&dead_pid) {
        if crate::session_table::SESSION_TABLE.snapshot(sid)
            .map(|s| s.leader_pid.is_none())
            .unwrap_or(false)
        {
            self.destroy_session(sid);
        }
    }

    // 4. GC sessions whose refcount reached 0.
    for sid in affected {
        crate::session_table::SESSION_TABLE.remove_if_unref(sid);
    }

    // ... existing on_process_exit body ...
}
```

- [ ] **Step 6: Build**

```
cd /home/vlb2bp/git/cluu
cargo build -p procmgr
```

Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add userspace/procmgr/src/main.rs
git commit -m "feat(procmgr): session verb handlers + cascade-destroy on leader/creator exit"
```

---

## Task 5: Compositor handoff verb + subscribe machinery

**Files:**
- Modify: `userspace/compositor/src/protocol.rs`
- Modify: `userspace/compositor/src/main.rs`
- Modify: `userspace/compositor/src/state.rs`

- [ ] **Step 1: Add label const + per-window session field**

In `userspace/compositor/src/protocol.rs`:

```rust
pub use cluu_proto::session::COMPOSITOR_SESSION_HANDOFF_LABEL;
```

In `userspace/compositor/src/state.rs`, locate the `Window` struct (or similar). Add field:

```rust
pub session_id: Option<u32>,
```

Initialize to `None` everywhere a `Window` is constructed today.

- [ ] **Step 2: Wire dispatch arm in compositor's main loop**

```
cd /home/vlb2bp/git/cluu
grep -n "msg.tag.label ==" userspace/compositor/src/main.rs | head -10
```

Add an arm:

```rust
if msg.tag.label == COMPOSITOR_SESSION_HANDOFF_LABEL {
    return self.handle_session_handoff(msg, payload, sender_tid);
}
if msg.tag.label == cluu_proto::session::SESSION_ENDED_LABEL {
    return self.handle_session_ended(msg, payload);
}
```

- [ ] **Step 3: Implement the two handlers**

```rust
fn handle_session_handoff(&mut self, msg: Message, payload: &[u8], _sender_tid: TidLike) -> ReplyResult {
    use cluu_proto::session::*;
    let req: CompositorSessionHandoffRequest = match postcard::from_bytes(payload) {
        Ok(r) => r,
        Err(_) => return self.reply_handoff_err(msg.tag.reply_id, SessionErr::Internal(0xE_BADENV)),
    };

    // Mint a procmgr-side receiving endpoint for SESSION_ENDED.
    let our_event_send = self.event_endpoint_send_cap();

    // Subscribe via libcluu wrapper.
    match libcluu::session::subscribe(req.token_sub, our_event_send) {
        Ok(()) => {
            self.tracked_sessions.insert(req.session_id);
            let reply: CompositorSessionHandoffReply = Ok(());
            self.reply_postcard(msg.tag.reply_id, COMPOSITOR_SESSION_HANDOFF_LABEL, &reply)
        }
        Err(e) => self.reply_handoff_err(msg.tag.reply_id, e),
    }
}

fn handle_session_ended(&mut self, _msg: Message, payload: &[u8]) -> ReplyResult {
    let event: cluu_proto::session::SessionEndedEvent = match postcard::from_bytes(payload) {
        Ok(e) => e,
        Err(_) => return ReplyResult::Ok,
    };
    // Close all windows for this session.
    let to_close: alloc::vec::Vec<u32> = self.windows.iter()
        .filter(|(_, w)| w.session_id == Some(event.session_id))
        .map(|(id, _)| *id)
        .collect();
    for window_id in to_close {
        self.close_window(window_id);
    }
    self.tracked_sessions.remove(&event.session_id);
    // Spawn a fresh login.
    self.spawn_login_window();
    ReplyResult::Ok
}

fn spawn_login_window(&mut self) {
    use cluu_proto::spawn::{SpawnEnvelope, ViewSource};
    let envelope = SpawnEnvelope {
        image: alloc::string::String::from("login"),
        args: alloc::vec::Vec::new(),
        env: alloc::vec::Vec::new(),
        view: ViewSource::Derive(self.compositor_view_token()),
        fd_inherit: alloc::vec::Vec::new(),
        session: None, // login is sessionless
        notify: None,
    };
    if let Err(e) = libcluu::ipc::spawn(envelope) {
        libcluu::print_log(&alloc::format!("compositor: login spawn failed {:?}\n", e));
    }
}

fn reply_handoff_err(&mut self, reply_id: u64, err: cluu_proto::session::SessionErr) -> ReplyResult {
    let reply: cluu_proto::session::CompositorSessionHandoffReply = Err(err);
    self.reply_postcard(reply_id, cluu_proto::session::COMPOSITOR_SESSION_HANDOFF_LABEL, &reply)
}

fn reply_postcard<R: serde::Serialize>(&mut self, reply_id: u64, label: u32, value: &R) -> ReplyResult {
    let bytes = postcard::to_allocvec(value).expect("ser");
    let mut words = [0u64; 6];
    words[0] = bytes.len() as u64;
    words[1] = cluu_proto::ABI_VERSION as u64;
    libcluu::ipc::reply(reply_id, label, words, &bytes)
}
```

Engineer wires `compositor_view_token`, `event_endpoint_send_cap`, `tracked_sessions` (a `BTreeSet<u32>` on compositor state), and `close_window` to existing internal compositor primitives.

- [ ] **Step 4: Build**

```
cd /home/vlb2bp/git/cluu
cargo build -p compositor
```

Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add userspace/compositor/src/main.rs userspace/compositor/src/state.rs userspace/compositor/src/protocol.rs
git commit -m "feat(compositor): COMPOSITOR_SESSION_HANDOFF + SESSION_ENDED subscriber"
```

---

## Task 6: Login binary rewrite

**Files:**
- Modify: `userspace/login/src/main.rs`

- [ ] **Step 1: Read existing login main**

```
cd /home/vlb2bp/git/cluu
wc -l userspace/login/src/main.rs
grep -n "PROCMGR_SESSION_LOGIN_LABEL\|authd\|password" userspace/login/src/main.rs | head -20
```

Existing flow: login draws its window, collects creds, sends `PROCMGR_SESSION_LOGIN_LABEL` to procmgr. The new flow replaces that single call with five steps.

- [ ] **Step 2: Replace the post-auth block**

After the existing `authd → AUTH_VALIDATE` succeeds:

```rust
use cluu_proto::session::{
    ProfileSpec, SessionCreateRequest,
    RIGHT_SESSION_QUERY, RIGHT_SESSION_SUBSCRIBE,
    CompositorSessionHandoffRequest, COMPOSITOR_SESSION_HANDOFF_LABEL,
};
use cluu_proto::spawn::{SpawnEnvelope, ViewSource};

// 1. SESSION_CREATE.
let create_reply = libcluu::session::create(SessionCreateRequest {
    user_name: user_name.clone(),
    profile: ProfileSpec {
        home: alloc::format!("/home/{}", user_name),
        initial_view: ViewSource::Derive(self.login_view_token()),
        env: alloc::vec![
            (alloc::string::String::from("HOME"), alloc::format!("/home/{}", user_name)),
            (alloc::string::String::from("USER"), user_name.clone()),
            (alloc::string::String::from("TERM"), alloc::string::String::from("xterm-256color")),
        ],
        umask: 0o022,
    },
});
let ok = match create_reply {
    Ok(o) => o,
    Err(e) => {
        libcluu::print_log(&alloc::format!("login: SESSION_CREATE failed: {:?}\n", e));
        return -1;
    }
};

// 2. Derive a narrowed token for compositor (subscribe + query only).
let token_sub = match libcluu::session::derive_token(
    ok.token, RIGHT_SESSION_SUBSCRIBE | RIGHT_SESSION_QUERY)
{
    Ok(t) => t,
    Err(e) => {
        libcluu::print_log(&alloc::format!("login: derive_token failed: {:?}\n", e));
        return -1;
    }
};

// 3. Hand off to compositor (COMPOSITOR_SESSION_HANDOFF on compositor:control).
let handoff_req = CompositorSessionHandoffRequest {
    session_id: ok.session_id,
    token_sub,
};
let payload = postcard::to_allocvec(&handoff_req).expect("ser");
let mut words = [0u64; 6];
words[0] = payload.len() as u64;
words[1] = cluu_proto::ABI_VERSION as u64;
let compositor_control = libcluu::registry::lookup("compositor:control")
    .expect("compositor:control must exist");
if let Err(e) = libcluu::ipc::call(compositor_control, COMPOSITOR_SESSION_HANDOFF_LABEL,
                                    words, &payload)
{
    libcluu::print_log(&alloc::format!("login: handoff IPC failed: {:?}\n", e));
    return -1;
}

// 4. Spawn the user's primary process (cluuterm by default).
let primary_envelope = SpawnEnvelope {
    image: alloc::string::String::from("cluuterm"),
    args: alloc::vec::Vec::new(),
    env: alloc::vec![
        (alloc::string::String::from("HOME"), alloc::format!("/home/{}", user_name)),
        (alloc::string::String::from("USER"), user_name.clone()),
    ],
    view: ViewSource::Derive(self.login_view_token()), // procmgr narrows per cluuterm manifest
    fd_inherit: alloc::vec::Vec::new(),
    session: Some(ok.token),
    notify: None,
};
let spawn_reply = libcluu::ipc::spawn(primary_envelope);
let primary_pid = match spawn_reply {
    Ok(r) => r.pid,
    Err(e) => {
        libcluu::print_log(&alloc::format!("login: primary spawn failed: {:?}\n", e));
        return -1;
    }
};

// 5. SESSION_SET_LEADER.
if let Err(e) = libcluu::session::set_leader(ok.token, primary_pid) {
    libcluu::print_log(&alloc::format!("login: set_leader failed: {:?}\n", e));
    return -1;
}

// 6. Exit cleanly.
return 0;
```

Delete the existing `PROCMGR_SESSION_LOGIN_LABEL` send and surrounding wait.

- [ ] **Step 3: Build**

```
cd /home/vlb2bp/git/cluu
cargo build -p login
```

Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add userspace/login/src/main.rs
git commit -m "feat(login): post-auth flow uses SESSION_CREATE + SET_LEADER"
```

---

## Task 7: Manifest updates (login + compositor)

**Files:**
- Modify: `/var/images/login/manifest.toml` (locate first)
- Modify: `/var/images/compositor/manifest.toml`

- [ ] **Step 1: Locate manifests**

```
cd /home/vlb2bp/git/cluu
find . -name "manifest.toml" -path "*/login/*" 2>/dev/null
find . -name "manifest.toml" -path "*/compositor/*" 2>/dev/null
```

- [ ] **Step 2: Update login's manifest**

Add/update directives:

```
ENTRYPOINT  /bin/login
RESTART     never
SESSIONLESS allow
RIGHTS      RIGHT_SESSION_CREATE \
            RIGHT_SPAWN \
            RIGHT_AUTH_VALIDATE \
            COMPOSITOR_HANDOFF
```

- [ ] **Step 3: Update compositor's manifest**

```
RESTART always
```

(Other directives unchanged.)

- [ ] **Step 4: Build + boot smoke**

```
cd /home/vlb2bp/git/cluu
cargo xtask build
bash scripts/harness_run.sh
```

Expected: boot reaches `compositor: ready`; login window appears.

- [ ] **Step 5: Commit**

```bash
git add var/images/login/manifest.toml var/images/compositor/manifest.toml
git commit -m "feat(manifests): login + compositor lifecycle declarations"
```

---

## Task 8: Compositor boot-time login spawn

**Files:**
- Modify: `userspace/compositor/src/main.rs`

- [ ] **Step 1: Locate the compositor's startup sequence**

```
cd /home/vlb2bp/git/cluu
grep -n "fn main\|fn run\|register.*compositor:client" userspace/compositor/src/main.rs | head -10
```

Identify the function that runs after framebuffer + input init.

- [ ] **Step 2: Add login spawn after init**

Right after the framebuffer is up and `compositor:client` is registered, add:

```rust
self.spawn_login_window();
```

The helper `spawn_login_window` was added in Task 5 Step 3.

- [ ] **Step 3: Build + boot smoke**

```
cd /home/vlb2bp/git/cluu
cargo xtask build
bash scripts/harness_run.sh
```

Expected: login window visible at boot.

- [ ] **Step 4: Commit**

```bash
git add userspace/compositor/src/main.rs
git commit -m "feat(compositor): spawn login window at boot"
```

---

## Task 9: Delete `PROCMGR_SESSION_LOGIN_LABEL` machinery + 2s timeout

**Files:**
- Modify: `userspace/procmgr/src/main.rs`
- Modify: `userspace/libcluu/src/ipc.rs`

- [ ] **Step 1: Identify everything to delete**

```
cd /home/vlb2bp/git/cluu
git grep -n "PROCMGR_SESSION_LOGIN_LABEL"
git grep -n "kill_system_compositor"
git grep -n "spawn_user_compositor"
git grep -n "system_compositor_pid"
git grep -n "session_mode"
git grep -n "COMPOSITOR_READY_LABEL"
git grep -n "wait.*COMPOSITOR_READY"
```

Each match is either a definition to remove or a caller to redirect.

- [ ] **Step 2: Delete the SESSION_LOGIN handler**

In `userspace/procmgr/src/main.rs`:
- Delete the dispatch arm `if msg.tag.label == PROCMGR_SESSION_LOGIN_LABEL { ... }`.
- Delete the function `fn handle_session_login`.
- Delete `fn kill_system_compositor`.
- Delete `fn spawn_user_compositor`.
- Delete the global `system_compositor_pid` field.
- Delete `session_mode` PARAM parsing.
- Delete the 8 admin `force_unregister(...)` calls on `compositor:*` services.
- Delete the `wait_label_with_timeout(COMPOSITOR_READY_LABEL, 2_000_ms)` site.

In `userspace/libcluu/src/ipc.rs`:
- Delete `PROCMGR_SESSION_LOGIN_LABEL` const.
- Delete `COMPOSITOR_READY_LABEL` const.

- [ ] **Step 3: Build clean**

```
cd /home/vlb2bp/git/cluu
cargo xtask build
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: both clean.

- [ ] **Step 4: Verify zero hits**

```
cd /home/vlb2bp/git/cluu
git grep -c "PROCMGR_SESSION_LOGIN_LABEL"   && echo "FAIL" || echo "PASS"
git grep -c "COMPOSITOR_READY_LABEL"        && echo "FAIL" || echo "PASS"
git grep -c "kill_system_compositor"        && echo "FAIL" || echo "PASS"
git grep -c "spawn_user_compositor"         && echo "FAIL" || echo "PASS"
git grep -c "system_compositor_pid"         && echo "FAIL" || echo "PASS"
git grep -c "session_mode"                  && echo "FAIL" || echo "PASS"
```

All must print PASS.

- [ ] **Step 5: Boot smoke + interactive login**

```
bash scripts/harness_run.sh
```

Expected: boot reaches `compositor: ready`; login window appears; root/root login succeeds; cluuterm window with shell prompt.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "refactor: delete PROCMGR_SESSION_LOGIN_LABEL machinery + 2s COMPOSITOR_READY wait"
```

---

## Task 10: Getty binary for text-VT

**Files:**
- Create: `userspace/getty/Cargo.toml`
- Create: `userspace/getty/src/main.rs`
- Create: `/var/images/getty/manifest.toml`
- Modify: `Cargo.toml` (workspace member)
- Modify: `/etc/autostart.toml`

- [ ] **Step 1: Add workspace member**

Add `"userspace/getty",` to workspace `Cargo.toml` members.

- [ ] **Step 2: Write `userspace/getty/Cargo.toml`**

Copy structure from another small binary like `userspace/probes/argvprobe/Cargo.toml`:

```toml
[package]
name = "getty"
version = "0.1.0"
edition = "2021"

[dependencies]
libcluu = { path = "../libcluu", features = ["posix"] }
cluu_proto = { path = "../cluu_proto" }
postcard = { workspace = true }

[[bin]]
name = "getty"
path = "src/main.rs"
```

- [ ] **Step 3: Write `userspace/getty/src/main.rs`**

```rust
#![no_std]
#![no_main]
extern crate alloc;
extern crate libcluu;

use cluu_proto::spawn::{FdInherit, FdRights, FdSource, SpawnEnvelope, ViewSource};
use cluu_proto::session::{ProfileSpec, SessionCreateRequest};

fn parse_tty_path(argv: &[&str]) -> alloc::string::String {
    if argv.len() >= 2 {
        alloc::string::String::from(argv[1])
    } else {
        alloc::string::String::from("/dev/tty1")
    }
}

#[no_mangle]
pub extern "C" fn main(argc: i32, argv: *const *const u8) -> i32 {
    // Build argv slice.
    let mut args: alloc::vec::Vec<&str> = alloc::vec::Vec::new();
    unsafe {
        for i in 0..argc {
            let p = *argv.offset(i as isize);
            if p.is_null() { continue; }
            let mut end = 0usize;
            while *p.add(end) != 0 { end += 1; }
            let slice = core::slice::from_raw_parts(p, end);
            if let Ok(s) = core::str::from_utf8(slice) { args.push(s); }
        }
    }

    let tty_path = parse_tty_path(&args);
    libcluu::print_log(&alloc::format!("getty: starting on {}\n", tty_path));

    // Open /dev/tty<n> for stdin/stdout/stderr.
    let fd_in  = libcluu::posix::open(&tty_path, libcluu::posix::O_RDONLY).expect("open tty");
    let fd_out = libcluu::posix::open(&tty_path, libcluu::posix::O_WRONLY).expect("open tty");
    let fd_err = libcluu::posix::open(&tty_path, libcluu::posix::O_WRONLY).expect("open tty");

    // Prompt for username + password.
    libcluu::posix::write(fd_out, b"cluu login: ").ok();
    let user_name = read_line(fd_in).unwrap_or_else(|| alloc::string::String::from("root"));

    // Disable ECHO before reading password.
    let mut t: cluu_proto::pts::Termios = unsafe { core::mem::zeroed() };
    unsafe { libcluu::posix::termios::tcgetattr(fd_in, &mut t as *mut _) };
    let saved = t;
    t.c_lflag &= !cluu_proto::pts::Termios::ECHO;
    unsafe { libcluu::posix::termios::tcsetattr(fd_in, 0 /* TCSANOW */, &t as *const _) };

    libcluu::posix::write(fd_out, b"password: ").ok();
    let password = read_line(fd_in).unwrap_or_default();

    // Restore termios.
    unsafe { libcluu::posix::termios::tcsetattr(fd_in, 0, &saved as *const _) };
    libcluu::posix::write(fd_out, b"\n").ok();

    // Validate via authd.
    let validated = libcluu::authd::validate(&user_name, &password);
    if !validated {
        libcluu::posix::write(fd_out, b"Login incorrect.\n").ok();
        return 1;
    }

    // SESSION_CREATE.
    let create_reply = libcluu::session::create(SessionCreateRequest {
        user_name: user_name.clone(),
        profile: ProfileSpec {
            home: alloc::format!("/home/{}", user_name),
            initial_view: ViewSource::Derive(getty_view_token()),
            env: alloc::vec![
                (alloc::string::String::from("TERM"), alloc::string::String::from("vt100")),
                (alloc::string::String::from("HOME"), alloc::format!("/home/{}", user_name)),
                (alloc::string::String::from("USER"), user_name.clone()),
            ],
            umask: 0o022,
        },
    });
    let ok = match create_reply {
        Ok(o) => o,
        Err(e) => {
            libcluu::print_log(&alloc::format!("getty: SESSION_CREATE failed {:?}\n", e));
            return 1;
        }
    };

    // Spawn the user's shell on this tty.
    let (stdin_cid,  stdin_rfd)  = libcluu::fd_table::vfs_addr(fd_in).expect("vfs_addr");
    let (stdout_cid, stdout_rfd) = libcluu::fd_table::vfs_addr(fd_out).expect("vfs_addr");
    let (stderr_cid, stderr_rfd) = libcluu::fd_table::vfs_addr(fd_err).expect("vfs_addr");

    let envelope = SpawnEnvelope {
        image: alloc::string::String::from("shell"),
        args: alloc::vec::Vec::new(),
        env: alloc::vec![
            (alloc::string::String::from("TERM"), alloc::string::String::from("vt100")),
            (alloc::string::String::from("HOME"), alloc::format!("/home/{}", user_name)),
            (alloc::string::String::from("USER"), user_name.clone()),
        ],
        view: ViewSource::Derive(getty_view_token()),
        fd_inherit: alloc::vec![
            FdInherit { child_fd: 0, source: FdSource::VfsFd { vfs_client_id: stdin_cid,  vfs_remote_fd: stdin_rfd  }, rights: FdRights::READ_ONLY },
            FdInherit { child_fd: 1, source: FdSource::VfsFd { vfs_client_id: stdout_cid, vfs_remote_fd: stdout_rfd }, rights: FdRights::WRITE_ONLY },
            FdInherit { child_fd: 2, source: FdSource::VfsFd { vfs_client_id: stderr_cid, vfs_remote_fd: stderr_rfd }, rights: FdRights::WRITE_ONLY },
        ],
        session: Some(ok.token),
        notify: None,
    };
    let shell_reply = libcluu::ipc::spawn(envelope);
    let shell_pid = match shell_reply {
        Ok(r) => r.pid,
        Err(e) => {
            libcluu::print_log(&alloc::format!("getty: shell spawn failed {:?}\n", e));
            return 1;
        }
    };

    // SET_LEADER.
    if let Err(e) = libcluu::session::set_leader(ok.token, shell_pid) {
        libcluu::print_log(&alloc::format!("getty: set_leader failed {:?}\n", e));
        return 1;
    }

    // Close parent-side fds and exit. Procmgr respawns getty via manifest RESTART always.
    libcluu::posix::close(fd_in);
    libcluu::posix::close(fd_out);
    libcluu::posix::close(fd_err);
    0
}

fn read_line(fd: i32) -> Option<alloc::string::String> {
    let mut buf = alloc::vec::Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = libcluu::posix::read(fd, &mut byte).ok()?;
        if n == 0 { return None; }
        if byte[0] == b'\n' { break; }
        buf.push(byte[0]);
    }
    alloc::string::String::from_utf8(buf).ok()
}

fn getty_view_token() -> u64 {
    // Getty inherits init's view via its primordial spawn; the exact token
    // comes through the ProcessInfo page.
    libcluu::process::self_view_token()
}
```

If helpers don't exist (e.g., `libcluu::authd::validate`), engineer creates trivial wrappers.

- [ ] **Step 4: Write `/var/images/getty/manifest.toml`**

```toml
ENTRYPOINT = "/bin/getty"
RESTART    = "always"
SESSIONLESS = "allow"

[rights]
session_create = true
spawn = true
auth_validate = true
```

(Adapt syntax to existing manifest format.)

- [ ] **Step 5: Add autostart entries**

In `/etc/autostart.toml` (locate via `find . -name autostart.toml`), append:

```toml
[[service]]
image = "getty"
args = ["/dev/tty1"]

[[service]]
image = "getty"
args = ["/dev/tty2"]

[[service]]
image = "getty"
args = ["/dev/tty3"]
```

- [ ] **Step 6: Build + boot smoke**

```
cd /home/vlb2bp/git/cluu
cargo xtask build
bash scripts/harness_run.sh
```

Expected: boot. Press Ctrl+Alt+F2 in the test harness (if available) → `cluu login:` prompt on `/dev/tty1`.

- [ ] **Step 7: Commit**

```bash
git add userspace/getty/ var/images/getty/ etc/autostart.toml Cargo.toml
git commit -m "feat(getty): text-VT login binary; 3 autostart entries"
```

---

## Task 11: procmgr::spawn consumes envelope.session

**Files:**
- Modify: `userspace/procmgr/src/spawn.rs`

- [ ] **Step 1: Wire session resolution in spawn()**

In Task 8 of plan 1, `procmgr::spawn` already had the session-resolution scaffolding. Now replace the placeholder `hooks::resolve_session_token` with the real call:

```rust
let session_id = match envelope.session {
    None => {
        if !is_system_caller(caller_pid, procmgr_self_pid())
            && !manifest.allow_sessionless
            && !hooks::caller_can_spawn_sessionless(caller_pid)
        {
            rollback_all(rollback);
            return Err(SpawnError::PermissionDenied);
        }
        None
    }
    Some(t) => {
        let resolved = crate::session_table::SESSION_TABLE.resolve(
            t, caller_pid, cluu_proto::session::RIGHT_SESSION_JOIN);
        match resolved {
            Err(_) => {
                rollback_all(rollback.clone());
                return Err(SpawnError::SessionRevoked);
            }
            Ok((sid, _)) => {
                rollback.session_id = Some(sid);
                Some(sid)
            }
        }
    }
};
```

- [ ] **Step 2: Wire session_id into ProcessEntry**

Pass `session_id` through to `hooks::insert_process_entry` (already part of plan 1 task 8 signature). Procmgr stores it.

- [ ] **Step 3: Build + boot smoke**

```
cd /home/vlb2bp/git/cluu
cargo build -p procmgr
bash scripts/harness_run.sh
```

Expected: boot + login.

- [ ] **Step 4: Commit**

```bash
git add userspace/procmgr/src/spawn.rs
git commit -m "feat(procmgr): spawn() resolves envelope.session via SessionObject table"
```

---

## Task 12: Acceptance markers

**Files:**
- Create: `userspace/probes/l3_*` (multiple)

For each marker in spec 3 §10 acceptance, scaffold a probe:

- [ ] **Step 1: Scaffold each probe**

Copy from `userspace/probes/argvprobe/Cargo.toml`. Add each to workspace.

Markers to land (high-priority first):
- `l3_session_create_requires_right` — non-privileged binary calls SESSION_CREATE → PermissionDenied.
- `l3_session_destroy_requires_control` — token without RIGHT_CONTROL → InsufficientRights.
- `l3_session_set_leader_twice` — second call → AlreadyHasLeader.
- `l3_session_set_leader_not_member` — pid not in session → LeaderNotMember.
- `l3_session_derive_widening_denied` — derive widening → InsufficientRights.
- `l3_session_creator_death_destroys_unfilled` — creator exits pre-SET_LEADER → session destroyed.
- `l3_leader_exit_sighup_members` — leader killed → member receives SIGHUP.
- `l3_subscriber_receives_session_ended` — subscriber registered → SESSION_ENDED on leader exit.
- `l3_no_compositor_swap` — compositor pid unchanged across login.

- [ ] **Step 2: Implement each probe**

Template (using `l3_session_set_leader_twice`):

```rust
#![no_std]
#![no_main]
extern crate alloc;
extern crate libcluu;
use cluu_proto::session::*;

#[no_mangle]
pub extern "C" fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    // 1. Create a session.
    let ok = match libcluu::session::create(SessionCreateRequest {
        user_name: alloc::string::String::from("test"),
        profile: ProfileSpec {
            home: alloc::string::String::from("/tmp"),
            initial_view: cluu_proto::spawn::ViewSource::Derive(
                libcluu::process::self_view_token()),
            env: alloc::vec::Vec::new(),
            umask: 0o022,
        },
    }) {
        Ok(v) => v,
        Err(_) => {
            libcluu::print_log(b"l3_session_set_leader_twice: SKIP no RIGHT_SESSION_CREATE\n");
            return 0;
        }
    };

    // 2. Spawn a helper that becomes a member (this binary's own pid serves
    //    if procmgr considers caller a member of the session it created;
    //    if not, spawn a tiny child).
    let leader_pid = libcluu::process::self_pid();

    // 3. First SET_LEADER: should succeed.
    let first = libcluu::session::set_leader(ok.token, leader_pid);
    if first.is_err() {
        libcluu::print_log(&alloc::format!(
            "l3_session_set_leader_twice: FAIL first set_leader: {:?}\n", first));
        return 1;
    }

    // 4. Second SET_LEADER: must return AlreadyHasLeader.
    let second = libcluu::session::set_leader(ok.token, leader_pid);
    match second {
        Err(SessionErr::AlreadyHasLeader) => {
            libcluu::print_log(b"l3_session_set_leader_twice: PASS\n");
            0
        }
        other => {
            libcluu::print_log(&alloc::format!(
                "l3_session_set_leader_twice: FAIL second got {:?}\n", other));
            1
        }
    }
}
```

Each marker is a small bespoke probe; engineer follows the same pattern.

- [ ] **Step 3: Run markers**

```
cd /home/vlb2bp/git/cluu
for m in l3_session_create_requires_right l3_session_destroy_requires_control \
         l3_session_set_leader_twice l3_session_set_leader_not_member \
         l3_session_derive_widening_denied l3_session_creator_death_destroys_unfilled \
         l3_leader_exit_sighup_members l3_subscriber_receives_session_ended; do
    HARNESS_FORCE_BUILD=1 CLUU_SHELL_AUTOSTART_CMD=$m MARKER_MODE=$m bash scripts/harness_run.sh
    grep "$m:" serial.log
done
```

Expected: each `<marker>: PASS`.

- [ ] **Step 4: Commit**

```bash
git add userspace/probes/l3_* Cargo.toml
git commit -m "test: spec 3 acceptance markers"
```

---

## Final verification

- [ ] **Spec 3 §10 grep proofs:**

```
cd /home/vlb2bp/git/cluu
echo "Zero-hit:"
git grep -c "PROCMGR_SESSION_LOGIN_LABEL"     # expect 0
git grep -c "COMPOSITOR_READY_LABEL"           # expect 0
git grep -c "kill_system_compositor"           # expect 0
git grep -c "spawn_user_compositor"            # expect 0
git grep -c "system_compositor_pid"            # expect 0
git grep -c "session_mode"                     # expect 0
git grep -c "wait.*COMPOSITOR_READY"           # expect 0

echo "One-match:"
git grep -c "PROCMGR_SESSION_CREATE_LABEL.*= 82"   # expect 1
git grep -c "PROCMGR_SESSION_SET_LEADER_LABEL.*= 88" # expect 1
git grep -c "fn handle_session_create" userspace/procmgr/  # expect 1
git grep -c "RIGHT_SESSION_CREATE" userspace/login/  # expect ≥ 1
git grep -c "RIGHT_SESSION_CREATE" userspace/getty/  # expect ≥ 1
```

- [ ] **Functional smoke (graphical):**

```
bash scripts/harness_run.sh
```

- Boot reaches `compositor: ready` + login window visible.
- root/root login → cluuterm window with shell prompt.
- `exit` in shell → fresh login window appears.
- Repeat 3 times — no leak, no kernel warnings.

- [ ] **Functional smoke (text-VT):**

- Boot, switch to VT1 → `cluu login:` prompt.
- user/password → shell on /dev/tty1.
- `exit` → getty respawns.

- [ ] **No new timeouts:**

```
grep -rn "wait.*timeout\|recv_with_timeout\|call_with_timeout" userspace/procmgr/src/main.rs | wc -l
```

Same as pre-plan-3.

- [ ] **Performance gate:**

Time from `cluu login:` to shell prompt: under 200 ms p99.

---

## Notes for the engineer

- **TDD where applicable:** Task 1 has unit tests. Task 3 (SessionTable) should add inline tests; engineer adds basic ones for create/resolve/derive/set_leader.
- **Cap discipline:** every place that hands a token to another process — distribute via narrow-derive, not raw share. Rights bitmask is the discipline.
- **DRY:** login and getty share most of the post-auth flow. Engineer may extract a shared `libcluu::login_helpers` module for the common pattern (SESSION_CREATE → DERIVE → SPAWN → SET_LEADER). Optional.
- **YAGNI:** no user switching, no leader migration, no /proc/sessions in plan 3. Deferred per spec §11.
- **Compositor crash recovery:** spec 3 §6 explicitly accepts that compositor crash = forced logout. Don't implement re-attach in plan 3.
- **Multi-user concurrent:** spec 3 §7 promises VT1+VT2+VT3+VT0 simultaneous logins. Verify in the multi-user marker (`l3_multiuser_concurrent_vts`) — may need extra harness scaffolding to drive multiple VTs.

---

## Spec 3 sections covered

| Spec § | Task(s) |
|---|---|
| §3 architecture | Task 4, 5, 6, 8 |
| §4 SessionObject | Task 3 |
| §5 wire format | Task 1 |
| §6 compositor lifecycle | Task 5, 7, 8, 9 |
| §7 text-VT | Task 10 |
| §8 delete 2s timeout | Task 9 |
| §9 migration | Tasks 1-12 |
| §10 acceptance | Task 12, final verification |
| §11 follow-ups | OUT of plan 3 scope |
