# CLUU Network Stack — Fix Plan

**Date:** 2026-07-11
**Source:** `docs/NET_STACK_ANALYSIS_Fable-5.md` (Claude Fable-5 deep analysis)
**Status:** Plan assembled, awaiting user approval before implementation

## Summary

Claude Fable-5 found 3 CRITICAL, 4 HIGH, 6 MEDIUM, 10 LOW issues. Architecture is sound — issues are in robustness, not design. Verdict: "close to shippable, Tier 0 fixes are ~1 day."

Codex gpt-5.5 attempted 3 times, disconnected each time before writing a report. Only Claude's analysis available.

## Tier 0 — Correctness (do before any perf work, ~1 day)

### C1: TCP recv drops buffered data when FIN arrives in same poll batch
- **File:** `userspace/netd/src/main.rs:237-247` (pending_recv drain, TCP arm)
- **Root cause:** Deferred TCP recv checks state FIRST — if CloseWait/Closed, replies EOF=0 without draining rx buffer. smoltcp keeps data readable in CloseWait. Immediate path at :824-848 has correct order (drain, then check state). Two paths disagree.
- **Fix:** In pending TCP arm, call `recv_slice` first; only reply EOF when it yields 0 bytes AND `!sock.may_recv()`.
- **Test:** Add `l2_http_close` probe that reads a body delivered with FIN in one batch.

### C2: recv() silently corrupts data at 4 KiB reply boundary
- **File:** `userspace/libcluu/src/posix/socket.rs:222-254`
- **Root cause:** Client reply buffer sized `4096.min(size_of::<Message>() + len)` — 4096 cap is inclusive of Message header. netd replies with up to 4096 payload bytes. Mismatch causes BufferTooSmall or stale-garbage tail.
- **Fix:** Size reply buffer `size_of::<Message>() + len`; clamp netd's per-reply payload to `reply_capacity - header`; never return count > bytes copied.
- **Test:** Add >4 KiB transfer probe.

### H3: Concurrent DNS resolve drops first requester's reply token
- **File:** `userspace/netd/src/main.rs:161,923-948`
- **Root cause:** `pending_dns` is single `Option`, DNS socket has 1 query slot. Second resolve overwrites first client's token → permanent hang.
- **Fix:** Make `pending_dns` a `Vec`, size DNS socket query slots to match, cancel+error on eviction.

## Tier 1 — Deferred-reply migration (~2-3 days)

### M1: Synchronous driver calls inside connect/accept block whole server
- **File:** `userspace/netd/src/main.rs:453-468` (drain_rx_tx uses blocking call_with_payload)
- **Root cause:** connect/accept spin loops make blocking IPC calls → netd fully blocked, no other client served. Violates AGENTS.md §7 (async runtime canonical).
- **Fix:** Land deferred connect/accept/recv plumbing from prior audit. Delete synchronous spin loops + inject_loopback_arp call sites.

## Tier 2 — Driver robustness (~1 day)

### H2: Missed interrupt permanently degrades NIC to 50ms polling
- **File:** `userspace/virtio-net/src/main.rs:241-254`
- **Root cause:** ISR status register read only in IRQ branch. Missed IRQ → INTx stays asserted → no further edges → falls to 50ms polling forever.
- **Fix:** Read `isr_status()` unconditionally before every drain (1 line).

### H4: TX completions reclaimed only on interrupt, bursts get EBUSY
- **File:** `userspace/virtio-net/src/main.rs:341,377-390`
- **Root cause:** NET_PKT_SEND never reclaims completed TX descriptors. 8 TX buffers exhausted → Busy → frame lost.
- **Fix:** Call `drain_tx()` at top of NET_PKT_SEND arm.

### C3: Device-controlled used.id can panic driver or corrupt free-list
- **File:** `userspace/virtio-core/src/virtqueue.rs:266-277`
- **Root cause:** `pop_used` indexes `cookies[head]` with device-supplied id, no bounds check. id >= 64 → OOB panic. Duplicate id → double-free descriptor.
- **Fix:** Bounds-check `head`, reject/ignore ids that are out of range, already free, or have None cookie.

### M4: Plain access to DMA-shared virtqueue rings
- **File:** `userspace/virtio-core/src/virtqueue.rs:232-263,282-288`
- **Root cause:** Ring fields read/written with ordinary loads/stores. Correct on x86 today, UB-class for smarter LLVM or non-x86.
- **Fix:** `read_volatile`/`write_volatile` on idx/flags/ring slots, Acquire fence after used.idx read.

## Tier 3 — Capability decision (needs user call)

### H1: NET capability bypass via registry
- **File:** `userspace/registry/src/main.rs:225-300`, `libcluu/src/registry.rs:431-451`, `netd/src/main.rs:91-93`
- **Root cause:** netd registers under public name `netd:main`. Registry SUBSCRIBE→GRANT performs zero capability inspection. Any binary with TOKEN_REGISTRY (granted to VFS/REGISTRY/SPAWN profiles) can obtain netd endpoint.
- **Options:**
  - (A) Remove `netd:main` public registration, deliver endpoint only via spawn envelope TOKEN_EXTRA_0 (structural fix, recommended)
  - (B) Document NET bit as advisory
- **Recommendation:** Option A — structural fix, matches philosophy §2/§3.

## Tier 4 — Hygiene (deferrable to v1.1)

- L2: ping never closes ICMP socket (leak)
- L3: ping single-shot hang on loss (block-forever, not spin)
- L4: wget/curl no HTTP parsing (404 → WGET_OK, no Content-Length)
- L5: curl `-o FILE` parsed but ignored
- L7: per-frame debug_print in datapath
- L8: virtio feature negotiation ordering
- M2/M3: bound netd rx_queue, add drop counters
- M5: modern-PCI capability parsing assumes single BAR
- M6: NET_REGISTER_RECV accepts raw token in data word

## Recommended Gate

**Ship after Tier 0 + Tier 1 + H2/H4 + H1 decision.** Add probes for HTTP-close and >4 KiB paths. Remaining MEDIUM/LOW are QEMU-invisible or edge-only, legitimately deferrable.

## Implementation Order

1. C1 (5 lines, netd main.rs pending TCP arm reorder)
2. C2 (socket.rs buffer sizing + netd reply clamp)
3. H3 (pending_dns Vec + DNS socket slots)
4. H2 (1 line, ISR read in virtio-net)
5. H4 (drain_tx in NET_PKT_SEND)
6. C3 (bounds check in virtqueue pop_used)
7. M1 (deferred connect/accept — largest change, depends on async runtime)
8. H1 (registry decision + netd registration removal)
