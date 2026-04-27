# CLUU shell pipes — design spec

**Date:** 2026-04-27
**Status:** Draft, awaiting user review.
**Implements:** Public GitHub issue #7 follow-on (option C / pipes), full version per
in-session decision matrix (level D + general POSIX `pipe(2)`).

---

## 1. Summary

Add full Unix-style pipelines to the CLUU shell:

```
$ cat /etc/motd | grep CLUU | head -3
```

Both ends of every pipe stage are real spawned binaries (or shell builtins
running in a worker thread) talking to each other through restricted-rights
IPC tokens. Builtin-vs-binary boundary cleaned up so file-utility commands
(`cat`, `ls`, `ps`, `touch`) become real `/bin/X` binaries instead of
shell-internal builtins. New binaries `/bin/grep`, `/bin/head`, `/bin/wc` added.

The kernel does not learn the word "pipe." A pipe is one IPC endpoint with two
rights-restricted tokens minted from it; lifecycle and EOF/SIGPIPE semantics
ride entirely on existing token-revocation.

## 2. Goals & non-goals

**Goals**

- `cat foo | grep bar | head -3`-style pipelines work end-to-end from the
  interactive shell.
- Stderr redirection: `cmd 2> file`, `cmd 2>&1`, `cmd 2>&1 | other`.
- `SIGPIPE` semantics: writer of a pipeline whose reader exits early is killed
  with SIGPIPE (default action), so `yes | head -3` terminates promptly.
- POSIX `pipe(2)` available to any program (not just the shell), so future
  code (micropython subprocess support, command substitution) can use it.
- File-utility commands cleanly partitioned: stays-builtin if and only if the
  command must mutate shell state.

**Non-goals (this round)**

- SharedRing-backed pipes for high throughput (deferred — see §13).
- Named FIFOs (`mkfifo`) — out of scope, but the `pipe()` mechanism is
  designed so they fit cleanly later.
- Process substitution `<(cmd)`, here-documents `<<EOF`, here-strings `<<<`.
- Fd numbers other than 0/1/2 in redirections (no `3>&1`, no
  `exec 5< file`).
- `set -o pipefail` (deferred — scaffolding noted in §10).
- Real-time signal queueing, `signalfd`-style pipe notifications.
- Migrating privileged binaries (`su`, `sudo`, `container`, `poweroff`,
  `reboot`) and test scaffolding (`jobchurn`, `mapfail`, `ext2*`, etc.).

## 3. Architecture overview

Three layers, each owning one concern.

```
                           ┌──────────────────────────────────┐
shell  ─── parses ────►    │ Pipeline { Vec<Command> }        │  (cluu_lang AST,
                           └──────────────────────────────────┘   already today)
                                          │
                             for each adjacent pair, ask procmgr
                                          │
                                          ▼
                           ┌──────────────────────────────────┐
procmgr  ── mints ────►    │ pipe = IPC endpoint object       │
                           │   write_token (send rights)      │
                           │   read_token  (recv rights)      │
                           │   pipe_id     (cleanup handle)   │
                           └──────────────────────────────────┘
                                          │
                  spawn(image, argv, stdin = read_token, stdout = write_token)
                                          │
                                          ▼
                /bin/cat ──[TTY_WRITE_LABEL msgs]──► /bin/grep
```

A pipe is **just an IPC endpoint** with two rights-restricted tokens minted
from it. Procmgr is the lifecycle authority: it allocates the endpoint, mints
the tokens, hands them to children at spawn, and revokes them on child exit.
Token revocation is what propagates EOF (reader sees recv-fail = 0 bytes) and
SIGPIPE (writer sees send-fail = EPIPE → raise(SIGPIPE)).

From a child's perspective, fd 1 (stdout) and fd 0 (stdin) behave identically
whether wired to a TTY or a pipe — same `TTY_WRITE_LABEL` send-protocol on the
write side, same recv loop on the read side. No fd-type dispatch in newlib's
write-to-tty path; the fd_table just dispatches kind→endpoint and the existing
TTY protocol handles both cases unchanged.

The microkernel discipline holds: kernel only sees endpoints and tokens; the
word "pipe" only exists in userspace.

## 4. Procmgr API additions

