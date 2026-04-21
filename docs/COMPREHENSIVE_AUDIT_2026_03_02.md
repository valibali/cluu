# CLUU Microkernel — Comprehensive Technical Audit

**Date**: 2026-03-02 (original), **revised 2026-04-21** with re-verification pass
**Scope**: Full codebase (68K Rust + 20K C/ASM, 184 source files, 30 crates)
**Method**: 9 parallel deep-dive analyses across every subsystem, with a re-
verification pass on 2026-04-21 against uncommitted WIP on `develop`.

---

## 0. Current State Update (2026-04-21)

**Status**: ~40 days of uncommitted work sits on `develop`. Last commit is
`d40502c` (2026-03-03). The working tree **builds cleanly** (`cargo xtask build`
succeeds end-to-end; no newlib/syscalls/crt0 drift, no link errors) and the
default harness case passes (`bash scripts/harness_run.sh`: all required markers
found, zero faults, healthy resource-delta accounting). The full
`cargo xtask harness-matrix` sweep has NOT been rerun in this pass.

The WIP is concentrated in two coherent themes:

- **Security hardening**: SMAP/SMEP + CLAC, Spectre V2 (IBPB + STIBP + retpoline),
  password hashing + rate limiting + audit logging, TPM 2.0 TIS driver as a new
  userspace crate (`tpmd`, 1,302 lines), measured-boot + sealed-storage + remote-
  attestation (PoC) modules in init.
- **IPC Tier-1 optimizations** (§12.3, T1.1–T1.5): inline syscall dispatch jump
  table, EndpointShard inner Mutex removed, token cache 4→8, `PerCpuReplyMap<UnsafeCell>`
  replacing `Mutex`, fast-path register save trimmed.

Plus **async notifications (A2)** — a new seL4-style notification subsystem
(`kernel/src/ipc/notification.rs`, 207 lines) with 8-shard registry and 4 new
invoke ops (80–83).

### 0.1 Verified Against Code (2026-04-21)

Every ✅ item in §12 was re-verified against the current source. All claims
are present and wired. Specifics:

| Area | Verification | Notes |
|---|---|---|
| SMAP/SMEP + CLAC | ✅ | CR4 setup at `main.rs:129-152`; CLAC at `syscall_entry.asm:82` and four interrupt stubs (`:137,:242,:382,:597`) |
| Spectre V2 (IBPB/STIBP) | ✅ | `spectre.rs` 65 lines; IBPB gated on CR3 change in context switch (`thread_manager.rs:~1449`) |
| Retpoline | ✅ | `"features": "-mmx,+retpoline"` in kernel triplet |
| Inline syscall dispatch (T1.1) | ✅ | 4-entry jump table (`syscall_entry.asm:153-180`) for send/recv/call/reply |
| EndpointShard inner Mutex removed (T1.2) | ✅ | All 11 call sites updated; single-lock path verified |
| Token cache 4→8 (T1.3) | ✅ | `TOKEN_CACHE_SIZE = 8`; arrays and LRU order expanded |
| PerCpuReplyMap (T1.4) | ✅ | `UnsafeCell`-wrapped, `unsafe impl Sync` with documented single-CPU/non-reentrant boundary |
| Fast-path register save (T1.5) | ✅ | 5 callee-saved pushes replaced with R15 + 8-byte alignment pad |
| Async notifications (A2) | ✅ | `notification.rs` 207 lines, 8 shards, 4 invoke ops (80–83), `ObjectRef::Notification(NotificationId)` + tag 0x08 |
| Deferred fault queue 4→16 (H9) | ✅ | Overflow counter present but never read |
| Wake queue 8→32 (H10) | ✅ | Overflow counter present but never read |
| tpmd + measured boot + sealed storage | ✅ | tpmd 1,302 lines; stub-mode detection via DID_VID probe; graceful `REPLY_ENODEV`; seal/unseal round-trip verified (`data_len == 32 && unsealed == secret`) |
| Password hashing + rate limit + audit log | ✅ | SHA-256 + RDRAND salt, constant-time verify; exponential backoff `min(2^(f-1), 300)`; structured `AUTH_*` events (login/sudo/su, OK+FAIL) |
| Remote attestation (SEC-4) | ⚠️ **PoC only** | `attestation.rs` sends `TPM2_Quote`, reads length, **does not verify signature** |

### 0.2 Residual Risks Found in this Pass

| # | Sev | Issue | Location |
|---|---|---|---|
| R1 | MEDIUM | **T1.5 fast-path assumes SysV ABI on `syscall_dispatch`**. No compile- or run-time check that RBX/RBP/R12–R14 are actually preserved. Silent corruption possible if dispatch is ever inlined or LLVM aggressively reallocates. | `syscall_entry.asm` |
| R2 | MEDIUM | **RDRAND salt fallback silently zeroes**. After 10 retries per u64, salt degrades to all zeros with no warning. Hashes still verify but become dictionary-attackable. | `libcluu/src/crypto.rs:208-244` |
| R3 | LOW | **Overflow counters unread**. `PENDING_WAKE_OVERFLOW` / `DEFERRED_FAULT_OVERFLOW` are incremented but not exposed via /proc, telemetry, or debug_print. Diagnostic value lost. | `thread_manager.rs:224,246,938,976` |
| R4 | LOW | **Attestation signature not verified** (SEC-4). `quote_reply.words[1]` is read as length; payload signature is logged-only. Acceptable for PoC; NOT production-ready attestation. | `init/attestation.rs:46-72` |
| R5 | LOW | **~30 compiler warnings** across workspace — dead fields/methods, unused imports (cluu-init variants, cluu-vfs helpers, cluu-procmgr fields, cluu-tpmd `off` writes). No errors. Cumulative drift signal since Phase L. | `cargo xtask build` |
| R6 | LOW | **`etc/users.toml` schema comment updated but all entries still `password = ""`**. The new `$sha256$<salt>$<hash>` format path has no in-tree exercise yet. | `etc/users.toml` |
| R7 | LOW | **Harness coverage quietly narrowed**. `scripts/harness_run.sh` relaxed `m0_boot`, `m6_ipc_compact`, `m6_ipc_rendezvous`, and `l2_jobchurn_heavy` to minimal markers ("SLO-heavy / redundant"). Reasonable pruning, but reduces regression-catching surface. | `scripts/harness_run.sh` |

### 0.3 Stale Claims in Doc Body (Fixed Inline Below)

§2.7, §4.4, §5.5, §7.5 and §11 were written before the hardening work and
still described the pre-hardening state (plaintext passwords, "no STIBP/IBPB",
"no async notifications", 4/8-slot queues, no audit logging, no rate limiting,
no resource quotas). These entries have been updated inline in this revision.

### 0.4 Recommended Landing Strategy

The WIP is coherent enough to land, but too large for a single commit.
Recommended split, in this order (each independently buildable and testable):

1. **Kernel IPC Tier-1 optimizations** — `syscall_entry.asm`, `endpoint.rs`,
   `thread_manager.rs`, `token/{mod,scope,table}.rs`, `syscall/handlers.rs`
   (T1.1–T1.5 only). Measurable IPC-latency win, self-contained.
2. **Kernel security hardening** — SMAP/SMEP (`main.rs`, `syscall_entry.asm`,
   `interrupts.asm`), `spectre.rs`, retpoline triplet flag. Pure defensive.
3. **Async notifications (A2)** — `notification.rs` + token machinery + 4
   invoke ops + libcluu helpers. Kernel-and-userspace but cohesive.
4. **TPM + userspace auth** — `tpmd` crate, `init/{measured_boot,sealed_storage,
   attestation}.rs`, `libcluu/crypto.rs`, procmgr password/rate-limit/audit
   changes, `etc/users.toml` comment. Large but thematically unified.

Between each commit, rerun the default harness case. Before commit 4 lands,
run `cargo xtask harness-matrix` at least once.

**Block landing on fixing R1 and R2.** Both are cheap:
- R1: add a small inline test (panic-if-mismatched sentinel pattern, or a
  `naked` wrapper that explicitly saves/restores the registers the fast path
  no longer touches) to assert ABI preservation at boot.
- R2: make RDRAND failure loud — at minimum `debug_print` a warning; ideally
  panic if salt degenerates to all zeros during a real password operation.

