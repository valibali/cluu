# PTS Plumbing Fix — Implementation Plan

Date: 2026-05-21
Spec: docs/superpowers/specs/2026-05-21-pts-plumbing-fix.md
Branch: develop

## Batch 1 — Fix A+B (cluuterm-only, no wire change)

Land in one commit. Render+input pluming alive end-to-end.

### Tasks

- [ ] **t1** `userspace/cluuterm/src/tty_backend.rs`: rewrite
  `Pts::handle_pts_write` (line 171-197):
  - Cook bytes unconditionally via `line_discipline.process_output(req)`.
  - Reply only when `extract_reply_id(msg).is_some()`.
  - Return `Some(cooked)` regardless of reply_token.
  - Keep TOSTOP check; on TOSTOP-deny, return None (caller writes ignored).
- [ ] **t2** Drop `Cluuterm.stdin_buf` (line 368) and
  `Cluuterm.pending_pts_read` (:372). Remove their initialisation in
  `new()` (:416-417).
- [ ] **t3** Drop `Cluuterm::try_flush_pending_pts_read` (:427-444) and
  `Cluuterm::handle_pts_read` (:719-722). Dead code.
- [ ] **t4** Rewrite `run()` PTS_READ_LABEL arm (:925-950) to delegate
  to `self.pts.handle_pts_read(req, &msg, 0, 0)`. Done. Pts owns reply.
- [ ] **t5** PTS_WRITE_LABEL arm (:906-923) unchanged in shape; the
  fixed `Pts::handle_pts_write` returns `Some(cooked)` so the
  `Cluuterm::handle_pts_write(&cooked)` (line 450 renderer) still
  fires.
- [ ] **t6** Drop the postcard-decode + raw-bytes-fallback in
  PTS_WRITE_LABEL arm. VFS sends raw bytes. Just `req = payload.to_vec()`.

### Verify batch 1

```
cargo xtask build
RUN_WAIT=60 MARKER_MODE=l2_cluuterm_shell_input bash scripts/harness_run.sh
```

Expected markers:
- `shell: read 4 bytes from fd 0` — kbd path alive
- `shell: unsupported command` — line dispatched
- fb_dump: prompt visible

If markers miss, batch 1 incomplete — diagnose before batch 2.

### Commit message (batch 1)

```
fix(cluuterm): unify pts stdin state + cook bytes without reply_token

- Pts::handle_pts_write cooks bytes via line_discipline always; reply
  optional (VFS uses fire-and-forget send for pts writes).
- Drop dead Cluuterm.stdin_buf + pending_pts_read; Pts.ready_bytes +
  pending_readers canonical.
- Route run() PTS_READ_LABEL through Pts::handle_pts_read.
- Drop postcard decode fallback; VFS sends raw bytes.

Bug A: shell stdout dropped before line discipline ran.
Bug B: kbd bytes landed in dead buffer, shell read blocked forever.

Refs: docs/superpowers/specs/2026-05-21-pts-plumbing-fix.md
```

## Batch 2 — Fix C (cap-monotone clamp in VFS derive_child_fd)

Independent of batch 1. Can land before or after.

### Tasks

- [ ] **t7** Audit `OpenFile` variants in `userspace/vfs/src/fd_table.rs`.
  Determine current rights tracking. If rights field absent on
  `OpenFile::Pts`, add `rights: u64`.
- [ ] **t8** Add `FileTable::rights(client_id, fd) -> Option<u64>` helper.
- [ ] **t9** `userspace/vfs/src/main.rs:1175-1176`: clamp `child_rights &=
  parent_rights` before `token_derive`. Log when clamp narrows
  (debug_print).
- [ ] **t10** Smoke: confirm normal pts spawn unaffected. Open pts RW
  (rights=READ|WRITE) → derive child with READ_ONLY → clamp passes;
  child gets READ_ONLY. Open pts RO → derive child requesting RW →
  clamp narrows to RO; log fires.

### Commit message (batch 2)

```
fix(vfs): clamp child_rights to parent rights in derive_child_fd

token_derive minted from VFS full-rights endpoint without consulting
the parent OpenFile's rights mask. Violated cap-monotone-decrease.

Now: child_rights &= parent_rights before derive. Logs narrowed-clamp.

Refs: docs/superpowers/specs/2026-05-21-pts-plumbing-fix.md
```

