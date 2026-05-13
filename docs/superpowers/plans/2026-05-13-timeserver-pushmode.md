# Timeserver Push-Mode — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend `timeserver` with periodic-tick push notifications (`TIME_SUBSCRIBE_PERIODIC` / `TIME_UNSUBSCRIBE` / `TIME_TICK`). Subscribers grant timeserver a SEND-only token to their notify endpoint; timeserver pushes ticks at the requested period with zero cumulative drift and auto-revokes dead subscribers.

**Architecture:** Per `docs/superpowers/specs/2026-05-13-timeserver-pushmode-design.md` (commit `e1be9d6`). Timeserver loop becomes deadline-driven: `recv_with_timeout(min(subscriber_deadlines) - now)`. On message → existing handlers + new subscribe/unsubscribe arms. On timeout → fire ticks for due subscribers.

**Tech Stack:** Rust (timeserver, libcluu), CLUU harness.

**Parent spec:** `docs/superpowers/specs/2026-05-13-timeserver-pushmode-design.md`.

**Restart caveat:** timeserver restart-on-crash is not yet wired. Subscriber tokens are revocable; on timeserver crash + restart, subscribers will need to re-subscribe. v1 deferred — single timeserver instance assumed alive for system lifetime.

---

## Task 1: libcluu — new IPC labels

**Files:**
- Modify: `userspace/libcluu/src/ipc.rs`.

- [ ] **Step 1: Add label constants**

Append to `userspace/libcluu/src/ipc.rs`:

```rust
// --- Timeserver push-mode (periodic-tick subscriptions). ---
// Subscribe to periodic ticks. Words: [period_ms: u32, notify_ep: u64].
// Reply words[0]: errno (0 ok, EINVAL if period_ms == 0 or > 60_000).
pub const TIME_SUBSCRIBE_PERIODIC_LABEL: u32 = 120;
// Unsubscribe. Timeserver matches on sender_tid. Words: [].
pub const TIME_UNSUBSCRIBE_LABEL: u32 = 121;
// Push from timeserver. Words: [tick_count_since_subscribe: u64, now_monotonic_ms: u64].
// Fire-and-forget; subscriber MUST NOT reply.
pub const TIME_TICK_LABEL: u32 = 122;
```

- [ ] **Step 2: Build**

```bash
cargo xtask build 2>&1 | tail -5
```

- [ ] **Step 3: Commit**

```bash
git add userspace/libcluu/src/ipc.rs
git commit -m "libcluu: add TIME_SUBSCRIBE_PERIODIC / UNSUBSCRIBE / TICK labels"
```

---

## Task 2: timeserver — subscriber state

Add a fixed-size subscriber table to timeserver.

**Files:**
- Create: `userspace/timeserver/src/subscribers.rs`.
- Modify: `userspace/timeserver/src/main.rs` (`mod subscribers;`).

- [ ] **Step 1: Create the module**

