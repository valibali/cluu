# Framebuffer Grant-Token Lease Design

## Status

Draft design for implementation planning. Working direction: service-owned
leases. Explicit fail-closed crash behavior is approved for current
implementation.

## Problem

Framebuffer ownership is currently ambiguous. `PixelRegion::alloc()` lets a
client allocate and map a frame token, while the client helper documentation
says the compositor owns and frees the token after window destruction. Other
clients, such as `imgview`, explicitly call `FrameFree` during cleanup. Xnes
exit then produces frame-table warnings including `refcount > 0` and
`retype_to_user on non-Untyped frame`.

The framebuffer is exclusive: either the default compositor owns it or exactly
one session-local fullscreen process owns it. The design must establish one
owner for framebuffer grant lifetime and ensure that physical frames are not
retyped or freed while the current owner still maps them.

## Goals

- Give display service one authoritative framebuffer lease owner.
- Make explicit client release idempotent and crash state fail-closed.
- Prevent stale fullscreen clients from affecting later ownership cycles.
- Ensure revoke, unmap, and frame-free ordering is explicit.
- Remove client-side `FrameFree` for attached grants.
- Add regression coverage for repeated Xnes spawn/exit cycles and handoffs.

## Non-goals

- No wall-clock lease expiry, heartbeat, or liveness polling.
- No runtime ACL or caller interrogation layer.
- No redesign of ordinary displayd copied `Surface` buffers.
- No new syscall; use existing capability/token invoke paths.

## Ownership model

Displayd owns the framebuffer lease and mints the grant. There is exactly one
active owner:

```text
compositor lease (default) XOR fullscreen-client lease
```

The fullscreen grant is delivered directly by displayd to the designated
client. vtmgr owns input routing; inputd continues to receive device events.
Displayd publishes lease transitions so vtmgr can route input to the same
owner.

The client receives a direct read/write, non-executable framebuffer mapping.
Clients must not call `FrameFree`. They explicitly unmap and acknowledge
release; displayd performs final reclamation. The lease table is lifecycle
bookkeeping, not a runtime ACL layer.

## Lease record

Displayd tracks one active lease record containing:

- owner kind (`Compositor` or `Fullscreen`);
- client process/session identity;
- monotonically increasing lease generation;
- root frame token and derived client grant;
- page count and mapping metadata;
- input-route transition state;
- lifecycle state.

The externally supplied handle is `(lease_id, generation)`, so an old message
cannot target a newly reused slot.

States:

```text
Active -> Revoking -> Released
                   -> Aborted
```

`Released` and `Aborted` are terminal. Repeated release is idempotent and does
not repeat frame destruction.

## Lifecycle

### Create

1. Fullscreen client requests ownership through displayd.
2. Displayd rejects the request until compositor has voluntarily released its
   framebuffer lease.
3. Displayd prepares the framebuffer and vtmgr input-route transition.
4. On successful preparation, displayd commits the ownership transition.
5. Displayd clears framebuffer contents to black.
6. Displayd allocates/mints a fresh generation-specific grant.
7. Displayd delivers the grant directly to the designated client.
8. Client maps the grant and starts fullscreen operation.

### Use

Only current owner may use the active grant. A stale generation returns
`InvalidCapability`. Displayd does not process framebuffer pixels or raw input;
the grant and vtmgr route provide direct ownership.

### Release/revoke

The canonical teardown order is:

1. Transition `Active` to `Revoking`.
2. Reject new map, damage, and present requests.
3. Prepare vtmgr route back to compositor.
4. Require fullscreen client to unmap and acknowledge release.
5. Release the GPU grant only after acknowledgement, then commit input-route return.
6. Clear framebuffer contents to black through the existing GPU/display path.
7. Mint a fresh compositor generation-specific grant.
8. Restore compositor ownership and require a redraw.
9. Mark the lease terminal and remove its active lookup entry.

Release is cooperative and fail-closed. Explicit unmap and acknowledgement are
required to restore compositor ownership. Missing acknowledgement, client
crash, endpoint disappearance, or suspected owner death leaves the lease in a
non-reclaiming fail-closed state: no automatic framebuffer regrant or
reclamation occurs. Process-watch and any hard-revocation path are deferred;
no new syscall is permitted.

### Client death

Displayd does not automatically reclaim or regrant on client crash, endpoint
disappearance, or suspected owner death. The lease record remains fail-closed
and compositor restoration requires an explicit, successfully acknowledged
transaction. Process-watch integration is deferred.

## API consequences

The fullscreen client API should distinguish local mapping teardown from lease
release:

- `unmap`: removes only the client virtual mapping;
- `release`: requests route preparation, unmaps, acknowledges to displayd, and
  waits for successful handoff;
- no attached-buffer `destroy` path may invoke `FrameFree` directly.

Existing `PixelRegion`/`imgview` cleanup must stop directly freeing attached
grants. The exact client helper names may change, but displayd remains the only
owner allowed to reclaim the grant.

## Errors and failure handling

- Invalid lease generation: `InvalidCapability`.
- Release of terminal lease: success or `AlreadyReleased`, with no side effect.
- Request while compositor owns framebuffer: `FramebufferBusy`.
- Operation during handoff: `LeaseTransitioning`.
- Invalid dimensions or page count: existing argument/resource error.
- Failed client mapping: displayd aborts unpublished lease and reclaims frames.
- Failed input-route preparation: displayd rolls back framebuffer preparation;
  no ownership transition is committed.
- Failed or missing fullscreen release acknowledgement: displayd leaves the
  lease fail-closed and compositor ownership suspended.
- Client crash, endpoint disappearance, or suspected death: no automatic
  framebuffer regrant or reclamation; process-watch is deferred.
- Display-service restart: no automatic framebuffer regrant or lease
  reclamation is performed in this slice; clients cannot retain a valid
  authority path after service teardown.

No error path may call `FrameFree` before unmapping and revoking references.

## Verification plan

Add focused tests for:

1. compositor owns framebuffer by default;
2. fullscreen request is rejected while compositor owns it;
3. successful prepare/commit handoff routes input to fullscreen client;
4. framebuffer is cleared before fullscreen grant delivery;
5. fullscreen unmap/acknowledge/release restores compositor and redraw;
6. stale generation after handoff cannot access framebuffer;
7. missing acknowledgement leaves the lease fail-closed;
8. failed input-route preparation rolls back handoff;
9. failed release acknowledgement leaves compositor suspended;
10. route prepare → unmap/ack → GPU release → clear → compositor restore;
11. repeated Xnes spawn/exit and handoff cycles.

Each lifecycle test must check that frame-table warnings do not appear and
that tracked, mapped, and PMM frame counts return to the pre-test baseline.
The QEMU harness should capture serial output and fail on the relevant
`FRAME_TABLE WARN` markers, while still allowing unrelated boot diagnostics.

## Implementation boundary

Implementation should first trace the concrete displayd grant path, vtmgr/inputd
route transition, process-exit notification, kernel token derivation, and
frame-table reference accounting. Keep revocation cooperative initially. If
hard revocation of a live fullscreen client proves necessary, add only the
smallest existing-token `InvokeOp`; do not add a new syscall or runtime ACL.
