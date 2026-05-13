# Input Routing — vtmgr-as-router Implementation Plan (v1)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make vtmgr the single source of truth for active-VT AND the sole input router. kbd shrinks to pure IRQ/decoder/forward driver. Eliminate the vtmgr/kbd `active_vt` desync (bug #1, 2026-05-12).

**Architecture:** vtmgr subscribes to `compositor:input` and `tty:0..3:main`, holding all 5 outbound send-tokens. kbd subscribes to `vtmgr:input` and forwards every `KBD_EVENT_LABEL` upstream. vtmgr's recv loop decides the destination per current active VT and re-emits `KBD_EVENT_LABEL` to the chosen target.

Two IPC hops per keystroke (kbd → vtmgr → target). Negligible latency at human typing rates; the win is true single-decider, modal-lock-trivially-enforceable, and SOLID for future inputd extraction (literal rename of `vtmgr:input` → `inputd:input` in registry).

**Tech Stack:** Rust (vtmgr, kbd, libcluu), CLUU harness (`scripts/harness_run.sh`).

**Parent design:** `docs/superpowers/designs/2026-05-13-input-routing-single-source.md` (commit `dee8256`).
**Parent specs:** `docs/superpowers/specs/2026-05-12-login-flow-design.md` §4.6, `docs/superpowers/specs/2026-05-10-tui-compositor-design.md`.

**Supersedes:** v0 plan from earlier today (kbd-as-router-with-cache). v0 left routing decisions in kbd; v1 moves them to vtmgr.

**Restart caveat:** vtmgr restart-on-crash is not yet wired (user note 2026-05-13). All revocable-token + re-subscribe behaviour is therefore correct-by-design today; it'll be exercised once Phase I restart wiring extends to vtmgr. Out of scope here.

---

## Task 1: libcluu — new IPC labels + RoutingTargetKind enum

Pure plumbing. `KBD_EVENT_LABEL` (=1) is reused for both legs of the forward — no new event label needed; only control-plane labels.

**Files:**
- Modify: `userspace/libcluu/src/ipc.rs`.
- Create: `userspace/libcluu/src/input_routing.rs`.
- Modify: `userspace/libcluu/src/lib.rs`.

- [ ] **Step 1: Add label constants**

In `userspace/libcluu/src/ipc.rs`, append after `COMP_CLOSE_REQUEST_LABEL = 101`:

```rust
// --- Input routing (vtmgr today; inputd post-extraction). ---
// client → vtmgr: request a VT switch. vtmgr decides per policy.
// Words: [vt: u32]. Reply: words[0] = errno (0 ok).
pub const VTMGR_REQUEST_VT_SWITCH_LABEL: u32 = 110;
// compositor → vtmgr: take/release modal lock on VT switching.
// Reserved per login-flow §4.6; impl is stub today.
pub const VTMGR_LOCK_VT_SWITCH_LABEL:   u32 = 111;
pub const VTMGR_UNLOCK_VT_SWITCH_LABEL: u32 = 112;
```

- [ ] **Step 2: Create the routing-types module**

```rust
// userspace/libcluu/src/input_routing.rs
//! Shared types for input-routing IPC.
//!
//! Router today is vtmgr; tomorrow it will be a dedicated inputd.
//! These types live in libcluu so both ends speak the same dialect
//! regardless of which process is the publisher.

#![allow(dead_code)]

/// Where keystrokes should go for the currently-active VT.
///
/// Used internally by the router (vtmgr today) to pick which output
/// send-token to use for an incoming event. NOT serialised on the
/// wire — the router holds the token table directly.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RoutingTargetKind {
    /// No active target yet (boot, quiesce, transition). Router drops events.
    None,
    /// Forward to the compositor's input endpoint.
    Compositor,
    /// Forward to tty:N's main endpoint. N is the VT index (0..=3).
    Tty(u8),
}
```

- [ ] **Step 3: Re-export**

In `userspace/libcluu/src/lib.rs`:

```rust
pub mod input_routing;
```

- [ ] **Step 4: Build**

```bash
cargo xtask build 2>&1 | tail -5
```

