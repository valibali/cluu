# Kernel Audit

CLUU's kernel has been audited five times. The current state is v5
(February 2026): overall **9.4/10 (A)**, with no critical, high, or medium
severity findings open. IPC is at ~950 to 1,250 cycles for a full
call/reply, within striking distance of seL4.

This chapter distills the kernel audit findings for the book. Finding IDs
(C-1 through C-8) are preserved so cross-references in code comments and
other chapters stay valid.

## Scope and limits

- **Target:** x86_64 single-CPU microkernel, seL4-inspired, with a POSIX
  compatibility layer in userspace.
- **Codebase at v5:** ~22.3K LOC (61 Rust files plus x86_64 assembly),
  1,009 LOC of assembly.
- **Known limitations:** single CPU only, x86_64 only, no IOMMU, PIC-only
  interrupts (no APIC).

## Audit history

| Version | Date | Overall | Correctness | Speed | Efficiency | Key change |
|---|---|---|---|---|---|---|
| v1 | Feb 2026 | 7.0 | 8.5 | 5.5 | 7.5 | Initial audit |
| v2 | Feb 2026 | 7.75 | 8.75 | 6.5 | 8.0 | Token cache, heap, IrqAck, debug gating |
| v3 | Feb 2026 | 8.0 | 9.25 | 6.5 | 8.5 | SHA-256, ObjectRef cache, resource limits, endpoint cleanup |
| v4 | Feb 2026 | 9.1 | 9.65 | 8.5 | 9.2 | Implicit reply caps, dead thread reaping, HMAC skip, ObjectRef passthrough |
| **v5** | **Feb 2026** | **9.4** | **9.6** | **9.2** | **9.4** | O(1) ReplyMap, per-CPU token cache, register IPC fast path |

## v5 ratings

| Category | Score | Grade | v4 | Summary |
|---|---|---|---|---|
| Correctness | 9.6/10 | A+ | 9.65 | O(1) ReplyMap verified correct. Per-CPU cache safe on single-CPU. Minor `UnsafeCell` risk documented. |
| Speed | 9.2/10 | A | 8.5 | All 3 v4 bottlenecks eliminated. Full RPC ~950 to 1,250 cycles. Register fast path for small messages. |
| Efficiency | 9.4/10 | A+ | 9.2 | Thread struct ~800B smaller. Static ReplyMap replaces dynamic BTreeMap. Per-CPU cache saves 3.1MB at max threads. |
| **Overall** | **9.4/10** | **A** | **9.1** | Sub-1,250 cycle full RPC achieved. Approaching seL4 parity. All v4 "Path to Sub-1,000" items done. |

**Verdict:** v5 completes the three optimizations identified in v4's
"Path to Sub-1,000 Cycle Full RPC" section. Full call/reply drops from
~1,195 to 1,625 cycles (v4) to **~950 to 1,250 cycles**, an additional
1.3x improvement and now within striking distance of seL4 (~850 to 1,000).
The O(1) ReplyMap eliminates BTreeMap allocation overhead, the per-CPU
token cache removes the `THREAD_REPOSITORY` lock from the hot path, and
the register IPC fast path skips page table walks for small messages
(at most 16 bytes). Correctness holds steady; one new INFO-level finding
(`UnsafeCell` safety invariant) is documented but safe on single-CPU.

## What changed in v5

| Item | Category | Status | Impact |
|---|---|---|---|
| O(1) ReplyMap (open-addressing hash table) | Speed (Priority 1) | Done | -100 to 200 cycles per RPC (BTreeMap to O(1)) |
| Per-CPU token cache (`UnsafeCell`, lock-free) | Speed (Priority 3) | Done | -45 to 90 cycles per cache hit (no `THREAD_REPOSITORY` lock) |
| Register IPC fast path for `sys_call` | Speed (Priority 4) | Done | -60 to 100 cycles for at-most-16-byte messages (no page table walk) |

## Correctness (9.6/10)

### O(1) ReplyMap (verified correct)

