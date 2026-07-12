# CLUU Networking Stack — Deep Analysis (Claude Fable 5)

**Date:** 2026-07-11
**Scope:** virtio-net driver + virtio-core, netd, libcluu socket API, IPC labels,
container manifests, wget/curl/ping, L2 probes.
**Method:** direct read of netd/socket.rs/kernel IPC + three parallel deep audits
(driver, capability chain, clients/probes), cross-checked against smoltcp 0.13.1
source and the kernel IPC/endpoint code.

## Relationship to the prior audit

`docs/NETD_WORKAROUND_AUDIT_2026_07_11.md` already catalogs 11 *workaround /
fragility* issues (loopback ARP injection, accept fd-swap, connect spin loop,
UDP wiring, DHCP `addrs.clear()`, TCP close/FIN, 200 ms poll tick, startup yield
loops, ping iteration poll) with concrete fixes and a landing order. **That
analysis is correct and this report does not repeat it.** This report is the
*correctness / safety / security* pass: it adds findings that audit did not
cover (silent data loss, buffer-boundary corruption, a capability-model hole, a
DMA trust-boundary panic, a missed-interrupt degradation, DNS concurrency) and
gives the overall architecture verdict and a merged fix plan. Where an issue
overlaps, it is marked *(see prior audit #N)*.

---

## 1. Findings

Severity: **CRITICAL** = silent wrong data or memory-unsafety on the happy path;
**HIGH** = data loss / hang / security-model break under realistic conditions;
**MEDIUM** = correctness bug behind an edge or load condition; **LOW** = latent
/ portability / hygiene.

### CRITICAL

**C1 — TCP recv drops buffered data when FIN arrives in the same poll batch.**
`userspace/netd/src/main.rs:237-247`.
In the `pending_recv` drain, the TCP arm checks socket **state first**: if the
state is `CloseWait` or `Closed` it immediately replies `0` (EOF) *without
draining the receive buffer*. But smoltcp keeps buffered data readable in
CloseWait — verified in smoltcp 0.13.1 `socket/tcp.rs:1181-1219`: `may_recv()`
and `can_recv()` both return true while `rx_buffer` is non-empty regardless of
state. For a `Connection: close` server (the common HTTP case) the payload and
the FIN routinely land in the same `iface.poll()`; the socket goes to CloseWait
with data still buffered, and the very next pending sweep answers the client's
blocked `recv()` with EOF/0 bytes, **discarding the response body**. The
immediate path at `main.rs:824-848` has the correct order (drain, *then* check
state) — the two paths disagree. Timing-dependent, so it reads as flaky net
tests rather than a hard failure. Root cause: EOF check ordered before the drain
in the deferred path only.
*Fix:* in the pending TCP arm, call `recv_slice` first; only reply EOF when it
yields 0 bytes **and** `!sock.may_recv()`.

**C2 — `recv()` silently corrupts data at the 4 KiB reply boundary.**
`userspace/libcluu/src/posix/socket.rs:222-254`.
The client reply buffer is sized `4096.min(size_of::<Message>() + len)`, i.e.
the 4096-byte cap is *inclusive of the Message header*. netd, however, replies
with up to `max_len.min(TCP_RX_BUF)` = up to 4096 **payload** bytes
(`netd/src/main.rs:59,826`). Once a read wants ≥ `4096 − sizeof(Message) + 1`
payload bytes with a full smoltcp rx buffer, one of two bad things happens: the
kernel returns `BufferTooSmall` (`kernel/src/ipc/endpoint.rs:766,1045`) and the
read fails as `ENOMEM`; **or**, if it fits the header math but not the payload,
`recv` returns netd's `received` count while copying only
`payload_len.min(to_copy)` bytes — leaving a stale-garbage tail in the caller's
buffer and permanently losing the bytes smoltcp already dequeued. wget and curl
both call `recv(fd, buf, 4096)`, so every download whose in-flight window fills
the rx buffer is exposed. Never hit in the harness because the test page is 41
bytes. Root cause: client reply buffer not sized to `header + payload`, and
`recv` trusting netd's length word over the bytes actually delivered.
*Fix:* size the reply buffer `size_of::<Message>() + len` (allocated, as the code
already does for the vec path) and clamp netd's per-reply payload to
`reply_capacity − header`; never return a count larger than bytes copied.

**C3 — Device-controlled `used.id` can panic the driver or corrupt the descriptor free-list.**
`userspace/virtio-core/src/virtqueue.rs:266-277`.
`pop_used` indexes `self.cookies[head as usize]` with the device-supplied ring
id (`u32→u16`), against a fixed 64-slot Vec. A malfunctioning/malicious device
returning id ≥ 64 is an out-of-bounds panic that kills the driver process
(`netdev:main` goes stale, whole stack down). A duplicated id still runs
`free_chain`, double-inserting descriptors into the free list so the same
descriptor later backs two live requests → DMA into aliased buffers. This is the
one genuine memory-unsafety class in the stack. QEMU never triggers it, but it
is unchecked input at the hardware trust boundary in a `no_std` service.
*Fix:* bounds-check `head` and reject/ignore ids that are out of range, already
free, or have a `None` cookie, before touching `free_chain`.

### HIGH

**H1 — NET capability is not structurally enforced: the registry offers a second, ungated path to netd.**
`userspace/registry/src/main.rs:225-300` (SUBSCRIBE), `libcluu/src/registry.rs:431-451`
(grant), `netd/src/main.rs:91-93,335` (registration).
The intended enforcement is sound: a non-NET binary never gets netd in
`TOKEN_EXTRA_0`, and `socket()` fails closed with `ENOSYS`
(`socket.rs:50-54`) — no runtime ACL, exactly per philosophy §2. **But netd also
registers itself under the well-known registry name `netd:main`, and the
registry SUBSCRIBE→GRANT broker performs zero capability inspection**: it
forwards any subscribe to the producer, and netd's grant handler
unconditionally mints an `IPC_SEND|IPC_CALL` token to whoever asked
(`registry.rs:444-451`). Any binary holding `TOKEN_REGISTRY` can therefore call
`subscribe_output("netd","main")`, obtain a live netd endpoint, and hand-build
`NET_SOCKET`/`NET_CONNECT` messages — with no NET profile bit. `TOKEN_REGISTRY`
is granted to any profile containing VFS, REGISTRY, or SPAWN
(`root-procmgr/src/main.rs:8014-8019`), so essentially every non-trivial binary
already holds the key. This directly contradicts the stated guarantee (and the
comment at `socket.rs:8-11`) that a NET-less binary is *structurally* unable to
reach netd. Note the same broker underlies all services — the observation is
that "NET capability" is decorative as long as netd is discoverable by name.
*Fix (structural, not an ACL):* stop registering netd under a public registry
name and deliver its endpoint **only** via the spawn envelope `TOKEN_EXTRA_0`
(mirrors how session-VFS redirection already works). Do **not** add a per-request
check in netd — that would itself be a runtime ACL and violate §3.

**H2 — Missed interrupt permanently degrades the NIC to 50 ms polling.**
`userspace/virtio-net/src/main.rs:241-254`, `kernel/src/devices/irq.rs:81-95`.
The ISR status register is read *only* in the IRQ-message branch (`idx == 1`);
the 50 ms timeout branch drains the rings without reading ISR. virtio INTx stays
asserted until ISR is read, and the kernel's IRQ→IPC delivery is fire-and-forget
— it silently drops the event on shard-lock contention or a full queue
(`irq.rs:89-91`). Drop one delivery and the line stays asserted, no further edges
are generated, the driver never again sees `idx == 1`, and all RX/TX completion
handling falls to the 50 ms fallback forever: ~640 pps ceiling, +50 ms latency,
silently. Also collides with the project's own "no timeouts as liveness guards"
rule.
*Fix:* read `isr_status()` unconditionally before every drain (one line), which
makes the interrupt self-healing and lets the timeout become a true idle sleep.

**H3 — Concurrent DNS resolve drops the first requester's reply token (permanent client hang).**
`userspace/netd/src/main.rs:161,923-948`.
`pending_dns` is a single `Option`, and the DNS socket is created with exactly
one query slot (`vec![None]`, `main.rs:140`). A second `NET_DNS_RESOLVE` while
one is outstanding overwrites `*pending_dns` at `main.rs:939`, dropping the first
client's reply token on the floor — that client blocks in `call` forever. Even
serially, only one query can be in flight. Root cause: single-slot DNS state
where the prior audit (issue 8) assumed a 4-slot `pending_dns` vector.
*Fix:* make `pending_dns` a `Vec`, size the DNS socket query slots to match, and
`cancel_query` + reply `-ENOENT` on eviction. (This is the shape the prior
audit's issue 8 already prescribes — the current code is a partial landing of it
and is worse than no DNS.)

**H4 — TX completions are reclaimed only on interrupt, so bursts get `-EBUSY` and frames are lost.**
`userspace/virtio-net/src/main.rs:341,377-390`; netd `drain_tx` at `main.rs:489-495`.
The `NET_PKT_SEND` handler never reclaims completed TX descriptors; with only 8
TX buffers, a netd TX burst exhausts `tx_free` and returns `Busy` even though the
device finished the frames long ago — the completions sit in the used ring
waiting for an IRQ message that is starved behind the continuous `NET_PKT_SEND`
stream (the kernel polls the listen endpoint at higher priority than the IRQ
endpoint, `handlers.rs:262`). netd discards the `-EBUSY` reply (`let _ =`) and the
frame is silently lost.
*Fix:* call `drain_tx()` at the top of the `NET_PKT_SEND` arm (or on `Busy`).

### MEDIUM

**M1 — Synchronous driver calls inside connect/accept block the whole server.**
`userspace/netd/src/main.rs:453-468` (`drain_rx_tx` uses blocking
`call_with_payload`), invoked from the 200-iteration connect (`:644-659`) and
accept (`:697-718`) loops, plus the ICMP/TCP/UDP send arms (`:767,782,800`).
While a connect or accept spins, netd is fully blocked: no other client is
served and no RX is processed except what `drain_rx_tx` pulls inline. This is the
fragility the prior audit's issues 2/3 target (deferred connect/accept) and its
cross-cutting note on the ICMP synchronous path; flagged here as the concrete
AGENTS.md §7 violation — a single-threaded server making a blocking downstream
`call` that can stall the loop. *(see prior audit #2, #3, cross-cutting)*

**M2 — RX frames dropped toward netd with no backpressure or accounting.**
`userspace/virtio-net/src/main.rs:309` (`let _ = send_msg_with_payload(...)`).
The kernel returns `WouldBlock` once netd's queue hits `MAX_QUEUE_LEN` (1024);
the driver discards the error and re-posts the buffer. Good: the send is truly
non-blocking, so the driver cannot deadlock against a busy netd. Bad: overflow
loses frames with zero telemetry, and each dropped send leaves a stale entry in
the kernel `waiting_senders` list that is never redeemed (spurious-wake source,
matches the known lossy-pending-wake debt).

**M3 — netd `rx_queue` is unbounded.**
`userspace/netd/src/main.rs:300-320,1029`.
Every `NET_PKT_RECV` frame is pushed into an unbounded `VecDeque`. smoltcp drains
it fully each poll, so steady-state depth is one inter-poll batch, but there is
no cap — a burst between polls, or any condition that slows `iface.poll`, grows
it without limit. Bound it and count drops.

**M4 — Plain (non-volatile) access to DMA-shared virtqueue rings.**
`userspace/virtio-core/src/virtqueue.rs:232-263,282-288`.
Ring fields the device writes (`used.idx`, ring elements, `flags`) are read/
written with ordinary loads/stores; the `fence(Acquire/Release)` calls only order
*atomic* accesses, and the Acquire fence in `pop_used` sits after the element
read rather than between the `used.idx` read and the element read. Correct on x86
today, but UB-class: a smarter LLVM or a non-x86 port can hoist the `used.idx`
load out of the drain loop.
*Fix:* `read_volatile`/`write_volatile` (or atomics) on `idx`/`flags`/ring slots,
with the Acquire fence placed after the `used.idx` read.

**M5 — Modern-PCI capability parsing assumes a single BAR and accepts partial cap sets.**
`userspace/virtio-core/src/transport/modern_pci.rs:99-112`,
`userspace/virtio-core/src/pci.rs:164-184`.
Only `common_cfg_bar` is mapped; `notify/isr/device_cfg` VAs are computed as
base+offset without checking their BAR indices match. `caps_complete` uses `||`
across the four cap offsets and forces `is_modern = true` on any parse, so a
device exposing only some caps aliases `common_cfg` to BAR offset 0 and writes
status/queue registers into whatever lives there. QEMU-only today; latent
portability landmine. Also queue size is written as 64 without clamping to the
device max (`modern_pci.rs:163-174`).

**M6 — `NET_REGISTER_RECV` accepts a raw token in a data word with no grant, overwritable by anyone.**
`userspace/virtio-net/src/main.rs:393-396`.
The RX-notify endpoint is taken from `msg.words[0]` with no sender validation and
silently overwrites any prior registration — a "capability" smuggled as an
integer through a data word rather than a proper grant, contrary to the cap-wire
discipline. Whoever last registers owns the RX stream. Works only because netd
uses plain `call` (so the `words[0]=len` clobber doesn't bite) and is the sole
caller.

### LOW

- **L1 — `recvfrom` back-fills peer address via a bogus `NET_POLL` call**
  (`socket.rs:302-326`); `NET_POLL` never populates `words[1..2]`, so the
  address is garbage. Prior audit #4/5 already prescribes taking addr/port from
  the `NET_RECV` reply words. *(see prior audit #4/5)*
- **L2 — ping never closes its ICMP socket** (`userspace/ping/src/main.rs`,
  success and all 7 error paths) → one leaked netd socket-table entry per run.
- **L3 — ping single-shot hang on loss.** One echo request, no retransmit, and
  `handle_icmp_recv` unconditionally parks in `pending_recv`
  (`netd/src/main.rs:973-975`), so ICMP recv never returns EAGAIN — the
  `for _ in 0..1000` bound and its EAGAIN branch are dead code; real loss hangs
  forever. *(prior audit #11 — confirmed and sharpened: it is a block-forever,
  not a spin)*
- **L4 — wget/curl have no HTTP parsing:** no status-line check (404/500 →
  exit 0 + `WGET_OK`), no header/body split, no `Content-Length`, no chunked
  decoding despite sending `HTTP/1.1`; byte count includes headers; termination
  relies on C1's accidental "first EAGAIN = EOF".
- **L5 — curl `-o FILE` parsed but ignored** (`curl/src/main.rs:156-160`, both
  branches write to stdout).
- **L6 — `alloc_contiguous` in dma-core is broken** (`dma-core/src/dma.rs:70-94`:
  can never allocate ≥2 pages and never checks physical contiguity). Unused by
  virtio-net; a loaded gun for any future multi-page ring.
- **L7 — per-frame `debug_print` in the datapath** (`virtio-net/src/main.rs:313,
  360-370`; netd `main.rs:492`): a 14-arg `format!` per transmitted frame plus
  per-batch traces dominate per-packet cost and serialize the datapath through
  the serial log.
- **L8 — feature negotiation ordering** violates virtio §3.1.1 (features written
  before ACKNOWLEDGE/DRIVER; `VERSION_1`/`F_MAC` acceptance never verified;
  `reset` doesn't poll status→0). QEMU tolerant. `modern_pci.rs:150-161`,
  `main.rs:129-130`.
- **L9 — driver hard-coded IRQ routing / single subscriber** (`idt.rs:1399-1421`,
  `irq.rs:34`): if virtio-net and virtio-blk ever share a PIRQ line, the later
  `irq_attach` steals the other's interrupts.
- **L10 — persistent recv error → 100% CPU spin** with no log/backoff
  (`virtio-net/src/main.rs:246` `Err(_) => continue`).

### Test-coverage gaps (zero probe coverage)

`close`/FIN-vs-RST, UDP send/recv, recv-EOF on peer close (C1 lives here), connect
errno fidelity, concurrent sockets / backlog / EADDRINUSE, transfers > 4 KiB
(C2 lives here), real DNS resolution (`l2_dns_basic` prints `DNS_OK` in *both*
match arms — it passes on failure; `main.rs:25-28`), and the ping loss/timeout
path. `l2_net_denied` only asserts `!has_netd()` (token-not-injected), never
attempts the registry path from H1, so it gives false assurance about the
"cannot reach netd" claim. `l2_socket_basic` is loopback-only and its closes
assert nothing.

---

## 2. Architecture assessment

**What's sound.**
- The layering is right: virtio-net is a pure frame-I/O leaf, netd wraps smoltcp,
  clients speak BSD sockets over IPC. This matches the microkernel discipline —
  no network knowledge in the kernel, one InvokeOp-free path (all NET verbs are
  IPC labels, no new syscalls).
- smoltcp is a good choice and is *mostly* used per its contract: `SocketSet`,
  the `Device`/`RxToken`/`TxToken` bridge, DHCP/DNS sockets, and the
  `iface.poll` cadence are idiomatic.
- The RX delivery path is genuinely non-blocking (driver→netd is a one-way async
  send), so the classic single-threaded cross-`call` deadlock is structurally
  avoided on the hot path — the most important thing to get right, and it is
  right.
- The primary capability path is correct and fails closed: NET-less spawn → no
  `TOKEN_EXTRA_0` → `socket()` returns `ENOSYS`, no runtime ACL.

**What's questionable.**
- **Two lifetimes for netd's own responsiveness.** The RX path is async, but
  connect/accept/send still spin synchronously (M1) and block the whole server.
  The stack is half-migrated to the async runtime the philosophy mandates. The
  prior audit's deferred-reply plan (its issue 3 as the enabler) is the correct
  and already-designed fix.
- **Trust boundaries are under-defended.** The device side (C3, M4, M5) and the
  registration side (M6) both trust their inputs. For QEMU-only this is
  invisible; as *code discipline* in an audited `no_std` service it is the
  weakest area.
- **The capability story has a hole (H1).** Not a netd bug per se — it is that a
  well-known registry name is a second discovery path that bypasses the envelope
  gate. Worth deciding deliberately: either netd is envelope-only, or the NET
  bit is acknowledged as advisory.
- **Correctness of the data path is fragile at exactly the boundaries the
  harness doesn't exercise** (C1 EOF, C2 4 KiB). The 41-byte test page hides both.

Overall: the *shape* is correct and philosophy-aligned; the *robustness* is at
early-prototype level, with the failure modes clustered in (a) deferred-reply
completeness, (b) device/registration trust boundaries, and (c) untested data
boundaries.

---

## 3. Fix plan (respects kernel freeze + philosophy)

All fixes are userspace; none needs a new syscall or a kernel change. Merge with
the prior audit's landing order rather than competing with it.

**Tier 0 — correctness, do before any perf work (small, high-value):**
1. **C1** reorder the deferred TCP recv to drain-then-EOF (~5 lines,
   `netd/src/main.rs:237`). Add an `l2_http_close` probe that reads a body
   delivered with FIN in one batch — this is the regression the harness is
   blind to.
2. **C2** size the client reply buffer to `header + len` and clamp netd's reply
   payload; never return a count exceeding bytes copied
   (`socket.rs:222`, `netd/src/main.rs:826`). Add a >4 KiB transfer probe.
3. **H3** make `pending_dns` a `Vec` and match DNS query slots; cancel + error on
   eviction (folds into prior audit #8).

**Tier 1 — the deferred-reply migration (prior audit #3 first, then 2/7/4-5/11):**
4. Land the prior audit's deferred connect/accept/recv plumbing, which also
   deletes M1's synchronous spin loops and the `inject_loopback_arp` call sites.
   Adopt the real errno vocabulary while there.

**Tier 2 — driver robustness:**
5. **H2** read ISR unconditionally per drain (1 line).
6. **H4** reclaim TX in the send arm.
7. **C3 / M4** bounds-check device-supplied `used.id`; make ring access volatile
   with a correctly-placed Acquire fence.
8. **M2 / M3** bound netd `rx_queue` and add drop counters; account driver→netd
   send failures.

**Tier 3 — capability decision (needs a call, not just a patch):**
9. **H1** either move netd endpoint delivery to envelope-only (remove the
   `netd:main` public registration) or explicitly document the NET bit as
   advisory. Extend `l2_net_denied` to attempt the registry path so the test
   matches the guarantee. Recommend envelope-only — it is the structural fix and
   the cheaper long-term story.

**Tier 4 — hygiene / portability (as touched):**
10. Client HTTP parsing (L4), ping socket close + real timeout (L2/L3), curl `-o`
    (L5), datapath `debug_print` gating (L7), virtio negotiation ordering and
    queue-size clamp (L8/M5), fix `l2_dns_basic`'s vacuous assertion.

---

## 4. Verdict

**Not shippable as-is, but close — and the gap is small, well-understood work,
not a redesign.** The architecture is the right shape and the one deadlock class
that would be fatal for a single-threaded server is already avoided. However
there are two silent-data-loss bugs on the realistic HTTP path (C1, C2) and a
memory-unsafety class at the device boundary (C3) that a serious hobby OS should
not ship, plus a capability-model hole (H1) that contradicts a headline
guarantee. None is hard to fix; C1/C2/H3 are a day's work and immediately make
wget/curl trustworthy for real (non-41-byte) downloads.

Recommended gate to "shippable for a hobby OS": **Tier 0 + Tier 1 + H2/H4 + a
decision on H1**, with new probes for the HTTP-close and >4 KiB paths so the
fixes are verified rather than asserted (philosophy §8). The remaining MEDIUM/LOW
items are legitimately deferrable to v1.1 as long as they are tracked, because
they are QEMU-invisible or edge-only. Ship after Tier 0/1; do not ship on the
strength of the current 41-byte harness pass alone.
