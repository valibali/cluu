# Capability Tokens

Capability tokens are the kernel's only authority primitive. Everything else —
process ownership, VFS views, IPC endpoints, memory mappings — is gated by
presenting a valid token.

## What a token is

A `Token` binds four things under one HMAC-SHA256 signature:

1. **Scope** — an opaque reference to the kernel object the token authorizes
   access to (`AddressSpaceId`, `EndpointId`, `FrameId`, `NotificationId`,
   `ReplyId`). Scopes are opaque and non-enumerable: `OpaqueScope` cannot be
   guessed or iterated to discover objects.
2. **Rights** — a bitmask of what the holder may do (`READ`, `WRITE`,
   `EXECUTE`, `CREATE`, `DESTROY`, `GRANT`, `MAP`, `MANAGE`,
   `THREAD_CONTROL`, `SPACE_MAP`, `IPC_SEND`, `IPC_CALL`, `IRQ_HANDLE`,
   `PCI_ACCESS`, ...).
3. **Issuer** — `Kernel` or `Authority(AuthorityId)`. Distinguishes
   kernel-minted root authority from authority minted by a userspace service so
   delegation chains are auditable.
4. **Expiration** — monotonic nanoseconds since boot. Every token carries a
   mandatory expiration. `Timestamp::far_future` / `NEVER` exist for root
   authority but are still finite encodings.

The signature is computed by `Signature::compute` using the kernel secret (held
in `table::kernel_secret`). Only the kernel can mint tokens that pass
`Token::verify`. `Token::Debug` redacts the signature so panic and log output
never leaks the HMAC bytes.

Userspace refers to tokens by `TokenHandle(usize)` — an opaque index into the
kernel's token table. `TokenHandle::from_raw` / `as_raw` exist for FFI-style
passing across IPC.

## How authority works

**Possession of a valid token *is* authority.** The kernel performs no per-call
ACL check. If a binary can name a token handle, it can use it. If it cannot, it
never sees the endpoint.

This means:
- No "who is the caller" interrogation at request time.
- No runtime identity resolution in the IPC path.
- No policy engine that could regress.
- "What can X do?" is answered by reading the static envelope and view, not by
  running code.

## Token derivation (monotone-narrowing)

`Token::derive` creates a new token with **narrower or equal** rights and
**shorter or equal** expiration than the parent. It refuses to escalate rights
or extend expiration. The kernel's `try_create_derived_token` wraps this and
installs the derived token in the table, additionally enforcing the global
token-count limit.

`derive_token(parent, new_rights, new_expire, issuer, object_ref)` is the
userspace-facing helper. `try_derive_token(...)` returns `Result<TokenHandle,
&'static str>` for error handling.

This is the structural enforcement of CLUU's monotone-narrowing authority
model: authority can only shrink as you descend the spawn tree.

## The InvokeOp dispatch table

The `Invoke` syscall (number 5) is the dispatch path for every kernel operation
that isn't raw IPC. It takes a `TokenHandle` and an `InvokeOp` number, validates
the token, checks rights, and dispatches to the handler.

52 invoke ops today:

| Group | Ops | Numbers |
|-------|-----|---------|
| **Thread** | `ThreadCreate`, `ThreadDestroy`, `ThreadSuspend`, `ThreadResume`, `ThreadSetPriority`, `ThreadSetFaultEndpoint`, `ThreadSetFSBase`, `ThreadGetId`, `ThreadGetStats`, `SchedGetOverflow` | 0–9 |
| **Space** | `SpaceCreate`, `SpaceDestroy`, `SpaceMap`, `SpaceUnmap`, `SpaceGrant`, `SpaceMapRange`, `SpaceProtect`, `SpaceGetStats` | 10–19 |
| **Futex** | `FutexWait`, `FutexWake` | 17–18 |
| **Token** | `TokenDerive`, `TokenRevoke`, `TokenGetInfo`, `TokenDeriveScoped` | 20–23 |
| **IRQ** | `IrqAttach`, `IrqAck` | 30–31 |
| **Endpoint** | `EndpointCreate`, `EndpointPeek` | 40–41 |
| **PCI** | `PciConfigRead`, `PciConfigWrite` | 50–51 |
| **I/O Port** | `PortIn8`, `PortIn16`, `PortIn32`, `PortOut8`, `PortOut16`, `PortOut32` | 52–57 |
| **Memory** | `VirtToPhys`, `PmmAllocLarge`, `PmmGetStats` | 58–59, 62 |
| **Clock** | `ClockNow`, `ClockFrequency` | 60–61 |
| **Frame** | `FrameAllocate`, `FrameFree`, `FrameGetPhys` | 70–72 |
| **Notification** | `NotificationCreate`, `NotificationSignal`, `NotificationWait`, `NotificationPoll` | 80–83 |
| **Thread enum** | `ThreadEnumerate`, `ThreadSetSession`, `ThreadSetSystemScope` | 84–86 |

Adding a new kernel operation means adding a variant to `InvokeOp` and a handler
in `syscall::handlers` — no new syscall number, no new entry point. This keeps
the attack surface bounded.

### SpaceProtect semantics

`SpaceProtect` (InvokeOp 16) changes page protection flags (RWX bits) on
already-mapped pages in an address space.