R3/R4/R5/R6/R7 can ride in follow-up commits.

### 0.5 Recommended Next Work (Post-Landing)

Security hardening + IPC Tier-1 + async notifications are large, diverse
wins. Before starting anything new, **measure** median IPC round-trip on the
Tier-1 build — the M1 milestone ("sub-1200 cycles") may already be achieved,
in which case declare M1 done.

Three viable next milestones, roughly in order of effort / demonstrability:

**Option A — M2: MicroPython port** *(recommended first)*
All prerequisites are already met (per MEMORY.md §Research Findings). Work is
`sched_yield()` stub (U5, ~10 LOC), `pthread_cancel` patch, and crate setup.
~1–2 weeks. Unlocks scripting, strong visible progress, zero kernel risk.

**Option B — U1/U2: raw input (PS/2 mouse + key up/down scancodes)**
~1 week. Unblocks Quake (M8) and gives an input-rich demo path without SMP
prerequisites.

**Option C — SMP-1 (per-CPU foundation)**
Much larger. §3.3 flags multiple CRITICAL SMP gaps (TLB shootdown, teardown
synchronization, shared BSP_STACK). Start with SMP-1 (per-CPU GDT/TSS/IST/
PerCpuData) because everything else depends on it. Only take this on when a
concrete SMP workload motivates it.

**Recommended path**: **A → B → (if needed) SMP**. MicroPython is cheap and
demonstrable; raw input is cheap and unlocks Quake; SMP is the biggest cliff
and should come once userspace is compelling enough to make SMP a felt need
rather than an abstract one.

---

## Table of Contents