```rust
// userspace/timeserver/src/subscribers.rs
//! Periodic-tick subscriber table.
//!
//! Capped at MAX_SUBSCRIBERS slots. Identified by sender_tid; re-subscribing
//! with the same tid replaces the prior slot.

const MAX_SUBSCRIBERS: usize = 64;
const MIN_PERIOD_MS: u32 = 10;
const MAX_PERIOD_MS: u32 = 60_000;
const MAX_CONSECUTIVE_FAILS: u8 = 3;

#[derive(Copy, Clone)]
pub struct Subscriber {
    pub tid: usize,            // sender_tid identifier
    pub notify_ep: usize,      // SEND-only token granted by subscriber
    pub period_ms: u32,        // rounded up to MIN_PERIOD_MS
    pub next_deadline_ms: u64, // anchored at subscribe + N*period
    pub tick_count: u64,       // monotonic, advanced on each successful send
    pub fail_count: u8,        // consecutive send failures
}

pub struct SubscriberTable {
    slots: [Option<Subscriber>; MAX_SUBSCRIBERS],
    len: usize,
}

impl SubscriberTable {
    pub const fn new() -> Self {
        Self { slots: [None; MAX_SUBSCRIBERS], len: 0 }
    }

    pub fn len(&self) -> usize { self.len }

    /// Insert or replace by tid. Returns Ok(()) on success or Err(errno).
    pub fn insert(&mut self, tid: usize, notify_ep: usize, period_ms: u32, now_ms: u64) -> Result<(), u64> {
        if period_ms == 0 || period_ms > MAX_PERIOD_MS {
            return Err(22 /* EINVAL */);
        }
        let period = period_ms.max(MIN_PERIOD_MS);
        // Replace existing slot for this tid.
        for slot in self.slots.iter_mut() {
            if let Some(s) = slot {
                if s.tid == tid {
                    *s = Subscriber {
                        tid, notify_ep, period_ms: period,
                        next_deadline_ms: now_ms.saturating_add(period as u64),
                        tick_count: 0, fail_count: 0,
                    };
                    return Ok(());
                }
            }
        }
        // Find empty slot.
        for slot in self.slots.iter_mut() {
            if slot.is_none() {
                *slot = Some(Subscriber {
                    tid, notify_ep, period_ms: period,
                    next_deadline_ms: now_ms.saturating_add(period as u64),
                    tick_count: 0, fail_count: 0,
                });
                self.len += 1;
                return Ok(());
            }
        }
        Err(28 /* ENOSPC */)
    }

    /// Remove by tid. Returns Ok or Err(ENOENT).
    pub fn remove(&mut self, tid: usize) -> Result<(), u64> {
        for slot in self.slots.iter_mut() {
            if let Some(s) = slot {
                if s.tid == tid {
                    *slot = None;
                    self.len -= 1;
                    return Ok(());
                }
            }
        }
        Err(2 /* ENOENT */)
    }

    /// Smallest next_deadline across all subscribers, or u64::MAX if empty.
    pub fn next_deadline_ms(&self) -> u64 {
        self.slots.iter().filter_map(|s| s.as_ref().map(|x| x.next_deadline_ms)).min().unwrap_or(u64::MAX)
    }

    /// Iterate mutable refs to subscribers whose deadline <= now_ms.
    /// Caller fires ticks + advances deadlines via [Subscriber] helpers.
    pub fn iter_due_mut<'a>(&'a mut self, now_ms: u64) -> impl Iterator<Item = &'a mut Subscriber> {
        self.slots.iter_mut().filter_map(move |s| s.as_mut()).filter(move |s| s.next_deadline_ms <= now_ms)
    }

    /// Remove a subscriber slot by tid (used when fail_count exceeds threshold).
    pub fn remove_tid(&mut self, tid: usize) {
        let _ = self.remove(tid);
    }
}

impl Subscriber {
    /// Advance to next deadline. If we're so late that we missed several
    /// periods, anchor to now_ms + period rather than letting fires pile up.
    pub fn advance_deadline(&mut self, now_ms: u64) {
        let next = self.next_deadline_ms.saturating_add(self.period_ms as u64);
        // If we're already past `next`, jump forward to (now + period) to skip
        // missed ticks (subscriber sees gap via tick_count).
        if next <= now_ms {
            self.next_deadline_ms = now_ms.saturating_add(self.period_ms as u64);
        } else {
            self.next_deadline_ms = next;
        }
    }

    /// Record a send result. Returns true if the subscriber should be removed.
    pub fn record_send(&mut self, ok: bool) -> bool {
        if ok {
            self.fail_count = 0;
            self.tick_count = self.tick_count.saturating_add(1);
            false
        } else {
            self.fail_count = self.fail_count.saturating_add(1);
            self.fail_count >= MAX_CONSECUTIVE_FAILS
        }
    }

    pub fn tick_count(&self) -> u64 { self.tick_count }
}
```

- [ ] **Step 2: Declare module in main.rs**

```rust
mod subscribers;
```

- [ ] **Step 3: Build**

```bash
cargo xtask build 2>&1 | tail -5
```

- [ ] **Step 4: Commit**

```bash
git add userspace/timeserver/src/subscribers.rs userspace/timeserver/src/main.rs
git commit -m "timeserver: scaffold subscriber table for push-mode"
```

---

## Task 3: timeserver — recv loop with deadline + subscribe/unsubscribe handlers

Replace the timeout=u64::MAX recv with a deadline-driven recv. Add subscribe/unsubscribe handlers.

**Files:**
- Modify: `userspace/timeserver/src/main.rs`.

- [ ] **Step 1: Locate current loop**

```bash
grep -n "ipc_recv_any\|TIME_GETCLOCK\|TIME_GETTIMEOFDAY" userspace/timeserver/src/main.rs
```

- [ ] **Step 2: Refactor loop**

