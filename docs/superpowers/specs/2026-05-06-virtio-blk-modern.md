# virtio-blk — modern, async, zero-copy redesign

**Status:** Design approved 2026-05-06.
**Scope:** userspace driver only — no kernel changes.
**Phase context:** Phase 2 → Phase 3 transition. Not on the explicit Phase 3 exit-criteria list; serves the strategic concern that disk I/O speed gates spawn perf and boot time.

---

## 1. Problem

Today's `userspace/virtio-blk/` driver is legacy-mode (pre-virtio-1.0), single-in-flight, polled, and bounce-buffered:

- One outstanding device request at a time.
- Service IPC handler is `recv_any → process → reply`; multiple consumers serialize at the service boundary, not just at the device.
- Polled completion via spin-loop on `used_idx`.
- Device DMAs into a static driver-owned buffer at phys `0x14000000`, then driver memcpys into the caller's reply buffer.

Measured throughput is ~20–30 MB/s. Round-trip-per-request analysis:

```
single-in-flight cap = request_size / round_trip
4 KB / 100µs        ≈ 40 MB/s   ← matches observed
64 KB / 100µs       ≈ 640 MB/s  ← what we'd see with bigger requests OR pipelining
```

The 7× gap is mostly *one in-flight at a time*; a smaller share is the per-request memcpy.

## 2. Goals & non-goals

**Goals:**
- Modern virtio (1.0+) transport on the same QEMU device.
- Zero-copy: device DMAs directly into the caller's pages.
- Multiple in-flight requests, concurrent across multiple consumers.
- IRQ-driven completion (not polled).
- Reusable transport core for Phase 4 virtio-net.
- SOLID: each layer has one job, depends only on traits/interfaces of the layer below, is independently testable.
- Closes the 7× throughput gap. Floor: **≥ 150 MB/s** on sustained sequential read in the harness; expected ~200 MB/s.

