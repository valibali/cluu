# CLUU Microkernel — Comprehensive Technical Audit

**Date**: 2026-03-02
**Scope**: Full codebase (68K Rust + 20K C/ASM, 184 source files, 30 crates)
**Method**: 9 parallel deep-dive analyses across every subsystem

---

## Table of Contents

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
- **No async notifications**: Unlike seL4's notification objects, CLUU only has
  blocking Send/Recv/Call. No way to signal without blocking.
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
| Spectre/Meltdown | Unmitigated | No STIBP/IBPB on kernel entry |
| SMAP/SMEP | Unmitigated | Not enabled (kernel can access userspace) |

### 4.5 vs. seL4 / Fuchsia

- **seL4**: Binary capability format (more efficient), formal verification, CSpace
  structure. CLUU's HMAC approach is unique — more conservative (expiring tokens)
  but less space-efficient than binary caps.
- **Fuchsia**: Handle-based with fine-grained rights hierarchy. Kernel validates
  all syscalls. CLUU delegates more to userspace via tokens.

**Rating: 8.7/10** — Strongest subsystem. SMAP/Spectre hardening needed for production.

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
| 4-slot deferred fault queue | MEDIUM | 5th concurrent IST fault dropped silently |
| 8-slot pending wake queue | MEDIUM | Lost wakes cause hung threads |
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
| Plaintext passwords | CRITICAL | /etc/users.toml stores passwords in plaintext |
| No login rate limiting | HIGH | Brute force possible via repeated login requests |
| No MAC (SELinux-style) | HIGH | ADMIN = god mode, no compartmentalization |
| No resource quotas | HIGH | No memory/CPU/FD limits. malloc loop = system OOM |
| No audit logging | HIGH | No record of logins, escalation, file access |
| No endpoint quotas | MEDIUM | Malicious service can create unlimited endpoints |
| Procmgr is SPOF | MEDIUM | Compromised procmgr = total system compromise |

### 7.6 vs. Docker / Fuchsia

| Aspect | CLUU | Docker | Fuchsia |
|---|---|---|---|
| Isolation | Capabilities + VFS views | Namespaces + cgroups | Capabilities + components |
| Resource limits | None | cgroups (strong) | Component manifest |
| Restart policies | Backoff + crash loop | Simple restart | Component policy |
| Authentication | Plaintext (weak) | OAuth/token | N/A |
| Nesting | 8-level cascade | Composable images | Strict hierarchy |

**Rating: 7.8/10** — Architecturally clean. Plaintext passwords and missing quotas
are critical gaps.

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

| Subsystem | Score | Notes |
|---|---|---|
| IPC & Syscalls | 8.5/10 | Architecturally excellent, stubbed grants |
| Security & Capabilities | 8.7/10 | Strongest subsystem, sound crypto |
| Scheduler & Threading | 8.5/10 | Solid single-CPU, FPU save implemented |
| Userspace Services | 7.5/10 | ~85% complete, missing pipes/TTY |
| Process Isolation | 7.8/10 | Good model, plaintext passwords |
| POSIX Layer | 7.0/10 | Sufficient for embedded |
| Memory Management | 6.5/10 | Correct but SMP-unsafe |
| Device Drivers | 3.5/10 | Minimal (QEMU only) |
| Build System | 8.0/10 | Developer-friendly |
| **OVERALL** | **7.3/10** | |

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
3. **Password hashing**: Plaintext is a deal-breaker
4. **Resource quotas**: No memory/CPU limits = trivial DoS
5. **Network stack**: Can't communicate with the outside world
6. **Filesystem writes**: ext2 is read-only
7. **Grant/Map zero-copy**: Transfer mechanism is stubbed
8. **Formal verification**: seL4's key advantage

---

## 12. Actionable Findings

### 12.1 Bugs

| ID | Severity | Location | Description |
|---|---|---|---|
| B1 | LOW | handlers.rs:477-484 | sys_call inline path limited to 16 bytes (by design, but inconsistent style with sys_send/sys_reply 4-chunk pattern — could unify for clarity) |

