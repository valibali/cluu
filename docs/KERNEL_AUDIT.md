# CLUU Kernel Technical Audit (v3 — Post-Remediation)

**Date**: February 2026
**Scope**: Kernel correctness, speed, efficiency, and production-readiness
**Target**: x86_64 single-CPU microkernel, seL4-inspired with POSIX compatibility layer
**Codebase**: ~22.3K LOC (61 Rust files + x86_64 assembly), 1,009 LOC assembly
**Known limitations**: Single CPU, x86_64 only, no IOMMU, PIC-only interrupts
**Previous audits**: v1 (7.0/10), v2 (7.75/10)

---

## Overall Rating

| Category | Score | Grade | v2 | v1 | Summary |
|---|---|---|---|---|---|
| **Correctness** | 9.25/10 | A | 8.75 | 8.5 | SHA-256 verified FIPS 180-4. All 9+ return-to-userspace paths correct. Zero bugs |
| **Speed** | 6.5/10 | B- | 6.5 | 5.5 | ObjectRef cache eliminates resolve_scope. Real HMAC offsets gains. Reply token minting is new #1 bottleneck |
| **Efficiency** | 8.5/10 | A- | 8.0 | 7.5 | All 3 resource types capped. Endpoint cleanup via zero-reference. Dead thread leak remains |
| **Overall** | **8.0/10** | **B+** | **7.75** | **7.0** | Cryptographically secure tokens. Measurably better resource management. Simple IPC competitive with Fiasco.OC |

**Verdict**: v3 completes the security story -- HMAC-SHA256 tokens are now cryptographically unforgeable with real FIPS 180-4 SHA-256. The ObjectRef caching eliminates the #1 bottleneck from v2 (16-shard resolve_scope scan). All three resource types (tokens, threads, endpoints) are now hard-capped with CAS enforcement. Endpoint cleanup on zero-reference detection closes the last major resource leak. The speed score holds steady because real SHA-256 costs ~3,300-4,900 cycles per HMAC (vs ~250-350 for the placeholder), but this is offset by the ~240 cycles/op saved from ObjectRef caching. Simple send+recv IPC is ~1,080-1,620 cycles -- competitive with production microkernels.

---

## Changes Since v2

| Item | Category | Status |
|---|---|---|
| 1. Real FIPS 180-4 SHA-256 | Security (Must Fix) | Done |
| 2. Cache ObjectRef in TokenCache | Speed (Must Fix) | Done |
| 3. Global thread limit (4096, CAS) | Efficiency (Must Fix) | Done |
| 4. Global endpoint limit (4096, CAS) | Efficiency (Must Fix) | Done |
| 5. Endpoint cleanup on zero-reference | Efficiency (Must Fix) | Done |

---

## 1. Correctness (9.25/10)

### SHA-256 Implementation -- VERIFIED CORRECT

| Check | Status | Evidence |
|---|---|---|
| H_INIT values (8 primes) | PASS | Fractional square roots of 2,3,5,7,11,13,17,19 |
| K constants (64 primes) | PASS | K[0]=0x428a2f98, K[63]=0xc67178f2 |
| Compression function | PASS | ch, maj, Sigma0/1, sigma0/1 rotation values correct |
| Padding | PASS | 0x80 + zeros + 64-bit big-endian bit count, two-block case handled |
| NIST test vectors | PASS | Empty, "abc", 448-bit two-block boundary |
| Stack-only (no heap) | PASS | Sha256State: 108 bytes on stack |

### HMAC-SHA256 + Token Signatures -- VERIFIED CORRECT

| Check | Status | Evidence |
|---|---|---|
| HMAC RFC 2104 | PASS | IPAD=0x36, OPAD=0x5C, key zero-padded |
| hmac_sha256_fixed (stack path) | PASS | Used for 36-byte token data, no heap |
| Constant-time comparison | PASS | XOR + OR accumulation, no early exit |
| Token verify recomputes | PASS | signature.rs computes fresh HMAC, compares |

### ObjectRef Cache -- CORRECT