Two new IPC ops, both server-side in procmgr's existing dispatch loop in
`userspace/procmgr/src/main.rs`.

### 4.1 `PROCMGR_PIPE_CREATE`

Caller asks for a fresh pipe.

```
Request:  Message { label: PROCMGR_PIPE_CREATE, words: [], payload: [] }
Reply:    Message { label: 0,
                    words:   [ status, write_token, read_token, pipe_id ],
                    payload: [] }
```

- `status` — 0 on success, errno-shaped on failure.
- `write_token`, `read_token` — capability tokens for the same underlying
  endpoint, with rights restricted to send-only and recv-only respectively.
- `pipe_id` — procmgr-side opaque handle (`usize` index into procmgr's pipe
  table) for explicit close. Zero is reserved as "invalid".

Caller is responsible for passing the tokens to wherever they need to go
(another process, the fd_table) and eventually calling `PIPE_CLOSE`.

### 4.2 `PROCMGR_PIPE_CLOSE`

Explicit teardown.

```
Request:  Message { label: PROCMGR_PIPE_CLOSE, words: [pipe_id], payload: [] }
Reply:    Message { label: 0, words: [status], payload: [] }
```

Procmgr revokes the *caller's* tokens for that pipe (the calling process may
hold either side, both sides, or neither — procmgr revokes whichever it
owns). The pipe entry remains in procmgr's table as long as any token from
any process is still active. When the last token is revoked, procmgr
destroys the underlying endpoint object and reclaims the `pipe_id` slot
(reuse-safety guarded by §10.5's generation counter).

Idempotent: closing a `pipe_id` for which the caller already revoked
everything returns `status=0`.

### 4.3 No `PROCMGR_SPAWN_PIPELINE` superop

The shell composes pipelines from `PIPE_CREATE` + N `CONTAINER_RUN` calls.
Pipelines are a shell concern, not a procmgr concern, and bundling them would
freeze a particular pipeline shape into a lower layer.

### 4.4 Cleanup on caller-process exit

Procmgr already gets exit notifications (the I7 design — init/procmgr cleanup
chain). Extend that path: when a process dies, walk its still-open pipe
entries and close them. So `pipe(2)`-callers don't strictly need to close on
the happy path, but `close(2)` should propagate to procmgr-pipe-close when the
last fd-table entry for a given pipe-end goes away, to get prompt EOF/SIGPIPE.

## 5. Newlib / libcluu integration

The `fd_table` in `userspace/libcluu/src/fd_table.rs` is already the right
abstraction — it maps fd → endpoint. We just need a new `FdKind` so read/write
paths can dispatch correctly.

### 5.1 `FdKind` extension

```rust
pub enum FdKind {
    Tty,                              // existing
    Vfs { remote_fd: usize },         // existing
    PipeRead  { pipe_id: usize },     // new
    PipeWrite { pipe_id: usize },     // new
}
```

The `endpoint: usize` slot stays; the kind disambiguates dispatch. `pipe_id`
rides along so `close()` can call `PROCMGR_PIPE_CLOSE` when the last fd_table
reference drops.

### 5.2 `pipe(2)` wrapper — new file `userspace/libcluu/src/posix/pipe.rs`

```rust
#[no_mangle]
pub extern "C" fn pipe(fds: *mut c_int) -> c_int {
    // 1. Send PROCMGR_PIPE_CREATE; receive (write_token, read_token, pipe_id).
    // 2. Allocate two fd_table slots.
    // 3. fds[0] = read fd  (FdKind::PipeRead,  endpoint=read_token,  pipe_id)
    //    fds[1] = write fd (FdKind::PipeWrite, endpoint=write_token, pipe_id)
    // 4. On any failure, free what was allocated and return -1 with errno.
}
```

Plus `pipe2(fds, flags)` for `O_CLOEXEC`. Since CLUU has no `exec`, the flag
is a no-op semantically, but newlib expects the symbol to exist.

### 5.3 Read dispatch

In `_read_r`/`read`: on `FdKind::PipeRead`, call `recv` on the endpoint with a
buffer-sized recv. Same shape as today's TTY-read path, minus the
`TTY_READ_REQUEST_LABEL` round-trip — for pipes there is no request, the
writer has already pushed bytes via `TTY_WRITE_LABEL` and they sit in the
endpoint queue.

- Returns: bytes copied on success.
- On `Error::TokenInvalid` (writer-side revoked): return 0 (EOF).
- On `Error::WouldBlock` with no data ready: block waiting for the next
  message (default), or return -1 + EAGAIN if the fd is non-blocking
  (deferred — non-blocking fds aren't supported in this round).

### 5.4 Write dispatch

In `_write_r`/`write`: on `FdKind::PipeWrite`, identical to today's TTY-write
path: `send_with_retry(endpoint, TTY_WRITE_LABEL, buffer)`.

- Returns: `buffer.len()` on success (full message delivered).
- On `Error::TokenInvalid` (reader-side revoked): set `errno=EPIPE`,
  return -1, and call `raise(SIGPIPE)` via the existing signal path.

### 5.5 Close dispatch

In `_close_r`/`close`: on `FdKind::PipeRead`/`PipeWrite`, drop the fd_table
entry; if this was the last fd in this process referencing that `pipe_id`,
call `PROCMGR_PIPE_CLOSE(pipe_id)`. Procmgr revokes only this side's token
(each side has its own); the *other* side's children still hold their token
and keep working until they too close.

### 5.6 Why this is small

The "same protocol as TTY" win means:

- Builtins inside pipelines work for free — when the shell hands a builtin a
  pipe write-token in the existing `stdout: usize` parameter, the builtin's
  `send_with_payload(stdout, TTY_WRITE_LABEL, ...)` calls work unchanged.
- `/bin/cat`'s `printf` continues to write to fd 1 — fd 1's endpoint just
  happens to be a pipe-token instead of a TTY token.
- No new IPC contract anywhere.

## 6. Shell pipeline executor

A new file `userspace/shell/src/pipeline.rs` exporting `PipelineExecutor`.
`commands.rs` is already 3700+ lines; pipeline orchestration is its own
concern (parser-output → spawn-orchestration), distinct from "is this a
builtin?" lookup.

### 6.1 Lifecycle of `cmd1 | cmd2 | cmd3`

```
1. Allocate N-1 pipes:
       p1 = procmgr.PIPE_CREATE()  → (w1, r1, id1)
       p2 = procmgr.PIPE_CREATE()  → (w2, r2, id2)

2. Spawn each command, wired:
       spawn(cmd1, stdin = inherited_stdin, stdout = w1)
       spawn(cmd2, stdin = r1,              stdout = w2)
       spawn(cmd3, stdin = r2,              stdout = inherited_stdout)

3. Drop shell's local copies of (w1, r1, w2, r2). This is critical:
   if shell keeps a token, EOF never propagates because procmgr won't
   revoke a token that's still referenced.

4. Wait for all N child PIDs.

5. $? = exit status of the LAST command (POSIX default).
       (`set -o pipefail` deferred — see §13.)
```

### 6.2 Builtins in pipelines

Builtins are in-process; they need a worker-thread treatment when piped.
Implementation:

- For each `Command` in the pipeline, if its first word is a builtin name,
  spawn a worker thread inside the shell process that calls the builtin's
  `run(stdout, ctx, args)` with `stdout = pipe_write_endpoint`.
- The worker thread exits when the builtin returns; its "exit code" is
  recorded in a per-pipeline result table for `$?`.

**Builtins that can legally be piped** (they don't mutate shell state):
`echo`, `true`, `false`, `test`/`[`, `expr`, `repeat`, `env`, `help`. Plus any
other builtin whose effect is bounded to its own stdout.

**Builtins that cannot be piped** reject pipeline use with a clear error:
`cd`, `pwd` (well, pwd could but we keep it consistent), `exit`, `set`,
`unset`, `jobs`, `fg`, `bg`, `kill`, `let`, `clear`, `spawn`, `spawnbg`,
`stop`, all session/privileged commands. Bash also rejects most of these in
pipelines or runs them in a subshell that has no effect.

### 6.3 Foreground signal handling during a pipeline

Today's TTY foreground-group mechanism broadcasts SIGINT to a single
foreground PID. Extend `CommandContext` to track a *list* of foreground PIDs
(the pipeline's children); broadcast signals to each. Small change — most of
the work is plumbing the list through.

### 6.4 Where `flatten_simple_command_from_stmt` goes

Today, `flatten_simple_command_from_stmt` returns `None` when
`pipeline.commands.len() != 1`, which silently no-ops the pipeline. New
control flow:

- 0 commands → no-op (today's behavior).
- 1 command → existing single-command path (today's behavior). This
  includes builtins-without-pipe.
- 2+ commands → `PipelineExecutor::run(pipeline, ctx)` — the new path.

`flatten_simple_command_from_stmt` stays for the single-command path; the new
path takes over for multi-command and shares helpers (`render_word`,
`expand_assigns`).

## 7. Builtin → /bin migration

### 7.1 Must migrate this round (blocks the demo)

| Today | Becomes | Reason |
|---|---|---|
| `CatBuiltin` | `/bin/cat` (stub exists at `userspace/cat/src/main.rs`, flesh out) | Demo target uses it. Builtin would shortcut around the pipe. |
| *(new)* | `/bin/grep` | Demo target. Minimum: literal-string match, `-n`, `-i`, `-v`, single file or stdin. **No regex this round.** |
| *(new)* | `/bin/head` | Demo target. Minimum: `-n N` with default 10. |

### 7.2 Migrate this round (cleanup, low effort, big consistency win)

| Today | Becomes | Reason |
|---|---|---|
| `LsBuiltin` | `/bin/ls` | Already opens VFS, formats text, doesn't touch shell state. |
| `PsBuiltin` | `/bin/ps` | Same. Reads `/proc`, formats. |
| `TouchBuiltin` | `/bin/touch` | Already opens VFS only. Trivial. |

### 7.3 Add this round (small, high value)

| New | Reason |
|---|---|
| `/bin/wc` | One day's work. Makes pipelines actually useful. Minimum: `-l`, `-w`, `-c`, default = all three. |

### 7.4 Stay as builtin (must — they mutate shell state)

`cd`, `pwd`, `exit`, `set`, `unset`, `env`, `jobs`, `fg`, `bg`, `kill`,
`true`, `false`, `test`, `[`, `expr`, `let`, `repeat`, `help`, `clear`,
`spawn`, `spawnbg`, `stop`.

### 7.5 Stay as builtin for now (defer migration)

`heap`, `echo` (bash keeps echo as both builtin and `/bin/echo`; we match),
`su`, `sudo`, `container`, `poweroff`, `reboot`, all the test scaffolding
(`jobchurn`, `jobmix`, `mapfail`, `mapcpfail`, `maperror`, `ringio`, `ext2*`,
`killdeny`, `regdeny`, `vtcrashtest`, `sudotest`, `sutest`, `escalatedeny`,
`suequaltest`, `shellcrash`).

Test scaffolding *should* eventually move to a `__harness_*`-prefixed
namespace registered only in test builds — file as a follow-up issue, do not
do it in this round.

### 7.6 What changes in `commands.rs`

For each migrated builtin: delete its `struct XBuiltin;` and `impl
BuiltinCommand for XBuiltin`, delete its `register(Box::new(XBuiltin))` line
in `DefaultBuiltins::register`. The shell's external-binary fallback (the
"binary in `/bin/`?" lookup performed when a name isn't a builtin) takes
over automatically.

## 8. Stderr redirection

Three patterns. All four redirection ops (`>`, `>>`, `<`, `2>`) are already
in the cluu_lang grammar; they're just not honored today.

### 8.1 `cmd 2> file` — redirect stderr to a file

The shell, before `container_run`-ing the command, opens `file` via VFS with
`O_WRONLY|O_CREAT|O_TRUNC` and passes the resulting endpoint as the child's
`stderr_token` (existing slot in `map_process_info_page`).

Caveat: a VFS-fd accepts a different protocol than a TTY endpoint. Solution:
extend the spawn protocol so `stderr_token` (and `stdout_token`, for
`>file`) can be either an IPC endpoint *or* a `(remote_fd, vfs_endpoint)`
pair. Procmgr emits a small trailer indicating fd 2's kind; child reads it
during `_start` and seeds `fd_table[2]` accordingly. Same mechanism extends
naturally to fd 1 for `cmd > file`.

### 8.2 `cmd 2>&1` — redirect stderr to wherever stdout points

Pure shell-side concern. When building the spawn args, shell sets
`stderr_token = stdout_token`. If stdout is a pipe write-end, stderr now goes
to the same pipe. No procmgr changes needed.

### 8.3 `cmd 2>&1 | other` — combination

Shell processes redirections in left-to-right order:

1. Set up `stdout = pipe_write` (from the `|`).
2. Apply `2>&1`: copy stdout's slot into stderr's slot.

Order matters and is bash-compatible: `cmd 2>&1 > file` (stderr → original
stdout = TTY, then stdout → file → stderr stays on TTY) vs `cmd > file 2>&1`
(stdout → file, then stderr dups stdout → both → file).

### 8.4 What's out of scope for redirection in this round

`<<` here-docs, `<<<` here-strings, `<>` rw, `&>` short-form, fd numbers
other than 0/1/2, process substitution `<(cmd)`. The grammar listed at the
top of §8 is the strict ceiling.

## 9. SIGPIPE semantics

The basic mechanism falls out of token-revocation, but the timing matters.

### 9.1 Happy path — writer outlives reader (`yes | head -3`)

```
1. yes spawned with stdout = pipe_write_token.
   head spawned with stdin  = pipe_read_token.
2. yes loops printf("y\n"); head reads 3 lines, exits.
3. head's exit triggers procmgr's child-exit handler, which revokes
   head's read_token (head was the only holder).
4. yes's next send_with_retry(write_token, TTY_WRITE_LABEL, ...) returns
   Error::TokenInvalid.
5. libcluu's _write_r catches that, sets errno=EPIPE, returns -1.
6. libcluu's signal layer sees EPIPE-on-write and calls raise(SIGPIPE).
7. Default SIGPIPE handler kills `yes`. Pipeline complete.
```

The cluu signal layer already has `raise()` and a default-action table from
Phase 3 Threading. We add one entry: `SIGPIPE → terminate`. Programs that
want to ignore SIGPIPE call `signal(SIGPIPE, SIG_IGN)` and check
`errno==EPIPE` themselves — standard POSIX.

### 9.2 Reverse case — reader outlives writer (`cat foo | grep bar`)

```
1. cat reaches EOF on foo, exits 0.
2. Procmgr revokes cat's write_token.
3. grep's next recv on its read endpoint returns Error::TokenInvalid.
4. libcluu's _read_r catches that, returns 0 — POSIX EOF.
5. grep loop ends naturally, exits 0.
```

No signal — just clean EOF.

### 9.3 Bytes-in-flight on early reader exit

Between the moment procmgr revokes the read_token and the writer hits
`Error::TokenInvalid`, the writer may have enqueued bytes the (now-dead)
reader will never consume. Those bytes sit in the kernel endpoint queue until
the endpoint object is destroyed, which happens when the *last* token (the
write_token, held by the writer) is revoked too — i.e., when the writer
process dies. Worst case: a few KB of unread bytes held until pipe teardown.
Acceptable for this round; not worth a workaround.

### 9.4 Multi-stage SIGPIPE chain (`cat huge | head -3 | wc -l`)

```
- wc reads "head"'s 3 lines, exits 0.
- procmgr revokes wc's read_token (= pipe2_read).
- head's next write to pipe2 → EPIPE → SIGPIPE → head dies.
- procmgr revokes head's read_token (= pipe1_read).
- cat's next write to pipe1 → EPIPE → SIGPIPE → cat dies.
- shell's wait loop reaps all three; $? = wc's status (last command).
```

The shell process itself ignores SIGPIPE (`signal(SIGPIPE, SIG_IGN)` at
startup) so it isn't killed when a pipeline child dies. Builtins running in
worker threads inside the shell process inherit the IGN disposition — if
their `send_with_payload` returns EPIPE, the worker bails out cleanly.

## 10. Error handling and edge cases

### 10.1 Exit-code propagation

`$?` after a pipeline = exit status of the last command. POSIX default. Not
"OR of all stages", not "first non-zero" — explicitly the *last*. This is
what bash does without `pipefail`.

### 10.2 `set -o pipefail` is deferred

Including the option later: `$?` becomes the rightmost non-zero exit code,
or 0 if all stages succeeded. One-line change once the per-stage exit codes
are already collected (they will be, since the wait loop reaps all PIDs).

### 10.3 Spawn failure mid-pipeline

If `cmd2` fails to spawn (image not found, container_run rejects), the
pipeline construction is in a partially-built state: `cmd1` may have already
started.

Decision: shell waits for `cmd1` anyway — it'll see SIGPIPE on its first
write because no reader exists for `pipe1` (we close `pipe1`'s read_token
since `cmd2` never accepted it). Reports the *spawn failure* as the
pipeline's exit status (e.g., `127 — command not found`). POSIX-aligned.

### 10.4 Shell crash mid-pipeline

If the shell process itself dies, procmgr's existing process-exit cleanup
walks the shell's pipe table, closes everything, propagates EOF/SIGPIPE to
children. Children die naturally. No orphan pipes.

### 10.5 Pipe `pipe_id` reuse

`pipe_id` is a procmgr-side index into a vector. Reused after close.
Idempotent close means double-close is fine; using a stale `pipe_id` (i.e.,
one that's been closed and possibly re-used by someone else) is *not* fine
but is detectable: each pipe entry has a `generation` counter bumped on
close, and `pipe_id` is split into `(index, generation)`. Stale-id close
returns `Error::InvalidArgument`.

### 10.6 Bounded queue overflow → writer blocks

Writer's `send_with_retry` already loops on `Error::WouldBlock` (today's
backpressure-by-spin behavior). A pipeline writer producing faster than the
reader drains will spin in user-space until queue space frees. Acceptable for
this round; SharedRing migration would replace with a proper notify wakeup
on space-available.

### 10.7 RAII in `PipelineExecutor`

`PipelineExecutor` holds a `Vec<PipeId>` of allocated pipes. `Drop` impl
closes any not-yet-explicitly-closed ones — guarantees no leak even if
spawn-mid-pipeline errors out.

### 10.8 Double-close at fd_table

`close(fd)` on a closed fd_table slot returns `EBADF` (today's behavior). No
new edge case.

### 10.9 What if reader never starts at all?

Writer spins forever in `send_with_retry`. Same problem exists today for TTY
writes when nobody is reading. Mitigation: signal-interrupt the retry loop —
if SIGPIPE arrives mid-spin, abort and surface EPIPE. (Spec'd as a libcluu
follow-up.)

### 10.10 Zombie reaping order

Shell tracks N child PIDs, waits for each. Order doesn't matter — procmgr
exit notifications come in any order. The wait loop just collects until all
PIDs are reaped. Existing job-control machinery already handles this.

## 11. Testing plan

### 11.1 Unit tests (libcluu-side, no QEMU)

In `userspace/libcluu/src/posix/pipe.rs#[cfg(test)]`:

- `pipe_returns_two_distinct_fds` — calling `pipe()` returns fd values that
  differ and are valid fd_table slots.
- `pipe_close_on_one_side_keeps_other` — close write side, read side still
  callable (will see EOF).

### 11.2 Procmgr-side integration test

A tiny test binary `userspace/pipeprobe/` that:

- Calls `pipe()`, gets `[r, w]`.
- Writes `b"hello\n"` to `w`.
- Reads up to 16 bytes from `r`, asserts content match.
- Closes both, exits 0.

Wired as a harness case `l2_pipe_smoke` (analogous to existing `argvprobe`
case). Proves the procmgr API + libcluu glue work in isolation, without any
shell involvement.

### 11.3 Shell-level harness cases

In `scripts/harness_cases.conf`:

| Case | Command | Asserts |
|---|---|---|
| `l2_pipe_basic` | `cat /etc/motd \| head -3` | first 3 lines of motd appear in capture |
| `l2_pipe_three` | `cat /etc/motd \| grep CLUU \| head -1` | one matching line, the right one |
| `l2_pipe_eof` | `echo hi \| cat` (echo dies first, cat sees EOF) | cat exits 0, output is "hi" |
| `l2_pipe_sigpipe` | `yes \| head -3` | head outputs 3 lines; yes is reaped (no infinite loop) |
| `l2_pipe_status_last` | `false \| true` then `echo $?` | prints `0` |
| `l2_pipe_status_last2` | `true \| false` then `echo $?` | prints `1` |
| `l2_pipe_builtin_left` | `echo foo \| /bin/cat` | "foo" reaches cat's stdout, prints to TTY |
| `l2_redir_stderr_file` | `nonexist 2>/tmp/err && cat /tmp/err` | error message captured in file |
| `l2_redir_stderr_dup` | `nonexist 2>&1 \| grep "not found"` | error matched through pipe |

### 11.4 Migration regression sweeps

After each migrated builtin, run the full harness suite and any case that
referenced the old builtin name (`l2_argv` for `cat`, `l2_owner_deny` for
`ls`, etc.). Don't break what works.

### 11.5 SLO check

The harness already has SLO sweeps (`scripts/harness_slo_sweep.sh`). Add a
single sweep run after pipes land to confirm spawn-time and steady-state RSS
haven't regressed by more than 5%. (Three new binaries + one new procmgr op
should be a small effect.)

## 12. Acceptance criteria

The pipes work is "done" when all of the following hold:

1. `cat /etc/motd | grep CLUU | head -3` typed at the interactive shell
   produces the expected three filtered lines on the TTY.
2. `yes | head -5` terminates promptly (no infinite spin).
3. `cmd 2>&1 | grep err` correctly funnels stderr through the pipe.
4. `cmd 2> /tmp/err` writes stderr to a VFS file.
5. `false | true; echo $?` prints `0`. `true | false; echo $?` prints `1`.
6. The full harness matrix is green (including the new `l2_pipe_*` and
   `l2_redir_*` cases). No SLO regression beyond 5%.
7. `CatBuiltin`, `LsBuiltin`, `PsBuiltin`, `TouchBuiltin` are deleted from
   `commands.rs`. `/bin/cat`, `/bin/ls`, `/bin/ps`, `/bin/touch`,
   `/bin/grep`, `/bin/head`, `/bin/wc` exist as real binaries.

## 13. Out of scope / future work

- **SharedRing-backed pipe upgrade.** Drop-in replacement behind the same
  `pipe(2)` API: replace endpoint-as-buffer with explicit ring + notify
  endpoints. Improves bulk throughput. File as a follow-up issue once the
  current design is in production and we have profile data showing it's
  worth doing.
- **`set -o pipefail`.** One-line change once per-stage statuses are
  collected (they will be).
- **Named FIFOs (`mkfifo`).** Reuse pipe endpoint mechanism, register the
  endpoint pair with VFS as an inode kind. Separate spec.
- **Privileged binary migration** (`su`, `sudo`, `container`, `poweroff`,
  `reboot` → `/bin/` or `/sbin/`). Separate cleanup round.
- **Test scaffolding namespace** (`jobchurn`, `mapfail`, `ext2*`, etc.):
  move to `__harness_*` prefix, register only in test builds. Separate
  cleanup round.
- **Process substitution `<(cmd)`**, **here-documents `<<EOF`**,
  **here-strings `<<<`**, **`&>`**: would need cluu_lang grammar
  extensions. Separate spec when there's a use case.
- **Non-blocking pipe fds and `O_NONBLOCK`**: micropython's asyncio may
  eventually want it. Add when there's a concrete user.

## 14. References

- `crates/cluu_lang/src/cluu.pest` — the pipe operator is already in the
  grammar.
- `crates/cluu_lang/src/ast.rs` — `Pipeline { commands: Vec<Command> }`.
- `userspace/shell/src/commands.rs:471` — `flatten_simple_command_from_stmt`,
  the function that today returns `None` for multi-command pipelines.
- `userspace/libcluu/src/fd_table.rs` — fd → endpoint mapping with `FdKind`.
- `userspace/libcluu/src/posix/file.rs:370` — `write_tty`, the existing
  TTY_WRITE_LABEL send path that pipe-write reuses.
- `userspace/libcluu/src/ipc.rs:175` — `SharedRingHeader`, the existing
  ring abstraction that the future SharedRing pipe upgrade would use.
- `userspace/procmgr/src/main.rs:5110` — `map_process_info_page`, where
  stdin/stdout/stderr tokens get baked into the child process's
  `ProcessInfo` page.
- Public GitHub issue #7 (shell builtins) — context for the
  builtin/binary boundary discussion.
