# PTS Plumbing Fix — Spec

Date: 2026-05-21
Status: Approved (architect-reviewed)
Branch: develop
HEAD at write: be000d4

## Context

After be000d4 landed (login → cluuterm → shell green to read(0) loop), pts
bidirectional plumbing is broken:

1. Shell write(1) → pts → cluuterm: no bytes reach the terminal window.
2. Kbd → cluuterm → pts → shell read(0): no bytes delivered to shell.

Architect review (2026-05-21) found **four** issues:

### Bug A — `Pts::handle_pts_write` drops bytes on missing reply_token

`userspace/cluuterm/src/tty_backend.rs:171-197`

VFS forwards shell stdout via `send_msg_with_payload` (fire-and-forget,
no reply slot) per `userspace/vfs/src/main.rs:1801`. Cluuterm's handler
bails at line 178-181 when `extract_reply_id` returns None, before
calling `line_discipline.process_output()`. All shell stdout dropped.

### Bug B — Dual parallel stdin buffers, mismatched

`Pts` owns `ready_bytes` + `pending_readers`. `Cluuterm` owns
`stdin_buf` + `pending_pts_read`. The `run()` PTS_READ_LABEL handler
(:925-950) uses the Cluuterm-level pair; `apply_service_actions::DeliverBytes`
(:730-733) writes to the Pts-level pair. Kbd bytes go to dead buffer;
shell read waits on different buffer forever.

### Bug C — VFS derive_child_fd cap-monotone violation

`userspace/vfs/src/main.rs:1176`:

```rust
let derived = match token_derive(self.endpoint, child_rights, u64::MAX) {
```

`token_derive` mints from VFS's **own full-rights endpoint** without
clamping `child_rights` against the parent's `OpenFile` rights. Child
can receive caps strictly broader than parent's. Violates
`[[vfs-view-caps-monotone]]`.

### Bug D — Cross-process deadlock risk in VFS PTS_READ

`userspace/vfs/src/main.rs:2997-3034`: VFS uses synchronous
`call_with_reply_buf` to cluuterm for pts reads. Cluuterm may defer
reply (PendingRead). During the deferred window, VFS thread is blocked
inside `call_with_reply_buf`. If cluuterm calls VFS for any reason
during that window → cycle. Currently masked because cluuterm's
recv loop avoids VFS calls during run, but fragile.

## Principles applied

- `[[vfs-view-caps-monotone]]` — child caps never broader than parent.
- `[[no-timeouts]]` — no time-bounded recv as deadlock guard.
- `[[send_msg_with_payload_clobbers_word0]]` — transport sets words[0] = payload_len.
- `[[path-a-stdio-assertion]]` — shell fd 0 VFS-backed loud-fail.
- `[[vfs-direct-token-optimization]]` — direct broker deferred; VFS-mediated
  pts I/O is correct shape for stdio rates.

## Design

### Fix A — Pts::handle_pts_write processes bytes always

Restructure handler:
1. Run TOSTOP check.
2. Cook bytes via `line_discipline.process_output(req)` unconditionally.
3. If `reply_token` present, send `reply_ok::<WriteReply>` ack.
4. Return cooked bytes for parser/render.

Reply becomes optional; cook is invariant.

### Fix B — Unify state into Pts (architect choice)

Drop `Cluuterm.stdin_buf` (tty_backend.rs:368) and
`Cluuterm.pending_pts_read` (:372). Keep `Pts.ready_bytes` +
`Pts.pending_readers` as canonical.

Route `run()` PTS_READ_LABEL handler through `Pts::handle_pts_read`.
Pts owns SIGTTIN, EIO-on-close, deferral, wake.

`apply_service_actions::DeliverBytes` already writes to
`pts.ready_bytes` and calls `pts.try_wake_pending_readers()` —
becomes the canonical wake path. No change needed at that site
beyond confirming it's hit.

`Cluuterm::try_flush_pending_pts_read` and `Cluuterm::handle_pts_read`
(line 427, 719) become dead code; remove.

### Fix C — VFS derive_child_fd cap-monotone clamp

`userspace/vfs/src/main.rs:1176`:

Each `OpenFile` variant carries an effective rights mask. For pts,
clone inherits read/write per the parent's mask. Clamp:

```rust
let parent_rights = self.files.rights(parent_cid, parent_fd);
let clamped_rights = child_rights & parent_rights;
let derived = token_derive(self.endpoint, clamped_rights, u64::MAX)?;
```

If `OpenFile` doesn't track rights today, extend it: add `rights: usize`
field on each variant. Plumb through `files.open()` and inherit on
clone.

### Fix D — VFS PTS_READ becomes asynchronous

Wire change: new label `PTS_READ_DELIVER` = 112.

```
PTS_READ flow (new):
  shell → VFS handle_read_grant (sync call, blocks shell)
  VFS pts arm:
    - park shell's reply_token in pending_pts_reads[pts_id]
    - send PTS_READ_LABEL fire-and-forget to cluuterm (signals "drain")
    - DO NOT reply to shell; shell blocks
  cluuterm PTS_READ_LABEL handler:
    - if ready_bytes non-empty, send PTS_READ_DELIVER to VFS w/ bytes
    - else mark "drain requested" flag, deliver on next DeliverBytes
  cluuterm DeliverBytes (after kbd):
    - if drain flag set, send PTS_READ_DELIVER to VFS w/ bytes; clear flag
  VFS PTS_READ_DELIVER handler:
    - pop pending_pts_reads[pts_id]
    - grant bytes to shell's space; reply to parked reply_token
```

VFS thread never blocks on cluuterm. Cross-process cycle gone.

New VFS state: `pending_pts_reads: BTreeMap<u32, ParkedRead>` where
`ParkedRead { reply_token, caller_space, target_base, requested }`.

New cluuterm state: `drain_requested: bool` (per Pts).

Wire payload `PTS_READ_DELIVER` (cluuterm→VFS):
- words[0] = payload_len (data bytes)
- words[1] = pts_id
- payload = cooked bytes

## Verification

1. Build: `cargo xtask build` green.
2. `MARKER_MODE=l2_cluuterm_shell_input bash scripts/harness_run.sh`
   must hit:
   - `shell: read 4 bytes from fd 0`
   - `shell: unsupported command`
3. Visual: `scripts/fb_dump.sh` shows prompt rendered in cluuterm window.
4. Cap-monotone audit: write a probe that opens /dev/pts/0 with restricted
   rights, derives child fd with broader rights, confirms VFS denies.
   Defer probe; manual code review of clamp.

## Out of scope

- Direct-broker pts (`[[vfs-direct-token-optimization]]`) — deferred.
- pts ioctl coverage beyond current verb set.
- Window leak post-login-exit (separate task #3).

## Linked memories

- `[[handoff-pts-plumbing-2026-05-20]]`
- `[[vfs-view-caps-monotone]]`
- `[[send_msg_with_payload_clobbers_word0]]`
- `[[path-a-stdio-assertion]]`
- `[[vfs-direct-token-optimization]]`
- `[[no-timeouts]]`