### 12.2 Resource Leaks

| ID | Severity | Location | Description |
|---|---|---|---|
| L1 | LOW | handlers.rs:1330-1434 | Frame registry map_count leak: if token re-lookup fails on error path, dec_map_count is never called |
| L2 | LOW | thread_manager.rs:420-447 | Reply map cleanup on thread death scans all REPLY_MAP_SLOTS — O(n) but acceptable at current size |

### 12.3 Missing Hardening

| ID | Priority | Description |
|---|---|---|
| H1 | CRITICAL | Enable SMAP/SMEP if CPU supports (prevents kernel accessing userspace directly) |
| H2 | CRITICAL | Hash passwords in /etc/users.toml (bcrypt/scrypt/Argon2) |
| H3 | HIGH | Add Spectre V2 mitigation (STIBP/IBPB on kernel entry) |
| H4 | HIGH | Add resource quotas (memory, FDs, CPU time per container) |
| H5 | ~~HIGH~~ DONE | ~~Implement FPU/SSE context save/restore (lazy or eager)~~ — Implemented: eager FXSAVE/FXRSTOR in assembly at every kernel entry/exit, per-CPU scratch buffer (gs:0x80), per-thread FpuState (commit dddd98e) |
| H6 | HIGH | Add audit logging (login attempts, privilege escalation, file access) |
| H7 | MEDIUM | Implement TTY line discipline (echo, canonical mode, signal delivery) |
| H8 | MEDIUM | Add pipe support in shell |
| H9 | MEDIUM | Expand deferred fault queue beyond 4 slots |
| H10 | MEDIUM | Expand pending wake queue beyond 8 slots |

### 12.4 Architecture Improvements

| ID | Priority | Description |
|---|---|---|
| A1 | HIGH | Implement grant/map zero-copy in transfer.rs |
| A2 | HIGH | Add async notifications (seL4-style) |
| A3 | MEDIUM | Add IOAPIC support for SMP and modern PCIe |
| A4 | MEDIUM | Make VFS multi-threaded or async |
| A5 | MEDIUM | Add priority inheritance on IPC |
| A6 | LOW | Unify sys_call inline to 4-chunk pattern (would need reply buffer in separate register or IPC redesign) |
| A7 | LOW | Add per-sender endpoint queue limits |

---

## 13. Roadmap: Sub-1000 Cycle IPC

### 13.1 Current State

**Measured IPC round-trip**: 1,195–1,625 cycles (after kernel audit v4 — 7.1x from original).

Cycle budget breakdown (estimated):

| Component | Cycles | % of Total |
|---|---|---|
| Syscall entry/exit (register save, SYSRET/IRETQ) | 200–250 | ~17% |
| Token lookup (per-CPU cache + shard lock) | 80–120 | ~8% |
| Endpoint queue ops (double lock, direct delivery scan) | 250–350 | ~24% |
| Reply ID alloc + CALL_REPLY_MAP | 40–60 | ~4% |
| Scheduler block/wake | 50–80 | ~5% |
| Memory access overhead (cache misses, TLB) | 30–50 | ~3% |
| Message copy + validation | 100–150 | ~10% |
| Context switch (CR3, FS_BASE, GPRs) | 200–300 | ~20% |

### 13.2 Tier 1: Inline Fast-Path (Target: 1,000–1,300 cycles)

These changes are localized, low-risk, and combinable:

| # | Optimization | Savings | Location |
|---|---|---|---|
| T1.1 | **Inline syscall dispatch** for sys_send/call/reply — branch directly in asm instead of `call syscall_dispatch` | 20–25 cycles | syscall_entry.asm |
| T1.2 | **Consolidate endpoint double-lock** — merge shard lock + endpoint mutex into single 16-shard scheme | 8–12 cycles | endpoint.rs |
| T1.3 | **Expand per-CPU token cache** from 4→8 entries with set-associative hash (better recv_any hit rate) | 10–15 cycles | table.rs |
| T1.4 | **Thread-local ReplyMap** — store CallReplyInfo in thread context, eliminate global CALL_REPLY_MAP Mutex | 15–20 cycles | thread_manager.rs |
| T1.5 | **Reduce register save** on fast-path — skip R12-R15 for Send/Reply (no context switch needed) | 15–20 cycles | syscall_entry.asm |

