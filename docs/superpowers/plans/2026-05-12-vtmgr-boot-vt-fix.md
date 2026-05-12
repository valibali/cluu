# vtmgr Boot-VT Fix — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make boot reliably land on the compositor VT without manual Ctrl-Alt-F5, closing the existing pin/control race in `userspace/vtmgr/src/context.rs`.

**Architecture:** Initialise `VtmgrContext::active_vt` to `DEFAULT_COMPOSITOR_VT` instead of `0`. Make the compositor-grant arrival path send `COMP_VT_ACTIVATE` whenever `active_vt == compositor_vt` and console-grant arrival send `CONSOLE_DEACTIVATE` for VT0 when `active_vt != 0`. Drop the `boot_switch_pending` machinery. This removes any dependency on the order in which `VTMGR_PIN_VT_LABEL` and the compositor-control grant arrive. Existing Ctrl-Alt-F1..F4 paths to switch back to console VTs are unchanged.

**Tech Stack:** Rust (vtmgr), libcluu IPC, CLUU harness (`scripts/harness_run.sh`).

**Parent spec:** `docs/superpowers/specs/2026-05-12-login-flow-design.md` §4.1.

---

## Task 1: Add boot-VT harness marker

Add a serial-log marker that captures `active_vt` at the end of `VtmgrContext::new` so any regression is observable.

**Files:**
- Modify: `userspace/vtmgr/src/context.rs:70-72`

- [ ] **Step 1: Add marker debug print**

Replace the existing `debug_print("vtmgr: ready")?` block:

```rust
        debug_print(&format!(
            "vtmgr: ready active_vt={} compositor_vt={}",
            DEFAULT_COMPOSITOR_VT,  // post-fix value; before fix this prints 0
            DEFAULT_COMPOSITOR_VT,
        ))?;
        yield_cpu()?;
```

(Note: before Task 2 lands the first arg is wrong by construction; that's why this marker is added in Task 1 and verified in Task 2 — it's the regression detector for the fix itself.)

- [ ] **Step 2: Build and run harness**

```bash
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_path_symlink_resolve bash scripts/harness_run.sh
grep -a "vtmgr: ready active_vt" /tmp/cluu-serial-com2.log
```

Expected after Task 1 alone: `vtmgr: ready active_vt=4 compositor_vt=4` (because the literal we passed is `DEFAULT_COMPOSITOR_VT`). This proves the marker fires but does NOT yet prove the fix because the actual `self.active_vt` field is still `0` until Task 2. The marker shape is the regression detector for Task 2.

- [ ] **Step 3: Commit**

```bash
git add userspace/vtmgr/src/context.rs
git commit -m "vtmgr: add boot active_vt marker for boot-VT fix regression test"
```

---

## Task 2: Initialise active_vt to DEFAULT_COMPOSITOR_VT

Flip the boot-time `active_vt` so the compositor's slot is active from the start.

**Files:**
- Modify: `userspace/vtmgr/src/context.rs:78`

- [ ] **Step 1: Change init value**

```rust
            active_vt: DEFAULT_COMPOSITOR_VT,
```

- [ ] **Step 2: Update the Task 1 marker to read from `self.active_vt`**

Move the marker so it prints the actual struct field, not the constant, by deferring the marker to right after the `Ok(Self { ... })` construction. Restructure `new()` to build the struct, then print, then return:

```rust
        let ctx = Self {
            endpoint,
            registry_endpoint,
            console_endpoint: 0,
            procmgr_spawn_endpoint: 0,
            active_vt: DEFAULT_COMPOSITOR_VT,
            vt_created: 1,
            vt_spawned: 0,
            requested_console: false,
            requested_procmgr_spawn: false,
            compositor_control: 0,
            requested_compositor: false,
            compositor_vt: DEFAULT_COMPOSITOR_VT,
            boot_switch_pending: false,
        };
        debug_print(&format!(
            "vtmgr: ready active_vt={} compositor_vt={}",
            ctx.active_vt, ctx.compositor_vt
        ))?;
        yield_cpu()?;
        Ok(ctx)
```

(The `boot_switch_pending` field is removed in Task 4. Leave it in place for this task to keep the diff isolated.)

- [ ] **Step 3: Build and run harness**

```bash
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_path_symlink_resolve bash scripts/harness_run.sh
grep -a "vtmgr: ready active_vt" /tmp/cluu-serial-com2.log
```

Expected: `vtmgr: ready active_vt=4 compositor_vt=4`.

- [ ] **Step 4: Commit**

```bash
git add userspace/vtmgr/src/context.rs
git commit -m "vtmgr: initialise active_vt to compositor slot at boot"
```

---

## Task 3: Fire activation on grant arrival, no matter the order

`active_vt = 4` at boot is necessary but not sufficient: neither the compositor nor the console knows about the active state until vtmgr sends `COMP_VT_ACTIVATE` / `CONSOLE_DEACTIVATE`. Today these are only sent inside `switch_vt`, which is in turn only fired by an explicit pin or hotkey. The fix: on each grant arrival, push the activation state out.

**Files:**
- Modify: `userspace/vtmgr/src/context.rs:115-151` (`handle_registry_message`)

- [ ] **Step 1: Push activation when compositor control arrives**

Replace the compositor-control branch in `handle_registry_message` (currently at ~lines 118-128) with:

```rust
                    if service_name == "compositor" && name == "control" {
                        self.compositor_control = token;
                        let _ = debug_print("vtmgr: compositor control subscribed");
                        if self.active_vt == self.compositor_vt {
                            let msg = Message::new(
                                COMP_VT_ACTIVATE_LABEL,
                                [0; 6], 0,
                            );
                            let _ = send(self.compositor_control, &msg, IpcFlags::empty());
                            let _ = debug_print("vtmgr: boot COMP_VT_ACTIVATE sent");
                        }
                    } else if name == "control" {
```

