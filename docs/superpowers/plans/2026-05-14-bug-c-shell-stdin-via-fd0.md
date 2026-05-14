# Plan 1: Close Bug C — shell stdin via POSIX fd 0 on both VT0-3 and cluuterm

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every shell instance — text VTs and cluuterm — read stdin via blocking `read(0)`, with fd 0 bound to `/dev/tty<N>` or `/dev/pts/<id>` via FDAC at spawn time. No `TTY_READ_LABEL` push protocol. Closes Bug C from the LoginCC pause notes (`project_loginCC_session_2026_05_13.md`).

**Architecture:** Procmgr opens the right `/dev/...` node at shell-spawn time using its own `VfsClient`, builds an FDAC payload, and injects it through the existing `spawn_service_with_env(... fdac_data, owner_tid, ...)` path. The tty service is reduced to a VFS backend that only answers `TTY_READ_REQUEST_LABEL` pulls; all push-send sites die. The cluuterm path already does the FDAC dance via `posix_spawn`; this plan verifies it round-trips correctly and patches whatever's preventing input from reaching the child shell today.

**Tech Stack:** Rust, CLUU microkernel + userspace, `cargo xtask build`, `scripts/harness_run.sh` for QEMU smoke tests.

---

## Pre-flight context (read once before T1)

Files this plan touches:

| File | Responsibility | Action |
|---|---|---|
| `userspace/procmgr/src/main.rs` | session_kind=0 path: build FDAC for `/dev/tty<N>` and pass to spawn | Modify around lines 2387-2503 (`handle_session_login` text branch) and 4309-4631 (`spawn_service_with_env`) |
| `userspace/procmgr/src/main.rs` | Helper that opens `/dev/tty<N>` via procmgr's own VfsClient and returns the procmgr-side fd + tid | Add new fn |
| `userspace/tty/src/main.rs` | Remove TTY_READ_LABEL push sites | Modify lines 168-194 (TTY_REGISTER path), 322-372 (key handling path) |
| `userspace/tty/src/context.rs` | Remove now-dead push state | Modify lines 40-83 (struct fields), 224-264 (wire_shell_stdin etc.), 334-364 (deliver_shell_line) |
| `userspace/shell/src/main.rs` | The existing `rebind_stdio_to_devtty` fallback stays as a defensive belt-and-suspenders. No new behavior change. | Verify |
| `userspace/cluuterm/src/tty_backend.rs` | Verify `pending_pts_read` lifecycle correctness | Diagnose + patch if T7 surfaces a bug |
| `scripts/harness_case_defaults.sh` | New marker setups | Add 2 entries |
| `scripts/harness_run.sh` | New `MARKER_MODE` cases + their required-marker lists | Add 2 entries |

Files this plan does NOT touch (deferred to plans 2-5):
- `userspace/compositor/**` — compositor swap is plan 3.
- `etc/envelopes.toml` — substitution is plan 2.
- `userspace/login/**` — no behavioral change needed yet.

Key existing helpers worth knowing:
- `userspace/libcluu/src/fs/client.rs:200 VfsClient::open` — returns `VfsFile { fd, size }`.
- `userspace/procmgr/src/main.rs:638` — pattern for `VfsClient::new(self.vfs_endpoint, 0)` calls from procmgr.
- `userspace/procmgr/src/main.rs:4427-4587` — FDAC parser. Reuse the FDAC byte layout: magic 0x46444143, count u32, then `count × FdAction(32 bytes)` entries.
- `userspace/libcluu/src/posix/process.rs:524 FDAC_MAGIC` and the `FdAction` layout for the byte format we need to match.

The boot log we keep referring to lives in `/tmp/cluu-serial-com2.log` after `bash scripts/harness_run.sh`.

---

## Task 1: Failing harness marker for text VT shell input

**Files:**
- Modify: `scripts/harness_case_defaults.sh:640` area (after `l2_cluuterm_login`)
- Modify: `scripts/harness_run.sh:1730` area (after the `l2_cluuterm_login` case block)

- [ ] **Step 1: Add a new MARKER_MODE for VT0 text shell input**

Append at the end of the existing per-mode switch in `scripts/harness_case_defaults.sh` (just before the closing `esac` / `;;` chain — match the pattern used by `l2_cluuterm_login`):

```bash
            l2_text_shell_input)
                TEST_COMMAND=""
                # VT0 text login flow: ctrl-alt-f1 to switch to VT0, type root +
                # password, then type `echo hi-from-vt0` and Enter. Marker is
                # the literal echoed back through tty -> /dev/console (which is
                # captured on COM2 since console writes mirror there). If the
                # shell never receives the line, the marker never fires and the
                # harness times out.
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 3\nsendkey ctrl-alt-f1\nsleep 1\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 1\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 2\nsendkey e\nsendkey c\nsendkey h\nsendkey o\nsendkey spc\nsendkey h\nsendkey i\nsendkey minus\nsendkey f\nsendkey r\nsendkey o\nsendkey m\nsendkey minus\nsendkey v\nsendkey t\nsendkey 0\nsendkey ret'
                ;;
```