**Combined Tier 1 savings: ~68–92 cycles → Target: ~1,100–1,530 cycles**

### 13.3 Tier 2: Structural (Target: 900–1,100 cycles)

These require deeper refactoring but have high payoff:

| # | Optimization | Savings | Location |
|---|---|---|---|
| T2.1 | **Cache ObjectRef in Token struct** — eliminate scope→ObjectRef BTreeMap lookup | 15–20 cycles | table.rs, scope.rs |
| T2.2 | **Cache first waiter pointer** on endpoints — O(1) direct delivery instead of linear scan | 10–15 cycles | endpoint.rs |
| T2.3 | **Lazy timestamp validation** — defer TSC read (~20 cycles) to cache miss path only | 8–12 cycles | table.rs |
| T2.4 | **Batch scheduler operations** — defer add_to_scheduler until after IPC completes | 10–15 cycles | scheduler.rs |
| T2.5 | **Cache-line align hot structures** — PerCpuData, token cache, endpoint shards on 64-byte boundaries | 5–8 cycles | multiple |

**Combined Tier 1+2 savings: ~116–162 cycles → Target: ~1,000–1,460 cycles**

### 13.4 Tier 3: Architectural (Target: <800 cycles)

These are substantial redesigns, only needed if Tier 1+2 don't meet target:

| # | Optimization | Savings | Notes |
|---|---|---|---|
| T3.1 | **Register-passable message format** — UserMessage in thread context registers, no memory copy for ≤48B messages | 20–30 cycles | Protocol change, affects all IPC users |
| T3.2 | **Implicit thread state** — use reply_id as "waiting" marker, inject reply value directly into caller's RAX | 15–20 cycles | Major scheduler refactor |
| T3.3 | **Lock-free endpoint queues** — atomic push/pop with crossbeam-style epoch reclamation | 30–50 cycles | Very high complexity, only viable post-SMP |
| T3.4 | **Direct IPC path** — when receiver is already blocked on recv, bypass queue entirely and copy message + switch in one operation | 40–60 cycles | seL4's key optimization |

**Theoretical minimum with all optimizations: ~700–900 cycles**
(seL4 achieves ~500 on optimized hardware with hand-tuned assembly)

### 13.5 Measurement Strategy

1. **TSC-pair benchmarks**: `RDTSC` before sys_call, after reply received
2. **Per-component breakdown**: Token-only, endpoint-only, scheduler-only microbenchmarks
3. **Regression gate**: Any change must show ≥5 cycle improvement in median (10K samples)
4. **QEMU `-icount shift=0`** for deterministic cycle counting

---

## 14. Roadmap: SMP (Symmetric Multi-Processing)

### 14.1 Current State

Single-CPU kernel with several SMP-aware design choices:

| Component | SMP Status |
|---|---|
| GS-relative per-CPU data | ✅ Framework exists (PerCpuData struct has cpu_id field) |
| AP detection (BOOTBOOT) | ✅ BSP/AP detection via CPUID, APs parked with `hlt` |
| LAPIC timer | ✅ Per-CPU LAPIC works (init called on BSP only) |
| CR3 per-thread | ✅ Each thread has own CR3, switched on context switch |
| IDT (global) | ✅ Shared IDT is correct for x86_64 SMP |
| GDT/TSS | ❌ Single global GDT+TSS (each CPU needs its own) |
| IST stacks | ❌ Single set of 3 IST stacks (GPF/PF/DF — concurrent faults would collide) |
| BSP_STACK | ❌ Single 64KB kernel stack |
| TLB shootdown | ❌ No IPI infrastructure, local INVLPG only |
| IOAPIC | ❌ PIC-only IRQ routing (single CPU destination) |
| Scheduler | ❌ Single global `Mutex<PriorityBitmapScheduler>` |
| PMM | ❌ Single global `Mutex<BuddyAllocator>` |
| Token table | ⚠️ 16-shard lock — scales, but cache invalidation is global |
| Reply maps | ❌ Single global Mutex each |

