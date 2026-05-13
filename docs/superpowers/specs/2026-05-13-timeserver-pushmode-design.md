# Timeserver Push-Mode — Spec

**Date:** 2026-05-13
**Status:** Draft, pre-plan.
**Owners:** kernel-team
**Related:** `docs/ARCHITECTURE.md` §5 (services map), `docs/superpowers/specs/2026-05-10-tui-compositor-design.md` (status-bar clock), `docs/superpowers/specs/2026-05-12-login-flow-design.md` (modal/focus, eventual cursor blink).

## 1. Goal

Add a periodic-tick push API to `timeserver` so subscribers wake naturally on every tick instead of polling-with-timeout. Eliminate the recv-timeout-as-clock pattern across the userspace (compositor status bar, future cluuterm cursor blink, future console animations, future status pollers). All consumers become true event-driven loops with no busy-wait and no cumulative drift.

## 2. Non-goals

- Sub-millisecond precision. Granularity 10 ms — anything finer needs a dedicated HPET/TSC scheduler.
- Per-CPU timers. Single timeserver for the whole system.
- Wall-clock notifications (RTC events). Out of scope; this is monotonic-tick only.
- Calendar callbacks (cron). Use the existing `CronCreate`-style pattern at the application layer.

## 3. Constraints

- Kernel freeze active through ~2026-10-21. This is **userspace-only** (timeserver + libcluu).
- Cap model is monotone-decreasing: subscribers grant timeserver a SEND-only token to their notify endpoint. Timeserver never gets broader rights than the subscriber granted.
- Timeserver must survive subscriber death without leaking entries (revoke on failed send N times).
- No new syscalls.

## 4. Current state

`userspace/timeserver/src/main.rs` accepts two labels:

- `TIME_GETCLOCK` — pull monotonic time.
- `TIME_GETTIMEOFDAY` — pull wallclock.

Both are pure RPC; timeserver has no per-client state. Loop is a simple recv-reply.

## 5. New API

### 5.1 IPC labels (libcluu/src/ipc.rs)

```rust
// Subscribe to periodic ticks. Words: [period_ms: u32, notify_ep: u64].
// Reply words[0]: errno (0 ok, EINVAL if period_ms == 0 or > 60_000).
// Subscriber is identified by sender_tid; timeserver dedupes per tid.
pub const TIME_SUBSCRIBE_PERIODIC_LABEL: u32 = 120;

// Unsubscribe (any pending TIME_TICK already enqueued may still arrive).
// Words: [] (timeserver matches on sender_tid).
pub const TIME_UNSUBSCRIBE_LABEL: u32 = 121;

// Push: timeserver → subscriber.  Words: [tick_count_since_subscribe: u64,
// now_monotonic_ms: u64]. Fire-and-forget; subscriber MUST NOT reply.
pub const TIME_TICK_LABEL: u32 = 122;
```

### 5.2 Subscription semantics