- **Args:** `(virt_addr, num_pages, flags)`. Flags use the same bit layout as
  `SpaceMap` (bit 0 read, bit 1 write, bit 2 exec, bit 6 user).
- **Returns:** number of pages updated.
- **Errors:** `PermissionDenied`, invalid address, unmapped page.
- With the C5 upgrade, userspace `mprotect` accepts `PROT_NONE` (clears the
  present bit) — enables guard pages and write-barrier pages.
- **Cost:** ~1200 cycles per call (same as other invoke ops). A full-heap
  `mprotect` sweep is heavy; batch via `num_pages`.

## Token revocation

`TokenRevoke` (InvokeOp 21) removes a token from the table. When a session is
destroyed, all tokens derived from the session cap are revoked, triggering
cascade teardown: processes lose their authority and cannot continue.

## Token inspection

`TokenGetInfo` (InvokeOp 22) lets a holder query its own token's scope, rights,
and expiration. This is how procmgr verifies a `VfsViewManager` cap before
installing a view (`resolve_view_mgr_cap` in VFS checks the token type tag).

## Userspace service discovery (registry)

The kernel's `Invoke` dispatch covers authority over kernel objects. Service
*discovery* (naming, endpoint wiring) lives in userspace: the `registry`
service maps `(service_name, endpoint_name)` pairs to producer-owned grant
endpoints. The registry never holds output endpoint tokens. It stores only a
grant endpoint per output, and the producer mints a transferable send token
on demand when a subscriber asks.

This keeps least-privilege intact: producers retain authority to grant, the
registry only brokers, and a compromised registry cannot forge tokens (it
never has them).

### Registry protocol (IPC labels)

These are userspace IPC message labels, not kernel `InvokeOp` numbers. They
ride on the `Call`/`Reply` syscalls.

| Label | Direction | Purpose |
|-------|-----------|---------|
| `REGISTRY_REGISTER` | producer → registry | register output metadata |
| `REGISTRY_UNREGISTER` | producer → registry | remove output metadata |
| `REGISTRY_LIST` | any → registry | list registered outputs (discovery/debug) |
| `REGISTRY_SUBSCRIBE` | requester → registry | request subscription to `(service, endpoint)` |
| `REGISTRY_SUBSCRIBE_REPLY` | registry → requester | status (0 ok, negative error) |
| `REGISTRY_GRANT_REQUEST` | registry → producer | ask producer to mint a send token |
| `REGISTRY_GRANT_DELIVER` | producer → requester | deliver the granted token |

Name payload format: `u16 service_len | u16 endpoint_len | service_bytes | endpoint_bytes`.

### Grant flow

```text
Requester                    Registry                    Producer
    |  SUBSCRIBE(svc, ep)       |                           |
    |-------------------------->|                           |
    |                           |  GRANT_REQUEST(ep, reply) |
    |                           |-------------------------->|
    |                           |                           | derive send token
    |                           |                           | send GRANT_DELIVER
    |                           |<--------------------------|
    |  GRANT_DELIVER(token)     |                           |
    |<--------------------------|                           |
```

The requester blocks on its registry control endpoint until it receives a
`GRANT_DELIVER` for the endpoint name.

### Startup defaults

Each process is given a registry send token in `TOKEN_REGISTRY` and a default
capability token (`TOKEN_IPC`) with `CREATE` and IPC rights to create
endpoints. Processes call `registry::init("service_name")` and
`registry::register_default_outputs()` at startup, then register additional
outputs explicitly.

### Replacing build-time wiring

Init no longer wires console/tty/kbd tokens at spawn time. Services register
their outputs (e.g. `console:write`, `tty:main`); consumers request
subscriptions at runtime (lazy wiring). If a subscribe reply returns an error,
the caller retries or backs off.

### Output ordering

Some consumers (tty) may emit output before a console subscription exists.
tty buffers a small amount of output in that window until it can forward it.

### Lifecycle

The registry removes entries on explicit unregister (on-exit cleanup is a
future item). If a producer dies, its tokens become invalid and consumers
must re-subscribe.

See [Service Catalog](../services/index.html) for the per-service registry
usage.

## The crypto

`klibcluu::crypto` provides:
- `sha256` — SHA-256 implementation.
- `hmac` — HMAC-SHA256.

These are shared between kernel and userspace (klibcluu is compiled for both
targets). The kernel secret is generated at boot and never leaves the kernel.

`constant_time_eq` in `signature.rs` prevents timing side-channels on signature
comparison.

## Documentation findings

### F-004 — Syscall count in docs contradicts code (open)

The original architecture documentation claimed "~12 syscalls total" and "basically a
dozen actual syscalls". The `SyscallNumber` enum in `kernel/src/syscall/mod.rs`
defines exactly 7: `Send(0)`, `Recv(1)`, `Call(2)`, `Reply(3)`, `Yield(4)`,
`Invoke(5)`, `DebugPrint(255)`. `dispatch_syscall` handles exactly these 7.
The book already states the correct count (see
[Getting Started](../getting_started/index.html) and
[Architecture](../architecture/index.html)). The minimal syscall surface is a
feature, not a bug; the code is correct, the old doc was wrong.