- [ ] **Step 5: Commit**

```bash
git add userspace/libcluu/src/ipc.rs userspace/libcluu/src/input_routing.rs userspace/libcluu/src/lib.rs
git commit -m "libcluu: add VTMGR_* control labels + RoutingTargetKind enum"
```

---

## Task 2: vtmgr — subscribe to all input destinations

vtmgr requests subscriptions to `compositor:input` and `tty:0..3:main`. Stores derived tokens.

**Files:**
- Modify: `userspace/vtmgr/src/context.rs`.

- [ ] **Step 1: Add token storage**

```rust
    compositor_input_ep: usize,
    tty_main_eps: [usize; VT_COUNT],
    requested_compositor_input: bool,
    requested_tty_main: u8,  // bit N = requested tty:N main
```

Init in `new`:

```rust
            compositor_input_ep: 0,
            tty_main_eps: [0; VT_COUNT],
            requested_compositor_input: false,
            requested_tty_main: 0,
```

- [ ] **Step 2: Add subscription requests**

In `ensure_subscriptions`, append:

```rust
        if !self.requested_compositor_input && self.compositor_input_ep == 0 {
            if registry::request_subscription("compositor", "input").is_ok() {
                self.requested_compositor_input = true;
            }
        }
        for vt in 0..VT_COUNT {
            let bit = 1u8 << vt;
            if (self.requested_tty_main & bit) == 0 && self.tty_main_eps[vt] == 0 {
                let svc = format!("tty:{}", vt);
                if registry::request_subscription(&svc, "main").is_ok() {
                    self.requested_tty_main |= bit;
                }
            }
        }
```

- [ ] **Step 3: Handle grants in `handle_registry_message`**

Add new branches BEFORE the catch-all `else if name == "control"`:

```rust
                    if service_name == "compositor" && name == "input" {
                        self.compositor_input_ep = token;
                        let _ = debug_print("vtmgr: compositor input subscribed");
                    } else if let Some(idx) = service_name.strip_prefix("tty:")
                        .and_then(|s| s.parse::<usize>().ok())
                    {
                        if name == "main" && idx < VT_COUNT {
                            self.tty_main_eps[idx] = token;
                            let _ = debug_print(&format!(
                                "vtmgr: tty:{} main subscribed", idx
                            ));
                        }
                    } else if service_name == "compositor" && name == "control" {
                        // existing arm
                        ...
                    } else if name == "control" {
                        // existing console arm
                        ...
                    }
```

- [ ] **Step 4: Build + harness sanity**

```bash
cargo xtask build 2>&1 | tail -5
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_vt4_default bash scripts/harness_run.sh
grep -aE "vtmgr: compositor input subscribed|vtmgr: tty:.*main subscribed" /tmp/cluu-serial-com2.log
```

Expected: compositor + tty:0 subscriptions visible. tty:1..3 may not show because they spawn on-demand; that's fine for this task.

- [ ] **Step 5: Commit**

```bash
git add userspace/vtmgr/src/context.rs
git commit -m "vtmgr: subscribe to compositor:input and tty:N:main outputs"
```

---

## Task 3: vtmgr — register `vtmgr:input` output

Expose an `input` output backed by vtmgr's main endpoint so kbd can subscribe.

**Files:**
- Modify: `userspace/vtmgr/src/context.rs`.

- [ ] **Step 1: Register the output**

In `VtmgrContext::new`, after the existing `registry::register_output("control", endpoint)?`:

```rust
        // Input event ingress for the router. kbd subscribes here.
        registry::register_output("input", endpoint)?;
```

- [ ] **Step 2: Build**

```bash
cargo xtask build 2>&1 | tail -5
```

- [ ] **Step 3: Commit**

```bash
git add userspace/vtmgr/src/context.rs
git commit -m "vtmgr: register 'input' output for upstream key forwarding"
```

---

## Task 4: vtmgr — input_routing module + active-target tracking

**Files:**
- Create: `userspace/vtmgr/src/input_routing.rs`.
- Modify: `userspace/vtmgr/src/main.rs` (`mod input_routing;`).
- Modify: `userspace/vtmgr/src/context.rs` (router field, helper, switch_vt hook, boot hook).

