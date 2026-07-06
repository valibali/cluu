# Input Routing — Single Source of Truth

**Date:** 2026-05-13
**Status:** Implemented (2026-07-06). inputd extracted per §7; see `.omo/plans/device-model-redesign.md` Phase 3.
**Owners:** kernel-team
**Related specs:**
- `docs/superpowers/specs/2026-05-12-login-flow-design.md` (compositor modal lock §4.6)
- `docs/superpowers/specs/2026-05-10-tui-compositor-design.md` (focus/modal/window)
- `docs/superpowers/specs/2026-05-12-compositor-menus-design.md` (menu keyboard ownership)
- `docs/ARCHITECTURE.md` (userspace service map)

## 1. Problem

CLUU today has two services that independently track "which VT is active":

- `vtmgr` (`userspace/vtmgr/src/context.rs`): owns `active_vt`, mutates it via `switch_vt`. Sends `COMP_VT_ACTIVATE` / `CONSOLE_DEACTIVATE` based on its own state.
- `kbd` (`userspace/kbd/`): owns its own `active_vt`, mutates it on Ctrl-Alt-Fn locally, decides which endpoint (compositor:input or tty:N) gets the keystroke.

The two can diverge. Plan-1's `active_vt = DEFAULT_COMPOSITOR_VT` fix made vtmgr boot to VT4 but kbd still boots to 0 — observed 2026-05-12: user sees VT4 (compositor) on screen, but typing went to tty:0 because kbd's `active_vt` was unchanged. Latent the moment focus has *any* meaning beyond "the kernel's VT".

Window focus inside VT4 is a worse version of the same problem: compositor will need to own which window receives input, but if kbd has *its own* opinion the model breaks immediately.

## 2. Prior art

| System            | Input router / focus oracle                                                                |
|-------------------|--------------------------------------------------------------------------------------------|
| Linux + console   | Kernel `fg_console`. Keyboard IRQ handler routes to that VT's tty line discipline.         |
| Linux + Wayland   | systemd-logind owns VT switching; compositor is the focus oracle and runs libinput inside. |
| X11               | Xorg holds focused window globally; clients subscribe.                                     |
| macOS             | WindowServer userspace daemon is sole input router; IOHIDFamily delivers raw events to it. |
| Windows           | win32k.sys (kernel) tracks foreground window; routes via Mach-port-equivalent.             |
| QNX               | `io-graphics` / Photon server is sole router; drivers send events to it.                   |
| seL4 (Genode etc) | `nitpicker` window server is sole router; one process per oracle.                          |

**Pattern**: every pro system has **one userspace process** that owns input routing + focus. The IRQ/decoder driver delivers raw events; the oracle decides targets. We are not currently following this pattern.

## 3. Decision

**Today (one CLUU service can do this):** `vtmgr` extends to own input routing. kbd becomes a pure cache-and-forward driver.

**Future (when input devices multiply):** Extract a dedicated `inputd` primordial. vtmgr shrinks back to VT lifecycle only. Compositor and tty are unchanged.

**Rationale for staying in vtmgr today**: vtmgr already owns the VT state machine. Adding routing-broadcaster duties keeps one process; no new primordial; matches systemd-logind shape during the small-scale phase.

**Rationale for SOLID separation**: every contract that crosses a service boundary today must be the same contract that inputd will speak tomorrow. No vtmgr-private protocol with kbd. No "trusted shortcuts". Extraction = relocating an existing module across a stable wire.

## 4. Roles

```
kbd (IRQ + decoder + dumb cache)
  │  raw scancode → keysym
  │  cache: RoutingTarget (initial: None)
  │
  ├──[VTMGR_REQUEST_VT_SWITCH(N)]──> vtmgr   ; on Ctrl-Alt-Fn only
  │
  └──[KEY_EVENT]──> cached.endpoint           ; every other key
                       (compositor:input or tty:N main)

vtmgr (VT lifecycle + input-routing oracle)
  │  active_vt (sole mutator)
  │  window-focus = N/A (compositor's job within VT4)
  │
  ├──[on boot / switch / restart]
  │      broadcast VTMGR_ACTIVE_VT_CHANGED(vt, target_endpoint)
  │      to all subscribers (kbd today; future: inputd, mouse, etc.)
  │
  └──[recv VTMGR_REQUEST_VT_SWITCH(N)]
         policy check (modal? quiesced?)
         do switch_vt(N) → COMP_VT_ACTIVATE/DEACTIVATE + CONSOLE_*
         broadcast new state

compositor (within VT4)
  │  sole mutator of window focus
  │  receives all keys via compositor:input while VT4 is active
  │  decides INPUT_FORWARD target
  │  enforces modal lock per login-flow §4.6
```

## 5. Contracts

All on libcluu's IPC label space; no vtmgr-private wire formats.

### 5.1 `VTMGR_ACTIVE_VT_CHANGED` (broadcast)

- Direction: vtmgr → any subscriber.
- Payload: `[vt: u32, target_endpoint: u64, target_kind: u32, padding...]`
  - `target_kind`: 0 = compositor input, 1 = tty:N main, 2 = nobody (boot/quiesce).
  - `target_endpoint`: the IPC endpoint kbd should send key events to. vtmgr resolves this once, kbd just stores the handle. Re-resolved if the underlying endpoint changes (rare).
- Reply: none. Fire-and-forget broadcast.
- Delivery: via registry's existing pub-sub channel under name `vtmgr:active_vt`. Subscribers call `request_subscription("vtmgr", "active_vt")` and get a grant; vtmgr publishes by sending to each granted endpoint.

### 5.2 `VTMGR_REQUEST_VT_SWITCH` (call)

