# VT-Switch Hardening — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix three pre-existing bugs surfaced 2026-05-12/13 by Plan-1 (VT4-default boot) and Plan-v1 (input routing). All three block visual verification of the post-bug-#4-fix pipeline.

| Bug | Symptom | Strategy |
|---|---|---|
| #5 | `ipc-depth` dump at ep=84 q=30 (compositor → cluuterm FRAME_READY flood) | Compositor gates broadcast on actual damage since last broadcast |
| #2 | Ctrl-Alt-F1 fires VT switch in vtmgr but fb stays on compositor frame | Probe first (console_endpoint race vs fb-ownership), then fix the actual cause |
| #3 | Compositor status-bar clock static at boot value | Render placeholder + dirty-every-second until timeserver resolved, then real wallclock |

**Architecture:** No new IPC. Each fix is local to one userspace crate.

**Tech Stack:** Rust (compositor, console, vtmgr), CLUU harness.

**Parent direction:** `docs/ROADMAP.md` ("hobby OS, TUI now, GUI 2027+"); plans `2026-05-12-vtmgr-boot-vt-fix.md` and `2026-05-13-input-routing-vtmgr.md`. Investigator RCA: see this session's serial-log analysis (commit 83ed873).

---

## Task 1: bug #5 — FRAME_READY damage gate

Compositor today flushes the fb at 60 Hz and immediately broadcasts `COMP_FRAME_READY_LABEL` to every registered window's input endpoint, regardless of whether anything actually changed. cluuterm's recv loop blocks on `posix_spawn` of /bin/login at boot for ~0.5 s; during that window 30 FRAME_READY messages pile up at its endpoint (ep=84 q=30 in serial). After /bin/login spawn returns and cluuterm resumes, the catch-all `_ => {}` arm drains them, but only after the damage is done — IPC scheduler logs `ipc-depth heavy_eps=1` warnings, and any genuine downstream message (PTS_WRITE etc.) sits behind the backlog.

**Files:**
- Modify: `userspace/compositor/src/main.rs` (`broadcast_frame_ready`).
- Modify: `userspace/compositor/src/state.rs` (track `last_broadcast_gen` per window).

- [ ] **Step 1: Locate `broadcast_frame_ready`**

```bash
grep -n "broadcast_frame_ready\|COMP_FRAME_READY_LABEL\|fn broadcast" userspace/compositor/src/*.rs
```

- [ ] **Step 2: Add per-window "last frame_ready sent at frame N" field**

In `userspace/compositor/src/state.rs`, find the `Window` struct and add:

```rust
    /// Frame counter at which we last sent a FRAME_READY to this window's
    /// input endpoint. Used by broadcast_frame_ready to skip windows that
    /// haven't been re-damaged since.
    pub last_frame_ready_frame: u64,
```

Init to 0 in the constructor.

Also add a frame counter to `Compositor`:

```rust
    /// Monotonic frame counter, incremented on every successful flush.
    pub frame_counter: u64,
```

Init to 0.

Find where the `Window` is also damage-tracked. The investigator's RCA says compositor has a `cell_dirty: Vec<...>` and a `damage_for_window(win_id)` style. There must be a way to check whether a specific window's interior was touched since last frame. If not, fall back to "any cell changed at all since `last_frame_ready_frame`" — i.e., only broadcast to windows that were updated.

Simplest gating: bump `frame_counter` per flush. Compare each window's `last_frame_ready_frame` against the frame at which its SHM `generation` last advanced (read at compose time). Don't broadcast if equal.

If the codebase doesn't already track "last generation observed per window", introduce one field on `Window`:

```rust
    pub last_observed_generation: u32,
```

set during the compose pass to whatever generation the SHM header had.

- [ ] **Step 3: Rewrite `broadcast_frame_ready`**

Pseudocode:

```rust
fn broadcast_frame_ready(comp: &mut Compositor) {
    comp.frame_counter = comp.frame_counter.wrapping_add(1);
    for win in &mut comp.windows {
        if win.input_endpoint == 0 { continue; }
        // Skip if no genuine update since last FRAME_READY to this window.
        if win.last_frame_ready_frame == comp.frame_counter.wrapping_sub(1)
            && win.last_observed_generation == /* previous gen for this window */
        {
            continue;
        }
        let msg = Message::new(COMP_FRAME_READY_LABEL, [0; 6], 0);
        let _ = send(win.input_endpoint, &msg, IpcFlags::empty());
        win.last_frame_ready_frame = comp.frame_counter;
    }
}
```

Pick the actual gating signal that matches the existing compose pipeline. The goal: zero FRAME_READY sent during steady state when no window changed; one per window per real damage event.