Replace the existing `loop { let (idx, len) = ipc_recv_any(..., u64::MAX) ... }` with:

```rust
    let mut subs = subscribers::SubscriberTable::new();
    let mut buf = [0u8; 256];
    let endpoints: [usize; 2] = [endpoint, control_endpoint];

    loop {
        let now_ms = monotonic_now_ms(clock_token, ticks_per_sec);
        let next_deadline = subs.next_deadline_ms();
        let timeout_ms = next_deadline.saturating_sub(now_ms);

        match libcluu::syscall::ipc_recv_any(&endpoints, &mut buf, timeout_ms) {
            Ok((idx, len, sender_tid)) => {
                if len < core::mem::size_of::<Message>() { continue; }
                if idx == 1 {
                    if let Some((msg, payload)) = parse_message(&buf[..len]) {
                        let _ = registry::handle_incoming_message(&msg, payload);
                    }
                    continue;
                }
                let msg = unsafe { (buf.as_ptr() as *const Message).read_unaligned() };
                let reply_token = extract_reply_id(&msg).unwrap_or(endpoint);
                match msg.tag.label {
                    TIME_GETTIMEOFDAY => reply_time(reply_token, clock_token, ticks_per_sec, false)?,
                    TIME_GETCLOCK    => reply_time(reply_token, clock_token, ticks_per_sec, true)?,
                    TIME_SUBSCRIBE_PERIODIC_LABEL => {
                        handle_subscribe(&mut subs, &msg, sender_tid, reply_token, now_ms);
                    }
                    TIME_UNSUBSCRIBE_LABEL => {
                        handle_unsubscribe(&mut subs, sender_tid, reply_token);
                    }
                    _ => {}
                }
            }
            Err(libcluu::Error::Timeout) | Err(libcluu::Error::WouldBlock) => {
                // Fall through to tick firing.
            }
            Err(_) => continue,
        }

        // Fire any due ticks.
        let now_ms = monotonic_now_ms(clock_token, ticks_per_sec);
        fire_due_ticks(&mut subs, now_ms);
    }
```

Note: `ipc_recv_any` here is the variant that returns `(idx, len, sender_tid)` — same signature compositor uses. If timeserver currently uses the 2-tuple variant, switch to `ipc_recv_any_with_sender` (whichever libcluu exports).

- [ ] **Step 3: Add helpers**

```rust
fn monotonic_now_ms(clock_token: usize, ticks_per_sec: u64) -> u64 {
    let now = libcluu::clock_now(clock_token).unwrap_or(0);
    (now * 1_000) / ticks_per_sec.max(1)
}

fn handle_subscribe(
    subs: &mut subscribers::SubscriberTable,
    msg: &Message,
    sender_tid: usize,
    reply_token: usize,
    now_ms: u64,
) {
    let period_ms = msg.words[0] as u32;
    let notify_ep = msg.words[1];
    let err = match subs.insert(sender_tid, notify_ep, period_ms, now_ms) {
        Ok(()) => 0,
        Err(e) => e,
    };
    let mut reply = Message::new(0, [err as usize, 0, 0, 0, 0, 0], 1);
    let _ = libcluu::ipc::reply(reply_token, &reply, libcluu::IpcFlags::empty());
    let _ = debug_print(&alloc::format!(
        "timeserver: subscribe tid={} period={}ms ep={} errno={}",
        sender_tid, period_ms, notify_ep, err
    ));
}

fn handle_unsubscribe(
    subs: &mut subscribers::SubscriberTable,
    sender_tid: usize,
    reply_token: usize,
) {
    let err = subs.remove(sender_tid).err().unwrap_or(0);
    let mut reply = Message::new(0, [err as usize, 0, 0, 0, 0, 0], 1);
    let _ = libcluu::ipc::reply(reply_token, &reply, libcluu::IpcFlags::empty());
}

fn fire_due_ticks(subs: &mut subscribers::SubscriberTable, now_ms: u64) {
    // Collect dead-after-this-call tids, since we can't mutate during iter.
    let mut to_remove: alloc::vec::Vec<usize> = alloc::vec::Vec::new();
    for s in subs.iter_due_mut(now_ms) {
        let msg = Message::new(
            TIME_TICK_LABEL,
            [s.tick_count_for_next() as usize, now_ms as usize, 0, 0, 0, 0],
            2,
        );
        let send_result = libcluu::ipc::send(s.notify_ep, &msg, libcluu::IpcFlags::empty());
        let should_remove = s.record_send(send_result.is_ok());
        s.advance_deadline(now_ms);
        if should_remove {
            to_remove.push(s.tid);
        }
    }
    for tid in to_remove {
        subs.remove_tid(tid);
        let _ = debug_print(&alloc::format!(
            "timeserver: subscriber tid={} removed (3x send fail)", tid
        ));
    }
}
```

