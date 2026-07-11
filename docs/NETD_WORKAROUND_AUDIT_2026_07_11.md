# netd Workaround & Fragility Audit — 2026-07-11

Scope: `userspace/netd/src/main.rs`, `userspace/libcluu/src/posix/socket.rs`,
`userspace/ping/src/main.rs`, `userspace/libcluu/src/registry.rs`,
`userspace/registry/src/main.rs`, smoltcp 0.13.1 source.

Constraints honored: kernel freeze (all fixes are userspace), no new syscalls
(one already-reserved IPC label used), no-timeouts rule (protocol timers and
network-loss deadlines are semantics, not deadlock guards; startup waits are
converted to blocking-forever recv), single-threaded netd on the libcluu
async runtime.

Verdict summary:

| # | Issue | Fix now (pre-v1)? | Size |
|---|-------|-------------------|------|
| 1 | Loopback ARP injection | **Yes** — self-healing ARP loopback in `NetdDevice` | ~25 lines |
| 2 | Accept fd_map swap | **Yes** — listener-slots fd model | ~80 lines |
| 3 | Connect spin loop / errno | **Yes** — deferred connect + real errnos | ~60 lines |
| 4 | UDP send | **Yes** | ~40 lines |
| 5 | UDP recv | **Yes** (shared with 4) | ~40 lines |
| 6 | DHCP `addrs.clear()` | **Yes** — retain-based removal | ~15 lines |
| 7 | TCP close / FIN | **Yes** — `close()` + drain list | ~30 lines |
| 8 | DNS | Yes, after 3–5 land | ~90 lines |
| 9 | 200 ms poll tick | **Yes** — `poll_delay()`-driven timeout | ~10 lines |
| 10 | Startup yield loops | **Yes** — delete; registry already parks | net-negative LOC |
| 11 | ping iteration poll | Yes — netd-side recv deadline | ~25 lines |

Defer to v1.1: dedicated `Medium::Ip` loopback stack, TIME_WAIT tuning,
SO_RCVTIMEO/SO_ERROR surface, listen backlog sizing.

Recommended landing order: 3 → 1 → 2 → 7 → 4/5 (+11 netd side) → 6 → 8 → 9 → 10.
3 first because every later item reuses its deferred-reply plumbing.

---

## 1. Loopback ARP injection

**Why it exists.** The interface is `Medium::Ethernet` (virtio-net needs
Ethernet frames), and smoltcp on Ethernet requires a neighbor-cache entry
before dispatching any unicast IP packet — including 127.0.0.1, which is just
another address on the interface. smoltcp has no internal loopback shortcut
and no public API to pre-fill the cache: `NeighborCache::fill` is `pub`
(neighbor.rs:82) but the `neighbor_cache` field of `InterfaceInner` is
private and `Interface`/`context()` expose no accessor. Injecting a forged
ARP reply into `rx_queue` is genuinely the only external way to populate the
cache in 0.13.1 — that's the constraint that forced the hack.

**Answering the three questions:**

- *Loopback medium?* `medium-ip` exists (feature-gated), but medium is a
  per-device capability. One `NetdDevice` can't be both; a `Medium::Ip`
  loopback means a **second** `(Device, Interface, SocketSet)` stack and
  per-destination stack selection at connect/listen time. Correct
  architecture, too big for now — see v1.1 note below.
- *Neighbor cache pre-fill?* Not reachable through public API (verified
  against 0.13.1 source). Ruled out.
- *"Don't ARP for 127.0.0.1"?* No such knob on Ethernet medium.

**Correct pre-v1 fix: make ARP-for-127/8 self-answering in the device.**
`NetdTxToken::consume` already reflects frames whose dst MAC == own MAC into
`rx_queue` (main.rs:907-911). Extend it: if the outgoing frame is ARP
(EtherType 0x0806 at offset 12) and the ARP *target protocol address*
(offset 38..42) is in 127.0.0.0/8, push it to `rx_queue` instead of
`tx_queue`:

```rust
// in NetdTxToken::consume, before the own-MAC check
let is_loopback_arp = len >= 42
    && frame[12] == 0x08 && frame[13] == 0x06   // EtherType ARP
    && frame[38] == 127;                        // target IP in 127/8
if is_loopback_arp || (len >= 6 && &frame[0..6] == self.own_mac) {
    self.rx_queue.push_back(frame);
} else {
    self.tx_queue.push_back(frame);
}
```