Implementer judgment: if the existing pipeline already exposes per-window damage flags (e.g. `win.dirty: bool` cleared after blit), reuse them. The principle is "FRAME_READY only after real damage to that window".

- [ ] **Step 4: Build + harness**

```bash
cargo xtask build 2>&1 | tail -5
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_vt4_default bash scripts/harness_run.sh
grep -aE "ipc-depth: heavy_eps|FRAME_READY|broadcast" /tmp/cluu-serial-com2.log | head -20
```

Expected: zero `ipc-depth: heavy_eps=1` lines (or a sharp reduction). `compositor: window registered` + initial frame still fires. Steady state: silent.

- [ ] **Step 5: Commit**

```bash
git add userspace/compositor/src/main.rs userspace/compositor/src/state.rs
git commit -m "compositor: gate FRAME_READY broadcast on per-window damage"
```

---

## Task 2: bug #2 — investigate console-side activate path

Plan v1 visual smoke proved kbd → vtmgr → switch_vt fires correctly, but VT0 fb stays on the compositor's last frame. Investigator's race hypothesis (`console_endpoint == 0` at switch time) is unlikely because boot sends `CONSOLE_DEACTIVATE(0)` ~18 s before the user Ctrl-Alt-F1 at t=25 s — endpoint is set well before. Need a probe to disambiguate.

Two plausible real causes:

A. `CONSOLE_ACTIVATE_LABEL` IS sent, console DOES call `switch_vt(0) -> repaint_all + backend.flush()`, but compositor's fb mapping is still writeable and the next compose tick stomps the console output.

B. `CONSOLE_ACTIVATE_LABEL` IS sent but console's `switch_vt(0)` short-circuits (e.g. internal state thinks it's already on VT0 from boot's `CONSOLE_DEACTIVATE` having NOT actually unregistered the buffer).

This task is investigation-only; no code change yet. The actual fix lands in Task 3.

**Files:** none modified during this task.

- [ ] **Step 1: Add temporary probes (uncommitted)**

In `userspace/console/src/main.rs`, find the `CONSOLE_ACTIVATE_LABEL` arm:

```rust
            CONSOLE_ACTIVATE_LABEL => {
                let vt_index = msg.words[0];
                let _ = debug_print(&format!(
                    "console: activate vt={} (probe)", vt_index
                ));
                console.switch_vt(vt_index);
                let _ = debug_print(&format!(
                    "console: activate vt={} done (probe)", vt_index
                ));
            }
```

In `userspace/console/src/renderer.rs::switch_vt`, after `backend.flush()`, add:

```rust
        let _ = debug_print(&format!(
            "console: switch_vt {} -> repaint+flush done (probe)", vt_index
        ));
```

In `userspace/vtmgr/src/context.rs::switch_vt`, in the compositor→console branch, before `let _ = send(self.console_endpoint, &act, ...);` add:

```rust
        let _ = debug_print(&format!(
            "vtmgr: sending CONSOLE_ACTIVATE({}) ep={} (probe)",
            new_vt, self.console_endpoint
        ));
```

In compositor's VT_DEACTIVATE handler, after marking `active = false`, add:

```rust
        let _ = debug_print("compositor: VT deactivate done; fb writes stopped (probe)");
```

- [ ] **Step 2: Reproduce + capture**

```bash
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_vt4_default bash scripts/harness_run.sh \
  > /tmp/harness.out 2>&1 &
HARNESS_PID=$!
for _ in $(seq 1 60); do
  grep -q "compositor: ready" /tmp/cluu-serial-com2.log 2>/dev/null && break
  sleep 0.5
done
sleep 2
echo "sendkey ctrl-alt-f1" | socat - "UNIX-CONNECT:/tmp/cluu-qemu-monitor.sock"
sleep 2
wait "$HARNESS_PID" || true
grep -aE "vtmgr: sending CONSOLE_ACTIVATE|console: activate|console: switch_vt.*done|compositor: VT deactivate|vtmgr: vt switch" /tmp/cluu-serial-com2.log
```

- [ ] **Step 3: Classify**

Three possible outcomes:

| Pattern | Cause |
|---|---|
| No `vtmgr: sending CONSOLE_ACTIVATE` line | vtmgr's `console_endpoint == 0` — Task 3 fixes the boot subscription order. |
| `vtmgr: sending` fires but `console: activate vt=0` doesn't | Send failing (rights? endpoint stale?) — Task 3 audits the derived token. |
| Both fire AND `console: switch_vt ... done` — but fb still stale | A: compositor still has fb mapped + writing OR B: console's switch_vt is a no-op for "current" VT — Task 3 forces unconditional full repaint and/or makes compositor relinquish fb on deactivate. |

