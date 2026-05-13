# Compositor Clock → Push-Mode Migration

> **For agentic workers:** Steps use checkbox (`- [ ]`) syntax.

**Goal:** Compositor stops polling timeserver every loop iteration. Subscribes once at startup with period_ms=1000, blocks on recv (no timeout), wakes on TIME_TICK to update the clock. Result: status-bar clock ticks at true 1 Hz; per-iteration IPC pressure to timeserver goes to zero.

**Architecture:** Per `docs/superpowers/specs/2026-05-13-timeserver-pushmode-design.md` §6 (compositor subscriber pattern). Push-mode Phase 1 (T1-T5) shipped.

**Files:**
- Modify: `userspace/compositor/src/main.rs`.
- Modify: `userspace/compositor/src/state.rs` if `time_ep` / `clock_seconds` move there.

---

## Task 1: subscribe at startup

In compositor's `main()`, after `registry::request_subscription("timeserver", "main")`, ALSO send a TIME_SUBSCRIBE_PERIODIC. The notify_ep is the compositor's main IPC endpoint (`comp.input_endpoint_global` or whichever endpoint the main loop already recvs on).

- [ ] **Step 1: Add subscribe call**

After `compositor: timeserver subscribed` debug-print (where `time_ep` becomes non-zero), submit:

```rust
            if time_ep != 0 && !pushmode_armed {
                let sub = Message::new(
                    libcluu::time::TIME_SUBSCRIBE_PERIODIC_LABEL,
                    [1000 /* period_ms */, comp_recv_ep /* notify_ep */,
                     0 /* tid stub: kernel-auth */, 0, 0, 0],
                    3,
                );
                let mut reply = Message::new(0, [0; 6], 0);
                if libcluu::ipc::call(time_ep, &sub, &mut reply).is_ok()
                    && reply.words[0] == 0 {
                    pushmode_armed = true;
                    let _ = debug_print("compositor: subscribed to timeserver pushmode 1000ms");
                }
            }
```

`comp_recv_ep` = the endpoint the recv loop already listens on (probably `tokens[0]` or `tokens[1]` — the same endpoint used in the message-receive loop). Use whichever is appropriate.

`pushmode_armed: bool` is a new local in `main()` initialised false.

- [ ] **Step 2: Handle TIME_TICK in recv loop**

Add a label match arm. Probably in the `match msg.tag.label` for the main endpoint:

```rust
        libcluu::time::TIME_TICK_LABEL => {
            // words[0] = tick_count, words[1] = now_monotonic_ms
            let now_ms = msg.words[1] as u64;
            comp.tick_clock(now_ms, now_ms / 1000);
            // dirty status row + schedule_frame is what tick_clock does.
            if comp.prev_cell_grid != comp.cell_grid {
                comp.schedule_frame(now_ms);
            }
        }
```

- [ ] **Step 3: Stop polling clock every iteration**

In the loop top, REMOVE the per-iteration `clock_now_ms` call OR keep it but cache. Replace:

```rust
        let now_ms = clock_now_ms(&mut time_ep);
        let now_secs = now_ms / 1000;
```

with:

```rust
        let now_ms = comp.last_clock_now_ms;
```

(Add `last_clock_now_ms: u64` to Compositor struct; update inside the TIME_TICK arm.)

This eliminates per-iteration IPC roundtrip to timeserver — main goal.

The existing `tick_clock(now_ms, now_secs)` call AFTER `match` block becomes dead (clock fires only on TIME_TICK now). Remove it.

The `next_clock_ms`-based timeout calc is also dead. Compositor's `next_timeout_ms` now only considers `next_frame_ms` (or u64::MAX if no frame scheduled).

- [ ] **Step 4: Build + harness**

```bash
cargo xtask build 2>&1 | tail -5
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_vt4_default bash scripts/harness_run.sh
grep -aE "compositor: subscribed to timeserver pushmode|timeserver: subscribe tid=|TIME_TICK" /tmp/cluu-serial-com2.log
```

Expected:
- `compositor: subscribed to timeserver pushmode 1000ms` ✓
- `timeserver: subscribe tid=N period=1000ms ep=X errno=0` ✓
- Status-bar clock advances every real second (manual visual via cargo xtask qemu).

- [ ] **Step 5: Commit**

```bash
git add userspace/compositor/src/main.rs userspace/compositor/src/state.rs
git commit -m "compositor: migrate clock to timeserver push-mode (1Hz tick)"
```