**Non-goals (deferred):**
- MSI-X interrupt delivery (kernel doesn't support yet; legacy IRQ via existing `irq_attach` is sufficient).
- Write path optimization (read is the boot/spawn bottleneck).
- Block-device cache *inside* the driver (VFS already has one).
- Recovery from device hang (one-shot fail-fast; primordial restart is the next-tier mitigation).

## 3. Architecture

Three crates, layered downward:

```
┌────────────────────────────────────────────────┐
│ libcluu::fs::client (sync + async wrappers)    │
├────────────────────────────────────────────────┤
│ BlkSessionClient                               │ caller-side helper
├────────────────────────────────────────────────┤
│ blkdev:main IPC: BLK_OPEN/SUBMIT/COMPLETE      │ wire protocol
├────────────────────────────────────────────────┤
│ userspace/virtio-blk/                          │ block-device service
│   BlkSession  BlkRequestQueue  BlkProtocol     │
├────────────────────────────────────────────────┤
│ userspace/virtio-core/                         │ reusable transport
│   trait Transport                              │
│   Virtqueue  ModernPciTransport  IrqSource     │
└────────────────────────────────────────────────┘
```

Open/Closed: virtio-net later adds a second `impl Transport` and reuses `Virtqueue` and `IrqSource` unchanged.

### 3.1 `userspace/virtio-core/` (new crate)

```
src/
  lib.rs                  // re-exports
  pci.rs                  // virtio modern capability scan
  transport/
    mod.rs                // trait Transport
    modern_pci.rs         // ModernPciTransport
  virtqueue.rs            // Virtqueue
  irq.rs                  // IrqSource (irq_attach wrapper)
  dma.rs                  // DmaPool — pinned region for desc tables, headers, status bytes
```

Public traits:

```rust
pub trait Transport {
    fn negotiate_features(&mut self, requested: u64) -> Result<u64>;
    fn configure_queue(&mut self, idx: u16, vq: &Virtqueue) -> Result<()>;
    fn notify(&self, queue_idx: u16);
    fn isr_status(&self) -> u8;
    fn set_driver_ok(&mut self) -> Result<()>;
}
```

`Virtqueue` is the descriptor ring + avail + used rings, with internal free-list and per-descriptor cookie storage:

```rust
pub struct Virtqueue {
    queue_size: u16,
    desc_table: DmaRegion,    // VRingDesc[queue_size]
    avail: DmaRegion,         // VRingAvail
    used: DmaRegion,          // VRingUsed
    free_head: u16,
    num_free: u16,
    last_used: u16,           // shadow used.idx
    cookies: Vec<Option<u64>>, // desc_idx → user cookie
}

impl Virtqueue {
    pub fn alloc_chain(&mut self, n: u16) -> Option<DescChain>;
    pub fn submit(&mut self, chain: DescChain, cookie: u64);
    pub fn pop_used(&mut self) -> Option<(u64 /*cookie*/, u32 /*len*/)>;
    pub fn free_capacity(&self) -> u16;
}
```

Invariants:
- `submit` advances `avail.idx`. One `notify()` covers any number of queued submits — the driver service batches submits during one recv-burst.
- `pop_used` is non-blocking; returns `None` when `used.idx == last_used`.

`IrqSource` wraps existing `irq_attach`: at construction, attaches the IRQ to a private endpoint; `wait_blocking()` does `ipc_recv_any` on it. The driver's main loop instead does `recv_any([control_endpoint, irq_endpoint, session_endpoints*])` and dispatches on index.

### 3.2 `userspace/virtio-blk/` (rewritten)

```
src/
  main.rs                 // service entry, recv loop, dispatcher
  protocol.rs             // VIRTIO_BLK_T_IN/OUT, header layout, status codes
  request_queue.rs        // BlkRequestQueue — owns the Virtqueue
  session.rs              // BlkSession — per-client state
  ipc.rs                  // BLK_OPEN_SESSION / BLK_SUBMIT / BLK_COMPLETE wire format
```

`BlkRequestQueue`:

```rust
impl BlkRequestQueue {
    pub fn submit(&mut self, sess: SessionId, req: BlkRequest) -> Result<RequestId>;
    pub fn poll_completions(&mut self) -> Vec<Completion>;  // drains used ring
    pub fn free_capacity(&self) -> u16;
}
```

`BlkSession` lives until the caller's completion endpoint is revoked (caller exit) or `BLK_CLOSE_SESSION`:

```rust
pub struct BlkSession {
    completion_endpoint: usize,
    caller_space_token: usize,
    granted_pages: BTreeMap<u64, GrantHandle>, // by caller_phys
    in_flight: BTreeMap<RequestId, RequestMeta>,
    queue_depth: u16,                          // default 32
    next_request_id: u64,
}
```

SRP boundary: `BlkSession` owns *only* per-client lifecycle. It does not touch the virtqueue directly; it submits via `BlkRequestQueue`. `BlkRequestQueue` does not know about sessions; it stores the cookie blob the session asked it to.

### 3.3 Caller-side helper — `libcluu::fs::client::BlkSessionClient`

```rust
pub struct BlkSessionClient { ... }
impl BlkSessionClient {
    pub fn open(blkdev_endpoint: usize) -> Result<Self>;
    pub fn read_blocking(&mut self, lba: u64, buf: &mut [u8]) -> Result<usize>;
    pub fn submit_async(&mut self, lba: u64, buf: &mut [u8]) -> Result<RequestHandle>;
    pub fn drain_completions(&mut self) -> Vec<(RequestHandle, Result<usize>)>;
}
```

Interface segregation: a caller using only `read_blocking` does not pay for the async surface; a caller using `submit_async` does not have a sync wrapper forced on it.

`read_blocking` internally is a thin wrapper that calls `submit_async` then loops on the completion endpoint until the matching `RequestHandle` arrives, queueing other completions for later `drain` (so a caller mixing modes does not lose completions).

## 4. Data flow

### 4.1 Session open (one-shot per consumer)

```
Caller                                     BlkService
  ├─ create completion_endpoint
  ├─ derive grant for it (IPC_SEND)
  ├─ ipc_call BLK_OPEN_SESSION ──────────► allocate SessionId
  │                                        store completion_ep + caller_space_token
  └◄────── reply (session_id) ────────────┘
```

### 4.2 Read submission (async, non-blocking)

```
Caller                                     BlkService
  ├─ buf: page-aligned, n = ceil(len/4K) pages
  ├─ space_grant(caller → driver, page1..pageN)
  ├─ ipc_send BLK_SUBMIT {
  │      session_id, request_id, lba,
  │      n_pages, caller_grant_base_in_driver,
  │      total_bytes
  │   } ────────────────────────────────► session.lookup
  │                                        for each granted page:
  │                                          phys = virt_to_phys(driver_space, va)
  │                                          desc[i].addr = phys
  │                                        prepend req_header desc (DmaPool)
  │                                        append status desc (DmaPool)
  │                                        vq.submit(chain, cookie=(sid|rid))
  │                                        // notify deferred to end of recv-burst
  │
  Caller continues; can submit more.       Service drains its current recv-burst,
                                           then transport.notify(0) once.
```

Notify batching is the single biggest throughput lever: 4 callers submitting in one scheduler quantum produces *one* exit-to-host instead of four.

### 4.3 Completion (IRQ-driven)

```
Hardware ISR fires
  → kernel dispatch_scancode-equivalent
  → IPC msg arrives at virtio-blk's irq_endpoint

BlkService recv loop wakes:
  ├─ read transport.isr_status() (clears interrupt)
  ├─ while let Some((cookie, len)) = vq.pop_used():
  │     (sid, rid) = unpack(cookie)
  │     status = read driver-mapped status byte (DmaPool)
  │     result = if status == OK { Ok(len) } else { Err(IoError) }
  │     send_with_payload(session.completion_endpoint,
  │                       BLK_COMPLETE, &{rid, result})
  │     space_revoke(caller → driver, granted_pages_for_this_req)
  │     session.in_flight.remove(rid)
  └─ done
```

### 4.4 Caller completion handling

- Caller using `submit_async` directly: their recv loop sees `BLK_COMPLETE` on its endpoint and matches `rid` against in-flight handles.
- Caller using `read_blocking`: wrapper does `submit_async`, then ipc_recv on the completion endpoint until it sees the matching `rid`. Other `BLK_COMPLETE` messages received in the meantime are pushed into `pending_completions` for the next `drain_completions` call.

### 4.5 Session close

- Caller exit (procmgr exit notification) or explicit `BLK_CLOSE_SESSION`.
- Driver revokes all outstanding grants, marks in-flight as orphaned (their completions, when they arrive, are silently consumed and dropped), frees the SessionId.

## 5. Error handling

| Failure | Detection | Behavior |
|---|---|---|
| Queue full at submit | `vq.free_capacity() < n` | `BLK_SUBMIT_NACK { rid, Busy }` on session endpoint. `submit_async` returns `Err(WouldBlock)`; `read_blocking` retries with bounded backoff. |
| Per-session depth cap exceeded | `session.in_flight.len() >= queue_depth` | Same NACK path. Default cap 32; prevents starvation. |
| Caller endpoint dead | `send_with_payload` → `Error::NotFound` | Mark session dead; drain remaining completions silently; revoke grants; free SessionId. |
| Caller crashes mid-flight | procmgr `PROC_EXIT_LABEL` (existing path) | Driver subscribes to procmgr exit events for tracked sessions; on exit, revokes grants and frees SessionId. |
| Bad submit payload (unknown sid, oversized n_pages, bad LBA) | Validated before touching virtqueue | `BLK_SUBMIT_NACK { rid, InvalidArgument }`. No descriptor consumed. |
| `virt_to_phys` fails (caller granted bad pages) | Returns `Err` | Same NACK path. Grant revoked. |
| Device returns status != OK | Status byte in DmaPool after completion | Completion delivered as `Err(IoError)`. Grant still revoked. |
| Device hang (IRQ never fires) | Per-request 5s deadline; periodic timer wakeup of recv loop checks deadlines | Failed requests reported as `Err(Timeout)`. After 3 consecutive timeouts: device marked dead, all subsequent submits get `Err(DeviceDead)`, fatal log. |
| ISR spurious | `isr_status() & 0x1 == 0` | Recv loop continues; no `pop_used()`. |
| `space_grant` fails on caller side | Caller's `submit_async` returns the error | Driver never sees the request. |

**Driver-side invariants:**
1. Every accepted submit either delivers a completion or revokes its grants on session close — never leaks pages or descriptor entries.
2. `pop_used()` order is the device's order, not submission order. The cookie carries `(session_id, request_id)`; the driver does not depend on order.
3. NACKs go on the same session completion endpoint as completions — caller has one channel to drain.

## 6. Testing

### 6.1 Unit tests in `userspace/virtio-core/`

- `Virtqueue::alloc_chain` + `submit` + `pop_used` round-trip against a `FakeTransport`. No QEMU needed.
- `alloc_chain` returns `None` when `num_free < n`.
- Avail and used index wraparound at `queue_size`.
- Cookie stored on submit is the cookie returned on `pop_used`.

### 6.2 Unit tests in `userspace/virtio-blk/`

- `BlkSession` lifecycle: open, 32 in-flight, 33rd is NACKed; close revokes grants; orphan submits after close are NACKed.
- `BlkRequestQueue::submit` builds the right descriptor chain shape: `[req_header(OUT_FROM_DRIVER) → buf_pages...(IN_FROM_DEVICE) → status(IN_FROM_DEVICE)]`.
- Cookie pack/unpack round-trip: `(sid, rid) → u64 → (sid, rid)`.

### 6.3 Integration tests in harness

- **`l2_blk_basic`** — read 4 KB from sector 0; verify magic bytes match the boot ext2 superblock signature. End-to-end submit + IRQ wake + completion routing.
- **`l2_blk_concurrent`** — spawn 4 children; each reads a different region for 5s in a tight loop. Assert: no missed completions, no leaked grants, no deadline timeouts. Per-caller stat: avg latency, requests/sec.
- **`l2_blk_session_teardown`** — child opens a session, reads, exits without `BLK_CLOSE`. Driver must revoke session via procmgr-exit hook. Subsequent boot iteration shows no leaked SessionId in `top` / driver telemetry.
- **`l2_blk_perf`** — single sequential read of 64 MB. Throughput must be **≥ 150 MB/s** (regression floor; expected ~200 MB/s).

### 6.4 Soak (Phase 3 alignment)

The 1000-pipeline soak from Phase 3's exit criteria implicitly exercises the new driver. After this design lands, that soak must still pass; `/proc/meminfo` must show bounded driver memory.

### 6.5 TDD plan implication

- Each task pair: failing test + minimum impl. e.g. *"`Virtqueue::alloc_chain` returns `None` when full"* red, then implement free-list, then green.
- Integration tests come at phase boundaries: after virtio-core units pass, after blk service compiles, after wiring complete.
- `l2_blk_perf` is *baseline + assert ≥ N MB/s*. If it fails on slow hardware we relax to a structural assertion (≤ 2 IPC roundtrips per request) rather than a specific MB/s.

## 7. Migration & freeze posture

**Pure userspace.** No kernel changes. Existing kernel facilities used:
- `irq_attach` (already wires legacy PIC IRQs to userspace endpoints — used by kbd today).
- `space_grant` / `space_revoke` (already used by IPC ring buffers and grant pages).
- `virt_to_phys` (already used by today's driver for descriptor addrs).
- `space_map` with `MAP_DEVICE` (already used for the BAR mapping).

The kernel freeze (through ~2026-10-21) does not gate this work. If during implementation a kernel hole surfaces (e.g. `space_grant` doesn't accept the granularity we need), the named-userspace-failure rule applies and we defer.

**Old driver retired in one shot.** Once the new driver passes `l2_blk_basic` + `l2_blk_perf`, the old `userspace/virtio-blk/src/virtio.rs` and `virtqueue.rs` are deleted. No flag-day migration of consumers because the consumer-facing API (libcluu::fs::client) keeps `read` semantics; new `submit_async` is purely additive.

## 8. Open questions

None at design time. Implementation phase may surface kernel surface gaps; if so, they get a named-userspace-failure ticket per the freeze rules and a separate decision.