## Batch 3 — Fix D (PTS_READ async with PTS_READ_DELIVER reverse label)

Bigger. Land after batch 1 confirmed green (so input path correctness
gives a baseline).

### Wire

- `userspace/cluu_wire/src/pts.rs`: add `PTS_READ_DELIVER_LABEL = 112`.
- Doc the new reverse direction (cluuterm → VFS).

### VFS state

- `pending_pts_reads: BTreeMap<u32, VecDeque<ParkedRead>>` keyed by pts_id.
- `struct ParkedRead { reply_token, caller_space, target_base, requested }`.

### VFS handle_read_grant pts arm (vfs/main.rs:2997-3034)

Replace sync call with:
1. Build `PTS_READ_LABEL` msg, fire-and-forget via `send_msg_with_payload`.
2. Park `(reply_token, target_base, target_space, requested)` in map.
3. Return without replying to shell (shell stays blocked).

### VFS PTS_READ_DELIVER handler (new, in main recv loop)

1. Read pts_id from msg.words[1].
2. Pop front ParkedRead from `pending_pts_reads[pts_id]`.
3. `grant_data_to_caller(...)` into shell's target_base.
4. `ipc::reply(parked.reply_token, &reply_msg, ...)`.

### Cluuterm PTS_READ_LABEL handler (run loop)

Rewrite as fire-and-forget consumer:
- If `pts.ready_bytes` non-empty, drain up to `requested` bytes, send
  `PTS_READ_DELIVER_LABEL` to VFS with bytes.
- Else mark `pts.drain_requested = Some(requested)`.

Cluuterm needs VFS endpoint cached. Already subscribed via registry
during init.

### Cluuterm apply_service_actions::DeliverBytes (line 730)

After `pts.ready_bytes.extend(bytes)`:
- If `pts.drain_requested.is_some()`, drain + send `PTS_READ_DELIVER_LABEL`,
  clear flag.

### Tasks

- [ ] **t11** Add `PTS_READ_DELIVER_LABEL = 112` to `cluu_wire/src/pts.rs`.
- [ ] **t12** Add `pending_pts_reads` map + `ParkedRead` struct to
  `userspace/vfs/src/main.rs`.
- [ ] **t13** Rewrite handle_read_grant pts arm to park + fire-and-forget.
- [ ] **t14** Add VFS PTS_READ_DELIVER_LABEL handler.
- [ ] **t15** Cache VFS endpoint in `Cluuterm`; subscribe at init.
- [ ] **t16** Rewrite cluuterm PTS_READ_LABEL arm to push-delivery model.
- [ ] **t17** Add `drain_requested` flag on Pts; wire in DeliverBytes.
- [ ] **t18** Remove dead `Pts::handle_pts_read` (pull-deferral form).

### Verify batch 3

Same as batch 1 markers, plus:
- No `vfs: derive_child_fd` errors during pts read flow.
- Stress: multiple keystrokes during deferred-read window; bytes arrive
  to shell, no VFS thread starvation.

### Commit message (batch 3)

```
refactor(vfs+cluuterm): pts read async via PTS_READ_DELIVER reverse label

Eliminates cross-process deadlock risk where VFS sync-called cluuterm
during pts read and cluuterm could not call VFS back during the
deferred-reply window.

New: VFS parks shell reply_token + caller space in pending_pts_reads.
PTS_READ_LABEL becomes a "drain hint" cluuterm → drains ready_bytes →
PTS_READ_DELIVER_LABEL (112) ships cooked bytes back. VFS grants to
shell's space and replies the parked token.

Refs: docs/superpowers/specs/2026-05-21-pts-plumbing-fix.md
```

## Final verification

After all batches:
- `cargo xtask build` green.
- `MARKER_MODE=l2_cluuterm_shell_input bash scripts/harness_run.sh` passes
  required markers.
- `MARKER_MODE=l2_text_shell_input bash scripts/harness_run.sh` still
  green (no regression).
- `scripts/fb_dump.sh -p <fb_phys>` shows rendered prompt + typed echo.

## Memory updates

After session end:
- Update `[[handoff-pts-plumbing-2026-05-20]]` → close; reference new
  commits.
- New project memory `pts-plumbing-fix-2026-05-21` with the 4-bug
  summary + commit hashes.
- Cross-link from `[[vfs-view-caps-monotone]]` to the cap-monotone fix.
