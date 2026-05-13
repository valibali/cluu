# Input Routing — vtmgr-as-oracle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make vtmgr the single source of truth for active-VT + input routing. kbd becomes a pure IRQ/decoder/cache-and-forward driver. Eliminate the vtmgr/kbd `active_vt` desync that lets the screen show VT4 while keystrokes go to VT0. Lay SOLID anchors so a future `inputd` extraction is mechanical.

**Architecture:** Per `docs/superpowers/designs/2026-05-13-input-routing-single-source.md` (commit `dee8256`). vtmgr broadcasts `VTMGR_ACTIVE_VT_CHANGED` on boot + every switch; kbd subscribes via registry and caches a `RoutingTarget`. kbd's Ctrl-Alt-Fn turns into `VTMGR_REQUEST_VT_SWITCH`; vtmgr decides. The today-hack of duplicating every keystroke to BOTH compositor:input AND tty:N goes away — kbd sends to exactly one target.

**Tech Stack:** Rust (vtmgr, kbd, libcluu), CLUU harness (`scripts/harness_run.sh`).

**Parent design:** `docs/superpowers/designs/2026-05-13-input-routing-single-source.md`
**Parent specs:** `docs/superpowers/specs/2026-05-12-login-flow-design.md` §4.6, `docs/superpowers/specs/2026-05-10-tui-compositor-design.md`.

---

## Task 1: libcluu — labels + RoutingTarget enum

Add the four new IPC labels and the shared `RoutingTarget` enum. No behaviour change yet — pure plumbing.

**Files:**
- Modify: `userspace/libcluu/src/ipc.rs` (label constants).
- Modify: `userspace/libcluu/src/lib.rs` (re-export `RoutingTarget` if it lives in a new module).
- Create: `userspace/libcluu/src/input_routing.rs` (new module holding the enum + helpers).

- [ ] **Step 1: Add IPC label constants**

In `userspace/libcluu/src/ipc.rs` append after the existing COMP_* block (after line ~230 where `COMP_CLOSE_REQUEST_LABEL = 101` lives):

```rust
// --- Input routing (vtmgr today; inputd post-extraction). ---
// vtmgr → subscribers: broadcast that active VT changed.
// Payload words: [vt: u32, target_kind: u32, padding[4]] + grant payload may include endpoint.
pub const VTMGR_ACTIVE_VT_CHANGED_LABEL: u32 = 110;
// client → vtmgr: request a VT switch. vtmgr decides.
// Reply words[0] = errno (0 ok).
pub const VTMGR_REQUEST_VT_SWITCH_LABEL: u32 = 111;
// client → vtmgr: take/release modal lock on VT switching.
// Reserved for compositor modal lock per login-flow §4.6; impl is stub today.
pub const VTMGR_LOCK_VT_SWITCH_LABEL:   u32 = 112;
pub const VTMGR_UNLOCK_VT_SWITCH_LABEL: u32 = 113;
```

- [ ] **Step 2: Create the input_routing module**

```rust
// userspace/libcluu/src/input_routing.rs
//! Shared types for input-routing IPC.
//!
//! The router today is vtmgr; tomorrow it'll be a dedicated inputd.
//! These types live in libcluu so both ends speak the same dialect
//! regardless of which process is the publisher.

#![allow(dead_code)]

/// Where keystrokes should go for the currently-active VT.
///
/// Carried in the `target_kind` word of `VTMGR_ACTIVE_VT_CHANGED`,
/// plus the resolved endpoint handle in the payload (so subscribers
/// don't have to do their own registry lookup).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RoutingTargetKind {
    /// No active target yet (boot, quiesce, transition). Subscribers
    /// drop incoming events until a real target arrives.
    None = 0,
    /// Forward to the compositor's input endpoint. VT4 path.
    Compositor = 1,
    /// Forward to tty:N's main endpoint. N is the VT index (0..=3).
    Tty = 2,
}

impl RoutingTargetKind {
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => RoutingTargetKind::Compositor,
            2 => RoutingTargetKind::Tty,
            _ => RoutingTargetKind::None,
        }
    }
}

/// Resolved routing target. Subscribers store this in their cache.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RoutingTarget {
    pub vt: u8,
    pub kind: RoutingTargetKind,
    /// Endpoint handle resolved by the publisher. 0 means "not delivered yet".
    pub endpoint: usize,
}

impl RoutingTarget {
    pub const NONE: Self = Self {
        vt: 0,
        kind: RoutingTargetKind::None,
        endpoint: 0,
    };
}
```