- Direction: any client → vtmgr.
- Payload: `[vt: u32]`.
- Reply: `[result: u32]` — 0 ok, non-zero error code (e.g. `EBUSY` if modal locked).
- vtmgr is the sole decider. If it refuses (modal compositor window open + caller not privileged), it just doesn't switch.

### 5.3 No new label needed for compositor

Compositor already exposes `compositor:input` via registry. kbd looks up that endpoint when `target_kind == 0`. Same for `tty:N:main`.

## 6. SOLID anchors

The point of doing this in vtmgr today is to make tomorrow's extraction mechanical. Specific design rules:

### S — Single Responsibility

- New file `userspace/vtmgr/src/input_routing.rs` holds the broadcast machinery and the policy decision (`should_allow_switch`). Touching this file should never touch `context.rs`'s `switch_vt` logic and vice versa.
- `kbd`'s `cache` lives in its own module `userspace/kbd/src/routing_cache.rs`. The IRQ/decoder path imports it as a *consumer*, not as a tangled state machine.

### O — Open/Closed

- `target_kind` is an enum (0/1/2) over the wire. Add a `target_kind = 3` for "mouse" (future) without breaking the label. Subscribers ignore unknown kinds.
- `VTMGR_ACTIVE_VT_CHANGED`'s payload layout reserves `padding` u64s for future fields (e.g., window-focus hint when compositor takes over).

### L — Liskov

- The `RoutingTarget` enum in libcluu is a typed wrapper:
  ```rust
  pub enum RoutingTarget {
      None,
      Compositor { endpoint: usize },
      Tty { vt: u8, endpoint: usize },
  }
  ```
  Any router (vtmgr today, inputd tomorrow) emits `RoutingTarget`; any client consumes it. Same trait shape on both sides.

### I — Interface Segregation

- kbd does NOT call vtmgr's "internal" APIs. The only kbd→vtmgr wire is `VTMGR_REQUEST_VT_SWITCH`. Everything else flows the other way as broadcasts.
- vtmgr does NOT call kbd's "internal" APIs. Period.
- Compositor does NOT learn vtmgr's `active_vt` directly — it learns "you're now active" via `COMP_VT_ACTIVATE` (existing). Compositor never reads kbd's state.

### D — Dependency Inversion

- The kbd → router relationship is *late-bound* via the registry. kbd subscribes to `vtmgr:active_vt`. When extraction happens, the subscription name changes to `inputd:active_vt`. No code path changes other than the literal string + the publisher process binary name. Same labels, same payloads, same RoutingTarget enum.

### Bonus — Closed under restart

- On vtmgr crash + restart, vtmgr re-broadcasts the active-VT state on first scheduling step. kbd's cache rehydrates without manual intervention. (Mirrors compositor's restart story in `project_init_monitoring`.)

## 7. Extraction path (future)

When we have a second input device (mouse, gamepad, touch), and we're past the kernel freeze:

1. Create `userspace/inputd/`. Copy the `input_routing.rs` module from vtmgr verbatim.
2. inputd subscribes to vtmgr's `active_vt` broadcast (vtmgr still owns VT). inputd re-broadcasts `inputd:active_vt` to kbd/mouse/touch drivers.
3. inputd accepts `VTMGR_REQUEST_VT_SWITCH` from clients and forwards to vtmgr.
4. kbd's `routing_cache.rs` swaps its subscription target from `vtmgr:active_vt` to `inputd:active_vt`. No other change.
5. Compositor's modal-lock contract: today compositor calls `VTMGR_REQUEST_VT_SWITCH` to refuse switches during a modal. Tomorrow inputd holds the policy; compositor still calls the same label, just at inputd.
6. vtmgr loses its `input_routing.rs` module. shrinks to pure VT lifecycle.

Total user-facing diff: one registry name change. Zero label changes. Zero protocol changes.

## 8. Initial-state race

kbd boots before vtmgr has broadcast its first state. Two paths:

- **A. kbd drops keys until first broadcast**: simplest. Acceptable because human typing latency >> boot ordering window. (Boot takes ~5 s; first broadcast within ~1 s of vtmgr ready.)
- **B. kbd buffers**: complex; no real benefit.

Pick A. Document in the implementation.

For vtmgr's boot broadcast: emit `VTMGR_ACTIVE_VT_CHANGED` immediately after the existing boot `CONSOLE_DEACTIVATE(0)` / `COMP_VT_ACTIVATE` pair fires, AND on every subsequent `switch_vt` completion.

## 9. Modal-lock placeholder

Login-flow §4.6 requires compositor to enforce modal lock. Mechanism in this design:

- Compositor sends `VTMGR_LOCK_VT_SWITCH(self_pid, reason_id)` to vtmgr.
- vtmgr's `should_allow_switch` returns false for the duration.
- Compositor sends `VTMGR_UNLOCK_VT_SWITCH(self_pid, reason_id)` to release.
- Today: not implemented. The `should_allow_switch` stub returns true always.
- Future: implement.

Labels reserved in libcluu now, even though the policy is a stub. (Open/Closed.)

## 10. What this design does NOT cover

- Hot-plug input devices (USB keyboard insert at runtime). Today's PS/2 is fixed; this design extends cleanly when devices appear.
- Multi-seat (multiple keyboards, separate users). Out of scope.
- Accessibility input (sticky keys, etc). Out of scope.
- Multi-display VT mapping. Out of scope.
- IME / dead keys / compose key. Out of scope.

All of the above slot in cleanly under inputd when it splits off.

## 11. Next steps

1. This memo committed.
2. Plan: `docs/superpowers/plans/2026-05-13-input-routing-vtmgr.md` (separate doc; derived from this memo).
3. Implement under SOLID rules above.
4. Bug #1 from 2026-05-12 hardening list becomes a consequence: kbd's `active_vt` field is deleted entirely as part of this work.