| Check | Status | Evidence |
|---|---|---|
| Linear probing terminates | PASS | Loop bounded by `REPLY_MAP_SLOTS` (256). `None` sentinel stops search. |
| Backward-shift deletion | PASS | Independent scan variable `j` advances unconditionally. Wrap-around `should_move` logic handles both `j >= empty` and `j < empty` cases correctly. |
| 50% load cap | PASS | `insert()` rejects when `count >= REPLY_MAP_SLOTS / 2` (128). Returns false. |
| Duplicate key rejection | PASS | `insert()` returns false if existing key found during probe. |
| Dead thread cleanup | PASS | Collects `to_remove` Vec, then removes one-by-one. Semantically equivalent to old `retain()` but works with open-addressing (cannot remove during iteration without breaking probe chains). |
| Callers woken outside lock | PASS | `drop(map)` before `wake_thread` calls (unchanged from v4). |
| `get`/`get_mut` correctness | PASS | `get_mut` uses two-phase: find index, then return mutable ref. Avoids borrow conflict. |
| `FAULT_REPLY_MAP` cleanup | PASS | Same pattern as `CALL_REPLY_MAP`. |
| `set_call_reply_info` returns bool | PASS | All callers check return and handle `Error::Overflow` or kill thread. |

### Per-CPU TokenCache (verified safe on single-CPU)

| Check | Status | Evidence |
|---|---|---|
| Non-reentrant invariant | PASS | Timer interrupt calls `schedule_and_switch` (no `lookup_token`). GPF/PF fault handlers call `try_forward_fault` which does NOT call `lookup_token`. Page fault demand pager never calls `lookup_token`. |
| LRU promotion | PASS | On hit at position `i > 0`: shift `lru_order[0..i]` right by 1, insert hit slot at `[0]`. Standard LRU. |
| LRU eviction | PASS | Evicts `lru_order[TOKEN_CACHE_SIZE-1]` (least recently used). Promotes to MRU after insert. |
| Generation invalidation | PASS | Global `revocation_generation()` mismatch triggers `invalidate_all()`. All 4 entries cleared. |
| Expiration check | PASS | Per-entry expiration checked on cache hit; stale entry cleared via `clear_handle()`. |
| No data race on single CPU | PASS | `UnsafeCell` access only from syscall context. Interrupts never touch it. No SMP. |

### Register IPC fast path (verified correct)

| Check | Status | Evidence |
|---|---|---|
| Buffer size = 56 bytes | PASS | `USER_MESSAGE_SIZE` const with `assert!(USER_MESSAGE_SIZE == 56)`. |
| `payload_len = buffer.len()` (56) | PASS | `let payload_len = buffer.len()`, NOT `inline_len`. Ensures `reply_id` at offset 48 is delivered. |
| Rollback on send failure | PASS | `take_call_reply_info(reply_id)` removes orphaned entry on `call_from_kernel_with_reply_id` error. |
| Overflow handling | PASS | `set_call_reply_info` returns false, surfaces `Error::Overflow` to userspace. |
| Size validation | PASS | `inline_len > IPC_REG_INLINE_MAX_CALL_PAYLOAD` (16) rejected. |
| Feature gate check | PASS | `register_fast_enabled()` checked before fast path entry. |
| Byte packing from registers | PASS | arg2 to `buffer[0..8]`, arg6 to `buffer[8..16]`, bounded by `inline_len`. |

### Fault reply map full (verified correct)

| Check | Status | Evidence |
|---|---|---|
| Reply map full kills thread | PASS | `mark_thread_dead(current_id)` plus return false on `set_fault_reply_info` failure. |
| Correct for unrecoverable situation | PASS | If a fault reply cannot be tracked, there is no way to resume the thread. |

### Previously verified (unchanged from v4)

- Implicit reply cap security (server_thread_id binding): PASS.
- Userspace cannot forge `reply_id`: PASS.
- `WouldBlock` rollback in `sys_call` slow path: PASS.
- `ObjectRef` passthrough to all 34 invoke handlers: PASS.
- SHA-256 FIPS 180-4: PASS.
- HMAC-SHA256 RFC 2104 (creation only): PASS.
- CAS resource counters: PASS.
- Return-to-userspace paths (10): ALL PASS.

### Findings

Finding IDs are stable across audit versions. Fixed findings retain their
ID with a `FIXED` status so historical references stay valid.