- [ ] **Step 4: Document the finding**

Edit Task 3 below to reflect the actual root cause from Step 3 (delete the two-cause alternative and keep only the one that matches the probe output). Don't commit any code changes from this task.

- [ ] **Step 5: Discard probes**

```bash
git checkout HEAD -- userspace/console/src/main.rs userspace/console/src/renderer.rs userspace/vtmgr/src/context.rs userspace/compositor/src/main.rs
```

(Or whichever files were edited above.)

---

## Task 3: bug #2 — actual fix

Cause selected in Task 2 Step 3. Fix accordingly.

### 3.A — if cause = `console_endpoint == 0`

vtmgr needs to delay any VT switch until `console_endpoint` is granted, OR have a deterministic startup order. The seL4-style fix: vtmgr refuses `VTMGR_REQUEST_VT_SWITCH` (and silently no-ops Ctrl-Alt-Fn-driven switches) until `console_endpoint != 0`.

```rust
// userspace/vtmgr/src/context.rs::switch_vt, very top:
    pub fn switch_vt(&mut self, new_vt: usize) {
        if new_vt >= VT_COUNT || new_vt == self.active_vt {
            return;
        }
        // Refuse to switch to a console-backed VT before console:0 grants
        // us its control endpoint. Switching now would silently drop
        // CONSOLE_ACTIVATE + CONSOLE_SWITCH_VT.
        let new_is_compositor = new_vt == self.compositor_vt;
        if !new_is_compositor && self.console_endpoint == 0 {
            let _ = debug_print("vtmgr: switch refused — console not ready");
            return;
        }
        // ... existing code
    }
```

### 3.B — if cause = console's `switch_vt` is no-op

Force unconditional repaint:

```rust
// userspace/console/src/renderer.rs::switch_vt
    pub fn switch_vt(&mut self, vt_index: usize) {
        // Allow re-activation of the current VT after the user navigated
        // away (compositor took over fb during VT4-default boot). Always
        // mark all cells dirty + reset cursor + repaint.
        self.active_vt = vt_index;
        self.mark_all_dirty();   // or whatever forces a full re-blit
        self.repaint_all();
        let _ = self.backend.flush();
    }
```

Remove any "already on this VT, skip" early-return that exists.

### 3.C — if cause = compositor still writing fb after deactivate

Compositor's `handle_vt_deactivate` must stop the compose pipeline:

```rust
// userspace/compositor/src/main.rs::handle_vt_deactivate
    comp.active = false;
    comp.deadlines.next_frame_ms = u64::MAX; // park the frame ticker
```

Plus check `comp.active` gate at every `tick_frame` / `flush` entry.

**Files:** narrowed in Task 2 Step 4.

- [ ] **Step 1: Apply the chosen fix.**
- [ ] **Step 2: Build + harness regression.**
- [ ] **Step 3: Visual smoke (same workflow as Plan-v1 Task 9):**

```bash
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_vt4_default bash scripts/harness_run.sh \
  > /tmp/harness.out 2>&1 &
HARNESS_PID=$!
for _ in $(seq 1 60); do
  grep -q "compositor: ready" /tmp/cluu-serial-com2.log 2>/dev/null && break
  sleep 0.5
done
sleep 2
FB_PHYS=$(grep -oE 'fb @[0-9A-Fa-f]+' /tmp/cluu-serial-com2.log | head -1 | sed 's/fb @/0x/')
echo "sendkey ctrl-alt-f1" | socat - "UNIX-CONNECT:/tmp/cluu-qemu-monitor.sock"
sleep 2
bash scripts/fb_dump.sh -p "$FB_PHYS" -o /tmp/vthard-vt0
wait "$HARNESS_PID" || true
```

Validate `/tmp/vthard-vt0.png` is visually different from a VT4 dump — unique colors should be >> 4, entropy > 0.05.

- [ ] **Step 4: Commit**

```bash
git add userspace/...
git commit -m "console/vtmgr/compositor: fix VT-activate visibility (Task 2 RCA pattern <A|B|C>)"
```

---

## Task 4: bug #3 — compositor clock

Today: clock string never updates because `clock_now_ms` returns 0 when `timeserver:main` isn't subscribed, and the dirty-comparison `now_secs != clock_seconds` is `0 != 0 == false`.

Two compatible fixes:

1. Compositor subscribes to `timeserver:main` properly at boot (request_subscription + grant handler), caches the endpoint. Once set, `clock_now_ms` works.
2. Until timeserver is subscribed, render the clock as `--:--:--` and mark row 0 dirty every second so the string at least re-blits (matches "system is alive" feedback). Once timeserver lands, switch to real time.