Why this is correct and self-healing: verified in smoltcp source
(`iface/interface/ipv4.rs:255-295`) — `process_arp` fills the neighbor cache
from the **sender** fields of *any* ARP aimed at one of our own addresses,
requests included ("We fill from requests too…"). smoltcp's outgoing ARP
request for 127.0.0.1 carries sender = (own MAC, 127.0.0.1) — source address
selection picks 127.0.0.1 for a 127/8 destination — and target = 127.0.0.1.
All three ingress checks pass (`has_ip_addr(target)` ✓, unicast sender ✓,
`in_same_network` ✓ since 127.0.0.1/8 is assigned). So the moment smoltcp
ARPs, its own request loops straight back and refreshes the cache — at boot,
at every 60 s expiry, mid-handshake, always. It also emits a reply addressed
to its own MAC, which the existing own-MAC rule loops back harmlessly (fills
the same entry again, emits nothing further).

Then **delete** `inject_loopback_arp()` and all three call sites (boot,
connect, accept). The 200-iteration yield loops go away separately via
issue 3/2.

**Worth doing now?** Yes. It's smaller than the workaround it replaces and
closes the expiry-mid-handshake hang class entirely.

**Risks.** Frame parsing at fixed offsets — safe because netd never uses VLAN
tags and smoltcp always emits untagged ARP of exactly 42 bytes. Real ARP
(target outside 127/8) is untouched and still exits the NIC. Keep the check
tight to 127/8 so a malicious/odd on-net peer address can't be spoofed into
loopback.

**v1.1 direction.** Dedicated loopback stack: `LoopDevice` (rx==tx VecDeque,
`Medium::Ip` capability, no ARP ever), own `Interface` with 127.0.0.1/8, own
`SocketSet`. netd routes each socket to a stack at `connect`/`listen` time
(dst/bind addr in 127/8 → loop stack). This also removes 127.0.0.1/8 from
the Ethernet interface, which is more correct (RFC 1122 says loopback must
never appear on the wire). Requires fd_map entries to carry a stack tag —
do it together with the issue-2 fd_map restructure if/when appetite exists.

---

## 2. TCP accept fd_map swap

**Why it exists.** smoltcp has no accept(): a listening `tcp::Socket` itself
transitions Listen → SynReceived → Established. To preserve "the listening
fd keeps listening", the handler swaps: accepted fd → old (now-established)
handle, listening fd → freshly created listening handle. It works but the
listen fd's *handle identity* changes every accept, and any other holder of
the old handle is stale.