| ID | Severity | Location | Description | Status |
|---|---|---|---|---|
| C-1 | ~~LOW~~ | ~~`handlers.rs`~~ | ~~`sys_invoke` discards `ObjectRef`~~ | **FIXED in v4** |
| C-2 | ~~LOW~~ | ~~`endpoint.rs`~~ | ~~`EndpointRepository` bypasses count~~ | **FIXED in v4** |
| C-3 | INFO | `table.rs` | TOCTOU window in zero-reference check. Safe on single-CPU; the check is idempotent. | Unchanged |
| C-4 | ~~INFO~~ | ~~`table.rs`~~ | ~~Shard lock on cache hit~~ | **FIXED in v4** |
| C-5 | INFO | `endpoint.rs` | Direct delivery reply binding race. Mitigated by strict `server_thread_id` check. | Unchanged |
| C-6 | INFO | `endpoint.rs` | Queued message delayed binding. Mitigated by `None != attacker_tid` check. | Unchanged |
| C-7 | INFO | `table.rs` | Per-CPU cache uses `UnsafeCell`. Safe on single-CPU; would need per-CPU indexing for SMP. | New in v5 |
| C-8 | INFO | `thread_manager.rs` | Dead thread cleanup allocates a `Vec` for `to_remove`. Small heap alloc during the cleanup path. | New in v5 |

**No CRITICAL, HIGH, or MEDIUM severity findings.**

The score moved 9.65 to 9.6 because the new `UnsafeCell` pattern adds to
the unsafe surface area, offset by improved error handling on reply map
full.

## Speed (9.2/10)

### v4 to v5 improvements

| Optimization | Cycles saved | Mechanism |
|---|---|---|
| O(1) ReplyMap | ~100 to 200 per RPC | Hash plus linear probe vs BTreeMap tree walk. Two ops per RPC (insert in `sys_call`, remove in `sys_reply`). |
| Per-CPU token cache | ~45 to 90 per cache hit | `UnsafeCell` direct access vs `THREAD_REPOSITORY` Mutex lock/unlock. |
| Register IPC fast path | ~60 to 100 per small call | Skip 4-level page table walk for `copy_from_user`. 16 bytes from registers. |

### IPC round-trip estimates

| Scenario | v4 cycles | v5 cycles | Speedup |
|---|---|---|---|
| Simple send+recv (cache hit) | ~650 to 950 | ~600 to 860 | 1.1x |
| Full `sys_call`/reply (cache hits, register path) | ~1,195 to 1,625 | ~950 to 1,250 | 1.3x |
| Full `sys_call`/reply (cache miss) | ~1,215 to 1,725 | ~1,010 to 1,440 | 1.2x |

### Comparison to production microkernels

| Kernel | Simple IPC | Full RPC | Notes |
|---|---|---|---|
| seL4 (x86_64) | ~850 to 1,000 | ~850 to 1,000 | No per-op capability check |
| **CLUU simple** | **~600 to 860** |  | Faster than seL4 simple IPC |
| **CLUU call/reply** |  | **~950 to 1,250** | Approaching seL4 parity |
| Fiasco.OC | ~1,200 to 1,800 | ~1,200 to 1,800 | L4 heritage |
| Zircon | ~3,000 to 5,000 | ~3,000 to 5,000 | Channel-based |

### Token lookup paths

| Path | v5 cycles | v4 cycles | Delta |
|---|---|---|---|
| Cache hit (per-CPU, no lock) | ~20 to 40 | ~50 to 80 | -30 to 40 (no Mutex) |
| Cache miss (shard lock only) | ~100 to 200 | ~100 to 200 | Unchanged |

### Remaining bottlenecks (ranked by impact)

| Rank | Issue | Cycles | Notes |
|---|---|---|---|
| 1 | Context switch hardware cost | ~300 to 400 | CR3 reload, TLB flush. Unavoidable. |
| 2 | Global revocation generation | varies | Single counter invalidates all caches. Per-shard counters would limit blast radius. |
| 3 | `copy_to_user` for reply delivery | ~60 to 100 | Register fast path is caller-side only; reply still copies to user buffer. |
| 4 | Mutex contention on ReplyMap | ~30 to 50 | Lock still acquired for O(1) ops; could use lock-free CAS for single-CPU. |
| 5 | Token table shard lock on cache miss | ~30 to 50 | Already rare with 4-entry LRU cache. |