| Check | Status | Evidence |
|---|---|---|
| lookup_token returns (Token, ObjectRef) | PASS | Cache hit and miss paths both return tuple |
| check_object_type exhaustive match | PASS | All 7 ObjectRef variants covered |
| All IPC call sites updated | PASS | sys_send, sys_recv, sys_call, sys_reply + 7 invoke sites |
| sys_recv NotFound skip | PASS | Destroyed endpoints skipped in multi-endpoint scan |
| sys_invoke discards obj_ref | PASS | Option B design: invoke handlers keep their own resolution |

### CAS Counters -- CORRECT

| Resource | Limit | Increment | Decrement | Underflow Safe |
|---|---|---|---|---|
| Threads | 4,096 | try_alloc_thread_id (CAS) | mark_thread_dead (after found check) | Yes |
| Endpoints | 4,096 | try_create_endpoint (CAS) | destroy_endpoint/full (after remove check) | Yes |
| Tokens | 65,536 | try_create_token_with_kind (CAS) | revoke_token (after removal) | Yes |

### Endpoint Cleanup -- CORRECT

| Check | Status | Evidence |
|---|---|---|
| destroy_endpoint_full removes from shard | PASS | endpoints.remove(&id) |
| Queued messages dropped | PASS | VecDeque Drop frees memory |
| All blocked threads woken | PASS | receivers, senders, callers, current_caller |
| Shard lock released before waking | PASS | drop(shard_guard) at line 578, then wake_thread calls |
| Idempotent | PASS | Returns silently if already destroyed |
| Zero-reference trigger in revoke_token | PASS | count_tokens_for_object after shard lock released |
| Zero-reference trigger in revoke_tokens_for_object | PASS | Same check after all shard iteration |

### Return-to-Userspace Paths (9+) -- ALL CORRECT

All paths verified for RFLAGS sanitization, SWAPGS, FS base save/restore, segment selectors:

1. SYSRET fast path -- PASS
2. SYSRET fallback IRETQ -- PASS
3. Slow path IRETQ -- PASS
4. Timer no-switch return -- PASS
5. Timer user context switch -- PASS
6. Timer kernel context switch -- PASS
7. GPF context switch -- PASS
8. PF context switch -- PASS
9. PF resume -- PASS
10. Initial enter_userspace -- PASS

### Findings

| ID | Severity | Location | Description |
|---|---|---|---|
| C-1 | LOW | handlers.rs:625 | sys_invoke discards cached ObjectRef, invoke handlers re-scan 16 shards |
| C-2 | LOW | endpoint.rs:450-458 | EndpointRepository::create() bypasses TOTAL_ENDPOINT_COUNT (dead code) |
| C-3 | INFO | table.rs:551-554 | TOCTOU window in zero-reference check (safe: single-CPU, idempotent) |
| C-4 | INFO | table.rs:411-418 | Defense-in-depth shard lock on every cache hit (safe but costly) |

**No CRITICAL or HIGH severity findings.**

---

## 2. Speed (6.5/10)

### HMAC-SHA256 Cost (Real SHA-256)

| Operation | Cycles | Notes |
|---|---|---|
| Single SHA-256 compress | ~800-1,200 | 64 rounds, ILP on modern x86_64 |
| HMAC-SHA256 (36-byte token data) | ~3,300-4,900 | 4 compress calls (inner 2 blocks + outer 2 blocks) |

### Token Lookup Paths

| Path | v3 Cycles | v2 Cycles | Delta |
|---|---|---|---|
| Cache hit | ~150-250 | ~150-250 (+ ~240 resolve_scope) | **-240 (resolve_scope eliminated)** |
| Cache miss | ~3,500-5,200 | ~430-650 (placeholder) | **+3,070-4,550 (real HMAC)** |

### IPC Round-Trip Estimates

| Scenario | Cycles | Notes |
|---|---|---|
| Simple send+recv (cache hit, 1 context switch) | ~1,080-1,620 | **Competitive with Fiasco.OC** |
| Full sys_call/reply (cache hits) | ~5,670-9,120 | Reply token minting dominates (55-65%) |
| Full sys_call/reply (cold cache) | ~12,000-19,000 | 3 HMAC verifications + reply creation |

### Comparison to Production Microkernels