Pick (1)+(2) together: clock works whenever it can, shows `--:--:--` when it can't, both modes update the dirty flag every second.

**Files:**
- Modify: `userspace/compositor/src/main.rs` (subscribe to timeserver).
- Modify: `userspace/compositor/src/render.rs` (`tick_clock` always dirties row 0 every second).
- Modify: `userspace/compositor/src/state.rs` (`time_ep: usize` field on Compositor if not already).

- [ ] **Step 1: Find current timeserver wiring**

```bash
grep -n "timeserver\|clock_now_ms\|tick_clock\|time_ep" userspace/compositor/src/*.rs
```

- [ ] **Step 2: Subscribe to timeserver in compositor startup**

If `time_ep` is currently looked up on-demand, replace with a subscription pattern matching the other services (`compositor:control` etc.).

```rust
        if !requested_timeserver && time_ep == 0 {
            if registry::request_subscription("timeserver", "main").is_ok() {
                requested_timeserver = true;
            }
        }
```

In the grant handler:

```rust
                    if service_name == "timeserver" && name == "main" {
                        time_ep = token;
                        let _ = debug_print("compositor: timeserver subscribed");
                    }
```

- [ ] **Step 3: Rewrite `tick_clock` to always step every second**

```rust
    fn tick_clock(&mut self, now_ms: u64) {
        if now_ms < self.deadlines.next_clock_ms { return; }
        self.deadlines.next_clock_ms = now_ms + 1000;
        // Always mark row 0 dirty so the clock cells re-blit even if
        // the time string didn't change (e.g. timeserver still 0).
        self.dirty_row(0);
        if now_ms == 0 {
            // timeserver unresolved — placeholder.
            self.clock_str = ArrayString::from("--:--:--").unwrap();
        } else {
            let secs = (now_ms / 1000) as u32;
            self.clock_str = format_hhmmss(secs);
        }
    }
```

(Use whatever existing string-buffer + dirty-row APIs the codebase has.)

- [ ] **Step 4: Build + harness**

```bash
cargo xtask build 2>&1 | tail -5
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_vt4_default bash scripts/harness_run.sh
grep -aE "compositor: timeserver subscribed|compositor: clock" /tmp/cluu-serial-com2.log
```

- [ ] **Step 5: Visual smoke — two PNGs 2 s apart, clock should differ**

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
bash scripts/fb_dump.sh -p "$FB_PHYS" -o /tmp/clock-t0
sleep 3
bash scripts/fb_dump.sh -p "$FB_PHYS" -o /tmp/clock-t3
wait "$HARNESS_PID" || true
# Diff the row-0 region (top 16 pixel rows * 1280 * 4 bytes BGRA = 81920 bytes)
cmp /tmp/clock-t0.bin /tmp/clock-t3.bin | head -5
diff <(xxd -l 81920 /tmp/clock-t0.bin) <(xxd -l 81920 /tmp/clock-t3.bin) | head -20
```

PASS if the first 81920 bytes differ between the two dumps. FAIL if identical.

- [ ] **Step 6: Commit**

```bash
git add userspace/compositor/src/main.rs userspace/compositor/src/render.rs userspace/compositor/src/state.rs
git commit -m "compositor: subscribe to timeserver + tick status-bar clock every second"
```

---

## Task 5: spec status + memory update

- [ ] **Step 1: Update spec**

`docs/superpowers/specs/2026-05-12-login-flow-design.md` §4.6 — append:

```
**VT-switch hardening**: bugs #2 (console repaint), #3 (compositor clock), #5 (FRAME_READY backpressure) fixed in plan `2026-05-13-vt-hardening.md`. Visual smoke confirms VT4 ↔ VT0 round-trip renders correctly.
```

- [ ] **Step 2: Memory update**

In `/home/vlb2bp/.claude/projects/-home-vlb2bp-git-cluu/memory/`, write a small project memory `project_vt_hardening_2026_05_13.md` summarising:
- Which 3 bugs were latent.
- Why they only became visible after Plan-1 (VT4-default) + Plan-v1 (router) landed.
- The pattern decision per Task 3 (A/B/C).
Index in MEMORY.md.

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/2026-05-12-login-flow-design.md
git commit -m "docs/spec: vt-hardening plan results cross-linked in §4.6"
```

---

## Self-review notes

- Files touched: compositor, console, vtmgr. Plus one spec edit. No libcluu, no kernel.
- Tasks 1 (#5) and 4 (#3) are independent and could land in either order. Task 2 (RCA) gates Task 3 (#2 fix).
- After all four tasks: re-run Plan #2 Task 5 visual smoke + Plan v1 Task 9 visual smoke; both should now succeed.
- All commits on `develop`. No force-push, no `--no-verify`, no `--amend`.