- [ ] **Step 3: Re-export in lib.rs**

In `userspace/libcluu/src/lib.rs` add (alphabetical):

```rust
pub mod input_routing;
```

- [ ] **Step 4: Build**

```bash
cargo xtask build 2>&1 | tail -5
```

Expected: clean. Pure additions; no caller touches the new module yet.

- [ ] **Step 5: Commit**

```bash
git add userspace/libcluu/src/ipc.rs userspace/libcluu/src/input_routing.rs userspace/libcluu/src/lib.rs
git commit -m "libcluu: add VTMGR_* input-routing labels + RoutingTarget enum"
```

---

## Task 2: vtmgr — new `input_routing` module (publisher + policy stub)

Move the routing oracle into its own file inside vtmgr. No `context.rs` surgery yet — only adding the new module and stubs.

**Files:**
- Create: `userspace/vtmgr/src/input_routing.rs`.
- Modify: `userspace/vtmgr/src/main.rs` (mod declaration + import).

- [ ] **Step 1: Create the module**

```rust
// userspace/vtmgr/src/input_routing.rs
//! Input-routing oracle.
//!
//! vtmgr owns this today; future `inputd` will lift this file
//! ~verbatim, swap the registry publish name, and shrink vtmgr back
//! to pure VT lifecycle. Keep the boundary clean.
//!
//! Responsibilities:
//!   1. Track who is subscribed to active-VT change broadcasts.
//!   2. Broadcast `VTMGR_ACTIVE_VT_CHANGED` on every state change.
//!   3. Resolve the per-state target endpoint (compositor:input or tty:N:main).
//!   4. Decide whether a `VTMGR_REQUEST_VT_SWITCH` is allowed (policy).
//!
//! Strictly no `switch_vt` logic here — that lives in `context.rs`.

use alloc::vec::Vec;
use libcluu::input_routing::{RoutingTarget, RoutingTargetKind};
use libcluu::ipc::{send, VTMGR_ACTIVE_VT_CHANGED_LABEL};
use libcluu::types::{IpcFlags, Message};
use libcluu::debug_print;

/// One subscriber that asked to be notified about active-VT changes.
/// Persists across vtmgr lifetime; the registry handles the grant.
pub struct RoutingSubscriber {
    pub endpoint: usize,
}

pub struct InputRouter {
    /// Current routing target. vtmgr's `context::switch_vt` calls
    /// `set_target` after every successful transition.
    target: RoutingTarget,
    subscribers: Vec<RoutingSubscriber>,
}

impl InputRouter {
    pub const fn new() -> Self {
        Self {
            target: RoutingTarget::NONE,
            subscribers: Vec::new(),
        }
    }

    pub fn target(&self) -> RoutingTarget {
        self.target
    }

    /// Add a subscriber. Idempotent on endpoint handle.
    pub fn add_subscriber(&mut self, endpoint: usize) {
        if endpoint == 0 { return; }
        if self.subscribers.iter().any(|s| s.endpoint == endpoint) {
            return;
        }
        self.subscribers.push(RoutingSubscriber { endpoint });
        // Send the current state to the new subscriber so it
        // immediately syncs even if it joined late.
        Self::broadcast_to(endpoint, self.target);
    }

    /// Update target and broadcast to all subscribers.
    pub fn set_target(&mut self, target: RoutingTarget) {
        if self.target == target { return; }
        self.target = target;
        let _ = debug_print(&alloc::format!(
            "vtmgr: routing target vt={} kind={:?} ep={}",
            target.vt, target.kind, target.endpoint
        ));
        for s in &self.subscribers {
            Self::broadcast_to(s.endpoint, target);
        }
    }

    fn broadcast_to(endpoint: usize, target: RoutingTarget) {
        let msg = Message::new(
            VTMGR_ACTIVE_VT_CHANGED_LABEL,
            [target.vt as usize, target.kind as usize, target.endpoint, 0, 0, 0],
            3,
        );
        let _ = send(endpoint, &msg, IpcFlags::empty());
    }

    /// Stub policy hook. Returns false to refuse a switch.
    /// Today: always allows. Future: consults modal-lock state.
    pub fn should_allow_switch(&self, _from: u8, _to: u8) -> bool {
        true
    }
}
```