### Path to sub-850 cycle full RPC (seL4 parity)

1. **Register-based reply delivery.** Skip `copy_to_user` for at-most-16-byte
   replies. Saves ~60 to 100 cycles.
2. **Lock-free ReplyMap.** On single-CPU, replace Mutex with
   interrupt-disable plus direct access. Saves ~30 to 50 cycles.
3. **Per-shard revocation generation.** Limits cache invalidation to the
   affected shard. Saves variable cycles on revocation-heavy workloads.

With items 1 and 2: estimated ~850 to 1,050 cycle full RPC, seL4 parity.

## Efficiency (9.4/10)

### Resource cleanup (all types fully cleaned)

| Resource | On process exit | On thread death | v4 | v5 |
|---|---|---|---|---|
| Physical frames | PASS | N/A | PASS | PASS |
| Page tables | PASS | N/A | PASS | PASS |
| Tokens | PASS (procmgr) | PASS | PASS | PASS |
| Endpoints | PASS | N/A | PASS | PASS |
| Thread structs | PASS | PASS | PASS | PASS |
| `CALL_REPLY_MAP` entries | PASS | PASS | PASS | PASS |
| `FAULT_REPLY_MAP` entries | PASS | PASS | PASS | PASS |

### Per-object memory overhead

| Object | Size | Limit | Max total | v4 to v5 change |
|---|---|---|---|---|
| Thread (TCB) | ~352 B | 4,096 | 1.4 MB | -800 B (TokenCache removed) |
| Token | ~120 to 200 B | 65,536 | 7.5 to 12.5 MB | Unchanged |
| Endpoint (empty) | ~200 B | 4,096 | 0.8 MB | Unchanged |
| ReplyMap (CALL) | ~18 KB static | 1 | 18 KB | New (replaces dynamic BTreeMap) |
| ReplyMap (FAULT) | ~10 KB static | 1 | 10 KB | New (replaces dynamic BTreeMap) |
| Per-CPU TokenCache | ~0.8 KB static | 1 | 0.8 KB | New (replaces per-thread) |
| **Static worst-case** |  |  | **~12 MB of 32 MB** | **Improved** |

### Memory savings analysis

| Change | Per unit | Units | Total savings |
|---|---|---|---|
| TokenCache removed from Thread | ~800 B | 4,096 max | ~3.1 MB saved at max threads |
| BTreeMap to static ReplyMap | -dynamic alloc | 2 maps | Eliminates heap fragmentation |
| Per-CPU cache (single static) | replaces 4,096 copies | 1 | ~3.1 MB saved (net) vs per-thread |
| Static ReplyMap overhead | ~28 KB | 2 maps | Fixed cost, no growth |

Net memory improvement: ~3.1 MB saved at max threads, traded for ~29 KB
of fixed static allocation. **107:1 ratio.**

### Reply map capacity

| Parameter | Value | Notes |
|---|---|---|
| Total slots | 256 | Per map (CALL and FAULT separate) |
| Max entries (50% cap) | 128 | Enforced by `insert()` |
| Concurrent calls supported | 128 | Sufficient for single-CPU kernel |
| Typical server concurrency | 1 to 16 | Most services handle one call at a time |
| Overflow behavior | `Error::Overflow` to caller | Graceful degradation, no crash |

128 concurrent calls is well beyond what a single-CPU kernel can actually
service simultaneously.

### Remaining DoS vectors

| Vector | Risk | Mitigation |
|---|---|---|
| IPC message queue fill | Medium | `MAX_QUEUE_LEN=1024` per endpoint, backpressure |
| Reply map fill (128 cap) | Low | `Error::Overflow` returned; attacker threads block themselves |
| `TIMEOUT_HEAP` stale entries | Low | Lazy cleanup with validity checks on pop |
| Endpoint waiter stale entries | Low | Ticket-based validation |

### Unsafe code