- [ ] **Step 2: Add the matching required-markers list in `scripts/harness_run.sh`**

Find the case block in `scripts/harness_run.sh` around line 1730 (the `l2_cluuterm_login)` entry) and add a new case right after it:

```bash
    l2_text_shell_input)
        # Text VT0 shell receives a typed line via POSIX read(fd 0) over
        # /dev/tty0. The shell echoes `hi-from-vt0` to fd 1 (= /dev/tty0)
        # whose write path forwards to the console service, which mirrors
        # to COM2. If the marker fires we know stdin round-trips end to
        # end through Path A on the legacy VT.
        required_markers=(
            "TSC calibrated"
            "tty:0: showing login prompt"
            "hi-from-vt0"
        )
        ;;
```

- [ ] **Step 3: Run the marker against the current build to confirm it fails**

```bash
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_text_shell_input bash scripts/harness_run.sh
```

Expected: harness times out or fails because the "hi-from-vt0" marker never appears in the serial log (shell on VT0 still has TOKEN_STDIN push wired through the old path, but the actual command-evaluation path isn't getting the bytes since we already removed the push half in tty commit 37fb703 without giving it a pull replacement).

- [ ] **Step 4: Commit**

```bash
git add scripts/harness_case_defaults.sh scripts/harness_run.sh
git commit -m "harness: add l2_text_shell_input marker for VT0 stdin round-trip"
```

---

## Task 2: Helper — open /dev/tty<N> from procmgr context

**Files:**
- Modify: `userspace/procmgr/src/main.rs` — add a helper method on `impl ProcessManager` near the existing VFS helpers (around line 633-757, next to `ensure_vfs_endpoint`).

- [ ] **Step 1: Write the helper**

Add this method on `impl ProcessManager` (just after `ensure_vfs_endpoint` at line 757):

```rust
    /// Open `/dev/tty<vt>` via procmgr's own VFS client. Used at session-login
    /// to seed FDAC entries for the legacy text shell.
    ///
    /// Returns `(client_id, remote_fd)` — the pair that the FDAC parser
    /// expects on a VFS-backed FdAction. `client_id` is procmgr's main-thread
    /// tid (what VFS authenticated the open under); `remote_fd` is VFS's
    /// table fd for the open. Both nonzero on success.
    fn open_dev_tty_for_session(&mut self, vt: usize) -> Result<(usize, usize)> {
        if vt >= 4 {
            return Err(Error::InvalidArgument);
        }
        if self.vfs_endpoint == 0 {
            self.ensure_vfs_endpoint()?;
        }
        // VFS authenticates by kernel sender_tid; pass 0 as the client-id
        // hint, VFS will overwrite with procmgr's authenticated tid.
        let client = VfsClient::new(self.vfs_endpoint, 0);
        let path = match vt {
            0 => "/dev/tty0",
            1 => "/dev/tty1",
            2 => "/dev/tty2",
            3 => "/dev/tty3",
            _ => unreachable!(),
        };
        const O_RDWR: usize = 2;
        let file = client.open_with(path, O_RDWR, 0)?;
        // Procmgr's main-thread tid is the sender_tid the kernel uses for
        // every IPC from procmgr's main loop. Capture once and cache.
        let procmgr_tid = self.procmgr_main_tid()?;
        Ok((procmgr_tid, file.fd))
    }
```

- [ ] **Step 2: Add the `procmgr_main_tid` helper**

Procmgr doesn't yet cache its own tid. Add this method on `impl ProcessManager` right after `open_dev_tty_for_session`:

```rust
    /// Return procmgr's main-thread tid, caching after first lookup.
    /// Used by VFS-backed FDAC injection so the FDAC parser knows which
    /// client_id VFS keys the open under.
    fn procmgr_main_tid(&mut self) -> Result<usize> {
        if self.cached_main_tid != 0 {
            return Ok(self.cached_main_tid);
        }
        // self.token is TOKEN_SELF (process cap), not a thread token. We need
        // the *thread* cap for the main thread. libcluu exposes one via the
        // current ProcessInfo.tokens[TOKEN_SELF] slot at boot; procmgr keeps
        // that as `self.token` already. thread_get_id accepts any thread cap;
        // pass it through and the kernel returns the calling thread's tid.
        let tid = libcluu::syscall::thread_get_id(self.token)?;
        self.cached_main_tid = tid;
        Ok(tid)
    }
```

- [ ] **Step 3: Add the `cached_main_tid` field**

In `impl ProcessManager` definition (around line 233-340), add the field. Find the existing `vfs_endpoint: usize,` (line 258) and add right after it:

```rust
    /// Cached value of procmgr's main-thread tid; 0 means not yet looked up.
    /// Used by VFS-backed FDAC injection (see `procmgr_main_tid`).
    cached_main_tid: usize,
```

And in the `Default` / constructor `ProcessManager::new` (search for `vfs_endpoint: 0,` around line 340) add:

```rust
            cached_main_tid: 0,
```

- [ ] **Step 4: Build to verify it compiles**

```bash
cargo xtask build 2>&1 | tail -5
```

Expected: `✓ Build complete: target/cluu.img`.

- [ ] **Step 5: Commit**

```bash
git add userspace/procmgr/src/main.rs
git commit -m "procmgr: helper to open /dev/tty<N> + cache main-thread tid"
```

---

## Task 3: Build FDAC payload for `/dev/tty<N>`

**Files:**
- Modify: `userspace/procmgr/src/main.rs` — new helper on `impl ProcessManager`, called from the session_kind=0 spawn site.

- [ ] **Step 1: Add FDAC payload builder**

The FDAC layout is shared with libcluu's posix_spawn (see `userspace/libcluu/src/posix/process.rs:521-540`). Mirror it. Add this method on `impl ProcessManager` (near `open_dev_tty_for_session`):

```rust
    /// Build an FDAC payload that targets fd 0/1/2 at the same VFS-backed
    /// file. Used by the legacy text-VT session spawn.
    ///
    /// Layout (matches libcluu/src/posix/process.rs):
    ///   u32 magic = 0x46444143
    ///   u32 count = 3
    ///   3 × FdAction { u32 target_fd, u32 flags, usize endpoint,
    ///                  usize vfs_client_id, usize vfs_remote_fd }
    fn build_devtty_fdac(&self, vfs_client_id: usize, vfs_remote_fd: usize)
        -> Vec<u8>
    {
        const FDAC_MAGIC: u32 = 0x46444143;
        let mut out = Vec::with_capacity(8 + 3 * 32);
        out.extend_from_slice(&FDAC_MAGIC.to_le_bytes());
        out.extend_from_slice(&3u32.to_le_bytes());
        for target_fd in 0u32..=2u32 {
            out.extend_from_slice(&target_fd.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());           // flags
            out.extend_from_slice(&0usize.to_le_bytes());         // endpoint (unused for VFS-backed)
            out.extend_from_slice(&vfs_client_id.to_le_bytes());
            out.extend_from_slice(&vfs_remote_fd.to_le_bytes());
        }
        out
    }
```

- [ ] **Step 2: Build to verify**

```bash
cargo xtask build 2>&1 | tail -3
```

Expected: success.

- [ ] **Step 3: Commit**

```bash
git add userspace/procmgr/src/main.rs
git commit -m "procmgr: build FDAC payload pointing fd 0/1/2 at a /dev/tty<N> open"
```

---

## Task 4: Wire FDAC into legacy text VT spawn

**Files:**
- Modify: `userspace/procmgr/src/main.rs:2416-2444` (the `spawn_service_with_env` call in `handle_session_login` text branch).

- [ ] **Step 1: Replace the spawn call with the FDAC-bearing variant**

Locate the existing call in the session_kind=0 path (around line 2425):

```rust
        // Temporarily wire stdout to target VT's tty
        let saved = self.tty_endpoints[0];
        self.tty_endpoints[0] = tty_ep;

        match self.spawn_service_with_env(
            SERVICE_PATH,
            DEFAULT_PRIORITY,
            &shell_argv_payload,
            shell_argc,
            &user_env,
            user_envc,
            1, // non-zero owner_tid to use caller_env_data
            spawn_seq,
            spawn_start,
            &[],
            profile,
            0,
            0,
            &[],
            None, // no caller view (session login uses SERVICE_PATH constant)
            &[],
            &[], // no redir
            THREAD_CREATE_START_SUSPENDED,
        ) {
```

Replace with:

```rust
        // Open /dev/tty<vt> via procmgr's VFS client so the child shell's
        // fd 0/1/2 are VFS-backed handles to the tty service, served via
        // TTY_READ_REQUEST_LABEL on read(2). This retires the old
        // TOKEN_STDIN push pattern for the legacy VT path.
        let (tty_client_id, tty_remote_fd) = match self.open_dev_tty_for_session(vt_index) {
            Ok(pair) => pair,
            Err(e) => {
                let _ = debug_print(&format!(
                    "procmgr: open /dev/tty{} failed: {:?}; aborting login",
                    vt_index, e
                ));
                reply_msg.words[0] = e.to_errno() as usize;
                if let Some(tok) = reply_token {
                    let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty());
                }
                return Ok(());
            }
        };
        let fdac_data = self.build_devtty_fdac(tty_client_id, tty_remote_fd);
        let procmgr_main_tid = match self.procmgr_main_tid() {
            Ok(t) => t,
            Err(e) => {
                let _ = debug_print(&format!(
                    "procmgr: procmgr_main_tid lookup failed: {:?}", e
                ));
                reply_msg.words[0] = e.to_errno() as usize;
                if let Some(tok) = reply_token {
                    let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty());
                }
                return Ok(());
            }
        };

        // Note: we no longer need to temporarily swap self.tty_endpoints[0]
        // because FDAC fully describes fd 0/1/2. Keep the swap commented in
        // case future stdout/log routing wants a fallback when FDAC parse
        // fails — for now, FDAC is the only source of truth.
        let _ = tty_ep; // suppress unused-warning until removed in Task 9

        match self.spawn_service_with_env(
            SERVICE_PATH,
            DEFAULT_PRIORITY,
            &shell_argv_payload,
            shell_argc,
            &user_env,
            user_envc,
            procmgr_main_tid, // VFS will see opens under this tid
            spawn_seq,
            spawn_start,
            &fdac_data,
            profile,
            0,
            0,
            &[],
            None,
            &[],
            &[], // no redir
            THREAD_CREATE_START_SUSPENDED,
        ) {
```

- [ ] **Step 2: Remove the dead `let saved` and the post-call `self.tty_endpoints[0] = saved;` line**

In the same `handle_session_login` body, find the line `self.tty_endpoints[0] = saved;` (was at around 2503) and delete it. Also delete the now-dead `let saved = self.tty_endpoints[0];` line above the spawn call.

- [ ] **Step 3: Build**

```bash
cargo xtask build 2>&1 | tail -5
```

Expected: clean build. If borrow-checker complains because `self.open_dev_tty_for_session` takes `&mut self` and the spawn call also borrows self mutably, that's expected — the rewrite above runs them sequentially with no overlapping borrow.

- [ ] **Step 4: Run the harness marker from Task 1**

```bash
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_text_shell_input bash scripts/harness_run.sh
```

Expected outcomes (read serial log even on success):

- `tty:0: showing login prompt` fires (no change).
- `vfs: open '/dev/tty0' client=<procmgr_tid>` should appear when procmgr seeds FDAC.
- `vfs: derive_child_fd parent_cid=<procmgr_tid> parent_fd=<remote_fd> child_tid=<shell_tid> child_fd=...` should appear three times (once per FDAC entry).
- `hi-from-vt0` may NOT appear yet — tty service's push side is gone but shell isn't reading fd 0 in this build because shell's `rebind_stdio_to_devtty` falls back to TOKEN_STDIN if it sees fd 0 already VFS-backed (and the recv-on-stdin loop is gone). That second issue is hit in Task 5.

Note: if `vfs: open '/dev/tty0' client=...` does NOT appear, the FDAC injection isn't taking effect. Investigate before moving on — likely a payload-format mismatch.

- [ ] **Step 5: Commit**

```bash
git add userspace/procmgr/src/main.rs
git commit -m "procmgr: FDAC-inject /dev/tty<N> as fd 0/1/2 for legacy VT shell"
```

---

## Task 5: Verify shell main loop reaches `read(0)` on the new fd

**Files:**
- Read-only inspection of `userspace/shell/src/main.rs:53-77` (the `rebind_stdio_to_devtty` block) and the main loop at `:163-189`.

- [ ] **Step 1: Read the current shell startup**

```bash
sed -n '53,90p' userspace/shell/src/main.rs
```

Confirm the flow is:
1. `fd0_is_vfs_backed` check.
2. If NOT, call `rebind_stdio_to_devtty(vt)`.
3. Main loop at line ~163 does `_read(0, …)` regardless.

With Task 4 landed, fd 0 IS now VFS-backed on a fresh VT0 spawn, so `rebind_stdio_to_devtty` is skipped. That's correct.

- [ ] **Step 2: Add a one-line trace to confirm at runtime**

Insert right after the `let fd0_is_vfs_backed = ...` block (the `if !fd0_is_vfs_backed { ... }` block), so it logs in either case:

```rust
    let _ = debug_print(&format!(
        "shell: stdin path = {}",
        if fd0_is_vfs_backed { "vfs-backed" } else { "rebind-attempt" }
    ));
```

- [ ] **Step 3: Build + harness**

```bash
cargo xtask build && \
HARNESS_FORCE_BUILD=0 MARKER_MODE=l2_text_shell_input bash scripts/harness_run.sh
grep -E "shell: stdin path|hi-from-vt0" /tmp/cluu-serial-com2.log
```

Expected: `shell: stdin path = vfs-backed` appears once per shell spawn. If we still see `rebind-attempt` after Task 4, the FDAC trailer in ProcessInfo isn't being read on the child side — check `libcluu::fd_table::init_stdio` (file at userspace/libcluu/src/fd_table.rs:335 onward) for whether PARAM_FD_VFS_OFFSET/LEN are being parsed.

- [ ] **Step 4: If marker `hi-from-vt0` fires here, skip Task 6 (cluuterm wasn't broken — Task 4 alone fixed both)**

If the harness still doesn't print `hi-from-vt0`, the round-trip from tty service → VFS → shell read(0) → shell command exec → shell write(1) → VFS → tty service → console isn't fully wired. Most likely failure: tty service's `try_satisfy_reads` is firing but `forward_to_console` for the shell-written `hi-from-vt0\n` isn't reaching COM2.

Diagnose by grep:

```bash
grep -E "vfs: open '/dev/tty0'|derive_child_fd|stdin path|hi-from" /tmp/cluu-serial-com2.log
```

If `derive_child_fd` appears but `hi-from` does not, the read-side works but the write-side is dropping. Investigate `userspace/vfs/src/main.rs:1564` (`OpenFile::Device` write path) and `userspace/tty/src/main.rs:121` (`TTY_WRITE_LABEL` handler) — both should already work since cluuterm pts writes via the same mechanism.

- [ ] **Step 5: Commit the trace**

```bash
git add userspace/shell/src/main.rs
git commit -m "shell: trace fd 0 transport at startup for harness verification"
```

---

## Task 6: Cluuterm path verification + diagnostic

**Files:**
- Read-only inspection of `userspace/cluuterm/src/tty_backend.rs:111-117` (`try_flush_pending_pts_read`) and `userspace/cluuterm/src/main.rs:241-340` (`spawn_shell_with_pts`).

- [ ] **Step 1: Add a debug trace to `try_flush_pending_pts_read`**

In `userspace/cluuterm/src/tty_backend.rs`, modify the helper added in commit f5d5c3f:

```rust
    pub fn try_flush_pending_pts_read(&mut self) {
        let (reply_token, max) = match self.pending_pts_read.take() {
            Some(p) => p,
            None => return,
        };
        if self.stdin_buf.is_empty() {
            self.pending_pts_read = Some((reply_token, max));
            return;
        }
        let data = self.handle_pts_read(max);
        let _ = libcluu::debug_print(&alloc::format!(
            "cluuterm: pts read flushed {} bytes", data.len()
        ));
        let reply = Message::new(
            PTS_READ_LABEL,
            [0, data.len(), 0, 0, 0, 0],
            2,
        );
        let _ = libcluu::ipc::reply_with_payload(reply_token, &reply, &data);
    }
```

Also at the PTS_READ_LABEL match arm in the `run()` loop (lines 387-407), add a trace when we defer:

```rust
                PTS_READ_LABEL => {
                    let max = msg.words[1].max(1);
                    let reply_token = libcluu::ipc::extract_reply_id(&msg).unwrap_or(0);
                    if reply_token == 0 {
                        // No reply slot — drop silently.
                    } else if self.stdin_buf.is_empty() {
                        let _ = libcluu::debug_print(
                            "cluuterm: pts read deferred (empty buf)"
                        );
                        self.pending_pts_read = Some((reply_token, max));
                    } else {
                        let data = self.handle_pts_read(max);
                        let _ = libcluu::debug_print(&alloc::format!(
                            "cluuterm: pts read served {} bytes immediately",
                            data.len()
                        ));
                        let reply = Message::new(
                            PTS_READ_LABEL,
                            [0, data.len(), 0, 0, 0, 0],
                            2,
                        );
                        let _ = libcluu::ipc::reply_with_payload(reply_token, &reply, &data);
                    }
                }
```

- [ ] **Step 2: Add a harness marker for cluuterm shell input**

In `scripts/harness_case_defaults.sh`, add after the new `l2_text_shell_input)`:

```bash
            l2_cluuterm_shell_input)
                TEST_COMMAND=""
                # Login on VT4 via the compositor login modal, then type
                # `echo hi-from-cluuterm` in the cluuterm window. The shell's
                # write(1) goes through VFS PTS_WRITE -> cluuterm -> renderer,
                # so the marker fires when the bytes return through the
                # debug_print path the shell command itself emits (echo
                # writes to fd 1 which is /dev/pts/0; we also see the line
                # via the `shell: parse <line>` trace if enabled, but the
                # most reliable marker is the literal string showing in
                # `cluuterm: pts write` traces if any exist).
                #
                # Reliable marker: shell's existing "shell: parse: ..." or
                # "shell: stdin path" debug_prints + the new
                # "cluuterm: pts read flushed N bytes" trace from Task 6.
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 5\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 1\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 4\nsendkey e\nsendkey c\nsendkey h\nsendkey o\nsendkey spc\nsendkey h\nsendkey i\nsendkey minus\nsendkey f\nsendkey r\nsendkey o\nsendkey m\nsendkey minus\nsendkey c\nsendkey l\nsendkey u\nsendkey u\nsendkey t\nsendkey e\nsendkey r\nsendkey m\nsendkey ret'
                ;;
```

In `scripts/harness_run.sh` add the case (after `l2_text_shell_input)`):

```bash
    l2_cluuterm_shell_input)
        required_markers=(
            "TSC calibrated"
            "cluuterm: /bin/shell spawned"
            "shell: stdin path = vfs-backed"
            "cluuterm: pts read flushed"
        )
        ;;
```

- [ ] **Step 3: Build + run**

```bash
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_cluuterm_shell_input bash scripts/harness_run.sh
grep -E "cluuterm: pts read|shell: stdin path|shell: parse" /tmp/cluu-serial-com2.log | head -30
```

Three possible diagnoses:

| Symptom | Diagnosis | Fix in step |
|---|---|---|
| no `cluuterm: pts read deferred` ever | shell never calls `read(0)`; fd 0 still wrong kind | Step 4a |
| `cluuterm: pts read deferred (empty buf)` but never `flushed` | input.rs `try_flush_pending_pts_read` not called or stdin_buf not populated | Step 4b |
| `cluuterm: pts read flushed N bytes` fires but shell still silent | data arrives at shell but the shell main loop discards it | Step 4c |

- [ ] **Step 4a: If shell never reads fd 0 (symptom 1)**

Inspect libcluu's `init_stdio` (`userspace/libcluu/src/fd_table.rs:335-410`). Confirm:
- `PARAM_FD_VFS_OFFSET` and `PARAM_FD_VFS_LEN` are nonzero in ProcessInfo.
- The trailer parse populates `vfs_meta[0]` with nonzero `(vcid, vfd)`.
- For fd 0, the branch `if vcid != 0 && vfd != 0` is taken, producing an `FdEntry::file(...)` with `remote_fd: Some(...)`.

Quick diagnostic: drop this debug_print into `init_stdio` right before the fd loop:

```rust
    let _ = crate::debug_print(&alloc::format!(
        "init_stdio: trailer_off={} trailer_len={} fd0_vfs=({}, {})",
        trailer_off, trailer_len, vfs_meta[0].0, vfs_meta[0].1
    ));
```

If `fd0_vfs=(0, 0)`, the trailer isn't being written by procmgr — return to Task 4 and check `fd_vfs_meta[0] = (child_cid, child_rfd);` in the FDAC parser. The likely fault is that VFS's `derive_child_fd` reply word `child_tid` (used as `child_cid` in `fd_vfs_meta[target_fd as usize] = (child_cid, child_rfd);`) is being clobbered by something — log the values right at that line in procmgr.

- [ ] **Step 4b: If PTS_READ is deferred but never flushed (symptom 2)**

Add this trace in `userspace/cluuterm/src/input.rs` `apply_effect`, right after `line_ready` block:

```rust
        let _ = libcluu::debug_print(&alloc::format!(
            "cluuterm: line_ready: {} bytes pushed; pending_pts_read={}",
            line.len(),
            term.pending_pts_read.is_some()
        ));
```

(Requires making `pending_pts_read` `pub` temporarily — revert before commit.)

If `line_ready` never fires for an Enter keypress, the discipline isn't seeing CR/LF — likely a keymap mismatch (HU layout vs `sendkey ret`). Try `sendkey kp_enter` as a workaround in the harness sequence.

- [ ] **Step 4c: If bytes arrive at shell but execute path swallows them (symptom 3)**

Inspect `handle_line_payload` in `userspace/shell/src/main.rs` and the `parse_and_execute_line` it calls. Likely shell builtin `echo` resolves and runs, but its stdout write is going somewhere we're not capturing on COM2. Add a trace inside `parse_and_execute_line` to confirm the line buffer reaches the parser.

- [ ] **Step 5: Apply the targeted fix that Step 4 surfaced and rerun marker**

Repeat until `l2_cluuterm_shell_input` passes.

- [ ] **Step 6: Commit the diagnostic traces (keep them; they're cheap and useful)**

```bash
git add userspace/cluuterm/src/tty_backend.rs userspace/cluuterm/src/input.rs userspace/libcluu/src/fd_table.rs userspace/shell/src/main.rs scripts/harness_case_defaults.sh scripts/harness_run.sh
git commit -m "harness+traces: l2_cluuterm_shell_input + pts-read lifecycle diag"
```

---

## Task 7: Remove tty service push code

**Files:**
- Modify: `userspace/tty/src/main.rs` (compute_path_completion at lines 403-437, TTY_REGISTER push handling at lines 168-194).
- Modify: `userspace/tty/src/context.rs` (remove now-dead fields + helpers).

- [ ] **Step 1: Delete `compute_path_completion` and its call site**

In `userspace/tty/src/main.rs`, delete the entire `compute_path_completion` function (lines ~403-437) and its single call from the discipline handler (around line ~328):

```rust
    if let Some((partial, tab_count)) = effect.tab_request {
        if let Some(completion) = compute_path_completion(&partial, tab_count, ctx) {
            ctx.forward_to_console(&completion);
            discipline.append_completion(&completion);
        }
    }
```

Replace with:

```rust
    // TAB completion through the shell required a recv loop on the shell's
    // stdin endpoint, which Path A retired. Re-wire later via fd-0-based
    // completion (separate spec). For now, just drop the tab event.
    let _ = effect.tab_request;
```

- [ ] **Step 2: Delete `deliver_shell_line`**

In `userspace/tty/src/context.rs`, remove the entire `deliver_shell_line` method (lines ~334-364). With `deliver_line` in main.rs already not calling it (removed in commit 37fb703), it's pure dead code.

- [ ] **Step 3: Delete `wire_shell_stdin`, `set_shell_stdin_route`, `configure_foreground` push side**

In `userspace/tty/src/context.rs`:

- Delete `set_shell_stdin_route` (lines ~224-228).
- Delete the body of `configure_foreground` (lines ~234-264) — replace its body with a no-op that just logs once for visibility:

  ```rust
      pub fn configure_foreground(&mut self, _endpoint: usize, _ctrl_c_notify: usize, _flags: usize) {
          let _ = debug_print("tty: TTY_REGISTER_LABEL push-config ignored (Path A)");
      }
  ```

- Delete the `wire_shell_stdin` method (look for `pub fn wire_shell_stdin` and remove).

- [ ] **Step 4: Delete `shell_stdin` and `shell_registered_stdin` fields**

In `userspace/tty/src/context.rs`, in the `TtyContext` struct (around lines 40-83), delete the two fields:

```rust
    pub shell_stdin: usize,
    shell_registered_stdin: usize,
```

And their initializations in the constructor (around lines 120-121):

```rust
            shell_stdin: 0,
            shell_registered_stdin: 0,
```

- [ ] **Step 5: Delete `ctrl_c_notify` Ctrl-C push if it pushed through shell_stdin**

In `userspace/tty/src/main.rs` Ctrl-C handler (lines ~342-347):

```rust
        if is_ctrl_c {
            if ctx.ctrl_c_notify != 0 {
                let _ = libcluu::ipc::send_with_payload(ctx.ctrl_c_notify, TTY_READ_LABEL, &[0x03]);
            }
```

Replace with:

```rust
        if is_ctrl_c {
            // Out-of-band Ctrl-C notify retired with the TTY_READ_LABEL push.
            // Job-control signal delivery via PROCMGR_PG_SIGNAL handles SIGINT.
            // Future: re-introduce an explicit "interrupt readers" mechanism
            // if foreground tools need it (separate task).
            let _ = ctx.ctrl_c_notify;
```

Keep the rest of the block (the PROCMGR_PG_SIGNAL send). Remove the `ctrl_c_notify` field if no other code reads it — run a `grep -rn ctrl_c_notify userspace/tty` and remove the field + its initialization if all references are now gone.

- [ ] **Step 6: Build**

```bash
cargo xtask build 2>&1 | tail -10
```

Expect: clean build. If warnings about `forward_ctrl_c` being unused, also remove that field — it was paired with the deleted push path.

- [ ] **Step 7: Run both markers**

```bash
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_text_shell_input bash scripts/harness_run.sh
HARNESS_FORCE_BUILD=0 MARKER_MODE=l2_cluuterm_shell_input bash scripts/harness_run.sh
```

Both must still pass.

- [ ] **Step 8: Commit**

```bash
git add userspace/tty/src/main.rs userspace/tty/src/context.rs
git commit -m "tty: delete TTY_READ_LABEL push path (Path A unification)"
```

---

## Task 8: Drop the shell-side `rebind_stdio_to_devtty` fallback

**Files:**
- Modify: `userspace/shell/src/main.rs:53-89` (the rebind helper + the call site).

- [ ] **Step 1: Confirm rebind never fires in either marker**

Re-run both markers from Task 7 and grep:

```bash
grep "shell: open /dev/tty" /tmp/cluu-serial-com2.log
```

Expected: no hits (procmgr always seeds fd 0 via FDAC now, so `fd0_is_vfs_backed` is always true).

- [ ] **Step 2: Delete `rebind_stdio_to_devtty` and its call**

In `userspace/shell/src/main.rs`, delete the entire `rebind_stdio_to_devtty` function (lines ~53-89) and the `extern "C"` block at the top declaring `_open`, `_dup2`, `_close`:

```rust
extern "C" {
    fn _read(fd: core::ffi::c_int, buf: *mut core::ffi::c_void, count: usize) -> isize;
    fn _open(path: *const u8, flags: core::ffi::c_int, mode: u32) -> core::ffi::c_int;
    fn _dup2(oldfd: core::ffi::c_int, newfd: core::ffi::c_int) -> core::ffi::c_int;
    fn _close(fd: core::ffi::c_int) -> core::ffi::c_int;
}
```

Keep only `_read`:

```rust
extern "C" {
    fn _read(fd: core::ffi::c_int, buf: *mut core::ffi::c_void, count: usize) -> isize;
}
```

In `run()`, delete the `if !fd0_is_vfs_backed { ... }` block. Also delete the `fd0_is_vfs_backed` computation if the trace from Task 5 stays, or assert it directly:

```rust
    // Procmgr seeds fd 0/1/2 via FDAC at every spawn. Shell unconditionally
    // reads stdin via POSIX read(0). If the assertion ever trips, procmgr
    // failed to wire FDAC and the child should exit rather than spin.
    let fd0_is_vfs_backed = libcluu::fd_table::FD_TABLE
        .lock()
        .get(0)
        .map(|e| e.remote_fd.is_some())
        .unwrap_or(false);
    if !fd0_is_vfs_backed {
        let _ = debug_print("shell: FATAL fd 0 not VFS-backed; parent FDAC missing");
        return Err(Error::InvalidState);
    }
```

- [ ] **Step 3: Build + re-run both markers**

```bash
cargo xtask build
HARNESS_FORCE_BUILD=0 MARKER_MODE=l2_text_shell_input bash scripts/harness_run.sh
HARNESS_FORCE_BUILD=0 MARKER_MODE=l2_cluuterm_shell_input bash scripts/harness_run.sh
```

Both must still pass.

- [ ] **Step 4: Commit**

```bash
git add userspace/shell/src/main.rs
git commit -m "shell: drop rebind fallback; assert fd 0 VFS-backed at startup"
```

---

## Task 9: Final cleanup — diagnostic traces

**Files:**
- Modify: traces added in Task 5, 6, etc.

- [ ] **Step 1: Remove the noisy traces, keep the assert + the harness-marker trace**

Delete the `cluuterm: pts read deferred` and `cluuterm: pts read served` traces from `userspace/cluuterm/src/tty_backend.rs`. Keep `cluuterm: pts read flushed N bytes` only if the harness marker depends on it — if so, also keep its required-marker entry; otherwise delete both.

The `shell: stdin path = vfs-backed` trace can stay (cheap, useful).

Delete the `init_stdio: trailer_off=...` trace from `libcluu/fd_table.rs`.

Delete the `cluuterm: line_ready: ...` trace from `userspace/cluuterm/src/input.rs` (added in Task 6, Step 4b).

- [ ] **Step 2: Run both markers one final time after the trace cleanup**

```bash
cargo xtask build
HARNESS_FORCE_BUILD=0 MARKER_MODE=l2_text_shell_input bash scripts/harness_run.sh
HARNESS_FORCE_BUILD=0 MARKER_MODE=l2_cluuterm_shell_input bash scripts/harness_run.sh
```

Both pass. The serial log should be noticeably quieter than during diagnostics.

- [ ] **Step 3: Update memory**

Edit `/home/vlb2bp/.claude/projects/-home-vlb2bp-git-cluu/memory/project_loginCC_session_2026_05_13.md`. Mark Bug C as CLOSED (with the commit hash from Task 8). Edit the MEMORY.md one-line index line for that entry accordingly.

Also add a new feedback memory file `feedback_path_a_stdio_assertion.md`:

```markdown
---
name: shell-fd0-must-be-vfs-backed
description: "Shell asserts fd 0 is VFS-backed at startup. If procmgr fails to wire FDAC the child exits — do NOT add a fallback that hides the spawn-side bug."
metadata:
  type: feedback
---

Shell unconditionally requires fd 0 to be VFS-backed (a `/dev/tty<N>` or
`/dev/pts/<id>` handle injected via FDAC at spawn). If a regression makes
procmgr forget the FDAC injection, the child exits immediately with a clear
FATAL message — and the harness flags it.

**Why:** Bug C masked itself for weeks because the shell had a fallback
recv_any path that silently kept the prompt visible without delivering
input. Loud-fail is better than silent-degrade.

**How to apply:** When touching procmgr's session spawn paths, never
return to the TOKEN_STDIN endpoint pattern for the shell. The only correct
shape is FDAC fd 0/1/2 pointing at a VFS-backed `/dev/...` node.
```

Append to `MEMORY.md` index a one-line entry for the new feedback memory.

- [ ] **Step 4: Commit cleanup + memory**

```bash
git add userspace/cluuterm/src/tty_backend.rs userspace/cluuterm/src/input.rs userspace/libcluu/src/fd_table.rs userspace/shell/src/main.rs
git commit -m "cleanup: drop Bug C diagnostic traces after both stdin markers green"

# memory commit is from the home dir, NOT the cluu repo
echo "Memory file edits are outside the repo — they persist directly to ~/.claude. No commit needed."
```

---

## Self-review checklist (run after completing all tasks)

- Both `l2_text_shell_input` and `l2_cluuterm_shell_input` harness markers pass on a fresh build (no `HARNESS_FORCE_BUILD=0` shortcut).
- `grep -rn "TTY_READ_LABEL" userspace/tty/` returns no send sites (only the const re-export).
- `grep -rn "shell_stdin\b" userspace/tty/` returns nothing.
- `grep -rn "TOKEN_STDIN" userspace/shell/` returns only the import (slot index reference) — no `info.tokens[TOKEN_STDIN]` lookups left.
- Memory MEMORY.md index reflects Bug C closed + the new fd-0 assertion feedback entry.