- [ ] **Step 2: Declare the module in main.rs**

In `userspace/vtmgr/src/main.rs`, alongside the other `mod` declarations, add:

```rust
mod input_routing;
```

- [ ] **Step 3: Build**

```bash
cargo xtask build 2>&1 | tail -5
```

Module exists but is unused — should compile cleanly thanks to `#![allow(dead_code)]` patterns or the new `RoutingSubscriber` being a `pub struct`. If a warning fails the build, suppress it locally with `#[allow(dead_code)]` on the items.

- [ ] **Step 4: Commit**

```bash
git add userspace/vtmgr/src/main.rs userspace/vtmgr/src/input_routing.rs
git commit -m "vtmgr: scaffold input_routing module (publisher + policy stub)"
```

---

## Task 3: vtmgr — wire the router to switch_vt + boot

Hook the new `InputRouter` into `VtmgrContext`. Emit broadcasts:
- after every `switch_vt` completion.
- once on boot, right after the Plan-1 `CONSOLE_DEACTIVATE(0)` / `COMP_VT_ACTIVATE` grant-arrival pair fires (so the initial state hits subscribers).

**Files:**
- Modify: `userspace/vtmgr/src/context.rs` (struct field, switch_vt, handle_registry_message).

- [ ] **Step 1: Add field**

In `VtmgrContext`:

```rust
    router: crate::input_routing::InputRouter,
```

Initialise in `new()`:

```rust
            router: crate::input_routing::InputRouter::new(),
```

- [ ] **Step 2: Resolve target after every switch**

In `switch_vt`, immediately after `self.active_vt = new_vt;` and the existing `vt switch` debug print, compute and publish:

```rust
        let target = self.resolve_target(new_vt);
        self.router.set_target(target);
```

Add a helper method on `VtmgrContext`:

```rust
    /// Resolve which endpoint should receive keystrokes for VT `new_vt`.
    fn resolve_target(&self, new_vt: usize) -> libcluu::input_routing::RoutingTarget {
        use libcluu::input_routing::{RoutingTarget, RoutingTargetKind};
        if new_vt == self.compositor_vt {
            RoutingTarget {
                vt: new_vt as u8,
                kind: RoutingTargetKind::Compositor,
                endpoint: 0, // resolved on subscriber side via registry, OR
                             // we can resolve here once we have a stash.
                             // For v1 leave 0 — subscriber does its own
                             // lookup. Future: stash composer-input ep.
            }
        } else {
            RoutingTarget {
                vt: new_vt as u8,
                kind: RoutingTargetKind::Tty,
                endpoint: 0,
            }
        }
    }
```

NOTE: We leave `endpoint: 0` for v1 because vtmgr doesn't currently track `compositor:input` or `tty:N:main` endpoints — those are kbd's subscriptions. Subscribers do their own registry lookup using `target.vt` + `target.kind`. The plumbing for "endpoint resolved by publisher" is documented in the design memo as a future enhancement. Once vtmgr knows the endpoints, this field gets populated and subscribers can skip the lookup.

- [ ] **Step 3: Initial broadcast on boot**

In `handle_registry_message`, the boot path already fires `CONSOLE_DEACTIVATE(0)` + `COMP_VT_ACTIVATE` (Plan-1 Task-3 commit `1990f54`). At the SAME sites, after the activate/deactivate messages, append:

```rust
                            // Publish initial routing state for subscribers
                            // that joined before this point.
                            let target = self.resolve_target(self.active_vt);
                            self.router.set_target(target);
```

Put this inside BOTH the compositor-control branch (where COMP_VT_ACTIVATE fires) AND the console-control branch (where CONSOLE_DEACTIVATE fires) so whichever grant arrives last triggers the broadcast. `set_target` is idempotent on identical state, so calling it twice is fine.

- [ ] **Step 4: Subscriber subscription handling**

Subscribers join via registry, but the registry's grant pattern needs vtmgr to register a NAMED OUTPUT for `active_vt`. Add to `registry::register_default_outputs` is invasive — simpler: register an output explicitly in vtmgr's startup. In `VtmgrContext::new`, after the existing `registry::register_output("control", endpoint)?;`:

```rust
        // Active-VT broadcast output. Subscribers call
        // request_subscription("vtmgr", "active_vt") to join.
        registry::register_output("active_vt", endpoint)?;
```

This re-uses the existing `endpoint` (vtmgr's main IPC), so subscribers actually subscribe to the same endpoint. Then in `handle_registry_message`, when a `SubscribeStatus` *grant* arrives for the active_vt output, vtmgr calls `self.router.add_subscriber(token)`.

Looking at the registry API: actually grants arriving FROM subscribers are tracked by the registry itself; vtmgr doesn't see each subscription explicitly. The simpler model: vtmgr publishes via `registry::publish_to("active_vt", &msg, &payload)` (if such a helper exists; otherwise vtmgr maintains the subscriber list manually).

**Implementation decision**: Pick the path that fits the existing registry helpers. If `registry::publish_to` doesn't exist, the simpler path is to keep `InputRouter.subscribers: Vec<RoutingSubscriber>` and have vtmgr update it whenever a new grant fires for `active_vt`. Check `userspace/libcluu/src/registry.rs` for the published-output helper API; implement accordingly. If neither pattern fits, fall back to:

```
// "active_vt" output endpoint = vtmgr's main endpoint.
// vtmgr asks each subscriber to register a notify endpoint via
// a new VTMGR_SUBSCRIBE_ACTIVE_VT_LABEL — but this complicates the
// design memo's "registry-only" rule. Avoid if at all possible.
```

If you reach the fallback path, STOP and report DONE_WITH_CONCERNS — the registry model needs clarification.

- [ ] **Step 5: Build**

```bash
cargo xtask build 2>&1 | tail -5
```

- [ ] **Step 6: Harness sanity (no kbd changes yet)**

```bash
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_vt4_default bash scripts/harness_run.sh
grep -aE "vtmgr: routing target|vtmgr: ready active_vt" /tmp/cluu-serial-com2.log
```

Expected: `vtmgr: routing target vt=4 kind=Compositor ep=0` fires at boot.

- [ ] **Step 7: Commit**

```bash
git add userspace/vtmgr/src/context.rs
git commit -m "vtmgr: publish active-VT routing state on boot + switch"
```

---

## Task 4: vtmgr — accept VTMGR_REQUEST_VT_SWITCH

Today kbd sends `VTMGR_SWITCH_VT_LABEL` (an existing label). Add an entry for `VTMGR_REQUEST_VT_SWITCH_LABEL` that goes through the new policy hook. Keep the old label working for one task (Task 5 swaps kbd over; Task 7 removes the old label).

**Files:**
- Modify: `userspace/vtmgr/src/main.rs` or `userspace/vtmgr/src/context.rs` — wherever the IPC dispatch lives.

- [ ] **Step 1: Locate the message dispatch**

```bash
grep -n "VTMGR_SWITCH_VT_LABEL\|VTMGR_PIN_VT_LABEL\|fn handle_kbd\|fn handle_message" userspace/vtmgr/src/*.rs
```

- [ ] **Step 2: Add the new arm**

Wherever `VTMGR_SWITCH_VT_LABEL` is matched, add a sibling arm BEFORE it:

```rust
        VTMGR_REQUEST_VT_SWITCH_LABEL => {
            let new_vt = msg.words[0];
            let from = self.active_vt as u8;
            let to = new_vt as u8;
            let allowed = self.router.should_allow_switch(from, to);
            if allowed && new_vt < VT_COUNT {
                self.switch_vt(new_vt);
            }
            // Reply with errno (0 ok, EBUSY if refused, EINVAL if oob).
            let err: u64 = if !allowed {
                libcluu::errno::EBUSY as u64
            } else if new_vt >= VT_COUNT {
                libcluu::errno::EINVAL as u64
            } else {
                0
            };
            if let Some(reply_id) = libcluu::ipc::extract_reply_id(msg) {
                let reply = Message::new(0, [err as usize, 0, 0, 0, 0, 0], 1);
                let _ = libcluu::ipc::reply(reply_id, &reply, IpcFlags::empty());
            }
        }
```

Adapt to whatever existing pattern uses `extract_reply_id` and `reply` (other handlers in `context.rs` will show the local idiom).

- [ ] **Step 3: Build + harness**

Same as Task 3 step 5-6. Nothing exercises the new label yet, so behaviour is unchanged.

- [ ] **Step 4: Commit**

```bash
git add userspace/vtmgr/src/context.rs
git commit -m "vtmgr: handle VTMGR_REQUEST_VT_SWITCH with policy hook"
```

---

## Task 5: kbd — new routing_cache module

Pure addition: subscribe to `vtmgr:active_vt`, cache the latest `RoutingTarget`, expose a `target()` getter. No callers yet.

**Files:**
- Create: `userspace/kbd/src/routing_cache.rs`.
- Modify: `userspace/kbd/src/main.rs` (mod decl).
- Modify: `userspace/kbd/src/context.rs` (registry subscription + dispatch new label).

- [ ] **Step 1: Create the cache module**

```rust
// userspace/kbd/src/routing_cache.rs
//! Cached active-VT routing target.
//!
//! Subscribes to vtmgr's `active_vt` output. Hands the cached target
//! back to the IRQ/decoder loop in main.rs. Pure follower — never
//! mutates state, never broadcasts.
//!
//! On boot the cache starts at `RoutingTarget::NONE`; keystrokes are
//! dropped silently until vtmgr's first broadcast arrives. Per the
//! design memo §8, this is the simpler of the two race resolutions.

use libcluu::input_routing::{RoutingTarget, RoutingTargetKind};

pub struct RoutingCache {
    target: RoutingTarget,
}

impl RoutingCache {
    pub const fn new() -> Self {
        Self { target: RoutingTarget::NONE }
    }

    pub fn target(&self) -> RoutingTarget {
        self.target
    }

    pub fn update(&mut self, vt: u8, kind: RoutingTargetKind, endpoint: usize) {
        self.target = RoutingTarget { vt, kind, endpoint };
    }
}
```

- [ ] **Step 2: Add field on KbdContext**

```rust
    pub routing_cache: crate::routing_cache::RoutingCache,
    pub requested_active_vt: bool,
```

Initialise in `new`:

```rust
            routing_cache: crate::routing_cache::RoutingCache::new(),
            requested_active_vt: false,
```

- [ ] **Step 3: Subscribe to vtmgr:active_vt**

In `ensure_subscriptions` (or equivalent — see `request_subscriptions` pattern), add:

```rust
        if !self.requested_active_vt {
            if registry::request_subscription("vtmgr", "active_vt").is_ok() {
                self.requested_active_vt = true;
            }
        }
```

- [ ] **Step 4: Handle the broadcast**

In `handle_registry_message`, add an arm that catches `VTMGR_ACTIVE_VT_CHANGED_LABEL`. Inspect existing branches in that function — registry events are decoded via `registry::handle_incoming_message` for grants and `SubscribeStatus`, but the BROADCAST message arrives as a regular IPC. So the broadcast goes to the main message dispatch, not the registry handler.

Find the main `match msg.tag.label` in kbd, add:

```rust
        VTMGR_ACTIVE_VT_CHANGED_LABEL => {
            let vt = msg.words[0] as u8;
            let kind = RoutingTargetKind::from_u32(msg.words[1] as u32);
            let ep = msg.words[2];
            ctx.routing_cache.update(vt, kind, ep);
            let _ = debug_print(&format!(
                "kbd: routing cache vt={} kind={:?} ep={}",
                vt, kind, ep
            ));
        }
```

- [ ] **Step 5: Build**

```bash
cargo xtask build 2>&1 | tail -5
```

- [ ] **Step 6: Harness sanity**

```bash
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_vt4_default bash scripts/harness_run.sh
grep -aE "kbd: routing cache|vtmgr: routing target" /tmp/cluu-serial-com2.log
```

Expected: `vtmgr: routing target vt=4 kind=Compositor ep=0` AND `kbd: routing cache vt=4 kind=Compositor ep=0` both present. kbd's behaviour is otherwise unchanged because nothing consumes the cache yet.

- [ ] **Step 7: Commit**

```bash
git add userspace/kbd/src/routing_cache.rs userspace/kbd/src/main.rs userspace/kbd/src/context.rs
git commit -m "kbd: scaffold routing_cache + subscribe to vtmgr:active_vt"
```

---

## Task 6: kbd — drop dual-write, route via cache, delete active_vt

The big switch. kbd now uses its cache to pick exactly one target per keystroke. Removes the "broadcast to both" hack.

**Files:**
- Modify: `userspace/kbd/src/context.rs` (remove `active_vt`, replace `send_to_tty` and `send_scroll`).
- Modify: `userspace/kbd/src/main.rs` (replace dual-write with cache-driven send).

- [ ] **Step 1: Replace `send_to_tty`**

In `userspace/kbd/src/context.rs:235`, rename `send_to_tty` → `send_to_active` and switch to the cache:

```rust
    pub fn send_to_active(&self, msg: &Message) {
        use libcluu::input_routing::RoutingTargetKind;
        let target = self.routing_cache.target();
        let ep = match target.kind {
            RoutingTargetKind::None => {
                return; // boot race / quiesce — drop silently
            }
            RoutingTargetKind::Compositor => self.compositor_input_ep,
            RoutingTargetKind::Tty => {
                let vt = target.vt as usize;
                if vt >= VT_COUNT { return; }
                self.tty_endpoints[vt]
            }
        };
        if ep == 0 { return; }
        for _ in 0..8 {
            match send(ep, msg, IpcFlags::empty()) {
                Ok(()) => return,
                Err(Error::WouldBlock) | Err(Error::Busy) => {
                    let _ = yield_cpu();
                    continue;
                }
                Err(_) => return,
            }
        }
        static FIRST_DROP_LOGGED: core::sync::atomic::AtomicBool =
            core::sync::atomic::AtomicBool::new(false);
        if !FIRST_DROP_LOGGED.swap(true, core::sync::atomic::Ordering::Relaxed) {
            let _ = debug_print("kbd: dropped keystroke (target backlog persistent)");
        }
    }
```

`send_to_tty` callers update accordingly.

- [ ] **Step 2: Drop the active_vt field**

In `KbdContext`, remove `active_vt: usize`. Anywhere it's read (look at `send_scroll` line 218 and `switch_vt` line 190+), rework to use the cache or remove entirely if obsolete.

- `send_scroll` uses `self.active_vt` for the CONSOLE_SCROLL_VT_LABEL message. The console subsystem needs to know which VT to scroll. Easiest fix: use `self.routing_cache.target().vt as usize` instead. (Scroll only makes sense on a tty target; if target is Compositor or None, no-op the scroll.)

- `switch_vt(new_vt)` becomes `request_vt_switch(new_vt)`:

```rust
    pub fn request_vt_switch(&mut self, new_vt: usize) {
        if new_vt >= VT_COUNT { return; }
        if self.vtmgr_endpoint == 0 { return; }
        let msg = Message::new(
            VTMGR_REQUEST_VT_SWITCH_LABEL,
            [new_vt, 0, 0, 0, 0, 0], 1,
        );
        let _ = send(self.vtmgr_endpoint, &msg, IpcFlags::empty());
        let _ = debug_print(&format!("kbd: requested vt switch -> {}", new_vt));
    }
```

NOTE: This is fire-and-forget for now (no reply handling). vtmgr broadcasts the new state when the switch completes; kbd's cache updates automatically. If vtmgr refuses the switch (modal lock), no broadcast → kbd's cache stays put → behaviour matches expectation.

Decision rationale: making this a `call_with_reply` for errno would require kbd to block on a reply mid-IRQ handling, which is bad. Fire-and-forget + state observation via broadcast is cleaner.

- [ ] **Step 3: Update main.rs**

In `userspace/kbd/src/main.rs`:

- `ctx.switch_vt(target_vt as usize)` → `ctx.request_vt_switch(target_vt as usize)`.
- Replace the dual-write block (lines ~104-121) with a single call:

```rust
        if event.ascii.is_some() || event.extended != 0 {
            let outbound = build_kbd_event(
                event.ascii,
                event.scancode,
                event.modifiers.as_bits(),
                event.extended,
            );
            ctx.send_to_active(&outbound);
        }
```

No more `if ctx.compositor_input_ep != 0 { ... send to compositor too ... }`. Cache picks the right one.

- [ ] **Step 4: Build**

```bash
cargo xtask build 2>&1 | tail -5
```

- [ ] **Step 5: Harness — autostart path (regression)**

```bash
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_path_symlink_resolve bash scripts/harness_run.sh
grep -aE "kbd: routing cache|kbd: dropped keystroke|procmgr: auto-login root|procmgr: container 'ls'" /tmp/cluu-serial-com2.log
```

Expected:
- `kbd: routing cache vt=4 kind=Compositor` PRESENT (boot lands on VT4).
- The l2_path_symlink_resolve marker is autostart-driven (not keyboard-driven), so it should still pass.

- [ ] **Step 6: Harness — visual smoke (no-autostart path)**

```bash
HARNESS_FORCE_BUILD=0 MARKER_MODE=l2_vt4_default bash scripts/harness_run.sh \
  > /tmp/harness.out 2>&1 &
HARNESS_PID=$!
for _ in $(seq 1 60); do
  grep -q "compositor: ready" /tmp/cluu-serial-com2.log 2>/dev/null && break
  sleep 0.5
done
sleep 2  # let cluuterm settle
FB_PHYS=$(grep -oE 'fb @[0-9A-Fa-f]+' /tmp/cluu-serial-com2.log | head -1 | sed 's/fb @/0x/')
# Type some keys: ascii 'a' (scancode 0x1E)
echo "sendkey a" | socat - "UNIX-CONNECT:/tmp/cluu-qemu-monitor.sock"
sleep 1
bash scripts/fb_dump.sh -p "$FB_PHYS" -o /tmp/routing-vt4-typed
wait "$HARNESS_PID" || true
grep -aE "kbd: routing cache|kbd: requested vt|/bin/login" /tmp/cluu-serial-com2.log | head -20
```

Expected: cluuterm window shows the typed `a` somewhere (login prompt should have rendered earlier via bug-4 fix; typed `a` appends to the login: input field).

If the cluuterm window is still blank, two possible causes:
- bug #5 (compositor FRAME_READY backpressure) still blocking cluuterm — unrelated to this task, OK to defer.
- Or routing's `endpoint=0` story is wrong because subscribers don't do the lookup as designed. Re-check Task 3 Step 4 implementation. The compositor_input_ep that kbd uses in `send_to_active` is the one kbd already subscribed to at startup — NOT the cache's endpoint. So the cache only contributes the `kind`, not the endpoint, in v1. This is intentional per Task 3's NOTE. If you find this confusing, revisit the design memo §5.1.

- [ ] **Step 7: Commit**

```bash
git add userspace/kbd/src/main.rs userspace/kbd/src/context.rs
git commit -m "kbd: route via cache, drop active_vt field + dual-write hack"
```

---

## Task 7: kbd — switch Ctrl-Alt-Fn to REQUEST_VT_SWITCH; vtmgr drops old label

Old `VTMGR_SWITCH_VT_LABEL` becomes dead. Migrate kbd, then delete from vtmgr.

**Files:**
- Modify: `userspace/kbd/src/context.rs:197` — already uses `VTMGR_SWITCH_VT_LABEL`. Task 6 introduced `request_vt_switch` that uses the new label. Verify Task 6 actually swapped this; if not, swap now.
- Modify: `userspace/vtmgr/src/...` — delete the `VTMGR_SWITCH_VT_LABEL` handler arm.
- Modify: `userspace/libcluu/src/ipc.rs` — leave the `VTMGR_SWITCH_VT_LABEL` constant alone (other callers may exist); or delete after `grep -rn` confirms none.

- [ ] **Step 1: Verify kbd no longer sends VTMGR_SWITCH_VT_LABEL**

```bash
grep -rn "VTMGR_SWITCH_VT_LABEL" userspace/
```

Expected: only the label constant in `libcluu/src/ipc.rs` and the now-orphaned arm in vtmgr.

- [ ] **Step 2: Remove the orphaned vtmgr arm**

Find the `VTMGR_SWITCH_VT_LABEL` match in vtmgr's dispatch; remove the entire arm.

- [ ] **Step 3: Decide on the libcluu constant**

If no caller uses it, comment it out OR delete. Prefer delete; orphan constants rot.

- [ ] **Step 4: Build + harness regression**

```bash
cargo xtask build 2>&1 | tail -5
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_path_symlink_resolve bash scripts/harness_run.sh
HARNESS_FORCE_BUILD=0 MARKER_MODE=l2_vt4_default bash scripts/harness_run.sh
```

Both should still pass.

- [ ] **Step 5: Commit**

```bash
git add userspace/vtmgr/src/... userspace/libcluu/src/ipc.rs
git commit -m "vtmgr/libcluu: drop legacy VTMGR_SWITCH_VT_LABEL"
```

---

## Task 8: spec status + memory update

- [ ] **Step 1: Update specs**

In `docs/superpowers/specs/2026-05-12-login-flow-design.md`, in §4.6 (compositor scope additions), append a note:

```
**Input routing**: per design memo `2026-05-13-input-routing-single-source.md` (commit `dee8256`), kbd subscribes to vtmgr's active-VT broadcast. Compositor still owns window focus within VT4. Modal-lock label `VTMGR_LOCK_VT_SWITCH` reserved in libcluu.
```

- [ ] **Step 2: Commit**

```bash
git add docs/superpowers/specs/2026-05-12-login-flow-design.md
git commit -m "docs/spec: cross-link login-flow §4.6 to input-routing design"
```

---

## Self-review notes

- Touches: `userspace/libcluu/src/{ipc.rs, input_routing.rs (new), lib.rs}`, `userspace/vtmgr/src/{input_routing.rs (new), context.rs, main.rs}`, `userspace/kbd/src/{routing_cache.rs (new), context.rs, main.rs}`, one spec edit. No kernel.
- SOLID gate: any future inputd extraction = move `vtmgr/src/input_routing.rs` to `inputd/src/`, change kbd's `request_subscription("vtmgr", "active_vt")` to `("inputd", "active_vt")`. That's literally it.
- Task 3 Step 4 has an open question about registry pub-sub plumbing. If `registry::publish_to` doesn't exist, Task 3 will need to add it. Don't bypass it with a private vtmgr-kbd wire.
- `endpoint=0` in v1's RoutingTarget is intentional — kbd reuses its existing subscriptions to compositor:input / tty:N:main. Future enhancement: publisher resolves and includes the endpoint.
- Task 6 visual smoke can fail purely because of bug #5 (FRAME_READY backpressure) — that's a separate bug. Document that in the task's `DONE_WITH_CONCERNS` if it happens; do not roll back this plan.
- All commits land on `develop`. No force-push, no `--no-verify`, no `--amend`.
