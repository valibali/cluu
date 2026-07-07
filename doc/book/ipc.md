# IPC

IPC is the only communication primitive in CLUU. There are no shared-memory
globals, no signals-as-data, no hidden channels. Every cross-service
interaction is a message pass through the kernel.

This chapter covers the userspace model: the rendezvous semantics, the
operations, buffer transfer, and the endpoint registry that wires services
together at runtime. For the kernel-side dispatch and message structures, see
[The Kernel](../kernel/index.html#ipc-kernelsrcipc). For the authority
primitive that gates every endpoint, see
[Capability Tokens](../capability_tokens/index.html).

## Synchronous rendezvous

CLUU IPC is synchronous rendezvous, inspired by L4. A sender blocks until a
receiver is ready, and a receiver blocks until a sender is ready. There is no
buffered queue inside the kernel: the two threads meet at a rendezvous point
and the message moves directly between them.

```text
Sender ----+      +---- Receiver
           |      |
           +-- IPC +
           |      |
Sender <-- +      + --> Receiver
```

This gives deterministic blocking behavior. A `send` to a endpoint with no
pending receiver sleeps the caller; a `recv` on an endpoint with no pending
sender sleeps the caller. No thread wakes up spuriously.

## Operations

Five verbs cover the whole surface:

- **`send`** delivers a message and does not wait for a reply.
- **`recv`** blocks for an incoming message.
- **`call`** is `send` plus wait-for-reply, atomically. It pairs with
  `reply`.
- **`reply`** answers a `call`. The original caller wakes.
- **`replyrecv`** combines `reply` to the previous caller with `recv` of the
  next message, used by servers in their request loop.

All five go through the `Send` / `Recv` / `Call` / `Reply` syscalls. No
operation outside this set is needed to build a server.

## Buffer transfer

A message can carry an optional buffer. Three transfer modes trade safety
against copy cost:

- **`Copy`** copies bytes between sender and receiver. Safe, no authority
  transfer, but pays the memcpy.
- **`Grant`** transfers page ownership from sender to receiver. Zero-copy:
  the physical frame moves, the sender loses access. The receiver now owns
  that frame.
- **`Map`** shares a mapping between sender and receiver. Zero-copy with
  shared access, used for large shared regions.

`Grant` and `Map` are the fast paths for bulk data. `Copy` is the default
when no frame-level authority is being handed over.

The fast path for a message with no buffer carries up to six register-passed
words (`MessageTag` plus `Message` words) without touching memory.

## Endpoint registry and dataflow

Userspace endpoint wiring is dynamic and lazy. Nothing is hard-wired at build
time.

The rules:

- Each process **owns its input endpoints**. An input is private to the
  process that created it.
- Output endpoints are **registered** with the registry service by name.
- Consumers **subscribe at runtime**. The registry brokers discovery and the
  grant flow.
- Tokens are **transferred via blocking send/recv**. No new syscall numbers
  are needed to move authority; it rides on the existing IPC path.
- An output can have **multiple subscribers**. An input stays per-process.

The flow:

```text
Requester -> registry.subscribe(producer, endpoint)
registry  -> producer.grant(requester)
producer  -> send(token) to requester (or via registry)
requester -> recv(token)
```

The registry stores **metadata only**: a name-to-endpoint mapping. Tokens are
issued by the endpoint owner and granted on demand, preserving least
privilege. The registry never holds authority itself; it only introduces
parties so they can hand authority directly to each other.

## Tokens over IPC

Authority to target an endpoint is proved by presenting a capability token
through the `Invoke` syscall path. The kernel verifies the token signature,
checks expiration, and confirms the rights bitmask covers the requested
operation before the message moves.

There is no per-thread ACL and no "who is the caller" check at request time.
If a binary can name a token handle, it can use it. If it cannot, it never
sees the endpoint. See
[Capability Tokens](../capability_tokens/index.html) for the full model,
including the monotone-narrowing derivation that keeps authority from
escalating as it descends the spawn tree.

## Pipes

A pipe is not a kernel concept. It is one IPC endpoint with two
rights-restricted tokens minted from it: a send-only `write_token` and a
recv-only `read_token`. Procmgr is the lifecycle authority — it allocates
the endpoint via `PROCMGR_PIPE_CREATE`, mints the tokens, hands them to
children at spawn, and revokes them on child exit.

Token revocation *is* EOF and SIGPIPE propagation. When the reader exits,
procmgr revokes its `read_token`; the writer's next `send` returns
`Error::TokenInvalid`, which libcluu's `_write_r` translates to
`errno=EPIPE` + `raise(SIGPIPE)` (default action: terminate). When the
writer exits, the reader's next `recv` returns `Error::TokenInvalid`,
which `_read_r` translates to a clean 0-byte EOF. No new IPC contract is
needed — pipe read/write reuses the existing `TTY_WRITE_LABEL` send
protocol, so a child's fd 0/1 dispatch identically whether wired to a TTY
or a pipe. `pipe_id` (with a generation counter) is procmgr's opaque
cleanup handle; `PROCMGR_PIPE_CLOSE` is idempotent and revokes only the
caller's side. The shell composes N-stage pipelines from N-1
`PIPE_CREATE` + N spawns; `$?` is the last command's status (no
`pipefail` yet).

## Shared memory (MAP_SHARE_PHYS)

Shared memory is not a separate IPC channel — it reuses the `space_map_range`
invoke op with the `MAP_SHARE_PHYS` flag (`0x800`). The kernel remaps the
caller's physical frames backing a source virtual address into a target
address space, read-only. Two processes share a frame by agreement: the
owner holds a token for the receiver's space (obtained via the normal IPC
token-transfer flow above) and calls `space_map_range` directly.

The `mmap` POSIX shim exposes this as `mmap(NULL, len, prot,
MAP_SHARED|MAP_ANONYMOUS, -1, src_virt)` for the same-space alias case.
There is no `shm_open`/`shm_unlink` and no `/dev/shm` filesystem — the
wrapper is the mmap path plus the existing invoke op, not a new IPC
mechanism. See [Memory Model](../memory_model/index.html#map-shared-wrapper-map_share_phys)
for the calling conventions and the cross-process sharing sequence.

## Plan lessons — IPC

Distilled implementation lessons from IPC-related plans. 2-5 lines each;
see the dated plan file for the long form.

### pipe-token-revocation-cascade (2026-04-27-pipes)

A pipe is one IPC endpoint with two rights-restricted tokens (write-only /
read-only) minted via `kernel::TokenDerive`. Revoking the parent endpoint
invalidates all derived child tokens — procmgr relies on this cascade for
`PIPE_CLOSE` and per-process exit cleanup. YAGNI deferral on the
`generation` counter (ABA protection for `pipe_id` reuse) was deliberate;
the structural fix lands only if a stale-id bug surfaces.

### diagnostic-first-pipe-reverify (2026-05-07-phase4-E-pipe-reverify)

The 3-stage pipe reverify was diagnostic-first: run a smoke against the
existing executor, capture exact failure or success, *then* fix. The
3-stage path worked; the real gap was env propagation through pipe stages,
not the pipe mechanism. The fix was lifting the ENV trailer from the
single-cmd path into a shared payload builder reused by `pipeline.rs`.
Don't leave a TODO in the executor — document the wait semantics.