`s.tick_count_for_next()` helper on Subscriber returns `self.tick_count.saturating_add(1)` so the TICK payload reflects "this is tick number N".

```rust
impl Subscriber {
    pub fn tick_count_for_next(&self) -> u64 {
        self.tick_count.saturating_add(1)
    }
}
```

- [ ] **Step 4: Build**

```bash
cargo xtask build 2>&1 | tail -10
```

If `ipc_recv_any_with_sender` doesn't exist for timeserver's call convention, adapt — the 2-tuple variant returns no sender_tid; in that case extract sender_tid from `msg.tag` or a header field. If neither works, fall back to attaching tid to the subscribe payload (subscriber sends its own tid). Document deviation.

- [ ] **Step 5: Harness sanity (no consumers yet)**

```bash
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_vt4_default bash scripts/harness_run.sh
grep -aE "timeserver: ready" /tmp/cluu-serial-com2.log
```

Should still print `timeserver: ready` and otherwise behave normally.

- [ ] **Step 6: Commit**

```bash
git add userspace/timeserver/src/main.rs userspace/timeserver/src/subscribers.rs
git commit -m "timeserver: deadline-driven loop + subscribe/unsubscribe/tick handlers"
```

---

## Task 4: smoke — l2_timeserver_pushmode_tick

Create a tiny test consumer that subscribes 100 ms and counts 10 ticks. Add a harness marker mode.

**Files:**
- Create: `containers/timetick_probe/Cluufile`, `userspace/c-programs/timetick_probe.c` (or Rust: `userspace/timetick_probe/`).
- Modify: `scripts/harness_run.sh` (new MARKER_MODE).
- Modify: `xtask/src/main.rs` (build the probe if not auto-discovered).

- [ ] **Step 1: Pick probe language**

Match existing probe pattern. If `userspace/c-programs/` is the convention, use C. Otherwise Rust. Inspect:

```bash
ls userspace/c-programs/ 2>/dev/null | head
ls containers/*/Cluufile | head
```

- [ ] **Step 2: Write the probe**

Probe responsibilities:
1. Lookup `timeserver:main`.
2. Register own endpoint as notify_ep.
3. Subscribe with period_ms=100.
4. recv loop counting TIME_TICK messages.
5. On tick 10: print `TIMETICK_PROBE: count=10`, then unsubscribe + exit.

Rough Rust sketch (~80 LOC):

```rust
#![no_std]
#![no_main]
extern crate alloc;
use libcluu::ipc::{TIME_SUBSCRIBE_PERIODIC_LABEL, TIME_UNSUBSCRIBE_LABEL,
                    TIME_TICK_LABEL};
use libcluu::types::Message;
use libcluu::{debug_print, registry, syscall, IpcFlags};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let info = libcluu::boot::process_info();
    let my_ep = info.tokens[libcluu::boot::TOKEN_EXTRA_0];
    registry::init("timetick_probe").unwrap();

    let time_ep = registry::subscribe_output("timeserver", "main").unwrap();
    let sub = Message::new(TIME_SUBSCRIBE_PERIODIC_LABEL,
        [100, my_ep, 0, 0, 0, 0], 2);
    let mut reply = Message::new(0, [0; 6], 0);
    libcluu::ipc::call(time_ep, &sub, &mut reply).unwrap();
    if reply.words[0] != 0 {
        let _ = debug_print("TIMETICK_PROBE: subscribe failed");
        return 1;
    }

    let mut buf = [0u8; 256];
    let mut count = 0u64;
    while count < 10 {
        let (_idx, len) = syscall::ipc_recv_any(&[my_ep], &mut buf, u64::MAX).unwrap();
        if len < core::mem::size_of::<Message>() { continue; }
        let msg = unsafe { (buf.as_ptr() as *const Message).read_unaligned() };
        if msg.tag.label == TIME_TICK_LABEL {
            count += 1;
        }
    }
    let _ = debug_print(&alloc::format!("TIMETICK_PROBE: count={}", count));

    let unsub = Message::new(TIME_UNSUBSCRIBE_LABEL, [0; 6], 0);
    let _ = libcluu::ipc::call(time_ep, &unsub, &mut reply);
    0
}
```