Two new unsafe accesses in v5: `PERCPU_TOKEN_CACHE.inner.get()` in
`try_cache_lookup` and `update_cache`. Both are sound on single-CPU
(syscall context is non-reentrant; no interrupt handler calls
`lookup_token`). Total ~282 unsafe blocks/fns across the kernel.

## Architecture assessment

### What improved in v5

1. **O(1) reply tracking.** 256-slot open-addressing hash table with
   linear probing and backward-shift deletion. Replaces BTreeMap for both
   `CALL_REPLY_MAP` and `FAULT_REPLY_MAP`. Fixed-size, no heap allocation,
   no fragmentation.
2. **Lock-free token cache on hit.** Per-CPU `UnsafeCell` bypasses
   `THREAD_REPOSITORY` entirely. Cache hit path: load generation counter,
   scan 4 entries, check expiration. Zero lock acquisitions.
3. **Register IPC for `sys_call`.** At-most-16-byte messages passed inline
   via arg2 plus arg6 registers. Avoids `copy_from_user` page table walk.
   Buffer padded to 56 bytes for `reply_id` injection at word index 5.
4. **Graceful reply map overflow.** All callers of `set_call_reply_info`
   and `set_fault_reply_info` handle the false return. `sys_call` returns
   `Error::Overflow`; the fault handler kills the thread.
5. **Smaller Thread struct.** ~800 bytes removed per thread (TokenCache
   field eliminated). Reduces `THREAD_REPOSITORY` memory pressure.

### Remaining architectural issues

1. **Global revocation generation.** Single counter still invalidates all
   caches on any token revocation. Per-shard counters would limit blast
   radius.
2. **Reply delivery still copies.** Register fast path is caller-side
   only; `sys_reply` still uses `copy_to_user` for reply delivery.
3. **Mutex on ReplyMap.** O(1) ops inside lock, but lock
   acquisition/release still costs ~30 to 50 cycles. Lock-free CAS
   possible on single-CPU.
4. **Per-CPU cache not SMP-ready.** `UnsafeCell` pattern requires per-CPU
   indexing for multi-CPU support.

## Recommendations

### Next priority fixes

| Priority | Issue | Impact | Effort |
|---|---|---|---|
| 1 | Register-based reply delivery | -60 to 100 cycles per reply | Medium |
| 2 | Per-shard revocation generation | Limits cache invalidation blast radius | Medium |
| 3 | Lock-free ReplyMap (single-CPU) | -30 to 50 cycles per RPC | Low |

### Future improvements

| Priority | Issue | Impact | Effort |
|---|---|---|---|
| 4 | O(1) token table (slab allocator) | -50 to 100 cycles per token op | Medium |
| 5 | RDRAND amortization (ChaCha20 CSPRNG) | -200 to 600 cycles per token creation | Medium |
| 6 | Priority inheritance | Eliminates priority inversion | High |
| 7 | FPU/SSE lazy context save | Enables SIMD in userspace | Medium |
| 8 | Lock-free endpoint queues | Eliminates shard contention (SMP prep) | High |
| 9 | Per-CPU token cache with CPU indexing | SMP readiness | Medium |

## Final verdict

CLUU v5 completes all three optimizations from v4's "Path to Sub-1,000
Cycle Full RPC" roadmap. Full call/reply IPC drops from ~1,195 to 1,625
cycles (v4) to **~950 to 1,250 cycles**, now within 10 to 25% of seL4
(~850 to 1,000). The O(1) ReplyMap eliminates BTreeMap overhead, the
per-CPU token cache removes lock contention from the hot path, and the
register IPC fast path skips page table walks for small messages.

Memory efficiency improves: Thread structs shrink by ~800 bytes each
(3.1 MB saved at max 4,096 threads), replaced by ~29 KB of fixed static
allocation. All resource cleanup remains correct with the new data
structures.

**Rating: A (9.4/10)**, up from A (9.1/10) in v4. The path to A+
(9.5+) requires register-based reply delivery (symmetric to the new
`sys_call` fast path) and per-shard revocation generation. The kernel now
achieves near-seL4 IPC performance while maintaining full capability-based
security, POSIX compatibility, and production-ready resource management.