**How other smoltcp users do it.** The established pattern (smoltcp examples,
embassy-net, Fuchsia's early smoltcp port) is a **listener pool**: N sockets
all `listen()` on the same endpoint (smoltcp explicitly permits multiple
listening sockets on one endpoint); accept scans for one that left Listen
state, hands it out, and backfills the pool with a fresh listener. N is the
effective backlog.

**Correct fix.** Restructure the fd_map value from `(SocketHandle, usize)`
to:

```rust
enum SockEntry {
    Stream { handle: SocketHandle },        // TCP conn, UDP, ICMP, RAW
    Listener {
        endpoint: IpListenEndpoint,
        slots: Vec<SocketHandle>,           // BACKLOG listening sockets
    },
}
```

- `NET_BIND`+`NET_LISTEN` on TCP: create `BACKLOG` (start with 2–4) sockets,
  `listen(endpoint)` each, store as `Listener`.
- `NET_ACCEPT`: find a slot with `!sock.is_listening() && sock.is_open()`;
  replace that slot with a new listening socket; mint a new fd →
  `Stream { handle }`. The listening fd never changes meaning; no handle
  ever silently repoints. If no slot is ready, **defer** — push
  `(reply_token, fd)` onto `pending_accept` and resolve it in the main loop
  (same pattern as `pending_recv`), which also deletes the 200-iteration
  accept spin loop and its `inject_loopback_arp` call.
- `NET_CLOSE` on a `Listener`: `abort()` + remove all slots.

**Worth doing now?** Yes — it's the enabling refactor for deferred accept,
close bookkeeping (issue 7), and the future loopback stack tag (issue 1
v1.1). Contained entirely in netd.

**Risks.** Memory: each backlog slot pins 8 KiB of socket buffers; keep
BACKLOG small (2–4) until needed. Two simultaneous SYNs land in two slots —
that's the point — but a full pool (all slots established, none accepted)
means further SYNs get RST from smoltcp; identical to a full backlog on
Unix, acceptable.

---

## 3. Connect: spin loop, errno conflation

**Why it exists.** Single-threaded server that must reply to the IPC call
before returning to the main loop → inline bounded spin. The bound is
iterations, not time, and every failure collapses to −22.

**Correct fix: deferred connect, exactly like `pending_recv`.** netd already
proves the pattern: hold the reply token, answer later from the main loop.
The client blocks inside its kernel `call` meanwhile — that *is* blocking
connect semantics, no client change needed.

```rust
// state
struct PendingConnect { reply_token: usize, label: u32, fd: usize,
                        handle: SocketHandle, started: Instant }
let mut pending_connect: Vec<PendingConnect> = Vec::new();

// NET_CONNECT handler
match sock.connect(iface.context(), remote, local) {
    Ok(()) => { pending_connect.push(...); /* no reply yet */ }
    Err(ConnectError::Unaddressable) => reply(-ENETUNREACH),
    Err(ConnectError::InvalidState)  => reply(-EISCONN),
}

// main loop, after iface.poll()
pending_connect.retain(|p| match sockets.get::<TcpSocket>(p.handle).state() {
    State::Established => { reply(p, 0); false }
    State::Closed => {
        // RST during handshake ⇒ refused; smoltcp timeout ⇒ timed out
        let timed_out = now - p.started >= CONNECT_TIMEOUT;
        reply(p, if timed_out { -ETIMEDOUT } else { -ECONNREFUSED });
        false
    }
    _ => true,
});
```

For the timeout leg, don't hand-roll a timer: `sock.set_timeout(Some(dur))`
(tcp.rs:782) makes smoltcp itself abort the handshake when SYN retransmits
go unanswered, driving the state to Closed on smoltcp's own wall clock. The
`started` timestamp only disambiguates *why* it closed. This is protocol
semantics, not a deadlock guard, so it doesn't violate the no-timeouts rule.

Also fix the errno vocabulary while here: netd currently returns −22 for
everything. Adopt real values (`ECONNREFUSED`, `ETIMEDOUT`, `ENETUNREACH`,
`EAGAIN`, `EPIPE`) — the client already passes `words[0]` straight to
`set_errno`, so it costs nothing and callers can finally tell no-route /
timeout / refused apart. The distinction the audit asked for falls out of
the state machine + `set_timeout`, no extra mechanism.

**Worth doing now?** Yes, first of all the fixes — issues 2, 4, 5, 8, 11 all
reuse this deferred-reply plumbing, and it deletes the worst inline loop.

**Risks.** Reply-token lifetime: entries must be dropped (and the token
answered with −EBADF or just discarded per kernel semantics) if the fd is
closed while pending — add a sweep in `NET_CLOSE`. A client dying mid-connect
leaves a dangling reply token; that exposure already exists for
`pending_recv`, so this adds no new class. One netd-wide caveat: deferred
replies mean a *blocking* client stays blocked in `call` — correct for
POSIX blocking sockets; nonblocking sockets (when they arrive) must be a
netd-side flag that answers −EINPROGRESS/−EAGAIN immediately instead.

---

## 4 + 5. UDP send / recv not wired

**Why it exists.** Plumbed at socket-creation only; send/recv arms were
never written because nothing exercised them yet (DNS is the first
customer).

**Correct smoltcp pattern.** `udp::Socket::send_slice(data, meta)` where
`meta: UdpMetadata` (from an `IpEndpoint`); `recv_slice(&mut buf) ->
(len, UdpMetadata)`. Datagrams, not streams — no connection state, but the
socket must be **bound** before send or recv (`BindError` otherwise).

`NET_SEND` (sendto already ships dst in words[2..3], socket.rs:278-281):

```rust
else if sock_type == NET_SOCK_UDP {
    let sock = sockets.get_mut::<UdpSocket>(handle);
    if sock.endpoint().port == 0 {                    // auto-bind ephemeral
        let port = alloc_ephemeral(next_ephemeral_port);
        let _ = sock.bind(port);
    }
    let dst = IpEndpoint::new(IpAddress::Ipv4(ipv4_from_word(dst_word)),
                              msg.words[3] as u16);
    match sock.send_slice(&payload[..data_len], dst.into()) {
        Ok(()) => { /* iface.poll + drain_tx */ payload_len }
        Err(SendError::BufferFull)   => -EAGAIN,
        Err(SendError::Unaddressable) => -EINVAL,
    }
}
```

Note: route the resulting frames through the async `drain_tx` path, not the
synchronous `call_with_payload` loop the ICMP arm uses — the ICMP arm should
migrate to `drain_tx` too (it currently blocks the whole server on the
driver's reply, against AGENTS.md §7).

`NET_RECV`: reuse `pending_recv` — the delivery sweep already looks up
`fd_map` per entry, so just add a UDP arm next to the ICMP one:

```rust
Some((handle, NET_SOCK_UDP)) => {
    let sock = sockets.get_mut::<UdpSocket>(handle);
    let mut tmp = [0u8; UDP_RX_BUF];
    match sock.recv_slice(&mut tmp) {
        Ok((len, meta)) => {
            reply_with_payload(rt,
                &Message::new(label, [len, ipv4_to_word(meta.endpoint.addr),
                                      meta.endpoint.port as usize, 0, 0, 0], 1),
                &tmp[..len]);
            true
        }
        Err(_) => false,   // stay pending
    }
}
```

The deferred-reply question answers itself: UDP recv parks identically to
ICMP; the only difference is the reply carries `(addr, port)` in
words[1..2]. TCP recv should *also* move into `pending_recv` instead of
returning −EAGAIN inline, so blocking `recv()` stops being a client-side
spin — same sweep, third arm.

Client side: `recvfrom` currently back-fills the peer address with a bogus
`NET_POLL` call (socket.rs:318-324 — NET_POLL doesn't even populate
words[1..2]). Delete that; take addr/port from the `NET_RECV` reply words,
which the TCP path already sends. Refactor `recv`/`recvfrom` around one
inner helper returning `(len, addr, port)`.

Add a UDP arm to `NET_POLL` (`can_recv`/`can_send`) as well.

**Worth doing now?** Yes — DNS-over-UDP for userspace resolvers and any
probe traffic needs it; ~80 lines total, no new concepts.

**Risks.** Ephemeral-port allocator is shared with TCP today; UDP and TCP
port spaces are independent, so either share the counter (harmless) or
split it. 4-entry packet metadata rings drop datagrams under burst — fine
for v1, bump when DNS retries surface it.

---

## 6. DHCP `addrs.clear()` nukes manual addresses

**Why it exists.** Simplest way to guarantee the old lease is gone.

**Correct pattern: track the DHCP-owned CIDR and remove only it.**

```rust
let mut dhcp_cidr: Option<IpCidr> = None;   // netd state, next to dhcp_handle

DhcpEvent::Configured(cfg) => {
    let new = IpCidr::Ipv4(cfg.address);
    iface.update_ip_addrs(|addrs| {
        if let Some(old) = dhcp_cidr { addrs.retain(|a| *a != old); }
        if !addrs.contains(&new) { let _ = addrs.push(new); }
    });
    dhcp_cidr = Some(new);
    // router: same discipline — remember whether the default route is
    // DHCP-owned before removing/replacing it.
}
DhcpEvent::Deconfigured => {
    iface.update_ip_addrs(|addrs| {
        if let Some(old) = dhcp_cidr.take() { addrs.retain(|a| *a != old); }
    });
    iface.routes_mut().remove_default_ipv4_route();
}
```

`update_ip_addrs` hands you a `heapless::Vec`, which has `retain`. The
127.0.0.1/8 entry then never needs re-adding, and future static addresses
survive renewals. While in this handler, also capture `cfg.dns_servers`
(dhcpv4.rs:41 — SLIRP hands out 10.0.2.3) and feed it to the DNS socket
(issue 8).

**Worth doing now?** Yes — trivial, and it's a prerequisite habit for
issue 8.

**Risks.** `IFACE_MAX_ADDR_COUNT` defaults to 2 in smoltcp — loopback + DHCP
already fills it. The `push` is checked so nothing breaks, but the first
static address will need the `iface-max-addr-count-3` (or higher) cargo
feature. Note it in Cargo.toml now.

---

## 7. No TCP close / FIN

**Why it exists.** fd teardown was conflated with socket teardown: removing
the handle from the SocketSet just deallocates it — smoltcp sends nothing,
and the peer's next segment gets an RST from the interface's
no-matching-socket path.

**Correct fix.** Separate the two lifetimes:

```rust
let mut draining: Vec<SocketHandle> = Vec::new();

NET_CLOSE (TCP arm) => {
    let sock = sockets.get_mut::<TcpSocket>(handle);
    sock.close();                    // queues FIN after pending tx data
    fd_map.remove(&fd);
    draining.push(handle);
    reply(0);                        // POSIX close doesn't wait for FIN/ACK
}

// main loop, after iface.poll()
draining.retain(|&h| {
    if sockets.get::<TcpSocket>(h).state() == State::Closed {
        sockets.remove(h); false
    } else { true }
});
```

TIME_WAIT: nothing to hand-roll — smoltcp runs the full state machine
(FinWait1/2 → TimeWait → Closed after its internal close delay) *as long as
the socket stays in the set*, which is exactly what the drain list does.
`abort()` remains available for a future SO_LINGER-0.

Related gap worth fixing in the same diff: peer-initiated close currently
looks like an infinite −EAGAIN to the client. In the recv path, when
`recv_slice` yields 0 bytes and `!sock.may_recv()`, reply `0` (EOF) instead
of −EAGAIN — without this, `recv()` on a peer-closed connection never
terminates even with everything else fixed.

**Worth doing now?** Yes — small, and RST-on-close will corrupt any
non-trivial client protocol (HTTP keep-alive, etc.) in ways that look like
netd bugs later.

**Risks.** Drained sockets pin 8 KiB buffers for the FIN handshake +
TIME_WAIT duration; bound the list (e.g. 16) and `abort()` the oldest past
the cap. A peer that never ACKs the FIN holds a slot until smoltcp's
socket timeout — set `sock.set_timeout(Some(...))` before `close()` to
bound it.

---

## 8. DNS path

**Correct smoltcp API** (verified against `socket/dns.rs`):

```rust
// startup — servers empty until DHCP delivers them
let dns_socket = dns::Socket::new(&[], vec![None; 4]);   // 4 query slots
let dns_handle = sockets.add(dns_socket);

// in DhcpEvent::Configured
let servers: Vec<IpAddress> =
    cfg.dns_servers.iter().map(|a| IpAddress::Ipv4(*a)).collect();
sockets.get_mut::<dns::Socket>(dns_handle).update_servers(&servers);

// NET_DNS_RESOLVE handler (label 0x419 — already reserved in ipc.rs:440)
let name = core::str::from_utf8(payload)?;
match dns_sock.start_query(iface.context(), name, dns::Type::A) {
    Ok(qh) => pending_dns.push((reply_token, label, qh)),   // defer
    Err(StartQueryError::NoFreeSlot) => reply(-EAGAIN),
    Err(_) => reply(-EINVAL),
}

// main loop sweep
pending_dns.retain(|(rt, label, qh)| {
    match dns_sock.get_query_result(*qh) {
        Err(GetQueryResultError::Pending) => true,
        Ok(addrs) => {
            // words[0]=count, words[1]=first A record; full list in payload
            reply_with_payload(...); false
        }
        Err(GetQueryResultError::Failed) => { reply(-ENOENT); false }
    }
});
```

Single-threaded wait is a non-issue: the DNS socket carries its own
retransmit/timeout state machine per query (Pending → Completed/Failure),
driven by `iface.poll` — the sweep just observes it. No loops, no manual
nameserver packets, and it rides the UDP dispatch inside smoltcp directly
(independent of the NET_SOCK_UDP fd plumbing, though issue 4 is still
wanted for userspace resolvers).

Client side: `gethostbyname`-shaped helper in libcluu that sends
`NET_DNS_RESOLVE` with the name as payload and blocks in `call` until the
deferred reply. Hostname support in `ping`/probes follows for free.

**Worth doing now?** Yes, but sequenced after 3–5 (it's the same deferred
pattern and wants DHCP-delivered servers from issue 6). ~90 lines.

**Risks.** `socket-dns` pulls `udp` internals but is already in Cargo.toml —
no build risk. Query slots leak if a client dies mid-query (reply token
dangles, slot recycles only via `get_query_result`/`cancel_query`); sweep
entries whose reply fails and `cancel_query` them. AAAA unsupported until
proto-ipv6 — return only A records, fine for v1.

---

## 9. 200 ms poll tick as the engine

**Reality check first:** RX is *already* event-driven — virtio-net is
IRQ-driven (main.rs:230-250 in virtio-net) and pushes `NET_PKT_RECV` IPC to
`pkt_recv_ep`, which is in netd's `ipc_recv_any_with_sender` token set, so
inbound frames wake netd immediately. Client ops likewise. The only thing
the fixed 200 ms actually paces is **smoltcp's timers** (retransmit, delayed
ACK, DHCP renew, TIME_WAIT, DNS retry).

**Correct fix: ask smoltcp when it next needs the clock.** That's exactly
what `Interface::poll_delay(now, &sockets)` (iface mod.rs:623) is for — the
canonical integration pattern:

```rust
let now = now_instant(clock_token);
let _ = iface.poll(now, &mut device, &mut sockets);
...
let timeout_ms = match iface.poll_delay(now_instant(clock_token), &sockets) {
    Some(d) if d == Duration::ZERO => 0,          // more work queued: don't sleep
    Some(d) => (d.total_millis() as u64).max(1),
    None => u64::MAX,                             // no timers: sleep until IPC
};
match ipc_recv_any_with_sender(&tokens, &mut buf, timeout_ms) { ... }
```

This makes netd exact instead of 200 ms-quantized: retransmits fire on
smoltcp's schedule, and a fully idle stack blocks forever until an IPC
arrives (true event-driven, and consistent with the no-timeouts rule — the
finite timeouts here are protocol timers smoltcp asked for, not guards).

Is 200 ms acceptable for a hobby OS? It *works*, but it quantizes every RTT
estimate and handshake step to 200 ms and burns wakeups when idle. Since the
fix is ~10 lines against existing APIs, do it now.

**Risks.** A `Some(ZERO)` result means poll again before sleeping — handle
it (loop or `timeout=0`) or you delay work by one recv. Guard against a
pathological 0-loop with a bounded "poll again immediately" count. Also keep
the pending_* sweeps *after* `iface.poll` so deferred replies aren't delayed
by a full sleep.

---

## 10. Startup yield loops

**Key discovery: the correct primitive already exists and already blocks.**
`registry::subscribe_output` → `wait_for_grant` does `ipc_recv_any(...,
u64::MAX)` (registry.rs:385-394, with the no-timeouts rationale in a
comment), and the registry **server parks subscriptions for
not-yet-registered outputs** ("no entry, pending", registry/src/main.rs:288-292)
and drains them when the output registers (main.rs:124-125). So a
subscription races nothing: issued early, it simply blocks until the
producer shows up. Every fixed-count yield loop in front of a
`subscribe_output` is vestigial.

Per site:

- **netd main.rs:93-95** — delete the 100-yield loop outright;
  `subscribe_output("netdev", "main")` parks until virtio-net registers.
  Caveat: the `Err(_) → run_idle` no-NIC fallback is *already* dead code —
  subscribe never returns NotFound for an absent service (it parks), so a
  no-NIC boot hangs netd in the grant wait today, 100 yields or not. If
  no-NIC boots matter, the fix is in virtio-net: register `netdev:main`
  unconditionally and answer `NET_GET_MAC` with an error when probe failed;
  netd's existing error path then routes to `run_idle` deterministically.
  Otherwise delete `run_idle`.
- **vfs main.rs:316** — delete the 100-yield loop; the
  `subscribe_output("session-procmgr"/"procmgr", …)` right below parks.
- **session-procmgr main.rs:214** — the 200-iteration loop polls
  `lookup_service` (immediate None if absent). Replace with
  `subscribe_output("session-vfs", &format!("main:{}", sid))` — same
  brokered grant, parks correctly.
- **login main.rs:657** — same disease, same cure:
  `subscribe_output("session-procmgr", &format!("spawn:{}", ok.session_id))`.
- **shell main.rs:108** — already calls `subscribe_output`; the 20-retry
  loop only guards transient send failure to the registry endpoint. Collapse
  to a single call; a failed send here means the registry endpoint is gone,
  which retrying won't fix.

**One real bug to fix while converting** (it's why ad-hoc loops felt safer):
`wait_for_grant` **drops non-matching Grant events on the floor**
(registry.rs:399-403 — no `cache_grant` on mismatch). A process with two
in-flight subscriptions can lose the second grant inside the first wait.
Fix in libcluu: on a mismatched `RegistryEvent::Grant`, call
`cache_grant(&format!("{}:{}", svc, name), token)`, and have
`wait_for_grant` check `lookup_cached` before blocking. Small, and it makes
concurrent subscriptions safe for everyone.

**Worth doing now?** Yes — net-negative LOC, removes a whole class of boot
flakes, and the harness will catch ordering regressions immediately.

**Risks.** Behavior change: "silently proceed without service" becomes
"block until service exists". That's the correct semantics (a hang is data,
per the no-timeouts memory), but any *genuinely optional* dependency must be
made structurally optional (like the virtio-net always-register scheme), not
timeout-optional.

---

## 11. ping 1000-iteration reply poll

**Why it exists.** No wall-clock in the loop; iteration count as a timeout
proxy. Note the loop is doubly odd: netd's ICMP recv is deferred
(`pending_recv` replies only when data arrives), so each `call` blocks until
*some* ICMP packet lands — the 1000 iterations mostly guard against
non-matching packets (wrong ident/seq) and would never fire on a silent
network… meaning **lost-packet ping currently hangs forever**, it doesn't
time out.

**Correct pattern: put the deadline in netd, where the clock and the
deferred queue already live.** Extend `NET_RECV` with a timeout argument
(words[3] = timeout_ms, 0 = infinite) and stamp pending entries:

```rust
// pending_recv entry gains: deadline: Option<Instant>
// main-loop sweep, after the delivery pass:
pending_recv.retain(|p| match p.deadline {
    Some(d) if now >= d => { reply(p, -ETIMEDOUT); false }
    _ => true,
});
```

Then ping becomes a single blocking `NET_RECV` with `timeout_ms = 2000` per
echo, looping only on ident/seq mismatch (with the remaining budget). A
timeout on packet loss is protocol semantics — the documented carve-out
from the no-timeouts rule, which targets deadlock guards.

Is client-side `clock_now()` an alternative? Ping does hold `TOKEN_CLOCK`,
so a wall-clock deadline loop would work, but it keeps the poll-shaped
traffic and every future client reimplements it. The netd-side deadline is
also exactly the mechanism SO_RCVTIMEO needs later — build it once.

**Worth doing now?** Yes, as a rider on the pending_recv rework in
issues 4/5 (~25 lines total). Until then the 1000-iteration loop is
tolerable for a smoke probe, but know that it's a hang, not a timeout, on
real loss.

**Risks.** Timeout replies race data arrival within one loop pass — run the
delivery sweep before the expiry sweep so data wins ties. Choose −ETIMEDOUT
(not −EAGAIN) so callers distinguish "timed out" from "try again".

---

## Cross-cutting notes

- **Errno discipline** (rider on issue 3): netd speaks −22 almost
  everywhere. One small table (`EAGAIN`, `EBADF`, `EINVAL`, `EPIPE`,
  `ECONNREFUSED`, `ETIMEDOUT`, `ENETUNREACH`, `ENOENT`) shared with
  socket.rs's newlib values turns every later fix into honest reporting for
  free.
- **ICMP send path** blocks the whole server on a synchronous
  `call_with_payload` to the driver (main.rs:647-652) — migrate it to
  `drain_tx` when touching NET_SEND for UDP (AGENTS.md §7).
- **All fixes are userspace, no new syscalls, one pre-reserved IPC label**
  (`NET_DNS_RESOLVE` 0x419). Kernel freeze untouched.
- **Suggested sequence** (each independently testable via harness probes):
  1. deferred connect + errnos (3) → 2. self-healing loopback ARP (1) →
  3. listener slots + deferred accept (2) → 4. close/FIN + recv-EOF (7) →
  5. UDP + recv metadata + recv deadline (4/5/11) → 6. DHCP retain + DNS
  servers (6) → 7. DNS resolve (8) → 8. poll_delay tick (9) → 9. delete
  yield loops + grant-cache fix (10).