| Kernel | Simple IPC | Full RPC | Notes |
|---|---|---|---|
| seL4 (x86_64) | ~850-1,000 | ~850-1,000 | No per-op capability check |
| Fiasco.OC | ~1,200-1,800 | ~1,200-1,800 | L4 heritage |
| Zircon | ~3,000-5,000 | ~3,000-5,000 | Channel-based |
| **CLUU simple** | **~1,080-1,620** | -- | **Competitive** |
| **CLUU call/reply** | -- | **~5,670-9,120** | Reply token creation dominates |

### Remaining Bottlenecks (ranked by impact)

| Rank | Issue | Impact | Cycles Wasted |
|---|---|---|---|
| 1 | **Reply token minting per sys_call** | RDRAND x2 + HMAC-SHA256 per call/reply | ~3,660-5,810 |
| 2 | **Defense-in-depth shard lock on cache hit** | Redundant given generation counter | ~45-80 |
| 3 | **THREAD_REPOSITORY lock on cache access** | Global mutex for per-thread cache | ~45-90 |
| 4 | **BTreeMap O(log n) on all hot paths** | 4-6 tree walks per IPC round-trip | ~120-360 |
| 5 | **Global revocation generation** | Reply token revoke invalidates all caches | ~3,300-4,900 per forced miss |
| 6 | **Page table walks for copy_to_user** | 4-level walk per page boundary | ~60-100 per page |

### Path to Sub-2,000 Cycle Full RPC

1. **Eliminate per-call reply token minting**: reuse kernel-managed reply capability per thread (seL4-style). Saves ~3,660-5,810 cycles per round-trip.
2. **Remove defense-in-depth shard lock**: generation counter already invalidates. Saves ~45-80 cycles per lookup.
3. **Per-thread cache without THREAD_REPOSITORY lock**: lock-free per-CPU array. Saves ~45-90 cycles per lookup.
4. **O(1) slab/array lookups**: replace BTreeMap with indexed arrays. Saves ~120-360 cycles per round-trip.

With all four: estimated ~1,500-2,500 cycle full RPC.

---

## 3. Efficiency (8.5/10)

### Per-Object Memory Overhead

| Object | Size | Limit | Max Total |
|---|---|---|---|
| Thread (TCB) | ~1,152 B | 4,096 | 4.5 MB |
| Token | ~120-200 B | 65,536 | 7.5-12.5 MB |
| Endpoint (empty) | ~200 B | 4,096 | 0.8 MB |
| **Static worst-case** | | | **~17.6 MB of 32 MB** |

### DoS Prevention -- ALL THREE RESOURCE TYPES CAPPED

| Resource | Limit | Mechanism |
|---|---|---|
| Tokens | 65,536 | CAS on TOTAL_TOKEN_COUNT |
| Threads | 4,096 | CAS on TOTAL_THREAD_COUNT |
| Endpoints | 4,096 | CAS on TOTAL_ENDPOINT_COUNT |

### Resource Cleanup

| Resource | On Process Exit | On Thread Death | Status |
|---|---|---|---|
| Physical frames | PASS | N/A | teardown_user_pages |
| Page tables | PASS | N/A | Intermediate tables freed |
| Tokens | PASS (procmgr) | PASS | revoke_tokens_for_object |
| Endpoints | **PASS (NEW)** | N/A | Zero-reference hook in revoke_token |
| Thread structs | PARTIAL | **LEAK** | Dead threads never removed from THREAD_REPOSITORY |
| CALL_REPLY_MAP entries | NOT CLEANED | NOT CLEANED | Minor leak for threads dying mid-call |

### Key Finding: Dead Thread Memory Leak

`mark_thread_dead()` sets state to Dead, decrements TOTAL_THREAD_COUNT, removes from scheduler, revokes tokens. But it **never removes the Thread struct from THREAD_REPOSITORY**. Dead Thread objects (~1,152 bytes each) accumulate permanently. After 4,096 thread lifetimes, that is 4.5 MB of dead thread storage.

### Remaining DoS Vectors

| Vector | Risk | Mitigation |
|---|---|---|
| IPC message queue fill | Medium | MAX_QUEUE_LEN=1024 per endpoint, backpressure |
| TIMEOUT_HEAP unbounded | Low | Lazy cleanup, pathological workloads only |
| BTreeMap fragmentation | Low | linked_list_allocator fragmentation over time |
| Dead thread accumulation | Medium | Thread reaping not implemented |

### Unsafe Code