Adapt `subscribe_output` to whatever helper exists in libcluu's registry.

Cluufile:
```
FROM minimal
PROFILE ipc registry
BUILD "cargo build --manifest-path userspace/timetick_probe/Cargo.toml --target triplets/x86_64-cluu-user.json -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem" target/x86_64-cluu-user/debug/timetick_probe.elf /bin/timetick_probe
ENTRYPOINT /bin/timetick_probe
PRELOAD
```

- [ ] **Step 3: Add MARKER_MODE**

In `scripts/harness_run.sh` add:

```bash
    l2_timeserver_pushmode_tick)
        SHELL_AUTOSTART_CMD_DEFAULT="/bin/timetick_probe"
        required_markers=(
            "TSC calibrated"
            "TIMETICK_PROBE: count=10"
        )
        ;;
```

Plus the case entry in the main switch.

- [ ] **Step 4: Build + run**

```bash
HARNESS_FORCE_BUILD=1 CLUU_SHELL_AUTOSTART_CMD="/bin/timetick_probe" \
    MARKER_MODE=l2_timeserver_pushmode_tick bash scripts/harness_run.sh
```

Expected: marker `TIMETICK_PROBE: count=10` fires within ~1.3 s of probe start.

- [ ] **Step 5: Commit**

```bash
git add containers/timetick_probe/Cluufile userspace/timetick_probe/ scripts/harness_run.sh \
        scripts/harness_case_defaults.sh xtask/src/main.rs
git commit -m "test: l2_timeserver_pushmode_tick probe subscribes 100ms x10"
```

---

## Task 5: smoke — l2_timeserver_pushmode_revoke

Probe that subscribes then exits without unsubscribing. Timeserver should remove after 3 failed sends.

**Files:**
- Create: `userspace/timetick_die/` (or extend probe with a "die" mode argv).
- Modify: scripts/harness_run.sh.

- [ ] **Step 1: Variant probe**

Either a second binary `timetick_die` that subscribes 50 ms then exits immediately, or extend `timetick_probe` to check argv[1] == "die" and exit after subscribe.

- [ ] **Step 2: MARKER_MODE**

```bash
    l2_timeserver_pushmode_revoke)
        SHELL_AUTOSTART_CMD_DEFAULT="/bin/timetick_probe die"
        required_markers=(
            "TSC calibrated"
            "timeserver: subscriber tid=.*removed (3x send fail)"
        )
        ;;
```

- [ ] **Step 3: Build + run + commit**

Per Task 4 step 4-5 with the new marker.

---

## Task 6: spec status + memory

- [ ] **Step 1: Update spec status**

In `docs/superpowers/specs/2026-05-13-timeserver-pushmode-design.md` §10, mark Phase 1 done with the commit SHA range.

- [ ] **Step 2: Memory**

Write `/home/vlb2bp/.claude/projects/-home-vlb2bp-git-cluu/memory/project_timeserver_pushmode_2026_05_13.md` describing:
- API shape (3 new labels).
- Cap flow (SEND-only token, 3-strike revoke).
- Sister plan: `2026-05-13-compositor-clock-pushmode.md` (next).
- Bench claim: zero cumulative drift vs old polling.

Index in MEMORY.md.

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/2026-05-13-timeserver-pushmode-design.md
git commit -m "docs/spec: timeserver push-mode Phase 1 marked done"
```

---

## Self-review notes

- Files touched: `userspace/timeserver/{src/main.rs, src/subscribers.rs (new)}`, `userspace/libcluu/src/ipc.rs`, probe crate, harness scripts, spec edit. No kernel.
- No new syscalls. Reuses existing IPC labels space + `clock_now` + `ipc::send` / `ipc::reply`.
- Failure isolation: 3-strike removal; coalesced ticks (one per loop iter per subscriber); drift-free anchor via `advance_deadline`.
- Compositor migration deferred to sister plan (`2026-05-13-compositor-clock-pushmode.md`).
- Restart-on-crash for timeserver out of scope — subscribers re-subscribe on grant re-arrival when restart wiring lands.
- All commits on `develop`. No force-push, no `--no-verify`, no `--amend`.
