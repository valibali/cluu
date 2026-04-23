# Shell-A — cwd + filesystem builtins

**Date:** 2026-04-22
**Status:** Design approved, pending implementation plan
**Part of:** Phase 1 (Shell usability) — first of three slices (A, B, C) that together satisfy the ROADMAP Phase 1 exit criteria
**Supersedes:** n/a

## Context

CLUU has a custom Rust shell at `userspace/shell/` (3,615 lines) backed by a pest grammar at `crates/cluu_lang/`. The grammar already parses pipelines, redirection, subshells, variable expansion, command substitution, and quoted strings; the executor is behind the grammar in most places. ROADMAP Phase 1 requires `cd`, `pwd`, basic file utilities, pipes, redirection, line editing, history, and tab completion. That full scope is too large for a single spec, so Phase 1 is decomposed into three sub-projects (see *Related specs* below). This spec covers the first slice.

Target end state for the shell overall is (c) from the brainstorming: both interactive daily-driver *and* POSIX-compliant scripting. That full target spans Shell-A through Shell-F; each lands incrementally.

## Scope

### In scope

- **`PARAM_CWD` plumbing**: inherit the parent's current working directory across `posix_spawn`.
- **Shell builtins**: `cd`, `pwd`.
- **New `/bin` binaries**: `mkdir`, `rm` (with `-r`/`-f`), `cp` (files only), `mv` (same-filesystem rename only).
- **Harness coverage**: seven new `l2_*` harness cases covering cwd inheritance and each builtin/binary.

### Explicitly out of scope

- Grammar changes. The existing pest grammar already parses every command form used here.
- `cd -` (switch to OLDPWD) — deferred to Shell-B.
- `cp -r` directory recursion — deferred until a concrete need appears.
- Cross-filesystem `mv` — deferred. Returns an explicit error for now.
- Glob expansion (`rm *.txt`) — deferred to Shell-E. The executor today passes the literal `*.txt` through.
- Tab completion of paths — deferred to Shell-C.
- `grep`, `head`, `tail`, `wc` — deferred to Shell-B, where they pair naturally with pipes.
- Any procmgr-side cwd state. Procmgr is and stays stateless for this attribute.

## Architecture

### cwd ownership: in-process only

The authoritative cwd lives in `libcluu::posix::dir::CWD` — a per-process `spin::Mutex<Option<String>>` that already exists. `chdir()` and `getcwd()` already work against it (`userspace/libcluu/src/posix/dir.rs:153+`). Shell-A does not change that ownership model.

Procmgr holds **no** cwd state. It sees the cwd only as a byte slice passing through a spawn request into a child's ProcessInfo page. This matches the project preference that procmgr be a spawn/reap gateway, not a directory service.

### Inheritance across `posix_spawn`

Two new ProcessInfo param slots:

- `PARAM_CWD_OFFSET` — byte offset within the 4 KB ProcessInfo page where the cwd string is stored.
- `PARAM_CWD_LEN` — length of the cwd string in bytes (no trailing NUL; length-prefixed).

Capacity: `CWD_MAX = 1024`. The full info page is 4 KB and already holds argv/envp; 1024 bytes comfortably accommodates realistic cwd strings while leaving room for everything else.

**Producer (parent)**: `libcluu`'s `posix_spawn` wrapper reads the local `CWD` static and passes the string inline in the spawn IPC payload to procmgr. If the string exceeds `CWD_MAX`, it is truncated (see *Edge cases*).

**Relay (procmgr)**: receives the spawn request, copies the cwd bytes verbatim into the child's info page alongside argv/envp, sets `PARAM_CWD_OFFSET` and `PARAM_CWD_LEN`. Does not parse, validate, or retain the value.

**Consumer (child)**: either crt0 (`userspace/newlib/crt0.S`) or `libcluu::runtime::init` (whichever is the cleaner fit — see *Open implementation questions*) reads the param slots and seeds the child's own `CWD` static before `main()` runs. If `PARAM_CWD_LEN == 0` or the slots are unset (e.g. bootstrap processes), the child defaults to `/`, matching today's behavior.

**Post-spawn**: `chdir()` in the child mutates only the child's own `CWD`. The parent does not observe it. Next `posix_spawn` from the child carries the updated value. No cross-process synchronization required.

### Why this design is safe by construction

- **No coherence window**: the value passed to the child is captured at spawn-payload-build time, which happens inside the parent's `posix_spawn` call. There is no asynchronous notify, no race between `chdir()` and subsequent `spawn()`.
- **No procmgr state growth**: procmgr's working set does not increase per-cwd. Every process carries its own.
- **No new syscalls**: extends the existing spawn IPC payload only.

## Component designs

### `cd` builtin

Location: new `CdBuiltin` struct in `userspace/shell/src/commands.rs`, registered via `DefaultBuiltins`.