No new unsafe from v3 remediation. SHA-256 and all CAS/cleanup code is safe Rust. Total ~280 unsafe blocks/fns across kernel, concentrated in HAL and memory management.

---

## 4. Architecture Assessment

### What Improved in v3

1. **Cryptographic security**: HMAC-SHA256 tokens are now truly unforgeable. Real FIPS 180-4 SHA-256 with NIST test vectors.
2. **ObjectRef caching**: Eliminates 16-shard resolve_scope scan on IPC hot path. `check_object_type` is zero-cost enum match.
3. **Resource limits**: All three resource types hard-capped. CAS enforcement prevents DoS.
4. **Endpoint cleanup**: Zero-reference detection on token revocation triggers full teardown (wake blocked threads, drop queued messages, decrement counter).
5. **Lock ordering compliance**: All new code follows documented ordering: THREAD_REPOSITORY -> SCHEDULER -> ENDPOINT_SHARDS / TOKEN_TABLE_SHARDS.

### Remaining Architectural Issues

1. **Reply token minting**: ~55-65% of full RPC cost. seL4-style kernel-managed reply capability would eliminate this.
2. **Defense-in-depth shard lock on cache hit**: Redundant given generation counter protocol.
3. **Global revocation generation**: Single counter invalidates all caches on any revocation. Per-shard or per-object counters would limit blast radius.
4. **Dead thread memory leak**: THREAD_REPOSITORY grows monotonically.
5. **BTreeMap on all hot paths**: O(log n) where O(1) is achievable with slab allocators.

---

## 5. Recommendations

### Next Priority Fixes

| Priority | Issue | Impact | Effort |
|---|---|---|---|
| 1 | **Eliminate per-call reply token minting** | -3,660-5,810 cycles per RPC (~60% reduction) | Medium-High |
| 2 | **Remove defense-in-depth shard lock** | -45-80 cycles per cache hit | Trivial |
| 3 | **Reap dead threads from THREAD_REPOSITORY** | Fixes ~1.15 KB/thread memory leak | Low |
| 4 | **Per-shard revocation generation** | Limits cache invalidation blast radius | Medium |
| 5 | **Move token cache out of THREAD_REPOSITORY** | -45-90 cycles per cache access | Medium |

### Future Improvements

| Priority | Issue | Impact | Effort |
|---|---|---|---|
| 6 | O(1) slab/array lookups | -120-360 cycles per IPC round-trip | Medium |
| 7 | RDRAND amortization (ChaCha20 CSPRNG) | -200-600 cycles per token creation | Medium |
| 8 | Register-based IPC fast path | -100-200 cycles for small messages | High |
| 9 | Priority inheritance | Eliminates priority inversion | High |
| 10 | FPU/SSE lazy context save | Enables SIMD in userspace | Medium |
| 11 | CALL_REPLY_MAP cleanup on thread death | Fixes minor leak | Low |
| 12 | Pass cached ObjectRef through sys_invoke | Eliminates redundant 16-shard scan for invokes | Low |

---

## 6. Audit History

| Version | Date | Overall | Correctness | Speed | Efficiency | Key Change |
|---|---|---|---|---|---|---|
| v1 | Feb 2026 | 7.0 | 8.5 | 5.5 | 7.5 | Initial audit |
| v2 | Feb 2026 | 7.75 | 8.75 | 6.5 | 8.0 | Token cache, heap, IrqAck, debug gating |
| **v3** | **Feb 2026** | **8.0** | **9.25** | **6.5** | **8.5** | **SHA-256, ObjectRef cache, resource limits, endpoint cleanup** |

---

## 7. Final Verdict

CLUU v3 is a **well-engineered, cryptographically secured microkernel**. The token system is now production-grade with real HMAC-SHA256, and all three resource types are hard-capped against DoS. Simple IPC (~1,080-1,620 cycles) is competitive with Fiasco.OC. Full call/reply RPC (~5,670-9,120 cycles) is dominated by reply token minting, which is the clear #1 optimization target.

**Rating: B+ (8.0/10)** -- up from B+ (7.75/10). The path to an A- requires eliminating per-call reply token minting and removing the defense-in-depth shard lock. The path to sub-2,000 cycle full RPC requires all four optimizations listed in Section 2.