- [ ] **Step 1: Create the module**

```rust
// userspace/vtmgr/src/input_routing.rs
//! Input router. Holds active target; forwards each KBD_EVENT to the
//! caller-resolved endpoint. Updated by `context::switch_vt` after every
//! transition.
//!
//! Future inputd extraction lifts this module ~verbatim; only the
//! registry output name changes from "vtmgr:input" to "inputd:input".

use libcluu::input_routing::RoutingTargetKind;
use libcluu::ipc::send;
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, yield_cpu};
use libcluu::error::Error;
use core::sync::atomic::{AtomicBool, Ordering};

const SEND_RETRY_BOUND: usize = 8;
static FIRST_DROP_LOGGED: AtomicBool = AtomicBool::new(false);

pub struct InputRouter {
    active: RoutingTargetKind,
}

impl InputRouter {
    pub const fn new() -> Self {
        Self { active: RoutingTargetKind::None }
    }

    pub fn set_active(&mut self, target: RoutingTargetKind) {
        if self.active != target {
            let _ = debug_print(&alloc::format!(
                "vtmgr: router target {:?} -> {:?}",
                self.active, target
            ));
            self.active = target;
        }
    }

    /// Forward `msg` to the endpoint resolved from the current active
    /// target. `lookup_endpoint` keeps context.rs internals out of this
    /// module. Returns true if a send was attempted.
    pub fn forward(
        &self,
        msg: &Message,
        lookup_endpoint: impl FnOnce(RoutingTargetKind) -> usize,
    ) -> bool {
        let ep = lookup_endpoint(self.active);
        if ep == 0 {
            return false;
        }
        for _ in 0..SEND_RETRY_BOUND {
            match send(ep, msg, IpcFlags::empty()) {
                Ok(()) => return true,
                Err(Error::WouldBlock) | Err(Error::Busy) => {
                    let _ = yield_cpu();
                    continue;
                }
                Err(_) => return false,
            }
        }
        if !FIRST_DROP_LOGGED.swap(true, Ordering::Relaxed) {
            let _ = debug_print("vtmgr: dropped keystroke (target backlog persistent)");
        }
        false
    }

    /// Modal lock placeholder. Today: allows always.
    pub fn should_allow_switch(&self, _from: u8, _to: u8) -> bool {
        true
    }
}
```

- [ ] **Step 2: Declare the module**

In `userspace/vtmgr/src/main.rs`:

```rust
mod input_routing;
```

- [ ] **Step 3: Add router field + endpoint-lookup helper to `VtmgrContext`**

```rust
    router: crate::input_routing::InputRouter,
```

```rust
            router: crate::input_routing::InputRouter::new(),
```

```rust
    pub fn lookup_target_endpoint(&self, kind: libcluu::input_routing::RoutingTargetKind) -> usize {
        use libcluu::input_routing::RoutingTargetKind;
        match kind {
            RoutingTargetKind::None => 0,
            RoutingTargetKind::Compositor => self.compositor_input_ep,
            RoutingTargetKind::Tty(n) => {
                let idx = n as usize;
                if idx < VT_COUNT { self.tty_main_eps[idx] } else { 0 }
            }
        }
    }
```

- [ ] **Step 4: Hook switch_vt**

After `self.active_vt = new_vt;`:

```rust
        use libcluu::input_routing::RoutingTargetKind;
        let target_kind = if new_vt == self.compositor_vt {
            RoutingTargetKind::Compositor
        } else {
            RoutingTargetKind::Tty(new_vt as u8)
        };
        self.router.set_active(target_kind);
```

- [ ] **Step 5: Boot-path hook**

In `handle_registry_message`, in BOTH the compositor-control branch and the console-control branch, after the existing activate/deactivate sends:

```rust
                            use libcluu::input_routing::RoutingTargetKind;
                            let target_kind = if self.active_vt == self.compositor_vt {
                                RoutingTargetKind::Compositor
                            } else {
                                RoutingTargetKind::Tty(self.active_vt as u8)
                            };
                            self.router.set_active(target_kind);
```