### 14.2 Phase SMP-1: Per-CPU Foundation (Prerequisites)

Before any AP wakes up, allocate all per-CPU structures from BSP:

| Task | Detail |
|---|---|
| Per-CPU GDT+TSS array | `[GdtTss; MAX_CPUS]` — each CPU loads its own via LGDT/LTR |
| Per-CPU IST stacks | 3 stacks × MAX_CPUS — avoid concurrent exception stack collision |
| Per-CPU PerCpuData | Array indexed by APIC ID, each with own kernel_rsp, user_rsp, cpu_id |
| Per-CPU kernel stack | 64KB per CPU (separate from BSP_STACK) |
| CPU topology discovery | Parse MADT ACPI table for LAPIC entries → build CPU map |

### 14.3 Phase SMP-2: IPI Infrastructure

The foundation for all SMP coordination:

| Task | Detail |
|---|---|
| LAPIC ICR wrapper | Write to Interrupt Command Register (0xFEE00300) to send IPIs |
| IPI vector allocation | Reserve vectors 0xF0-0xFF for: TLB shootdown, reschedule, halt, panic |
| TLB shootdown protocol | Sender: set dirty_cpu bitmask + target address, send IPI. Receiver: INVLPG + ACK via atomic flag |
| Cross-CPU reschedule | IPI to wake idle CPU when new thread becomes runnable |
| Halt/panic broadcast | Stop all CPUs during kernel panic or shutdown |

### 14.4 Phase SMP-3: IOAPIC

Replace 8259 PIC with IOAPIC for proper multi-CPU interrupt routing:

| Task | Detail |
|---|---|
| IOAPIC driver | MMIO at address from ACPI MADT. Program redirection table entries |
| IRQ→CPU affinity | Map IRQ lines to specific CPUs (or lowest-priority delivery) |
| PIC disable | Mask all PIC IRQs after IOAPIC takes over |
| MSI-X support | Modern PCIe devices write interrupt directly to LAPIC (no IOAPIC needed) |
| IrqAck fix | Currently stubbed (returns NotImplemented). Implement EOI for IOAPIC |

### 14.5 Phase SMP-4: Per-CPU Scheduler

The biggest single change — eliminate the global scheduler lock:

| Task | Detail |
|---|---|
| Per-CPU ready queues | Each CPU has own PriorityBitmapScheduler instance |
| Work-stealing | Idle CPU steals from busiest CPU's expired queue |
| Thread affinity | `ThreadSetAffinity` invoke op — pin thread to CPU or set mask |
| Load balancing | Periodic (every N ticks) rebalance across CPUs |
| CURRENT_THREAD per-CPU | Move from global atomic to PerCpuData field |

### 14.6 Phase SMP-5: Lock Refinement

Reduce contention on remaining global locks:

| Lock | Strategy |
|---|---|
| THREAD_REPOSITORY | RWLock (many readers, rare writes) or lock-free concurrent map |
| PMM BuddyAllocator | Per-CPU page freelists (batch alloc 64 pages, return to global when exhausted) |
| CALL_REPLY_MAP | Per-thread (see IPC Tier 1.4) — eliminates lock entirely |
| Token table | Already 16-shard — sufficient for ≤16 CPUs |
| Frame registry | RCU for read path (mostly lookups) |

### 14.7 Phase SMP-6: AP Bring-Up

The actual multi-CPU boot sequence:

```
BSP:
  1. Parse MADT → discover AP APIC IDs
  2. Allocate per-CPU structures (GDT, TSS, IST, stack, PerCpuData)
  3. Copy AP trampoline to low memory (below 1MB, real-mode accessible)
  4. For each AP:
     a. Send INIT IPI (reset AP)
     b. Wait 10ms
     c. Send SIPI (Startup IPI) with trampoline address
     d. Wait for AP to increment CPU_READY_COUNT

AP trampoline (16-bit real mode → 64-bit long mode):
  1. Enable A20, load GDT, enter protected mode
  2. Enable PAE + PGE, load kernel CR3
  3. Enable long mode (EFER.LME), jump to 64-bit code
  4. Load per-CPU GDT (LGDT), TSS (LTR)
  5. Set GS base to per-CPU PerCpuData
  6. Initialize LAPIC timer
  7. Increment CPU_READY_COUNT
  8. Enter idle loop (wait for scheduler work)
```

### 14.8 SMP Testing Strategy

| Test | Purpose |
|---|---|
| 2-CPU ping-pong IPC | Verify cross-CPU IPC correctness |
| N-CPU parallel malloc | Stress PMM lock contention |
| TLB shootdown correctness | Map/unmap pages while other CPU accesses them |
| Priority inversion | High-priority thread on CPU0 waiting for lock held by low-priority on CPU1 |
| Thundering herd | All CPUs wake on single endpoint notification |
| Graceful shutdown | Halt all APs, then BSP |

---

## 15. Roadmap: Security Hardening & TPM

### 15.1 Current Crypto Infrastructure

| Primitive | Status | Location |
|---|---|---|
| HMAC-SHA256 | ✅ Implemented | klibcluu/src/crypto/hmac.rs |
| SHA-256 | ✅ FIPS 180-4 compliant | klibcluu/src/crypto/sha256.rs |
| CSPRNG (RDRAND) | ✅ With TSC fallback | klibcluu/src/crypto/random.rs |
| Constant-time compare | ✅ XOR-based | token/signature.rs |
| Kernel secret | ✅ 256-bit, ephemeral (regenerated each boot) | token/table.rs |
| Boot manifest HMAC | ✅ Verifies initrd integrity | bootstrap.rs |

**Weakness**: TSC-based RNG fallback (when RDRAND unavailable) is NOT cryptographically
secure. Token scope generation could be predictable on old hardware.

### 15.2 Phase SEC-1: Quick Wins (P1 — Days)

#### SEC-1.1: Enable SMAP/SMEP

Prevent kernel from accidentally reading/executing userspace memory.

```
CR4 bit 20 (SMEP): Kernel cannot execute user-mapped pages
CR4 bit 21 (SMAP): Kernel cannot read/write user-mapped pages
```

Changes needed:
- `kernel/src/architecture/x86_64/mod.rs`: Set CR4 bits during init
- Audit all kernel code that legitimately accesses user memory → wrap with CLAC/STAC
- QEMU: Use `-cpu Broadwell` or higher (SMAP/SMEP supported)

#### SEC-1.2: Password Hashing

Replace plaintext password verification in procmgr:

```
Current:  record.password == password           (plaintext, timing-vulnerable)
Target:   bcrypt_verify(record.hash, password)  (2^80+ brute-force resistance)
```

Changes needed:
- `userspace/procmgr/src/main.rs`: Replace `==` with bcrypt/scrypt verify
- `/etc/users.toml` → `/etc/shadow`: Store `$2b$12$salt$hash` format
- Add salt generation (16 bytes from RDRAND)

#### SEC-1.3: Login Rate Limiting

- Track failed attempts per user in procmgr memory
- Exponential backoff: 1s, 2s, 4s, 8s, ... 5min cap
- Reset on successful login

### 15.3 Phase SEC-2: Hardware Security (P2 — Weeks)

#### SEC-2.1: TPM 2.0 TIS Driver

QEMU exposes TPM via TIS (TPM Interface Specification) at MMIO `0xFED40000`:

| Register | Offset | Purpose |
|---|---|---|
| Access | 0x000 | Locality request/release |
| Status | 0x018 | Command ready, data available |
| Data FIFO | 0x024 | Command/response byte stream |
| IntEnable | 0x008 | Interrupt configuration |
| Sts.commandReady | bit 6 | TPM ready to accept command |
| Sts.dataAvail | bit 4 | Response ready to read |