(`boot_switch_pending` handling is removed in Task 4; the conditional above replaces the prior `if self.boot_switch_pending { ... }` block entirely.)

- [ ] **Step 2: Push deactivation when console control arrives if compositor is active**

Replace the console-control branch (currently at ~lines 129-132) with:

```rust
                    } else if name == "control" {
                        self.console_endpoint = token;
                        let _ = debug_print("vtmgr: console control subscribed");
                        if self.active_vt != 0 {
                            let de = Message::new(
                                CONSOLE_DEACTIVATE_LABEL,
                                [0, 0, 0, 0, 0, 0], 1,
                            );
                            let _ = send(self.console_endpoint, &de, IpcFlags::empty());
                            let _ = debug_print("vtmgr: boot CONSOLE_DEACTIVATE(0) sent");
                        }
```

- [ ] **Step 3: Build and run harness**

```bash
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_path_symlink_resolve bash scripts/harness_run.sh
grep -a -E "vtmgr: ready active_vt|vtmgr: boot COMP_VT_ACTIVATE sent|vtmgr: boot CONSOLE_DEACTIVATE" /tmp/cluu-serial-com2.log
```

Expected: all three markers present, in any order. Both the `COMP_VT_ACTIVATE` and `CONSOLE_DEACTIVATE(0)` lines must appear at least once during boot.

- [ ] **Step 4: Commit**

```bash
git add userspace/vtmgr/src/context.rs
git commit -m "vtmgr: push boot activation on grant arrival, drop pin-order dependence"
```

---

## Task 4: Drop the now-dead `boot_switch_pending` machinery

`boot_switch_pending` is no longer reached: the activation is unconditionally pushed by Task 3.

**Files:**
- Modify: `userspace/vtmgr/src/context.rs` (struct, init, `handle_pin_vt`)

- [ ] **Step 1: Remove the field and all uses**

Delete from the struct (~line 56):

```rust
    boot_switch_pending: bool,
```

Delete from `Self { ... }` in `new()` (~line 86):

```rust
            boot_switch_pending: false,
```

Replace `handle_pin_vt` (~lines 162-180) with the simplified version that no longer needs the deferred-switch arm:

```rust
    pub fn handle_pin_vt(&mut self, vt_index: usize, service_name: &str) {
        if service_name == "compositor" && vt_index < VT_COUNT {
            self.compositor_vt = vt_index;
            let _ = debug_print(&format!(
                "vtmgr: compositor pinned to VT{}",
                vt_index
            ));
            if self.active_vt != vt_index {
                self.switch_vt(vt_index);
            }
        }
    }
```

- [ ] **Step 2: Verify compile**

```bash
cargo xtask build 2>&1 | tail -20
```

Expected: no errors. If `boot_switch_pending` is referenced anywhere else (it should not be), the compiler points to it.

- [ ] **Step 3: Run harness regression**

```bash
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_path_symlink_resolve bash scripts/harness_run.sh
grep -a -E "vtmgr: ready active_vt|vtmgr: boot COMP_VT_ACTIVATE sent|vtmgr: boot CONSOLE_DEACTIVATE|vtmgr: vt switch" /tmp/cluu-serial-com2.log | head -20
```

Expected: same lines as Task 3 step 3 still present. No `vtmgr: vt switch 0 -> 4` (because no implicit switch happens — we never were "on" VT0).

- [ ] **Step 4: Commit**

```bash
git add userspace/vtmgr/src/context.rs
git commit -m "vtmgr: drop boot_switch_pending (replaced by grant-arrival activation)"
```

---

## Task 5: Visual smoke — confirm boot lands on compositor

Manual visual check (no automated harness for visual fb content yet; rely on debug log + ad-hoc QEMU run).

**Files:** none.

- [ ] **Step 1: Boot in QEMU with display**

```bash
cargo xtask qemu
```

Wait for boot to settle (~5 s).

- [ ] **Step 2: Confirm visual state**

Expected:
- Compositor's blank/desktop frame is visible at boot. No console banner.
- Ctrl-Alt-F1 switches to console VT1 (now showing console getty / shell after later plans).
- Ctrl-Alt-F5 (or whatever VT4 hotkey is) returns to the compositor.

If the screen is still on VT0 at boot: a step was wrong. Re-grep the serial log for the markers added in Tasks 1–3; one of them is missing.

- [ ] **Step 3: Document** in the spec's "Status" section.

Edit `docs/superpowers/specs/2026-05-12-login-flow-design.md` §4.1:
prepend a line `**Status:** done in plan 2026-05-12-vtmgr-boot-vt-fix.`

```bash
git add docs/superpowers/specs/2026-05-12-login-flow-design.md
git commit -m "docs/spec: vtmgr boot-VT fix marked done"
```

---

## Self-review notes

- All five tasks touch only `userspace/vtmgr/src/context.rs` plus one spec edit. No kernel changes, no other crates.
- `boot_switch_pending` is introduced in Task 2 (kept as carry-over) and removed in Task 4. The two-step keeps each commit's diff small and bisectable.
- The harness marker `vtmgr: ready active_vt=4 compositor_vt=4` is the single line that proves the fix; it survives all later plans.
- No new IPC labels, no protocol changes.
- Ctrl-Alt-Fn behavior (existing `switch_vt`) is untouched, so toggling between compositor and console VTs continues working.
- VT0 console still gets pre-created (`vt_created: 1`); the only difference is vtmgr now tells it to deactivate during boot so it doesn't stomp the fb.