`set_active` is idempotent.

- [ ] **Step 6: Build + harness**

```bash
cargo xtask build 2>&1 | tail -5
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_vt4_default bash scripts/harness_run.sh
grep -aE "vtmgr: router target" /tmp/cluu-serial-com2.log
```

Expected: `vtmgr: router target None -> Compositor` at boot.

- [ ] **Step 7: Commit**

```bash
git add userspace/vtmgr/src/main.rs userspace/vtmgr/src/input_routing.rs userspace/vtmgr/src/context.rs
git commit -m "vtmgr: input_routing module + active-target tracking"
```

---

## Task 5: vtmgr — recv loop forwards KBD_EVENT

**Files:**
- Modify: `userspace/vtmgr/src/main.rs` (or wherever message dispatch lives).

- [ ] **Step 1: Locate dispatch**

```bash
grep -n "match.*tag.label\|VTMGR_PIN_VT" userspace/vtmgr/src/*.rs
```

- [ ] **Step 2: Add KBD_EVENT_LABEL arm**

```rust
        KBD_EVENT_LABEL => {
            ctx.router.forward(&msg, |kind| ctx.lookup_target_endpoint(kind));
        }
```

Ensure `KBD_EVENT_LABEL` is in the imports.

- [ ] **Step 3: Build**

```bash
cargo xtask build 2>&1 | tail -5
```

Nothing fires until Task 7 swaps kbd. Compile success is the test.

- [ ] **Step 4: Commit**

```bash
git add userspace/vtmgr/src/main.rs
git commit -m "vtmgr: dispatch KBD_EVENT_LABEL through router.forward"
```

---

## Task 6: vtmgr — handle VTMGR_REQUEST_VT_SWITCH

**Files:**
- Modify: `userspace/vtmgr/src/main.rs`.

- [ ] **Step 1: Add the arm**

```rust
        VTMGR_REQUEST_VT_SWITCH_LABEL => {
            let new_vt = msg.words[0];
            let allowed = ctx.router.should_allow_switch(
                ctx.active_vt as u8, new_vt as u8
            );
            let err: u64 = if !allowed {
                libcluu::errno::EBUSY as u64
            } else if new_vt >= VT_COUNT {
                libcluu::errno::EINVAL as u64
            } else {
                ctx.switch_vt(new_vt);
                0
            };
            if let Some(reply_id) = libcluu::ipc::extract_reply_id(&msg) {
                let reply = Message::new(0, [err as usize, 0, 0, 0, 0, 0], 1);
                let _ = libcluu::ipc::reply(reply_id, &reply, IpcFlags::empty());
            }
        }
```

- [ ] **Step 2: Build**

```bash
cargo xtask build 2>&1 | tail -5
```

- [ ] **Step 3: Commit**

```bash
git add userspace/vtmgr/src/main.rs
git commit -m "vtmgr: handle VTMGR_REQUEST_VT_SWITCH with policy hook"
```

---

## Task 7: kbd — single-target forwarding via vtmgr

**Files:**
- Modify: `userspace/kbd/src/context.rs`.
- Modify: `userspace/kbd/src/main.rs`.

- [ ] **Step 1: Replace fields**

Remove from `KbdContext`:
- `active_vt: usize`
- `compositor_input_ep: usize`
- `tty_endpoints: [usize; VT_COUNT]`
- `requested_compositor: bool`
- per-tty subscription bookkeeping (`requested_tty: u8` etc.)

Add:
- `vtmgr_input_ep: usize`
- `vtmgr_control_ep: usize`
- `requested_vtmgr_input: bool`
- `requested_vtmgr_control: bool`

Update all init sites accordingly.

NOTE: the existing `vtmgr_endpoint` field (used today for `VTMGR_SWITCH_VT_LABEL`) is renamed → `vtmgr_control_ep`, repointed to "vtmgr:control".

- [ ] **Step 2: Rewrite `request_subscriptions`**