- One subscription per (subscriber tid, period_ms) tuple. Re-subscribe with a different period replaces the previous slot for that tid.
- `period_ms` rounded up to the nearest 10 ms (timeserver's tick granularity).
- Maximum periods per timeserver instance: 64 subscribers (configurable).
- Tick deadlines are anchored from the subscribe time + N×period, NOT from "last delivery time". No cumulative drift.
- If a subscriber's recv is slow, ticks DO NOT pile up — timeserver coalesces (delivers at most one tick per subscriber per its internal loop iteration). The tick payload's `tick_count_since_subscribe` lets the subscriber detect missed ticks.
- Subscriber's notify endpoint must be a SEND-rights token derived from the subscriber's recv endpoint. Timeserver stores the token; on subscriber death (token revocation) the next send fails, timeserver removes the entry after 3 consecutive failed sends.

### 5.3 Timeserver internal loop

Today: blocking recv → handle → reply → loop. Each iteration is one RPC.

Push-mode:
1. Compute `next_deadline = min(subscribers.deadline)`.
2. `timeout = next_deadline.saturating_sub(now_ms())`.
3. `recv_any_with_timeout(timeout)`.
4. On message:
   - `TIME_GETCLOCK`/`TIME_GETTIMEOFDAY` → reply (existing path).
   - `TIME_SUBSCRIBE_PERIODIC` → validate, insert/update entry, reply errno.
   - `TIME_UNSUBSCRIBE` → remove entry by sender_tid, reply errno.
5. On timeout (no message in window): walk subscribers, fire ticks for any whose deadline ≤ now, advance their deadlines by period.

Each tick send uses non-blocking `IpcFlags::NON_BLOCKING` (define if not present) — if subscriber's queue is full, skip this tick; subscriber will see the gap via `tick_count_since_subscribe`.

Failure counter: per subscriber, `consecutive_send_fails`. Reset on success. Remove subscriber when count reaches 3.

### 5.4 Tick payload semantics

`words[0] = tick_count_since_subscribe`: monotonic counter, 1 on first tick. Subscriber uses this to detect missed ticks (e.g., expected count == 5 but got 7 → 2 missed).

`words[1] = now_monotonic_ms`: same value timeserver's `TIME_GETCLOCK` would return at send time. Saves subscriber an extra RPC.

## 6. Subscriber pattern (compositor example)

Today:
```rust
loop {
    let now_ms = clock_now_ms(&mut time_ep);   // pull
    let timeout = comp.deadlines.next_timeout_ms(now_ms);
    let m = ipc::recv_with_timeout(..., timeout);
    match m {
        Ok(msg) => handle(msg),
        Err(Timeout) => { tick_clock(now_ms); ... },
    }
}
```

Push-mode:
```rust
// Once at startup:
let sub_msg = Message::new(TIME_SUBSCRIBE_PERIODIC_LABEL,
    [1000, my_notify_ep, 0, 0, 0, 0], 2);
let _ = ipc::call(timeserver_ep, &sub_msg, ...);

loop {
    let (msg, _) = ipc::recv_any(...);  // NO timeout
    match msg.label {
        TIME_TICK_LABEL => {
            let tick_count = msg.words[0];
            let now_ms     = msg.words[1];
            comp.tick_clock(now_ms, now_ms / 1000);
        }
        // ... other labels
    }
}
```

Cluuterm cursor blink (future, 500 ms period):
```rust
let sub = Message::new(TIME_SUBSCRIBE_PERIODIC_LABEL, [500, my_ep, ...], 2);
ipc::call(time_ep, &sub, ...);
loop {
    let m = ipc::recv_any(...);
    match m.label {
        TIME_TICK_LABEL => term.toggle_cursor_visible(),
        ...
    }
}
```

Multiple periods per subscriber (e.g., compositor needs 1000 ms clock AND 16 ms frame ticker): submit two subscriptions OR pick the smaller period and divide inside the consumer. v1 supports only one period per tid — if a consumer needs two, it spawns a child thread for the second.

## 7. Cap flow

```
subscriber                            timeserver
    │ register_output("notify", ep)
    │ token_derive(ep, SEND_only) → T_sub
    │
    │── TIME_SUBSCRIBE_PERIODIC(period, T_sub) ──▶
    │                                                stores (sender_tid, period, T_sub)
    │                                                anchors next_deadline = now + period
    │ ◀──────── reply errno=0 ──────────────────────
    │
    │ ... ticks fire via T_sub at period rate ...
    │ ◀────────── TIME_TICK(count, now) ─────────────
    │ ◀────────── TIME_TICK(count, now) ─────────────
    │ ...
    │
    │── TIME_UNSUBSCRIBE ──▶
    │                                                removes by sender_tid
    │ ◀────────── reply errno=0 ──────────────────────
```

T_sub is SEND-only — timeserver cannot recv on subscriber's behalf. Subscriber dies → kernel revokes T_sub → timeserver's send fails → entry removed after 3 fails.

## 8. Backwards compat

`TIME_GETCLOCK` and `TIME_GETTIMEOFDAY` remain unchanged. Existing callers (libcluu::time::query_endpoint) untouched. Push-mode is purely additive.

## 9. Test plan

L1 (unit):
- Subscribe with period_ms=0 → EINVAL.
- Subscribe with period_ms>60_000 → EINVAL.
- Subscribe twice from same tid → second replaces first.
- Unsubscribe nonexistent tid → errno (ENOENT).
- Anchor calculation: next deadline = subscribe_time + N×period (verify N=1..5).

L2 (harness):
- `l2_timeserver_pushmode_tick`: subscribe with 100 ms period, count 10 TICK messages received in ≤ 1.2 s. Asserts tick_count increments 1..10 and now_monotonic_ms advances.
- `l2_timeserver_pushmode_revoke`: subscribe, kill subscriber, expect timeserver to log `consecutive failure ... removed` within 3 ticks.
- `l2_compositor_clock_pushmode` (migration test): after compositor migrates, dump fb 3 s apart; row 0 (clock) must differ.

## 10. Implementation phases

Each is its own plan:

1. **`2026-05-13-timeserver-pushmode.md`**: implement labels + timeserver state + tests. Compositor still polls. Land + green harness.
2. **`2026-05-13-compositor-clock-pushmode.md`**: migrate compositor to push-mode. Remove `clock_now_ms` from per-iter top. `clock_now_ms` becomes pull-only at startup or as fallback.
3. **`(future) 2026-05-XX-cluuterm-cursor-blink.md`**: cluuterm subscribes 500 ms tick, toggles cursor cell. Independent.

This spec covers phase 1 + phase 2.

## 11. Open questions

- Should TICK payload include the subscriber's subscription handle (in case one tid has multiple subs in the future)? v1: no — one sub per tid. Future API extension.
- Adaptive batching: if subscriber's queue is full, should timeserver pause the period until subscriber catches up? v1: no, just drop. Simpler.
- Negative drift correction: if timeserver itself wakes late (>2× period), should it fire one tick now then re-anchor? v1: fire one tick, advance deadline by N×period where N = floor((now - deadline) / period) + 1. Stays anchored.

## 12. References

- Wayland: `wl_callback` (frame timing) + linux-dmabuf timed surfaces.
- QNX: pulses + timer_create with SIGEV_PULSE.
- seL4-derived: Nitpicker's timer client.
- macOS: CFRunLoopTimerRef → user-process notification.