**Driver architecture** (microkernel style):
- Kernel: Map MMIO region, expose via Frame capability
- Userspace `tpmd` service: Send/receive TPM commands via FIFO
- IPC protocol: `TPM_SUBMIT_CMD` label, shared buffer for command/response

**Core TPM 2.0 commands needed**:
- `TPM2_Startup(TPM_SU_CLEAR)` — initialize after reset
- `TPM2_PCR_Extend(index, digest)` — extend measurement register
- `TPM2_PCR_Read(selection)` — read current PCR values
- `TPM2_GetCapability(property)` — query algorithms, PCR count
- `TPM2_Quote(PCRs, nonce)` — signed attestation (Phase SEC-4)
- `TPM2_Create/Load/Unseal` — sealed storage (Phase SEC-3)

**QEMU setup**:
```bash
swtpm socket --daemon --ctrl type=unixio,path=/tmp/swtpm.sock \
  --tpmstate dir=/tmp/tpm-state --tpm2
qemu-system-x86_64 ... \
  -chardev socket,id=chrtpm,path=/tmp/swtpm.sock \
  -tpmdev emulator,id=tpm0,chardev=chrtpm \
  -device tpm-tis,tpmdev=tpm0
```

#### SEC-2.2: Measured Boot

Extend TPM PCRs at each boot stage to create a cryptographic chain:

| PCR | Extended By | Measurement |
|---|---|---|
| 0 | Firmware/BOOTBOOT | Bootloader hash |
| 9 | Kernel init | SHA-256(kernel ELF sections) |
| 13 | Kernel init | SHA-256(initrd TAR archive) |
| 14 | Init process | SHA-256(each primordial service binary) |

**Verification**: Remote verifier retrieves PCR values via TPM Quote, compares
against golden measurements → confirms system booted unmodified.

### 15.4 Phase SEC-3: Sealed Storage (P3 — Months)

TPM-sealed secrets that only unseal when PCRs match expected values:

| Use Case | Mechanism |
|---|---|
| Disk encryption key | Seal to PCR[0,9,13] — key only released on clean boot |
| Kernel secret persistence | Seal to PCR[0,9] — same token signing key across reboots |
| User credential binding | Seal user key to PCR[14] — invalidate if services modified |

**Architecture**:
```
Boot: TPM2_Unseal(sealed_blob, policy=PCR[0..9])
  → Success: Use unsealed key for token signing
  → Failure: PCRs changed → kernel generates new ephemeral key
             (all old tokens invalidated — clean slate)
```

### 15.5 Phase SEC-4: Remote Attestation (P4 — Future)

Full attestation chain for cloud/multi-tenant deployment:

1. **Attestation Identity Key (AIK)**: TPM-resident asymmetric key, never leaves TPM
2. **Quote generation**: `tpmd` signs PCR values with AIK on request
3. **Verification server**: External entity validates quote signature + PCR values
4. **Trust decision**: Only attested systems join the trusted workload pool

### 15.6 Phase SEC-5: Spectre/Meltdown Mitigation

| Mitigation | Mechanism | Where |
|---|---|---|
| IBPB | Flush branch predictor on kernel entry | syscall_entry.asm |
| STIBP | Restrict branch prediction across SMT threads | MSR write on init |
| KPTI | Separate kernel/user page tables | vmm.rs (major) |
| Retpoline | Replace indirect calls with return-based trampoline | Rust compiler flag |