```rust
    pub fn request_subscriptions(&mut self) {
        if !self.requested_vtmgr_input && self.vtmgr_input_ep == 0 {
            if registry::request_subscription("vtmgr", "input").is_ok() {
                self.requested_vtmgr_input = true;
            }
        }
        if !self.requested_vtmgr_control && self.vtmgr_control_ep == 0 {
            if registry::request_subscription("vtmgr", "control").is_ok() {
                self.requested_vtmgr_control = true;
            }
        }
        // procmgr:spawn subscription stays for shutdown combo (existing).
        ...
    }
```

Drop the old compositor:input / per-tty / "vtmgr" (old name) blocks.

- [ ] **Step 3: Rewrite grant handler**

In `handle_registry_message`'s `Grant` arm:

```rust
                    if service_name == "vtmgr" && name == "input" {
                        self.vtmgr_input_ep = token;
                        let _ = debug_print("kbd: vtmgr:input subscribed");
                    } else if service_name == "vtmgr" && name == "control" {
                        self.vtmgr_control_ep = token;
                        let _ = debug_print("kbd: vtmgr:control subscribed");
                    } else if service_name == "procmgr" && name == "spawn" {
                        // existing procmgr arm
                        ...
                    }
```

Remove the old compositor:input + tty grant arms.

- [ ] **Step 4: Replace `switch_vt` → `request_vt_switch`**

```rust
    pub fn request_vt_switch(&self, new_vt: usize) {
        if new_vt >= VT_COUNT { return; }
        if self.vtmgr_control_ep == 0 { return; }
        let msg = Message::new(
            VTMGR_REQUEST_VT_SWITCH_LABEL,
            [new_vt, 0, 0, 0, 0, 0], 1,
        );
        let _ = send(self.vtmgr_control_ep, &msg, IpcFlags::empty());
        let _ = debug_print(&format!("kbd: requested vt switch -> {}", new_vt));
    }
```

Fire-and-forget. vtmgr broadcasts state via its routing decisions, not via a reply.

- [ ] **Step 5: Add `send_to_router`**

```rust
    pub fn send_to_router(&self, msg: &Message) {
        if self.vtmgr_input_ep == 0 { return; }
        for _ in 0..8 {
            match send(self.vtmgr_input_ep, msg, IpcFlags::empty()) {
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
            let _ = debug_print("kbd: dropped keystroke (vtmgr backlog persistent)");
        }
    }
```

- [ ] **Step 6: Rewrite main.rs forward block**

Replace the existing dual-write block (~lines 104-121):

```rust
        if event.ascii.is_some() || event.extended != 0 {
            let outbound = build_kbd_event(
                event.ascii,
                event.scancode,
                event.modifiers.as_bits(),
                event.extended,
            );
            ctx.send_to_router(&outbound);
        }
```

- [ ] **Step 7: Update Ctrl-Alt-Fn site**

In main.rs ~line 79: `ctx.switch_vt(target_vt as usize)` → `ctx.request_vt_switch(target_vt as usize)`.

- [ ] **Step 8: Handle `send_scroll` regression**

`send_scroll` uses the now-gone `active_vt`. v1 punt:

```rust
    pub fn send_scroll(&self, _direction: usize) {
        // TODO(v2): route scroll via vtmgr or query active VT from kernel.
        // Scroll is non-essential to login flow.
    }
```

Document in commit message.

- [ ] **Step 9: Build + harness, both paths**

```bash
cargo xtask build 2>&1 | tail -5
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_path_symlink_resolve bash scripts/harness_run.sh
HARNESS_FORCE_BUILD=0 MARKER_MODE=l2_vt4_default bash scripts/harness_run.sh
grep -aE "kbd: vtmgr.*subscribed|kbd: requested vt|vtmgr: router target" /tmp/cluu-serial-com2.log | head
```

Expected:
- `kbd: vtmgr:input subscribed` + `kbd: vtmgr:control subscribed` both present.
- `vtmgr: router target None -> Compositor` once.
- No `dropped keystroke` markers (idle system).

- [ ] **Step 10: Commit**