0. [Current State Update (2026-04-21)](#0-current-state-update-2026-04-21)
1. [Codebase Overview](#1-codebase-overview)
2. [Kernel IPC & Syscall Subsystem](#2-kernel-ipc--syscall-subsystem)
3. [Memory Management](#3-memory-management)
4. [Security & Capability Model](#4-security--capability-model)
5. [Scheduler & Threading](#5-scheduler--threading)
6. [Userspace Service Architecture](#6-userspace-service-architecture)
7. [Process Isolation & Containers](#7-process-isolation--containers)
8. [POSIX Compatibility Layer](#8-posix-compatibility-layer)
9. [Device Drivers & Hardware Abstraction](#9-device-drivers--hardware-abstraction)
10. [Build System & Testing](#10-build-system--testing)
11. [Scorecard & Production Comparison](#11-scorecard--production-comparison)
12. [Actionable Findings](#12-actionable-findings)
13. [Roadmap: Sub-1000 Cycle IPC](#13-roadmap-sub-1000-cycle-ipc)
14. [Roadmap: SMP](#14-roadmap-smp)
15. [Roadmap: Security Hardening & TPM](#15-roadmap-security-hardening--tpm)
16. [Roadmap: Userspace & Driver Ecosystem](#16-roadmap-userspace--driver-ecosystem)
17. [Consolidated Roadmap Timeline](#17-consolidated-roadmap-timeline)

---

## 1. Codebase Overview

| Component | Files | Lines | Language |
|---|---|---|---|
| Kernel (kernel/) | ~50 .rs | ~15,000 | Rust |
| Userspace services | ~80 .rs | ~35,000 | Rust |
| libcluu (POSIX layer) | ~30 .rs | ~12,000 | Rust |
| xtask (build system) | 1 .rs | 3,976 | Rust |
| Assembly | 3 .asm/.S | 1,354 | x86_64 NASM |
| C programs & tools | 60+ .c/.h | 18,709 | C |
| **Total** | **184 .rs** | **~68,000 Rust + 20K C/ASM** | |

30 Cargo crates, 41 containers, 50+ test scenarios.

### Top 10 Largest Files

| File | Lines | Role |
|---|---|---|
| userspace/procmgr/src/main.rs | 5,157 | Process manager (control plane) |
| xtask/src/main.rs | 3,976 | Build system orchestrator |
| userspace/vfs/src/main.rs | 3,483 | Virtual filesystem service |
| userspace/shell/src/commands.rs | 3,305 | Shell built-in commands |
| kernel/src/syscall/handlers.rs | 2,939 | Syscall dispatch + invoke ops |
| kernel/src/mm/vmm.rs | 1,838 | Virtual memory manager |
| kernel/src/sched/thread_manager.rs | 1,562 | Thread lifecycle + reply maps |
| userspace/libcluu/src/syscall.rs | 1,359 | Userspace syscall wrappers |
| userspace/console/src/renderer.rs | 1,288 | Console VT renderer |
| kernel/src/ipc/endpoint.rs | 1,232 | IPC endpoint implementation |

---

## 2. Kernel IPC & Syscall Subsystem

### 2.1 Architecture

**7 syscalls** (Send, Recv, Call, Reply, Yield, Invoke, DebugPrint) — matching seL4's
philosophy of minimal kernel. The `Invoke` syscall dispatches to **34 operations**
covering threads, address spaces, tokens, futex, IRQ, PCI, I/O ports, and timers.

**IPC model**: seL4-style synchronous rendezvous with implicit reply capabilities.
`sys_call` injects a kernel-allocated ReplyId into the message — userspace cannot
forge reply caps. The endpoint model uses sharded locks (16 shards) with a per-CPU
4-entry token cache that bypasses locking entirely on cache hit.

**Measured IPC round-trip**: 1,195–1,625 cycles (7.1x improvement from original).

### 2.2 Syscall Entry (x86_64)

**Dual-path design** (syscall_entry.asm):

- **Fast path (SYSRET)**: No context switch, ~40 cycles. Minimal save of RCX, R11,
  callee-saved registers. RFLAGS sanitized, RCX canonicality validated.
- **Slow path (IRETQ)**: Full 184-byte Context save including FS_BASE (TLS via
  rdmsr), CR3, all GPRs. Calls `schedule_and_switch` for context switch.

**Security hardening on ALL return paths**:
```asm
; Clear TF/IOPL/NT/RF/AC, ensure IF + reserved bit 1
and r11, ~((1 << 8) | (3 << 12) | (1 << 14) | (1 << 16) | (1 << 18))
or  r11, (1 << 9) | (1 << 1)
```

**Intel SYSRET bug mitigation** (syscall_entry.asm:197-200):
```asm
mov r10, rcx
shr r10, 47
test r10, r10
jnz .sysret_unsafe_fallback     ; Non-canonical → safe IRETQ instead
```

### 2.3 Inline Register IPC

Three syscalls support a register-based fastpath that avoids memory copies for
small messages. The design is **intentionally asymmetric** due to register
availability:

| Syscall | Data Registers | Max Inline | Reason |
|---|---|---|---|
| sys_send | arg2, arg4, arg5, arg6 | 32 bytes | arg3=len only |
| sys_reply | arg2, arg4, arg5, arg6 | 32 bytes | arg3=len only |
| sys_call | arg2, arg6 | 16 bytes | arg4=reply_buf, arg5=reply_len consumed |

**sys_send** (handlers.rs:86-96) — correct 4-chunk pattern:
```rust
let chunks = [args.arg2, args.arg4, args.arg5, args.arg6];
let mut copied = 0usize;
for chunk in chunks {
    if copied >= inline_len { break; }
    let bytes = chunk.to_ne_bytes();
    let chunk_len = core::cmp::min(bytes.len(), inline_len - copied);
    buffer[copied..copied + chunk_len].copy_from_slice(&bytes[..chunk_len]);
    copied += chunk_len;
}
```

**sys_call** (handlers.rs:477-484) — 2-register pattern (by design):
```rust
let a2_bytes = args.arg2.to_ne_bytes();
let a6_bytes = args.arg6.to_ne_bytes();
let copy_a2 = core::cmp::min(8, inline_len);
buffer[..copy_a2].copy_from_slice(&a2_bytes[..copy_a2]);
if inline_len > 8 {
    let copy_a6 = core::cmp::min(8, inline_len - 8);
    buffer[8..8 + copy_a6].copy_from_slice(&a6_bytes[..copy_a6]);
}
```

**Why sys_call is limited to 16 bytes**: In the call path, arg4 and arg5 carry the
reply buffer pointer and length respectively (handlers.rs:404-405). These are stored
in CallReplyInfo (line 466-467) so the kernel knows where to write the reply when
it arrives. This leaves only arg2 and arg6 free for inline message data.

This is NOT a bug — it is a necessary register allocation trade-off. The constant
`IPC_REG_INLINE_MAX_CALL_PAYLOAD = 16` (line 29) correctly reflects this.

### 2.4 Reply Map & Thread Death Cleanup

The kernel maintains two reply maps:
- `CALL_REPLY_MAP`: tracks in-flight IPC calls (caller→reply buffer info)
- `FAULT_REPLY_MAP`: tracks fault forwarding (faulted thread→recovery info)

**Reply ID allocation** (thread_manager.rs:482-484):
```rust
pub fn alloc_reply_id() -> ReplyId {
    ReplyId::new(NEXT_REPLY_ID.fetch_add(1, Ordering::SeqCst))
}
```
Monotonic u64 counter — infeasible to brute-force (2^64 space).

**Thread death cleanup** (thread_manager.rs:397-472) is comprehensive:
1. Mark thread dead, clear timeout, disarm recv_wait
2. Remove from scheduler
3. Revoke all tokens referencing the dead thread
4. **Scan CALL_REPLY_MAP**: remove entries where dead thread is caller; wake blocked
   callers with error when dead thread was server
5. **Scan FAULT_REPLY_MAP**: remove entries where dead thread is the faulted thread
6. Remove Thread struct from repository

This is a **full cleanup path** — no orphaned reply map entries remain after thread
death.

### 2.5 Grant/Map Transfer

**Status: FULLY STUBBED** (transfer.rs:92-214)

All three transfer modes (copy_buffer, grant_buffer, map_buffer) validate inputs
but return `Ok(())` without performing actual transfers. The BufferTransfer struct
takes allocator + mapper generics but doesn't use them (`_allocator`, `_mapper`).

This means zero-copy page transfer between address spaces is **not functional**.
IPC relies entirely on copy-based transfer via physmap.

### 2.6 Strengths

- Minimal 7-syscall surface dramatically reduces attack vectors
- Implicit reply caps are unforgeable (kernel-only allocation, monotonic u64)
- Lock-free token cache on IPC hot path (generation counter invalidation)
- seL4-style fault forwarding via fault endpoints (label 0xFA017)
- RFLAGS sanitization verified on all 9 return-to-userspace paths
- Intel SYSRET canonical address bug mitigated with IRETQ fallback
- Comprehensive reply map cleanup on thread death

### 2.7 Weaknesses

- **128 in-flight call limit**: REPLY_MAP_SLOTS/2 = 128. The 129th concurrent RPC
  returns Overflow. Could cause application-level failures under high concurrency.
- **Grant/Map fully stubbed**: Zero-copy page transfer not implemented. All IPC
  involves memory copies (2 copies for full Call+Reply round-trip).
- **No batched operations**: Each invoke is a full syscall transition.
- ~~**No async notifications**~~ — **FIXED (A2)**. 8-shard `Notification`
  registry with signal/try_wait/poll and 4 invoke ops (80–83). See
  `kernel/src/ipc/notification.rs` and §0.1.
- **Endpoint queue limits**: MAX_QUEUE_LEN=1024 per endpoint. Single sender can
  fill queue, blocking all others. No per-sender quota.

### 2.8 Comparison to seL4 / L4 / QNX

| Feature | CLUU | seL4 | L4 | QNX |
|---|---|---|---|---|
| Syscall count | 7 | ~5 | ~5 | ~50 |
| IPC model | Sync rendezvous | Sync rendezvous | Sync rendezvous | Sync + async |
| Reply caps | Implicit (kernel) | Implicit (kernel) | Explicit | N/A |
| Zero-copy | Stubbed | Grant/Map | Grant/Map | Shared memory |
| Async notify | No | Yes (notifications) | Yes | Yes (pulses) |
| Priority inherit | No | Yes | No | Yes |
| Token/Cap model | HMAC-signed tokens | CSpace slots | CSpace slots | Capability bits |
| IPC latency | 1,195-1,625 cycles | ~500-1,000 cycles | ~500 cycles | ~1,000 cycles |

---

## 3. Memory Management

### 3.1 Architecture

- **Physical memory**: Two-phase buddy allocator (orders 0-9, 4KB-2MB). Bitmap
  pre-physmap, intrusive free lists post-physmap. O(1) alloc, O(log n) free with
  coalescing via XOR buddy addressing.
- **Virtual memory**: PageTableManager with hybrid 2MB/4KB pages. Physmap avoids
  temporary kernel PTEs. Stack verification before CR3 switch.
- **Frame capabilities**: FrameId registry with bidirectional lookup (FrameId→phys,
  phys→FrameId) and map_count reference tracking.
- **Heap**: Lazy allocation via page fault handler. Guard at 0x1000 (NULL protection).

### 3.2 Strengths

- Intrusive free lists stored in free pages (zero external allocation overhead)
- 2MB huge pages for aligned bulk RAM (6x fewer TLB misses)
- Physmap design eliminates temporary kernel PTEs (security win)
- Device MMIO pages marked NO_CACHE, excluded from PMM teardown
- Frame registry provides ownership tracking and reference counting

### 3.3 Weaknesses

| Issue | Severity | Detail |
|---|---|---|
| No TLB shootdown | CRITICAL (SMP) | Local INVLPG only. Other CPUs see stale entries. |
| Teardown race | CRITICAL (SMP) | Process::Drop doesn't synchronize with scheduler on other CPUs. |
| No Copy-on-Write | HIGH | map_to() allows same frame in multiple spaces with no write protection. |
| No guard page (space.rs) | HIGH | Address space stack region has no guard page at bottom (pthread stacks do). |
| Frame registry not transactional | MEDIUM | Race between lookup_by_phys and dec_map_count. |
| Double-free silent | MEDIUM | bitmap_set_used returns without error if bit already set. |
| BTreeMap overhead | LOW | Frame registry uses BTreeMap for both directions. O(log n) per operation. |

### 3.4 vs. Production

- **Linux**: Slab allocator (SLUB) + RCU teardown + per-CPU freelists + CoW + TLB
  shootdown IPI. CLUU has none of these.
- **seL4**: Capability-based untyped memory with explicit revocation. More
  disciplined than CLUU's frame registry.
- **Zircon**: VMO (virtual memory objects) with ownership semantics. More
  sophisticated than CLUU's frame model.

**Rating: 6.5/10** — Correct for single-CPU. SMP requires TLB shootdown, teardown
synchronization, and per-CPU page freelists.

---

## 4. Security & Capability Model

### 4.1 Token Architecture

Token = HMAC-SHA256 signed {OpaqueScope(128-bit random), Rights, Issuer, Expiry}.

- **OpaqueScope**: 128-bit CSPRNG random. Non-enumerable, non-guessable.
- **Rights**: Bit mask (IPC_SEND, IPC_RECV, IPC_CALL, SPACE_MAP, GRANT, CREATE, etc.)
- **Signature**: HMAC-SHA256(scope||rights||issuer||expiry, kernel_secret[32])
- **Verification**: Constant-time XOR comparison (timing-attack resistant)

16 sharded token table, 65536 max tokens. Generation counter invalidates all
per-CPU caches on any revocation.

### 4.2 Derivation Invariant

```rust
// token/mod.rs:330-338 — Rights can ONLY narrow
pub fn derive(&self, new_rights: Rights, ...) -> Option<Token> {
    if !self.role.contains(new_rights) { return None; }  // Escalation impossible
    if new_expiry > self.expire_at { return None; }      // Cannot extend
    // ...
}
```

This is the **security crown jewel**: capability monotonic narrowing is enforced
arithmetically. A child token can never have rights its parent lacked.

### 4.3 Syscall Security

- **Userspace pointer validation**: NULL check, USERSPACE_MAX boundary, checked_add
  overflow, per-page USER flag verification via page table walk.
- **RFLAGS sanitization**: Clears TF/IOPL/NT/RF/AC, sets IF on all return paths.
- **SYSRET bug**: Canonical RCX check with IRETQ fallback.
- **IST stacks**: Dedicated stacks for GPF (IST 1) and PF (IST 2).

### 4.4 Attack Surface Analysis

| Vector | Severity | Status |
|---|---|---|
| Token forgery (brute HMAC) | Infeasible | 2^256 HMAC-SHA256 |
| Userptr to kernel memory | Blocked | USERSPACE_MAX + per-page check |
| Reply ID brute-force | Infeasible | 2^64 monotonic space |
| TOCTOU pointer attack | Mitigated | Per-page translation in copy_from_user |
| Message sender forgery | Blocked | Kernel-injected sender TID |
| Endpoint message flooding | Partial | Bounded queue (1024) but no per-sender limit |
| Spectre V2 | Mitigated (IBPB+STIBP+retpoline) | IBPB on cross-CR3 switch, STIBP at boot, retpoline in kernel triplet. Meltdown/KPTI deferred (QEMU/modern HW has HW fix; 30%+ perf cost). |
| SMAP/SMEP | Enabled | CR4 bits 20-21 set at boot; CLAC on all kernel entries (syscall + 4 interrupt stubs). |

### 4.5 vs. seL4 / Fuchsia

- **seL4**: Binary capability format (more efficient), formal verification, CSpace
  structure. CLUU's HMAC approach is unique — more conservative (expiring tokens)
  but less space-efficient than binary caps.
- **Fuchsia**: Handle-based with fine-grained rights hierarchy. Kernel validates
  all syscalls. CLUU delegates more to userspace via tokens.

**Rating: 9.2/10** (was 8.7) — Strongest subsystem. SMAP/SMEP + Spectre V2
(IBPB/STIBP/retpoline) landed; password hashing + rate limiting + audit
logging landed. KPTI deferred.

---

## 5. Scheduler & Threading

### 5.1 Architecture

Linux 2.6-era **O(1) priority bitmap** with 256 levels. Active/expired arrays with
epoch-based fairness. Default quantum: 10 ticks at 250Hz = 40ms.

Two modes:
- **INITMODE** (cooperative): For ordered boot. Only threads without COOPERATIVE flag
  get preempted.
- **NORMALMODE** (preemptive): Full preemption via timer interrupt.

### 5.2 Context Switch

184-byte TCB (repr(C, align(64))). Key fields:
- GPRs (RAX-R15, RBP, RSP): offsets 0x00-0x78
- RIP: 0x80, RFLAGS: 0x88, CS: 0x90, SS: 0x98
- CR3: 0xA0, FS_BASE: 0xA8 (TLS)

FS_BASE saved via `rdmsr(MSR_FS_BASE=0xC0000100)` and restored via `wrmsr` on
every context switch and all fault handler paths.

### 5.3 Fault Handling

seL4-style fault endpoints. FaultState captures: fault_type, fault_addr, error_code,
saved_context, reply_id. Fault message (label 0xFA017) sent to thread's
fault_endpoint. Handler replies with label=0 (resume) or nonzero (kill).

IST-safe deferred fault queue (4 slots) using lock-free CAS for IST exception
safety. Drained every timer tick.

### 5.4 Strengths

- O(1) scheduling with provable fairness (epoch guarantee)
- Correct SWAPGS on all 9 return-to-userspace paths (verified)
- Recv-wait ticket system prevents IPC delivery races
- Assembly register save order matches repr(C) struct (verified)
- IRETQ frame construction verified (SS, RSP, RFLAGS, CS, RIP order)

### 5.5 Weaknesses

| Issue | Severity | Detail |
|---|---|---|
| ~~No FPU/SSE save~~ | ~~MEDIUM~~ FIXED | Eager FXSAVE/FXRSTOR at all kernel entry/exit points (commit dddd98e) |
| Shared BSP_STACK | HIGH (SMP) | Single 64KB kernel stack shared by all threads |
| ~~4-slot deferred fault queue~~ | ~~MEDIUM~~ FIXED (H9) | Expanded to 16 slots; overflow counter added (but not yet exposed to userspace — R3) |
| ~~8-slot pending wake queue~~ | ~~MEDIUM~~ FIXED (H10) | Expanded to 32 slots; overflow counter added (but not yet exposed — R3) |
| No priority inheritance | LOW | Priority inversion possible on IPC |
| Fixed 40ms quantum | LOW | No per-priority adjustment (unlike CFS) |
| TID 1 hardcoded as init | LOW | Fragile assumption if boot order changes |

### 5.6 vs. Linux CFS / seL4 MCS / QNX

| Feature | CLUU | Linux CFS | seL4 MCS | QNX |
|---|---|---|---|---|
| Algorithm | O(1) bitmap | Red-black tree | Budget-based | Adaptive |
| Time quantum | Fixed 40ms | Dynamic per nice | Per-SC budget | Partition budget |
| Fairness | Epoch-based | Proportional vruntime | Budget enforcement | Partition guarantee |
| SMP | No | Yes | Yes | Yes |
| FPU save | Yes (eager) | Yes (lazy/eager) | Yes | Yes |
| RT guarantee | No | SCHED_FIFO/RR | Yes | Yes |

**Rating: 8.5/10** — Excellent single-CPU design with FPU save. Missing SMP.

---

## 6. Userspace Service Architecture

### 6.1 Service Hierarchy

```
Kernel
└── Init (bootstrap, primordial monitoring)
    ├── registry    (service discovery, capability distribution)
    ├── timeserver  (clock, sleep)
    ├── procmgr     (process/container/session management — 5,157 lines)
    ├── vfs         (filesystem, 4 backends — 3,483 lines)
    └── virtio-blk  (block storage, ext2)
        └── Procmgr spawns:
            ├── kbd, console, tty, vtmgr (system services)
            ├── shell (per-session)
            └── containers (user applications)
```

### 6.2 VFS Architecture

4 backend types via `MountBackend` trait:
- **InitrdBackend**: Tar archive in memory (O(n) lookup)
- **RemoteBackend**: IPC forwarding to external service (virtio-blk/ext2)
- **DeviceBackend**: /dev/{null,zero,urandom,tty0-4,console}
- **ProcfsBackend**: Dynamic /proc via procmgr IPC queries

File cache: LRU, 32MB total, 8MB per file. 4 shared rings (64KB each) for bulk
remote reads.

### 6.3 Registry (Capability Distribution)

seL4-style grant protocol:
1. Service A subscribes to "ServiceB:output"
2. Registry forwards grant request to ServiceB
3. ServiceB grants endpoint token to Registry
4. Registry delivers token to Service A

Owner verification: only the original registrant (sender_tid) can update or
unregister an output. O(1) pending subscription lookup via BTreeMap.

### 6.4 Console & VT

4 independent VtScreens with ANSI CSI parser, 200-line scrollback per VT,
dirty-cell tracking for partial redraws, atomic VT switching (single IPC message).

### 6.5 Strengths

- Clean separation: init handles boot, procmgr handles user-facing services
- Declarative service specs (SERVICE_LIST is data-driven)
- Init primordial monitoring (death of registry/timeserver/procmgr/vfs/virtio-blk
  triggers init panic)
- VFS plugin architecture (easy to add backends)
- Atomic VT switch prevents render glitches

### 6.6 Weaknesses

| Issue | Severity | Detail |
|---|---|---|
| No pipe support in shell | HIGH | `cat file \| grep foo` doesn't work |
| Single-threaded VFS | HIGH | One slow remote op blocks all mounts |
| No TTY line discipline | HIGH | No echo, no canonical mode, no stty |
| Registry is SPOF | MEDIUM | No redundancy or fallback |
| 15+ BTreeMaps in procmgr | MEDIUM | O(log n) chains compound at scale |
| Ad-hoc IPC labels | LOW | No central registry, collision possible |

**Rating: 7.5/10** — Sound architecture at ~85% completion.

---

## 7. Process Isolation & Containers

### 7.1 Isolation Model

Isolation is **exclusively capability-based** (not UID-based):

1. **Address space**: Per-process page tables (CR3). No cross-process memory access.
2. **Token isolation**: Processes can only IPC to endpoints they hold tokens for.
   Token derivation only narrows rights — escalation is arithmetically impossible.
3. **VFS view isolation**: Per-client mountlist. Unauthorized paths return ENOENT
   (concealment, not EACCES).
4. **Container isolation**: Each process tagged with container_id. Private storage
   per container (/var/containers/c-{id}/{data,tmp,log}).

### 7.2 Container System

Every process runs in exactly one container. ContainerInstance tracks: name,
instance_name, session_id, container_id, parent_container_id, pid, image_path,
restart_policy, restart_count.

**Restart policies**:
- **Never**: Exit → cleanup
- **Always**: Exit → immediate restart (safety valve: max 10 restarts/60s, then
  exponential backoff)
- **OnFailure**: Non-zero exit → restart with backoff (1s, 2s, 4s, ..., 30s cap).
  Window-based crash loop detection.

**Cascading cleanup**: Container death recursively destroys all children. 8-level
nesting limit prevents resource exhaustion.

**Instance naming**: Per-session counter ("editor", "editor.2", "editor.3").

### 7.3 Session Management

Session = top-level user container bound to VT + user identity.

```
login → procmgr validates /etc/users.toml → spawn shell
     → build VFS view (user profile + home dir)
     → wire VT stdin to shell
     → insert into session_table
```

Session isolation: different sessions can't see each other's processes, filesystems,
or endpoints. Admin can see all via /proc; regular users see only own session.

### 7.4 User/Permission Model

Users defined in `/etc/users.toml`. CapProfile bits:

| Bit | Name | Description |
|---|---|---|
| 0 | IPC | Create endpoints, send/receive |
| 1 | SPAWN | Request procmgr to spawn |
| 2 | REGISTRY | Subscribe/register services |
| 3 | VFS | Access filesystem |
| 4 | DEVICE | Hold device tokens (IRQ, PCI) |
| 5 | SPACE_GRANT | Grant memory pages |
| 6 | NET | (Reserved) |
| 7 | ADMIN | System-wide admin ops |

Predefined profiles: SANDBOXED (0x01), USER (0x0F), ADMIN (0x8F), SERVICE (0x3F),
SUPERVISOR (0xFF).

**sudo**: Validates password, spawns elevated container up to user's `escalate`
ceiling. `su`: Requires capability narrowing (caller must outrank target).

### 7.5 Weaknesses

| Issue | Severity | Detail |
|---|---|---|
| ~~Plaintext passwords~~ | ~~CRITICAL~~ FIXED (SEC-1.2) | SHA-256 + RDRAND salt (`$sha256$<salt>$<hash>`), constant-time verify. Legacy plaintext path still accepted for migration (no in-tree users have a real hash yet — R6). |
| ~~No login rate limiting~~ | ~~HIGH~~ FIXED (SEC-1.3) | Per-user exponential backoff `min(2^(f-1), 300)s`, applied to login + sudo + su. |
| No MAC (SELinux-style) | HIGH | ADMIN = god mode, no compartmentalization |
| ~~No resource quotas~~ | ~~HIGH~~ FIXED (H4, v1) | `max_processes` + `max_priority` enforced per-container in procmgr. Memory/CPU/FD limits still absent. |
| ~~No audit logging~~ | ~~HIGH~~ FIXED (H6, v1) | Structured `AUTH_LOGIN_{OK,FAIL,RATE}`, `AUTH_SUDO_{OK,FAIL}`, `AUTH_SU_{OK,FAIL}` events via `audit_log()` in procmgr. |
| No endpoint quotas | MEDIUM | Malicious service can create unlimited endpoints |
| Procmgr is SPOF | MEDIUM | Compromised procmgr = total system compromise |
| RDRAND silent zero-salt | MEDIUM (R2) | On RDRAND failure, salt degrades to zeros without warning. Fix before landing. |

### 7.6 vs. Docker / Fuchsia

| Aspect | CLUU | Docker | Fuchsia |
|---|---|---|---|
| Isolation | Capabilities + VFS views | Namespaces + cgroups | Capabilities + components |
| Resource limits | None | cgroups (strong) | Component manifest |
| Restart policies | Backoff + crash loop | Simple restart | Component policy |
| Authentication | Plaintext (weak) | OAuth/token | N/A |
| Nesting | 8-level cascade | Composable images | Strict hierarchy |

**Rating: 8.5/10** (was 7.8) — Architecturally clean. Password hashing,
rate limiting, audit logging, and first-tier resource quotas (max_processes,
max_priority) have landed. Remaining gaps: MAC, memory/CPU/FD quotas,
endpoint per-sender quotas.

### 7.7 Novelty Assessment: Containers as an OS Primitive

#### The Interesting Question

The individual isolation *mechanisms* (capabilities, VFS views, HMAC tokens) are
borrowed — seL4, Plan 9, JWT. That's not where the novelty lies. The more
interesting question is: **what does it mean that a microkernel makes "container"
the primary unit of process organization, designed in from day one?**

In every other system, the container concept was grafted onto something else:

| System | Process Primitive | Container Story |
|---|---|---|
| **Unix/Linux** | Process (fork/exec, PID) | Containers bolted on 30 years later via namespaces + cgroups (2008). A process can exist outside any container. |
| **Docker** | Linux process in namespace | Userspace tooling over kernel namespaces. The kernel doesn't know what a "container" is. |
| **Kubernetes** | Pod (group of Docker containers) | Orchestration layer over Docker over Linux. Three layers of indirection. |
| **seL4** | Thread + CSpace | No container concept at all. Userspace defines its own grouping. |
| **QNX** | Process (message-passing) | No container concept. Resource partitions exist but are separate from process identity. |
| **Fuchsia** | Component (manifest-declared) | Closest to CLUU. Components are isolated units with declared capabilities. But components are a *framework* concern, not an OS-level identity. |
| **Genode** | Subsystem (recursive) | Subsystems are containers of sorts, but the model is recursive virtualization, not lifecycle management. |
| **FreeBSD Jails** | Process in jail | Jails are a kernel primitive, but a process can exist outside a jail. Jails were retrofitted (2000). |
| **Solaris Zones** | Process in zone | Zones are a kernel primitive. Again retrofitted (2005). Non-global zones optional. |

#### What CLUU Does Differently

In CLUU, there is **no such thing as a process outside a container**. The container
is not a namespace wrapper, not an orchestration layer, not an optional jail. It is
the fundamental unit of:

- **Identity**: Every PID maps to exactly one container_id (`pid_to_container_id`)
- **Lifecycle**: Entrypoint death = container death = cascade kill all children
- **Policy**: Restart behavior is a container property, declared at build time in the Cluufile
- **Capability scope**: CapProfile (what tokens the process receives) is per-container
- **Filesystem view**: VFS mounts are bound to the container, not the process
- **Session membership**: Walk `parent_container_id` chain upward to find the owning session
- **User visibility**: `container list` is the *only* way to see running workloads

Meanwhile, the kernel knows nothing about containers. It has threads, address
spaces, endpoints, and tokens. The container abstraction lives entirely in procmgr
(userspace), built on top of those primitives via IPC.

This is a specific architectural position that none of the systems above occupy:

```
                    Kernel-level containers    Userspace containers
                    ────────────────────────   ─────────────────────
Optional:           FreeBSD Jails              Docker, Kubernetes
                    Solaris Zones

Universal:          (nobody)                   CLUU
(no escape hatch)
```

FreeBSD jails and Solaris zones are kernel-level but optional — a process can exist
in the "global zone." Docker containers are universal within Docker but are a
userspace illusion over kernel namespaces — the kernel sees ordinary processes.
Fuchsia components are the closest parallel, but they're a framework concern (the
component runner), not an OS-level identity that procmgr, VFS, /proc, and the shell
all agree on.

CLUU occupies the bottom-right cell: containers are universal (no process can
escape) but entirely userspace (the kernel is unaware). This means:

1. **The kernel stays minimal** — no container syscalls, no namespace machinery.
   The 7-syscall surface is unchanged whether you have 1 container or 100.
2. **Policy is replaceable** — swap procmgr for a different implementation and
   the container model changes without kernel modification.
3. **Containers compose with capabilities** — a container's isolation isn't
   enforced by namespace flags but by which tokens it holds. Fewer tokens = more
   isolated. The container boundary and the capability boundary are the same thing.

#### Why This Matters (And Why It's Modest)

The honest assessment: this is a **design insight, not a research breakthrough**.

The insight is that if your kernel gives you strong-enough capability primitives
(unforgeable tokens, per-process address spaces, IPC-only communication), you don't
*need* kernel-level container support. The container abstraction falls out naturally
from a userspace process manager that tracks parent-child relationships and hands
out narrowed tokens. No namespaces. No cgroups. No special syscalls. Just IPC and
capabilities.

This is philosophically cleaner than Linux's approach (where containers are a pile
of 7 independent namespace types + cgroups v2 + seccomp filters + AppArmor/SELinux
profiles, none of which were designed to work together). It's also less capable —
CLUU has no resource quotas, no CPU/memory limits, no I/O bandwidth controls.
Linux's complexity exists for reasons.

The closest existing articulation of this idea is in the **Genode** project, which
argues that recursive subsystem composition over capability primitives obviates
container mechanisms. CLUU arrives at a similar conclusion through a more pragmatic,
less recursive design. Fuchsia's component model is converging on the same idea
from the opposite direction (top-down framework rather than bottom-up process
manager).

#### Novelty Verdict

| Aspect | Assessment |
|---|---|
| Container as universal mandatory boundary | **Moderately novel**. No other microkernel does exactly this. FreeBSD/Solaris have mandatory-capable jails/zones but they're kernel-level and retrofitted. |
| Container = capability boundary (no namespace machinery) | **Novel synthesis**. The idea that capabilities alone suffice for container isolation, without any namespace mechanism, is implicit in seL4/Genode literature but CLUU is one of the first to actually build a complete container UX on top of it. |
| Declarative Cluufile → manifest → token grants | **Minor novelty**. Similar to Fuchsia .cml manifests. CLUU's version is simpler and more Docker-like in syntax. |
| Session-as-container-tree (parent_container_id walk) | **Not novel**. Unix process groups + sessions. More structured, same semantics. |
| Restart policies on containers | **Not novel**. Docker/Kubernetes/systemd. Identical concepts, similar syntax. |
| Build-time capability binding | **Interesting but not novel**. Fuchsia does this. Android manifest permissions do this. The Cluufile is just a simpler version. |

**Bottom line**: The individual mechanisms are borrowed. The isolation *techniques*
are prior art. But the architectural decision — that a microkernel's userspace can
provide a complete, mandatory, no-escape-hatch container experience using only
capability tokens and IPC, with zero kernel container support — is a genuine
contribution to the design space. It's not a research paper, but it's a proof of
concept that this design point is viable, and it's cleaner than anyone else's
implementation of the same idea.

CLUU demonstrates that **containers don't need to be a kernel feature**. They can
be an emergent property of strong capabilities. That's worth something.

---

## 8. POSIX Compatibility Layer

### 8.1 Coverage: ~65-70% POSIX-2008 Base

| Area | Coverage | Implemented | Missing |
|---|---|---|---|
| File I/O | 90% | open, read, write, close, stat, lseek, dup/dup2, mkdir, rmdir, unlink, rename | fcntl locks, ioctl, symlinks |
| Process | 60% | posix_spawn, waitpid, getpid, _exit | **fork, exec** (returns ENOSYS) |
| Threads | 95% | create/join/detach, mutex, cond, once, key, TLS | pthread_cancel |
| Signals | 50% | signal, sigaction (partial), raise | Async delivery, sa_mask, sigprocmask |
| Time | 85% | gettimeofday, clock_gettime, sleep, nanosleep | CLOCK_THREAD_CPUTIME_ID, alarm |
| I/O Mux | 90% | poll, select | epoll, kqueue |
| Memory | 80% | mmap (anon + file), munmap, mprotect, sbrk | mremap, MAP_SHARED, mlock |

### 8.2 Key Design Decisions

- **No fork()**: Returns ENOSYS. Spawn model (posix_spawn) only. This is
  architecturally correct for a microkernel but breaks applications expecting fork.
- **Signals not async**: Handlers only invoked via raise(), not from faults/interrupts.
- **Feature-gated allocator**: `#[cfg(feature = "c-runtime")]` delegates to newlib
  malloc; otherwise uses linked-list allocator with dynamic heap growth.
- **4 fds by default**: stdin(0), stdout(1), stderr(2), stdlog(3). Differs from Unix.

### 8.3 Threading Excellence

TLS variant II (x86_64): FS:0 = TCB self-pointer, FS:8 = thread token, FS:16..528
= 64 pthread_key values. Stack guard page per pthread. Futex-based 3-state mutex
(unlocked=0, locked=1, contended=2). Up to 4 iterations of key destructors per
POSIX spec.

**Rating: 7.0/10** — Strong for embedded/microkernel. Fork-less design limits
general app compatibility.

---

## 9. Device Drivers & Hardware Abstraction

### 9.1 What Works

| Driver | Status | Notes |
|---|---|---|
| PIC (8259) | Working | Legacy only, remapped to vectors 32-47 |
| LAPIC timer | Working | TSC calibration via PIT (median of 3) |
| virtio-blk | Working | Dual-mode (legacy + modern), polling only |
| ext2 | Read-only | Superblock/inode/directory support |
| Keyboard | Working | US + Hungarian layouts, scancode → ASCII |
| Console | Working | 4 VTs, ANSI CSI parser, scrollback, dirty tracking |
| Framebuffer | Working | Direct FB access, dirty region bounding |

### 9.2 What's Missing

| Gap | Severity | Impact |
|---|---|---|
| No IOAPIC/MSI-X | CRITICAL | PIC only. Cannot use modern PCIe or SMP. |
| No network | CRITICAL | No virtio-net, no TCP/IP |
| No USB (XHCI) | HIGH | No modern peripherals |
| No AHCI/NVMe | HIGH | Only virtio-blk |
| No mouse | HIGH | No PS/2 or USB pointer |
| No audio | MEDIUM | No AC97/HDA |
| Serial UART stubbed | MEDIUM | Interrupt handler logs but never reads UART |
| virtio-blk polling | MEDIUM | No interrupt-driven I/O, busy-waiting |
| No scancode up events | MEDIUM | Blocks games/raw input |
| ext2 read-only | HIGH | No file creation |

### 9.3 vs. Linux

About **1/10 feature parity** in drivers. Architecturally sound (userspace drivers
per microkernel design), but minimal feature set.

**Rating: 3.5/10** — Sufficient for QEMU demo. Unusable on real hardware without
IOAPIC.

---

## 10. Build System & Testing

### 10.1 Build Pipeline

Rust-based xtask (3,976 LOC) with 5 parallel-safe phases:
1. Dependencies (klibcluu, libcluu, newlib, crt0)
2. Kernel (NASM assembly + Cargo with custom triplet)
3. Init primordials (6 crates)
4. Containers (41 auto-discovered from containers/*)
5. Packaging (initrd tar.gz, ext2 userdisk, BOOTBOOT disk image)

Rich terminal UI with per-task progress bars, live logs, tree rendering.

### 10.2 Container System (Cluufiles)

Declarative syntax: FROM, PROFILE, ENTRYPOINT, BUILD, COPY, ENV, RESTART, DEVICES,
DENY. Auto-discovered from containers/ directory. Generates manifest.toml with
capability whitelist.

### 10.3 Test Infrastructure

- Unit tests: cargo xtask test (kernel tests with mock mode)
- Integration harness: QEMU + keystroke injection + regex matching
- 50+ test scenarios with matrix sweep
- SLO fairness sweep (scheduling variance measurement)
- GDB integration (pause at startup, auto/manual modes)

### 10.4 CI/CD

Single workflow: repo hygiene checks (no tracked binaries, no generated files,
binary MIME scan). **No full-build CI** (no system to build/test on).

**Rating: 8.0/10** (hobby) / 5.0/10 (production) — Developer-friendly but lacks
automated build/test.

---

## 11. Scorecard & Production Comparison

### Subsystem Ratings

Ratings updated 2026-04-21 to reflect the uncommitted hardening work.

| Subsystem | Score | Δ | Notes |
|---|---|---|---|
| IPC & Syscalls | 8.8/10 | +0.3 | Tier-1 optimizations (T1.1–T1.5) + async notifications (A2); grants still stubbed |
| Security & Capabilities | 9.2/10 | +0.5 | SMAP/SMEP + Spectre V2 + retpoline + password hashing + rate limiting + audit logging |
| Scheduler & Threading | 8.7/10 | +0.2 | Fault/wake queues enlarged (H9/H10); still single-CPU |
| Userspace Services | 7.5/10 | 0 | ~85% complete, missing pipes/TTY, now includes tpmd |
| Process Isolation | 8.5/10 | +0.7 | Hashed passwords, rate limit, audit log, max_processes/max_priority quotas (H4) |
| POSIX Layer | 7.0/10 | 0 | Sufficient for embedded |
| Memory Management | 6.5/10 | 0 | Correct but SMP-unsafe |
| Device Drivers | 3.5/10 | 0 | Minimal (QEMU only) |
| Build System | 8.0/10 | 0 | Developer-friendly |
| **OVERALL** | **7.7/10** | +0.4 | Modulo R1/R2 before the hardening commits land |

### Where CLUU Sits

```
Production ─────────────────────────────────── Toy
Linux  QNX  Fuchsia  seL4  Redox  CLUU  xv6  MikeOS
```

CLUU sits between Redox and seL4 — better capability semantics than Redox, more
complete userspace than seL4, but lacks driver ecosystem and SMP of either.
Significantly more advanced than educational kernels (xv6, MINIX 3).

### What CLUU Does Better Than Expected

1. **IPC performance** (1,195-1,625 cycles) rivaling seL4 research kernels
2. **Capability model** — monotonic derivation, HMAC-signed, time-bounded
3. **Code clarity** — 68K lines of readable Rust vs. Linux's 30M
4. **Container restart policies** — exponential backoff + crash loop detection
5. **VFS view concealment** — ENOENT instead of EACCES (security best practice)

### What's Genuinely Missing for Production

1. **SMP**: Single CPU, shared BSP_STACK, no TLB shootdown, no IOAPIC
2. ~~**FPU/SSE**: Any SIMD code corrupts across context switches~~ — FIXED (commit dddd98e)
3. ~~**Password hashing**: Plaintext is a deal-breaker~~ — FIXED (SEC-1.2), modulo R2 (RDRAND fallback)
4. **Resource quotas**: Only max_processes + max_priority (H4 v1); memory/CPU/FD limits absent
5. **Network stack**: Can't communicate with the outside world
6. **Filesystem writes**: ext2 is read-only
7. **Grant/Map zero-copy**: Transfer mechanism is stubbed
8. **Formal verification**: seL4's key advantage

---

## 12. Consolidated TODO List

*Last updated: 2026-03-11. Items marked ✅ are complete, 🔧 are in progress.*

---

### 12.1 Bugs & Resource Leaks

| # | Priority | Status | Description | Location |
|---|---|---|---|---|
| B1 | LOW | ✅ | sys_call inline path limited to 16 bytes — inconsistent with sys_send/sys_reply 4-chunk pattern | handlers.rs:477-484 |
| L1 | LOW | ✅ | Frame registry map_count leak: token re-lookup failure on error path skips dec_map_count | handlers.rs:1330-1434 |
| L2 | LOW | ✅ | Reply map cleanup on thread death scans all REPLY_MAP_SLOTS — O(n), acceptable at current size | thread_manager.rs:420-447 |

---

### 12.2 Security Hardening

| # | Priority | Status | Description | Files |
|---|---|---|---|---|
| SEC-1.1 | CRITICAL | ✅ | **Enable SMAP/SMEP** — CR4 bits 20-21, CLAC on all kernel entries (syscall, timer, GPF, PF, generic fault) | main.rs:129-152, syscall_entry.asm, interrupts.asm |
| SEC-1.2 | CRITICAL | ✅ | **Hash passwords** — SHA-256 with RDRAND salt, constant-time verify, `$sha256$<salt>$<hash>` format | procmgr/main.rs, libcluu/crypto.rs |
| SEC-1.3 | HIGH | ✅ | **Login rate limiting** — exponential backoff (1s→5min cap), per-user LoginAttempt tracking | procmgr/main.rs |
| SEC-2.1 | MEDIUM | ✅ | **TPM 2.0 TIS driver** — userspace `tpmd` service, full TIS FIFO protocol, TPM2 command builders, IPC server with stub mode | userspace/tpmd/, libcluu/ipc.rs |
| SEC-2.2 | MEDIUM | ✅ | **Measured boot** — init extends PCRs via tpmd IPC: PCR9 (initrd), PCR14 (×N primordial binaries), best-effort | userspace/init/src/measured_boot.rs |
| SEC-3 | LOW | ✅ | **TPM sealed storage** — PoC seal/unseal round-trip via tpmd IPC (CreatePrimary+Create+Load+StartAuthSession+PolicyPCR+Unseal); stub mode graceful skip | tpmd/main.rs, init/sealed_storage.rs |
| SEC-4 | LOW | ✅ | **Remote attestation** — AIK creation (RSA-2048 signing) + TPM2_Quote (PCR 9+14 signed attestation); stub mode graceful skip | tpmd/main.rs, init/attestation.rs |
| SEC-5.1 | LOW | ✅ | **Spectre V2 (IBPB/STIBP)** — CPUID detection, IBPB on cross-CR3 context switch, STIBP set at boot; graceful no-op if unsupported | spectre.rs, thread_manager.rs |
| SEC-5.2 | LOW | DEFERRED | **KPTI** — Meltdown mitigation. Deferred: affects only pre-2018 Intel; QEMU/modern HW has hardware fix. 600+ lines, 30%+ perf cost. IBPB+Retpoline cover Spectre V2. | vmm.rs (major) |
| SEC-5.3 | LOW | ✅ | **Retpoline** — `+retpoline` target feature in kernel JSON; LLVM generates thunks for all indirect calls/jumps | triplets/x86_64-cluu-kernel.json |
| H4 | HIGH | ✅ | **Resource quotas v1** — max_processes + max_priority enforcement in procmgr (userspace-only) | procmgr/main.rs |
| H5 | — | ✅ | **FPU/SSE context save/restore** — eager FXSAVE/FXRSTOR in assembly, per-CPU scratch buffer (gs:0x80) | commit dddd98e |
| H6 | HIGH | ✅ | **Audit logging v1** — structured auth event logging (login/sudo/su success+fail) via debug_print | procmgr/main.rs |
| H9 | MEDIUM | ✅ | **Expand deferred fault queue** 4→16 slots, overflow counter added | thread_manager.rs |
| H10 | MEDIUM | ✅ | **Expand pending wake queue** 8→32 slots, overflow counter added | thread_manager.rs |

---

### 12.3 IPC Performance (Current: 1,195–1,625 cycles)

**Tier 1 — Inline Fast-Path (target: 1,000–1,300 cycles):**

| # | Status | Optimization | Est. Savings | Location |
|---|---|---|---|---|
| T1.1 | ✅ | **Inline syscall dispatch** — jump table in asm for syscalls 0-3 (send/recv/call/reply), dedicated `extern "C"` entry points skip SyscallNumber parse + dispatch match | 20–25 cycles | syscall_entry.asm, syscall.rs |
| T1.2 | ✅ | **Consolidate endpoint double-lock** — removed inner Mutex from EndpointShard, shard lock sufficient (single-CPU, non-reentrant). 11 call sites updated. | 8–12 cycles | endpoint.rs |
| T1.3 | ✅ | **Expand token cache** 4→8 entries, LRU linear scan (no hash change needed) | 10–15 cycles | table.rs |
| T1.4 | ✅ | **Thread-local ReplyMap** — PerCpuReplyMap<UnsafeCell> replaces Mutex, lock-free access | 15–20 cycles | thread_manager.rs |
| T1.5 | ✅ | **Reduce fast-path register save** — removed 5 callee-saved pushes, keep only R15 + 8-byte alignment pad | 15–20 cycles | syscall_entry.asm |

**Tier 2 — Structural (target: 900–1,100 cycles):**

| # | Status | Optimization | Est. Savings | Location |
|---|---|---|---|---|
| T2.1 | TODO | **Cache ObjectRef in Token** — eliminate scope→ObjectRef BTreeMap lookup | 15–20 cycles | table.rs, scope.rs |
| T2.2 | SKIP | **Cache first waiter pointer** — REJECTED: VecDeque front is already O(1), complexity outweighs ~5-15 cycle benefit | 10–15 cycles | endpoint.rs |
| T2.3 | TODO | **Lazy timestamp validation** — defer TSC read to cache miss only | 8–12 cycles | table.rs |
| T2.4 | SKIP | **Batch scheduler ops** — REJECTED: wake_thread() already called outside shard lock, deferred wake mechanism (try_lock + queue_pending_wake) already exists | 10–15 cycles | scheduler.rs |
| T2.5 | TODO | **Cache-line align hot structs** — 64-byte boundaries | 5–8 cycles | multiple |

**Tier 3 — Architectural (target: <800 cycles, only if Tier 1+2 insufficient):**

| # | Status | Optimization | Est. Savings | Notes |
|---|---|---|---|---|
| T3.1 | TODO | **Register-passable messages** — no memcpy for ≤48B | 20–30 cycles | Protocol change |
| T3.2 | TODO | **Implicit thread state** — inject reply into caller's RAX | 15–20 cycles | Major scheduler refactor |
| T3.3 | TODO | **Lock-free endpoint queues** — atomic push/pop | 30–50 cycles | Only viable post-SMP |
| T3.4 | TODO | **Direct IPC path** — bypass queue when receiver blocked | 40–60 cycles | seL4's key optimization |

---

### 12.4 SMP (Symmetric Multi-Processing)

| # | Status | Phase | Tasks |
|---|---|---|---|
| SMP-1 | TODO | **Per-CPU Foundation** | Per-CPU GDT+TSS, per-CPU IST stacks (3×MAX_CPUS), per-CPU PerCpuData array, per-CPU kernel stack (64KB each), MADT ACPI parse for CPU topology |
| SMP-2 | TODO | **IPI Infrastructure** | LAPIC ICR wrapper, IPI vectors 0xF0-0xFF (TLB shootdown, reschedule, halt, panic), TLB shootdown protocol (dirty bitmask + INVLPG + ACK), cross-CPU reschedule IPI |
| SMP-3 | TODO | **IOAPIC** | IOAPIC MMIO driver (from ACPI MADT), IRQ→CPU affinity / lowest-priority delivery, PIC disable after IOAPIC takeover, MSI-X support, IrqAck EOI for IOAPIC (PIC path already works) |
| SMP-4 | TODO | **Per-CPU Scheduler** | Per-CPU PriorityBitmapScheduler, work-stealing from expired queue, ThreadSetAffinity invoke op, periodic load balancing, CURRENT_THREAD in PerCpuData |
| SMP-5 | TODO | **Lock Refinement** | THREAD_REPOSITORY → RWLock, PMM → per-CPU page freelists, CALL_REPLY_MAP → per-thread, Frame registry → RCU reads |
| SMP-6 | TODO | **AP Bring-Up** | INIT+SIPI sequence, AP trampoline (16-bit→64-bit), per-CPU LGDT/LTR/GS-base/LAPIC-timer, CPU_READY_COUNT sync |

---

### 12.5 Userspace & Driver Ecosystem

**Tier 1 — Input & Application Ports:**

| # | Status | Task | Effort | Enables |
|---|---|---|---|---|
| U1 | TODO | **PS/2 mouse driver** — IRQ12 handler, relative motion, buttons | 200 LOC | Quake, GUI |
| U2 | TODO | **Raw keyboard scancodes** — key up/down events (not just ASCII) | 150 LOC | Games, raw input |
| U3 | TODO | **Shell pipe `\|` syntax** — parse pipe chains, wire FDs | 200 LOC | Composable commands |
| U4 | TODO | **MicroPython port** — REPL on VT, sched_yield stub, pthread_cancel patch | 1-2 weeks | Scripting |
| U5 | TODO | **sched_yield() wire-up** | 10 LOC | MicroPython threading |

**Tier 2 — Network Stack:**

| # | Status | Task | Effort | Detail |
|---|---|---|---|---|
| N1 | TODO | **virtio-net driver** | 1-2 weeks | Clone virtio-blk pattern, RX/TX virtqueues, DMA buffers |
| N2 | TODO | **ARP + IPv4** | 1 week | Address resolution, IP header parse/build |
| N3 | TODO | **UDP** | 3 days | Connectionless datagram, DNS resolution |
| N4 | TODO | **TCP** | 2-3 weeks | 3-way handshake, sliding window, retransmit, congestion |
| N5 | TODO | **Socket service** | 1-2 weeks | IPC-based socket API (socket/bind/connect/send/recv) |
| N6 | TODO | **DHCP client** | 3 days | Automatic IP configuration |

**Tier 3 — Storage & Filesystem:**

| # | Status | Task | Effort | Impact |
|---|---|---|---|---|
| S1 | TODO | **ext2 journaling** (ext3 compat) | 3-4 weeks | Crash-safe writes |
| S2 | TODO | **Symlink support** | 1 week | POSIX compliance |
| S3 | TODO | **virtio-blk interrupt mode** | 1 week | Eliminate polling |
| S4 | TODO | **AHCI/SATA driver** | 2-3 weeks | Real hardware disk support |
| S5 | TODO | **NVMe driver** | 2-3 weeks | Modern SSD support |
| S6 | TODO | **tmpfs** | 1 week | /tmp in RAM |

**Tier 4 — Multimedia & GUI:**

| # | Status | Task | Effort | Impact |
|---|---|---|---|---|
| M1 | TODO | **AC97 audio driver** | 2 weeks | Sound output |
| M2 | TODO | **Window manager service** | 3-4 weeks | Multi-window GUI |
| M3 | TODO | **MAP_SHARED mmap** | 1-2 weeks | Zero-copy framebuffer sharing |
| M4 | TODO | **virtio-gpu driver** | 3 weeks | Hardware-accelerated 2D/3D |
| M5 | TODO | **Quake 1 port** | 3-4 weeks | Flagship demo |
| M6 | TODO | **XHCI (USB 3.0)** | 4-6 weeks | Modern peripherals |

---

### 12.6 Architecture Improvements

| # | Priority | Status | Description | Location |
|---|---|---|---|---|
| A1 | HIGH | TODO | **Grant/map zero-copy** — implement actual page transfer in transfer.rs (currently stubbed) | transfer.rs |
| A2 | HIGH | ✅ | **Async notifications** — seL4-style Notification object with signal/wait/poll, 8-shard registry | ipc/notification.rs, token/scope.rs, token/table.rs |
| A3 | MEDIUM | TODO | **VFS multi-thread or async** — single-threaded VFS blocks all mounts on slow remote op | vfs/main.rs |
| A4 | MEDIUM | TODO | **Priority inheritance on IPC** — prevent priority inversion | endpoint.rs, scheduler.rs |
| A5 | LOW | TODO | **Unify sys_call inline** to 4-chunk pattern | handlers.rs (needs IPC redesign) |
| A6 | LOW | TODO | **Per-sender endpoint queue limits** — prevent single sender flooding | endpoint.rs |

---

## 13. Milestones

| Milestone | Description | Metric |
|---|---|---|
| **M0: Tier 0 complete** | FPU save + IrqAck | ✅ Done (2026-03-03) |
| **M1: Sub-1200 IPC** | IPC Tier 1 optimizations | Implementation ✅ (T1.1–T1.5); **measurement pending** on current tree. |
| **M5a: Measured boot** | SEC-2 (TPM + PCR extend) | ✅ Done (2026-03-11, uncommitted). PCR 9 = initrd; PCR 14 = each primordial; stub mode when TPM absent. |
| **M5b: Remote attestation** | SEC-4 (TPM2_Quote) | ⚠️ PoC done. Signature verification absent (R4). |
| **M2: MicroPython runs** | Port + REPL on VT | `>>> print("hello")` works. **Recommended next** — all prereqs met. |
| **M3: 2-CPU SMP boot** | SMP Phases 1-4 | Both CPUs scheduling independently |
| **M4: `ping` works** | Network Tier 2 (N1-N3) | ICMP echo request/reply via virtio-net |
| **M6: Sub-1000 IPC** | IPC Tier 2 optimizations | Median round-trip ≤1,000 cycles |
| **M7: `ssh` inbound** | Network Tier 2 complete (N4-N6) | TCP listener accepts connections |
| **M8: Quake playable** | Tier 1 input + Tier 4 port | 30fps software render with mouse+keyboard |

---

*Generated by 12 parallel deep-dive agents analyzing 68,000+ lines of source code.
Updated 2026-03-03 with Tier 0 completion (FPU/SSE, IrqAck).
Re-verified 2026-04-21 against uncommitted WIP on `develop`; see §0 for current
state, residual risks, landing strategy, and recommended next work.*