**Priority**: Lower than TPM (Spectre requires real hardware for meaningful testing;
QEMU doesn't accurately model speculative execution).

---

## 16. Roadmap: Userspace & Driver Ecosystem

### 16.1 Current State

| Category | What Works | What's Missing |
|---|---|---|
| **Block I/O** | virtio-blk (legacy+modern), ext2 R/W | AHCI, NVMe, journaling |
| **Filesystem** | VFS with 4 backends, file cache (32MB) | Symlinks, hard links, ext4 |
| **Display** | 4 VTs, ANSI CSI, scrollback, framebuffer | Window manager, GPU accel |
| **Input** | PS/2 keyboard (US+HU layouts) | Mouse, raw scancodes, USB |
| **Audio** | None | AC97, HDA |
| **Network** | None | virtio-net, TCP/IP, sockets |
| **Pipes** | ✅ POSIX pipe() works | Shell `\|` syntax not parsed |
| **TTY** | ✅ Line discipline (canonical/raw, echo) | OPOST, ISIG, flow control |
| **Threads** | ✅ Full pthreads (1046 LOC) | pthread_cancel |
| **Signals** | ✅ signal/sigaction/raise | Async delivery from kernel |
| **Job control** | ✅ jobs/fg/bg/stop | Process groups, SIGTSTP |

### 16.2 Tier 0: Unblock Everything (Week 1) — COMPLETE

**IrqAck implementation** — ~~currently stubbed~~ ALREADY IMPLEMENTED (audit was incorrect).
`invoke_irq_ack` at handlers.rs:2394-2427 is fully functional: checks IRQ_ACK right,
validates IRQ range 0-15, sends EOI to APIC+PIC.

**FPU/SSE context save** — IMPLEMENTED (commit dddd98e). Eager FXSAVE/FXRSTOR in
assembly at every kernel entry/exit from userspace. Per-CPU scratch buffer at gs:0x80
(512 bytes in PerCpuData). Rust scheduler memcpys between scratch and per-thread
FpuState on context switch. Note: cannot disable SSE in kernel code (Rust nightly
rejects both `-sse2` and `+soft-float` on x86_64 ABI), hence assembly-based approach.

### 16.3 Tier 1: Input & Application Ports (Weeks 2-4)

| Task | Effort | Depends On | Enables |
|---|---|---|---|
| PS/2 mouse driver | 200 LOC | IrqAck | Quake, GUI |
| Raw keyboard scancodes (up/down events) | 150 LOC | — | Games, raw input |
| Shell pipe `\|` syntax | 200 LOC | — | Composable commands |
| MicroPython port | 1-2 weeks | — | Scripting, REPL |
| sched_yield() wire-up | 10 LOC | — | MicroPython threading |

### 16.4 Tier 2: Network Stack (Weeks 5-10)

| Phase | Task | Effort | Detail |
|---|---|---|---|
| Net-1 | **virtio-net driver** | 1-2 weeks | Clone virtio-blk pattern, RX/TX virtqueues, DMA buffers |
| Net-2 | **ARP + IPv4** | 1 week | Address resolution, IP header parse/build |
| Net-3 | **UDP** | 3 days | Connectionless datagram, DNS resolution |
| Net-4 | **TCP** | 2-3 weeks | 3-way handshake, sliding window, retransmit, congestion |
| Net-5 | **Socket service** | 1-2 weeks | IPC-based socket API (socket/bind/connect/send/recv) |
| Net-6 | **DHCP client** | 3 days | Automatic IP configuration |

**Architecture**: Userspace network daemon with IPC interface:

```
virtio-net (driver)
  ↕ IPC (raw frames)
netd (network daemon)
  ├── ARP table
  ├── IP routing
  ├── TCP state machines
  └── Socket endpoint registry
  ↕ IPC (socket API)
Applications (curl, httpd, ssh...)
```

**Alternative**: Port lwIP (~300K LOC, mature) instead of custom TCP/IP. Faster to
production-ready but larger dependency. Requires async event model (seL4-style
notification objects or poll-based IPC).

### 16.5 Tier 3: Storage & Filesystem (Weeks 11-14)

| Task | Effort | Impact |
|---|---|---|
| **ext2 journaling** (ext3 compat) | 3-4 weeks | Crash-safe writes |
| **Symlink support** | 1 week | POSIX compliance, app compat |
| **virtio-blk interrupt mode** | 1 week | Eliminate polling, reduce CPU usage |
| **AHCI/SATA driver** | 2-3 weeks | Real hardware disk support |
| **NVMe driver** | 2-3 weeks | Modern SSD support |
| **tmpfs** | 1 week | /tmp in RAM, faster than ext2 for temp files |

### 16.6 Tier 4: Multimedia & GUI (Weeks 15+)

| Task | Effort | Impact |
|---|---|---|
| **AC97 audio driver** | 2 weeks | Sound output (Quake, media) |
| **Window manager service** | 3-4 weeks | Multi-window GUI |
| **Shared memory IPC** (MAP_SHARED mmap) | 1-2 weeks | Zero-copy framebuffer sharing |
| **virtio-gpu driver** | 3 weeks | Hardware-accelerated 2D/3D |
| **Quake 1 port** | 3-4 weeks | Flagship demo application |
| **XHCI (USB 3.0)** | 4-6 weeks | Modern peripherals |

### 16.7 Async Notifications (Cross-Cutting)

Currently CLUU only has blocking Send/Recv/Call. The network stack, GUI, and audio
all need non-blocking event delivery. Options:

| Approach | Effort | Compatibility |
|---|---|---|
| **seL4-style notification objects** | 2-3 weeks | Clean, proven design. Kernel adds Notification object type with signal/wait/poll ops |
| **Extend poll() to IPC endpoints** | 1 week | Already partially done (TTY_POLL). Generalize to any endpoint |
| **Async IPC flag** | 1 week | Non-blocking sys_send that returns immediately. Queue overflow → drop |

**Recommendation**: seL4-style notifications are the right long-term answer. They
compose well with the existing capability model (notification = token-protected
object, same derivation rules).

---

## 17. Consolidated Roadmap Timeline

### Near-Term (Weeks 1-4): Foundation

```
Week 1:  SEC-1 (SMAP/SMEP, password hashing, rate limiting)
         Tier 0 (IrqAck, FPU context save) — COMPLETE (2026-03-03)

Week 2:  IPC Tier 1 (inline dispatch, double-lock consolidation,
         8-entry token cache, thread-local ReplyMap)

Week 3:  Tier 1 apps (mouse driver, raw scancodes, shell pipes)

Week 4:  MicroPython port (proof of concept, REPL on VT)
```

### Mid-Term (Weeks 5-14): Ecosystem

```
Weeks 5-6:   SMP Phase 1-2 (per-CPU structures, IPI infrastructure)
Weeks 7-8:   SMP Phase 3-4 (IOAPIC, per-CPU scheduler)
Weeks 9-10:  Network Tier 2 (virtio-net, ARP, IP, UDP, TCP)
Weeks 11-12: SEC-2 (TPM TIS driver, measured boot)
Weeks 13-14: Storage Tier 3 (ext2 journaling, symlinks, AHCI)
```

### Long-Term (Weeks 15+): Production Readiness

```
Weeks 15-16: SMP Phase 5-6 (lock refinement, AP bring-up, testing)
Weeks 17-18: IPC Tier 2-3 (structural + architectural optimizations)
Weeks 19-20: Network Tier 2 cont. (socket service, DHCP, DNS)
Weeks 21-22: SEC-3 (TPM sealed storage, persistent kernel secret)
Weeks 23-24: Multimedia (AC97, window manager, Quake port)
Week 25+:    SEC-4 (remote attestation), virtio-gpu, USB
```

### Target Milestones

| Milestone | Target | Metric |
|---|---|---|
| **M1: Sub-1200 IPC** | Week 2 | Median IPC round-trip ≤1,200 cycles |
| **M2: MicroPython runs** | Week 4 | `>>> print("hello")` on VT |
| **M3: 2-CPU SMP boot** | Week 8 | Both CPUs scheduling threads independently |
| **M4: `ping` works** | Week 12 | ICMP echo request/reply via virtio-net |
| **M5: Measured boot** | Week 12 | TPM PCRs contain kernel+initrd hashes |
| **M6: Sub-1000 IPC** | Week 18 | Median IPC round-trip ≤1,000 cycles (SMP-ready) |
| **M7: `ssh` inbound** | Week 20 | TCP listener accepts connections |
| **M8: Quake playable** | Week 24 | 30fps software render with mouse+keyboard |

---

*Generated by 12 parallel deep-dive agents analyzing 68,000+ lines of source code.*