```bash
git add userspace/kbd/src/main.rs userspace/kbd/src/context.rs
git commit -m "kbd: route via vtmgr (single source) — drop active_vt + dual-write"
```

---

## Task 8: drop legacy VTMGR_SWITCH_VT_LABEL

**Files:**
- Modify: `userspace/vtmgr/src/main.rs`.
- Modify: `userspace/libcluu/src/ipc.rs`.

- [ ] **Step 1: Verify no users**

```bash
grep -rn "VTMGR_SWITCH_VT_LABEL" userspace/
```

Expected: only the constant + a now-dead vtmgr arm.

- [ ] **Step 2: Delete arm + constant**

Edit out both. If `grep` finds a stray reference, fix it in this task before deleting the constant.

- [ ] **Step 3: Build + harness regression**

```bash
cargo xtask build 2>&1 | tail -5
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_path_symlink_resolve bash scripts/harness_run.sh
```

- [ ] **Step 4: Commit**

```bash
git add userspace/vtmgr/src/main.rs userspace/libcluu/src/ipc.rs
git commit -m "vtmgr/libcluu: drop legacy VTMGR_SWITCH_VT_LABEL"
```

---

## Task 9: visual smoke + spec status

- [ ] **Step 1: Type-and-dump**

Per `reference_fb_dump_smoke_workflow.md`:

```bash
HARNESS_FORCE_BUILD=0 MARKER_MODE=l2_vt4_default bash scripts/harness_run.sh \
  > /tmp/harness.out 2>&1 &
HARNESS_PID=$!
for _ in $(seq 1 60); do
  grep -q "compositor: ready" /tmp/cluu-serial-com2.log 2>/dev/null && break
  sleep 0.5
done
sleep 2
FB_PHYS=$(grep -oE 'fb @[0-9A-Fa-f]+' /tmp/cluu-serial-com2.log | head -1 | sed 's/fb @/0x/')
echo "sendkey a" | socat - "UNIX-CONNECT:/tmp/cluu-qemu-monitor.sock"
sleep 1
bash scripts/fb_dump.sh -p "$FB_PHYS" -o /tmp/router-typed-a
wait "$HARNESS_PID" || true
```

Compare `/tmp/router-typed-a.png` unique-color count and entropy against the post-bug-4-fix baseline. Higher = good (the typed `a` made it through the new pipeline and rendered).

If still blank: bug #5 (FRAME_READY backpressure) is the prime suspect — separate ticket, do not roll back this plan.

- [ ] **Step 2: Spec status update**

`docs/superpowers/specs/2026-05-12-login-flow-design.md` §4.6 — append:

```
**Input routing**: kbd → vtmgr → target (compositor:input or tty:N:main). vtmgr is the sole router. Implemented per plan `2026-05-13-input-routing-vtmgr.md`. Modal-lock labels VTMGR_LOCK_VT_SWITCH / UNLOCK reserved; policy hook stubbed (always allows).
```

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/2026-05-12-login-flow-design.md
git commit -m "docs/spec: login-flow §4.6 cross-references input-routing plan"
```

---

## Self-review notes

- Files touched: `userspace/libcluu/{src/ipc.rs, src/input_routing.rs (new), src/lib.rs}`, `userspace/vtmgr/{src/input_routing.rs (new), src/context.rs, src/main.rs}`, `userspace/kbd/{src/context.rs, src/main.rs}`, one spec edit. No kernel.
- Cap flow (per design memo):
  - kbd → vtmgr: T_V_IN (input), T_V_CTL (control).
  - vtmgr → compositor: T_C; vtmgr → tty:N: T_TN.
  - compositor → vtmgr (future modal lock): T_V_CTL_C.
  - All minted via registry's existing `token_derive(... SEND | CALL ...)` flow.
- `send_scroll` regression accepted in v1; reroute via vtmgr in a later plan.
- No broadcast carries token handles. Router holds endpoints directly. v0 plan's `endpoint=0`-in-broadcast confusion is gone.
- Restart-on-crash not exercised — Phase I restart wiring doesn't yet apply to vtmgr per user note.
- All commits land on `develop`. No force-push, no `--no-verify`, no `--amend`.