- **0 args**: read `HOME` env var via existing `read_env_var` helper in `userspace/shell/src/main.rs`. If present, `chdir(HOME)`. If absent, `chdir("/")`.
- **1 arg**: `chdir(arg)`.
- **≥ 2 args**: print `cd: too many arguments` to stderr, exit 1.
- On `chdir()` failure: print `cd: <path>: <errno_message>` to stderr, store non-zero exit status in a new `CommandContext.last_status: i32` field (added in Shell-A, read by Shell-B's `echo $?`). Successful `cd` sets it to 0.

No caching, no `OLDPWD` tracking, no `~` expansion (Shell-E handles that).

### `pwd` builtin

Location: new `PwdBuiltin` struct in `userspace/shell/src/commands.rs`.

- No args. Any args → print `pwd: too many arguments` to stderr, exit 1.
- Happy path: call `getcwd()`, write bytes followed by `\n` to stdout via `send_with_payload(stdout, TTY_WRITE_LABEL, …)`, exit 0.

### `/bin/mkdir`

Location: new crate `userspace/mkdir/` (`Cargo.toml`, `src/main.rs`), following the pattern of `userspace/cat/`.

- **`mkdir path…`**: for each arg, call `libcluu::fs::mkdir(path, 0o755)`. On failure, print `mkdir: <path>: <errno_message>` to stderr and continue with remaining args. Exit 0 iff all succeeded.
- **`mkdir -p path…`**: for each arg, walk components from the root, creating each intermediate, silently ignoring `EEXIST` on intermediates. `EEXIST` on the final component with `-p` is also ignored. Any other error → reported + non-zero exit.

Flag parsing: single-pass scan before the positional args. `-p` only. Unknown flag → `mkdir: unknown option '<flag>'` to stderr, exit 1.

**Combined short flags** (`-pp` or similar) are not meaningful for mkdir's one-flag set; treated as unknown. For `rm` below, combined forms like `-rf` / `-fR` *are* supported (standard Unix behavior).

### `/bin/rm`

Location: new crate `userspace/rm/`.

- **`rm path…`**: for each arg, `unlink(path)`. If the target is a directory, fail with `rm: <path>: is a directory` (exit non-zero) unless `-r`.
- **`-r`/`-R`**: iterative tree walk. Maintain a `Vec<String>` work stack. For each directory popped, readdir, push child paths, then `rmdir` the directory after children are gone. Post-order, bounded by heap rather than Rust stack.
- **`-f`**: suppress `ENOENT`; do not treat as error for exit status purposes. Does not suppress other errors (permissions, etc.).
- **Hard guard**: if any arg, after canonicalization, resolves to `/`, refuse with `rm: refusing to remove root directory` to stderr and exit 1 *before* processing any other arg. Guard is unconditional — not bypassable by `-rf`.

### `/bin/cp`

Location: new crate `userspace/cp/`.

- **`cp src dst`**: if `dst` exists and `stat(dst).S_ISDIR`, treat as `cp src dst/<basename(src)>`. Otherwise write to `dst` as a filename.
- **`cp src1 src2 … destdir/`**: all sources copied into `destdir`. `destdir` must exist and be a directory; error otherwise.
- **Copy loop**: `open(src, O_RDONLY)` → `open(dst, O_WRONLY | O_CREAT | O_TRUNC, src_mode & 0o777)` → 64 KB `read()`/`write()` loop. Close both on success and on error.
- **Refuse same-path self-copy**: compare canonicalized src and dst; if equal, `cp: '<path>' and '<path>' are the same file`, exit 1.
- No `-r`. Attempting to copy a directory without `-r` → `cp: <path>: is a directory`, exit non-zero.

### `/bin/mv`

Location: new crate `userspace/mv/`.

- **`mv src dst`**: first attempt `rename(src, dst)`. On success, exit 0.
- **`mv src1 src2 … destdir/`** (≥ 3 args): final arg must exist and be a directory; otherwise `mv: target '<dst>' is not a directory`, exit 1. For each source, call `rename(src_i, destdir/<basename(src_i)>)`. Each failure is reported independently; exit status is non-zero if any source failed.
- On `EXDEV` (cross-filesystem) for any source: print `mv: cross-device rename not yet supported` to stderr, exit non-zero. No copy+unlink fallback yet.
- On other errors: report and exit non-zero.

## Edge cases

- **cwd overflow** (> `CWD_MAX` at spawn time): procmgr truncates to `CWD_MAX` bytes, emits a `debug_print` with the parent PID and truncated length, child proceeds with the truncated string. Only triggers under pathologically deep nesting; logged rather than erroring.
- **cwd deleted by another process**: no special handling. The cwd string is just bytes; the next relative `open()` returns `ENOENT` from the VFS. Matches POSIX behavior when a cwd is unlinked. `pwd` continues to print the recorded string.
- **`cd` with no `HOME` and no arg**: `cd` to `/`. Consistent with early Unix behavior where `$HOME` unset degrades to root.
- **Relative-path resolution with `..` past `/`**: `libcluu::posix::dir` already canonicalizes; no new work. Add an existing-behavior assertion in the harness test for `l2_cd`.
- **`rm /` variants**: canonicalization happens before the guard check. `rm /`, `rm ///`, `rm /.`, `rm /../`, all refused identically.
- **`cp a a` (same path)**: refused before opening the destination, to avoid truncating the file to empty.
- **`mkdir -p` with existing file at an intermediate component**: fails with `mkdir: cannot create directory '<path>': File exists` — this is the standard behavior; the `-p` exemption for `EEXIST` applies only when the existing entry is itself a directory.

## Testing

### Harness cases (added to `scripts/harness_cases.conf`)

| Case | Startup command | Pass condition |
|---|---|---|
| `l2_cd` | `cd /etc && pwd` | COM2 capture contains `/etc\n` |
| `l2_cd_inherit` | `cd /tmp && spawn pwdprobe` | output (from `pwdprobe`, see below) contains `/tmp` |
| `l2_mkdir` | `mkdir /tmp/a && mkdir -p /tmp/b/c/d && ls /tmp/b/c` | output contains `d` |
| `l2_rm` | `mkdir /tmp/x && rm -r /tmp/x && ls /tmp` | output does not contain `x` |
| `l2_cp` | `cp /etc/users.toml /tmp/u && cat /tmp/u` | output contains a known substring of `users.toml` |
| `l2_mv` | `mkdir /tmp/mv && cp /etc/users.toml /tmp/mv/a && mv /tmp/mv/a /tmp/mv/b && ls /tmp/mv` | output contains `b` and not `a` |
| `l2_rm_root_refuse` | `rm -rf /` | non-zero exit from `rm`; follow-up `ls /` still shows `etc` |

Each case uses the existing harness pattern: `MARKER_MODE=l2_<name>` with a matching entry in `scripts/harness_case_defaults.sh` setting `SHELL_AUTOSTART_CMD_DEFAULT`.

**New helper: `pwdprobe`** — a ~10-line C program under `userspace/c-programs/` that calls `getcwd(buf, sizeof buf)` and writes the result to stdout. Added to the ext2 image build alongside the existing probes.

### Unit tests

- **`libcluu::boot::serialize_cwd_into_info_page`** (new helper) — round-trip test: serialize a known string, deserialize, assert byte-equal; plus a truncation test at `CWD_MAX + 1`.
- **`libcluu::posix::dir::normalize_path`** — if not already covered, add a test case for `"/foo/../.."` resolving to `/`.
- No unit tests for the shell builtins or the four binaries. Their logic is thin; harness coverage is the real gate. Adding unit tests here would be ceremony without value — revisit if any binary grows real internal logic.

## Open implementation questions (resolve during planning)

1. **crt0 vs `libcluu::runtime::init` for seeding `CWD`**: crt0 runs first and is assembly — reading a param slot and calling a Rust helper is straightforward but crosses an ABI. `libcluu::runtime::init` runs after crt0 and is Rust-native. The planning step should pick based on whichever point actually has `ProcessInfo` mapped and accessible without additional plumbing.
2. **Flag-parsing helper**: each binary hand-rolls flag parsing. If the code ends up repetitive, extract a tiny `libcluu::cli` helper. Decide after writing the first two — don't pre-abstract.
3. **`mkdir` mode bits**: default `0o755` above. If `libcluu::fs::mkdir` doesn't accept a mode today, extend it or hardcode on the VFS side. Planning step should check.

## Related specs

- **Shell-B** (future): executor for `|`, `<`, `>`, `>>`, `2>`, plus grammar additions for `&&`, `||`, `&`, plus `grep`/`head`/`tail`/`wc`/`cat` polish. Unblocks `echo $?`.
- **Shell-C** (future): raw-mode TTY, line editor, history ring, tab completion.
- **Shell-D/E/F** (later phase): POSIX control flow, expansions, scripting polish. Moves toward target (c).

## Follow-ups discovered during implementation

- **CWD trailer applies to two IPC paths, not one.** The plan only wired the
  trailer through `posix_spawn` (libcluu) → `PROCMGR_SPAWN_LABEL`. The shell's
  `spawn`/`spawnbg`/`container run` builtins do *not* call `posix_spawn`; they
  call `PROCMGR_CONTAINER_RUN_LABEL` directly. Without the trailer on that path
  too, `cd /tmp; spawn pwdprobe` would silently see `cwd=/`. Resolved by
  promoting `CWD_MAGIC` to `libcluu::ipc` and emitting the trailer from both
  shell paths via `build_container_run_payload`. Any future spec that touches
  spawn semantics must enumerate both labels.
- **`spawn` and `container run` are two surface commands wrapping the same
  IPC.** `SpawnBuiltin::spawn_process` and `ContainerBuiltin::container_run`
  both build a `PROCMGR_CONTAINER_RUN_LABEL` payload with `name.as_bytes()` and
  differ only in shell-side wrapping (foreground/job tracking vs admin-style
  inline wait). Keeping the `build_container_run_payload` helper shared
  prevents trailer drift, but the duplication is real and predates Shell-A.
  Worth folding into Shell-B or a dedicated shell-cleanup spec — pick one
  surface, deprecate the other.

## Acceptance

Shell-A is complete when:

1. All seven new harness cases pass in `scripts/harness_matrix.sh`.
2. Existing harness matrix remains green (no regressions).
3. A C program spawned from the shell observes the parent's cwd via `getcwd()`.
4. `rm -rf /` cannot damage the root of any mounted filesystem.
