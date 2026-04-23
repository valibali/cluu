# Shell-A Plan 2 — argv plumbing + `/bin/mkdir`, `/bin/rm`, `/bin/cp`, `/bin/mv`

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the four filesystem-utility binaries work end-to-end from the shell. This requires first wiring argv through the `PROCMGR_CONTAINER_RUN_LABEL` IPC path and the Rust userspace `_start`, which is missing infrastructure that Plan 1 left as an open implementation question.

**Architecture:** The `PROCMGR_SPAWN_LABEL` path already carries argv end-to-end (libcluu → procmgr → `ProcessInfo.params[6,7]` → crt0.S). The `PROCMGR_CONTAINER_RUN_LABEL` path — which is what the shell's `spawn` verb uses — currently carries only a container name. We extend that path to transport argv bytes between the container name and the existing CWD trailer, mirroring the posix_spawn wire format. On the child side, we teach libcluu's Rust `_start` to read `argc`/`argv` from the already-populated `ProcessInfo` params (same slots crt0.S reads for C programs) and hand a Rust-ergonomic `libcluu::args()` helper to the four binaries.

**Tech Stack:** Rust (libcluu, procmgr, shell, four new crates), QEMU headless harness, existing pest grammar (no changes).

**Scope boundary:** This plan implements only what Shell-A's spec (`docs/superpowers/specs/2026-04-22-shell-a-design.md`) says is *in-scope* that Plan 1 did not ship. Bare-command dispatch (typing `mkdir` without `spawn`) is explicitly deferred to Shell-A.5 per spec follow-up #3; harness cases here use `spawn <cmd> <args>` syntax and will be bulk-rewritten when Shell-A.5 lands.

---

## Background for the implementer

- Plan 1 (`docs/superpowers/plans/2026-04-22-shell-a-cwd-cd-pwd.md`) shipped cwd inheritance, `cd`, `pwd`, and the CWD magic trailer on *both* IPC paths (`PROCMGR_SPAWN_LABEL` and `PROCMGR_CONTAINER_RUN_LABEL`). It did **not** wire argv through `CONTAINER_RUN`, and it did **not** teach Rust `_start` to read argc/argv. This plan closes both gaps.
- `posix_spawn` (libcluu `userspace/libcluu/src/posix/process.rs:350-497`) is the reference wire-format implementation. Its payload is `[path\0][argv[0]\0]...[env[0]\0]...[FDAC][cwd_bytes][u32 cwd_len LE][u32 CWD_MAGIC LE]`, with `argc` in `msg.words[1]` and `fdac_offset` in `msg.words[2]`. procmgr's `handle_spawn_message` decodes this at `userspace/procmgr/src/main.rs:3228-3407`.
- `spawn_service_with_env` → `map_process_info_page` (`userspace/procmgr/src/main.rs:4968+`) *already* accepts `argv_payload: &[u8]` and `argc: usize` and writes them into the child's ProcessInfo page with `params[PARAM_ARGC] = argc`, `params[PARAM_ARGV_OFFSET] = argv_data_offset`. We do **not** touch `map_process_info_page` — it's already correct; we just pass non-empty argv through the container_run path.
- The named constants are in `userspace/libcluu/src/boot.rs`: `PARAM_ARGC = 6`, `PARAM_ARGV_OFFSET = 7`, `PARAM_ENVC = 8`, `PARAM_ENV_OFFSET = 9`, `PARAM_CWD_OFFSET = 10`, `PARAM_CWD_LEN = 11`. `PAGE_SIZE = 4096` in `userspace/libcluu/src/mem.rs:10`. Usable payload after ProcessInfo struct is ~3592 bytes.
- The pest grammar (`crates/cluu_lang/src/cluu.pest`) already parses each whitespace-separated word as a separate `cmd_item`. `spawn /bin/mkdir /tmp/a` reaches `BuiltinCommand::run` as `args = ["/bin/mkdir", "/tmp/a"]`. The grammar needs no changes; only `SpawnBuiltin`'s argument handling does.
- crt0.S (`userspace/newlib/crt0.S`) already reads `params[6]` and `params[7]` and builds argv[] on the stack for C programs. Rust `_start` at `userspace/libcluu/src/runtime.rs:66` declares `extern "C" { fn main() -> i32; }` and calls main with no arguments. We replace that with a Rust-native decoder that populates a `libcluu::args()` accessor *without* changing any binary's `main()` signature — this avoids a codebase-wide ripple.
- Harness markers that must appear in the COM2 log are emitted via `libcluu::debug_print`, not via TTY writes. COM2 captures debug_print; TTY writes go to a different serial channel which is not the harness marker source. Every new binary must call `debug_print` with a stable marker prefix after each successful or refused operation.
- Read the design spec first: `docs/superpowers/specs/2026-04-22-shell-a-design.md`. Plan 1's follow-ups section (bottom of spec) notes the two-IPC-paths issue and the Shell-A.5 deferral — re-read it before starting.
- Build: `cargo xtask build` after new crates are added to the xtask manifest list at `xtask/src/main.rs:2242-2260`.
- Test: `scripts/harness_suite.sh --case <case_name>` runs a single case. `scripts/harness_suite.sh` without args runs the full matrix. **Important:** if a stray QEMU is running, a build or case run will fail with "Failed to get write lock" — `pkill -9 qemu-system-x86` first.

---

## File structure

**Create:**
- `userspace/libcluu/src/args.rs` — decodes argc/argv from `ProcessInfo.params[PARAM_ARGC/PARAM_ARGV_OFFSET]` into a `Vec<String>`.
- `userspace/mkdir/Cargo.toml`, `userspace/mkdir/src/main.rs`
- `userspace/rm/Cargo.toml`, `userspace/rm/src/main.rs`
- `userspace/cp/Cargo.toml`, `userspace/cp/src/main.rs`
- `userspace/mv/Cargo.toml`, `userspace/mv/src/main.rs`
- `userspace/argvprobe/Cargo.toml`, `userspace/argvprobe/src/main.rs` — tiny Rust probe that echoes argv via `debug_print`; used by `l2_argv` to verify argv plumbing end-to-end before the four real binaries land.
- `containers/mkdir/Cluufile`, `containers/rm/Cluufile`, `containers/cp/Cluufile`, `containers/mv/Cluufile`, `containers/argvprobe/Cluufile`

**Modify:**
- `userspace/libcluu/src/lib.rs` — `pub mod args;` export.
- `userspace/libcluu/src/ipc.rs` — promote `build_container_run_payload` to accept an argv slice (the shell currently owns this; we move the argv-aware variant to libcluu so it can be called by anyone who spawns via CONTAINER_RUN).
- `userspace/libcluu/src/runtime.rs:66-104` — read argc/argv from ProcessInfo and populate the new `args` module's state before calling `main()`.
- `userspace/shell/src/commands.rs` — `SpawnBuiltin::run`, `parse_spawn_args`, `spawn_process`, `container_run`, `build_container_run_payload` all need argv-aware variants.
- `userspace/procmgr/src/main.rs` — `handle_container_run` (line 4175+) must parse argv from the payload and thread it through `spawn_service_with_env` to `map_process_info_page`.
- `xtask/src/main.rs:2242-2260` — add the five new Rust crates to the manifest list.
- `scripts/harness_cases.conf` — six new cases (`l2_argv`, `l2_mkdir`, `l2_rm`, `l2_cp`, `l2_mv`, `l2_rm_root_refuse`).
- `scripts/harness_case_defaults.sh` — six matching `MARKER_MODE` branches.
- `scripts/harness_run.sh` — six matching `required_markers` branches.

---

## Wire-format reference (read once, return as needed)

The `CONTAINER_RUN` payload, post-Plan-2, is:

```
[container_name_bytes (no NUL)]
[argv[0]\0][argv[1]\0]...[argv[argc-1]\0]     <- argv block, omitted if argc == 0
[cwd_bytes]                                    <- CWD trailer (Plan 1)
[u32 cwd_len LE]
[u32 CWD_MAGIC LE = 0x20445743]                <- "CWD "
```

Message words:
- `msg.tag.label = PROCMGR_CONTAINER_RUN_LABEL`
- `msg.tag.words = 4` (was 3)
- `msg.words[0] = payload.len()`
- `msg.words[1] = notify_endpoint`
- `msg.words[2] = 0` (fdac_offset, unused on this path)
- `msg.words[3] = argc`                       **← NEW SLOT**

argv is located between the last byte of the container name and the start of the CWD trailer. The name has no NUL terminator on this path (unlike `SPAWN`'s `path\0`); procmgr already trims name bytes by locating the FDAC/param boundary. We extend that boundary logic to also recognize argv: when `argc > 0`, the name ends at the first NUL byte in the payload. When `argc == 0`, the name ends at the start of the CWD trailer (today's behavior).

This asymmetry (SPAWN has `path\0`; CONTAINER_RUN has `name` then `argv[0]\0`) is intentional to avoid touching Plan-1 wire-format for the empty-argv case, which every existing `spawn X` call will exercise. When `argc > 0`, name and argv share a contiguous "strings" block with NUL as separator. When `argc == 0`, name is raw bytes followed directly by the CWD trailer — byte-for-byte identical to Plan 1 output.

---

## Task 1: Add `libcluu::args` module — ProcessInfo argv decoder

This is pure new code and can ship independently. It's used by `runtime::_start` (Task 4) and by every new binary.

**Files:**
- Create: `userspace/libcluu/src/args.rs`
- Modify: `userspace/libcluu/src/lib.rs`

- [ ] **Step 1: Create the module.**

Create `userspace/libcluu/src/args.rs`:

```rust
//! Decode `argv` from `ProcessInfo.params[PARAM_ARGC / PARAM_ARGV_OFFSET]`.
//!
//! The procmgr writes argv bytes contiguously into the child's ProcessInfo
//! page (at `argv_data_offset`), each string NUL-terminated. `params[6]` holds
//! `argc` and `params[7]` holds the byte offset within the 4 KB page. This
//! module decodes that into an owned `Vec<String>`, called once from
//! `runtime::_start` and cached.
//!
//! C programs use crt0.S for the same decode; this module is the Rust
//! equivalent and shares the wire format verbatim.

use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

use crate::boot::{process_info, PARAM_ARGC, PARAM_ARGV_OFFSET, PROCESS_INFO_ADDR};
use crate::mem::PAGE_SIZE;

static ARGS: Mutex<Option<Vec<String>>> = Mutex::new(None);

/// Populate the cached argv list from ProcessInfo. Called by `_start` before
/// `main()` runs; safe to call once per process.
pub fn init() {
    let mut slot = ARGS.lock();
    if slot.is_some() {
        return;
    }
    *slot = Some(decode_from_process_info());
}

/// Owned copy of the process's argv, empty if none.
pub fn args() -> Vec<String> {
    ARGS.lock().clone().unwrap_or_default()
}

/// Decode argv bytes from the ProcessInfo page. Returns `Vec<String>` on
/// success, empty Vec on any failure (unmapped page, bogus offsets, non-UTF-8).
///
/// Safety: reads from `PROCESS_INFO_ADDR`'s page. This page is always mapped
/// read-only during the process's lifetime (procmgr guarantees this before
/// jumping to `_start`). Bounds-check every byte offset against `PAGE_SIZE`.
fn decode_from_process_info() -> Vec<String> {
    let info = process_info();
    let argc = info.params[PARAM_ARGC] as usize;
    let argv_offset = info.params[PARAM_ARGV_OFFSET] as usize;
    if argc == 0 || argv_offset == 0 {
        return Vec::new();
    }

    let page_base = PROCESS_INFO_ADDR & !(PAGE_SIZE - 1);
    let page_end = page_base + PAGE_SIZE;
    let mut cursor = page_base + argv_offset;

    let mut out = Vec::with_capacity(argc);
    for _ in 0..argc {
        if cursor >= page_end {
            break;
        }
        // Scan up to the next NUL, bounded by page end.
        let mut len = 0usize;
        while cursor + len < page_end {
            // SAFETY: bounds-checked above against page_end.
            let byte = unsafe { *((cursor + len) as *const u8) };
            if byte == 0 {
                break;
            }
            len += 1;
        }
        if cursor + len >= page_end {
            // Unterminated string — give up rather than read past the page.
            break;
        }
        // SAFETY: bounds-checked; we've scanned `len` in-bounds bytes.
        let slice = unsafe { core::slice::from_raw_parts(cursor as *const u8, len) };
        match core::str::from_utf8(slice) {
            Ok(s) => out.push(String::from(s)),
            Err(_) => {
                out.push(String::new());
            }
        }
        cursor += len + 1; // step past the NUL
    }
    out
}
```

- [ ] **Step 2: Export the module.**

In `userspace/libcluu/src/lib.rs`, add near the other `pub mod` declarations:

```rust
pub mod args;
```

Place it alphabetically if the file is alpha-ordered, otherwise next to `pub mod allocator;`.

- [ ] **Step 3: Build check.**

Run: `cargo check --manifest-path userspace/libcluu/Cargo.toml --target triplets/x86_64-cluu-user.json -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem`
Expected: clean compile. `args::args()` is unused for now — harmless.

- [ ] **Step 4: Commit.**

```bash
git add userspace/libcluu/src/args.rs userspace/libcluu/src/lib.rs
git commit -m "libcluu: add args module to decode argv from ProcessInfo params"
```

---

## Task 2: Rust `_start` calls `args::init()` before `main()`

The module from Task 1 populates nothing until `init()` is called. The only safe place to call it is `_start`, after `allocator::init()` runs but before `main()` is entered.

**Files:**
- Modify: `userspace/libcluu/src/runtime.rs:66-104`

- [ ] **Step 1: Add the init call.**

In `userspace/libcluu/src/runtime.rs`, inside the `#[cfg(feature = "posix")]` block at lines 76-82, append an `args::init()` call after `init_env()`:

```rust
    #[cfg(feature = "posix")]
    {
        crate::posix::init_tls();
        crate::fd_table::init_stdio();
        let _ = crate::registry::init("app");
        crate::posix::init_cwd();
        crate::posix::init_env();
        crate::args::init();
    }
```

For non-`posix` builds, also add an `args::init()` call outside the cfg block. Insert immediately after the closing brace of the `#[cfg(feature = "posix")]` block, before `let exit_code = unsafe { main() };`:

```rust
    #[cfg(not(feature = "posix"))]
    crate::args::init();
```

The two-flavor init keeps the no-posix build path (used by some bare-IPC services) from being broken.

- [ ] **Step 2: Build check across profiles.**

Run: `cargo check --manifest-path userspace/libcluu/Cargo.toml --target triplets/x86_64-cluu-user.json -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem`

Also run a full xtask build to make sure no downstream crate breaks:

Run: `cargo xtask build`
Expected: clean build. No behavioral change yet — `args()` just returns an empty Vec because `params[PARAM_ARGC]` is still 0 for every spawn until Tasks 5-6 land.

- [ ] **Step 3: Commit.**

```bash
git add userspace/libcluu/src/runtime.rs
git commit -m "libcluu: call args::init() from _start before main"
```

---

## Task 3: Promote argv-aware payload builder to `libcluu::ipc`

The shell currently owns `build_container_run_payload` (at `userspace/shell/src/commands.rs:1179`). Since procmgr-container-run is used by things other than the shell (future tools), and since the CWD trailer lives in `libcluu::ipc` already (per Plan 1 follow-up), the argv-aware version belongs there too.

**Files:**
- Modify: `userspace/libcluu/src/ipc.rs` — add `build_container_run_payload_with_argv`.
- Modify: `userspace/shell/src/commands.rs:1179-1189` — call the libcluu helper, delete local copy.

- [ ] **Step 1: Find where `CWD_MAGIC` lives in libcluu::ipc.**

Run: `rg -n "CWD_MAGIC|build_container_run_payload" userspace/libcluu/src/ userspace/shell/src/`
Expected: `CWD_MAGIC` in `userspace/libcluu/src/ipc.rs`, `build_container_run_payload` in `userspace/shell/src/commands.rs:1179`.

- [ ] **Step 2: Add the new helper to `libcluu::ipc`.**

Near `CWD_MAGIC` in `userspace/libcluu/src/ipc.rs`, add:

```rust
/// Build a `PROCMGR_CONTAINER_RUN_LABEL` payload with optional argv.
///
/// Wire format:
///   `[name_bytes][argv[0]\0][argv[1]\0]...[cwd_trailer]`
///
/// When `args` is empty, the output is byte-identical to the pre-argv format
/// (name + CWD trailer), preserving backwards compatibility.
///
/// Returns `(payload, argc)`. Callers pass `argc` as `msg.words[3]` so procmgr
/// knows how many NUL-terminated strings follow the name.
pub fn build_container_run_payload_with_argv(name: &str, args: &[&str]) -> (alloc::vec::Vec<u8>, usize) {
    use crate::boot::CWD_MAX;

    let argc = args.len();
    let argv_bytes_est: usize = args.iter().map(|a| a.len() + 1).sum();
    let mut payload = alloc::vec::Vec::with_capacity(name.len() + argv_bytes_est + CWD_MAX + 8);
    payload.extend_from_slice(name.as_bytes());

    for arg in args {
        payload.extend_from_slice(arg.as_bytes());
        payload.push(0); // NUL separator
    }

    let cwd_string = crate::posix::current_dir_string();
    let cwd_bytes = cwd_string.as_bytes();
    let cwd_len = cwd_bytes.len().min(CWD_MAX);
    payload.extend_from_slice(&cwd_bytes[..cwd_len]);
    payload.extend_from_slice(&(cwd_len as u32).to_le_bytes());
    payload.extend_from_slice(&CWD_MAGIC.to_le_bytes());

    (payload, argc)
}
```

- [ ] **Step 3: Update the shell to call through the libcluu helper.**

In `userspace/shell/src/commands.rs:1179-1189`, replace the local `build_container_run_payload` function body with a call to the new libcluu helper. The existing signature stays (`fn build_container_run_payload(name: &str) -> Vec<u8>`) — the shell's zero-arg callers don't yet want argv. Keep this as a thin wrapper so the diff in Task 4 stays focused:

```rust
/// Thin wrapper around `libcluu::ipc::build_container_run_payload_with_argv`
/// for the zero-arg case. Plan 2 Task 4 adds argv-carrying callers.
fn build_container_run_payload(name: &str) -> Vec<u8> {
    libcluu::ipc::build_container_run_payload_with_argv(name, &[]).0
}
```

- [ ] **Step 4: Build check.**

Run: `cargo check --manifest-path userspace/libcluu/Cargo.toml --target triplets/x86_64-cluu-user.json -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem`
Then: `cargo check --manifest-path userspace/shell/Cargo.toml --target triplets/x86_64-cluu-user.json -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem`
Expected: both clean.

- [ ] **Step 5: Sanity regression — `m1_recv` still green.**

Run: `scripts/harness_suite.sh --case m1_recv`
Expected: PASS. Byte-identical wire format for zero-arg spawns; nothing should have changed.

- [ ] **Step 6: Commit.**

```bash
git add userspace/libcluu/src/ipc.rs userspace/shell/src/commands.rs
git commit -m "libcluu: promote argv-aware container_run payload builder; shell delegates"
```

---

## Task 4: Thread argv through `SpawnBuiltin` and `spawn_process`

Now make the shell actually pass args when the user types `spawn /bin/mkdir /tmp/a`. The pest grammar already splits these into separate words; the executor flattens them into `args: &[String]` on `SpawnBuiltin::run`. Today `parse_spawn_args` at `userspace/shell/src/commands.rs:809-840` extracts only the path and priority and silently discards everything after. We change this.

**Files:**
- Modify: `userspace/shell/src/commands.rs:809-840` (parse_spawn_args) and `userspace/shell/src/commands.rs:842-874` (SpawnBuiltin::run) and `userspace/shell/src/commands.rs:1191-1216` (spawn_process).

- [ ] **Step 1: Update `parse_spawn_args` to also return the argv tail.**

In `userspace/shell/src/commands.rs:809-840`, change the signature and body:

```rust
fn parse_spawn_args(args: &[String]) -> Option<(String, usize, ForegroundMode, Vec<String>)> {
    if args.is_empty() {
        return None;
    }
    let mut idx = 0usize;
    let mut mode = ForegroundMode::SignalOnCtrlC;
    let mut mode_explicit = false;
    while idx < args.len() {
        match args[idx].as_str() {
            "-i" | "--interactive" => {
                mode = ForegroundMode::PassCtrlCToChild;
                mode_explicit = true;
                idx += 1;
            }
            "-s" | "--signal" => {
                mode = ForegroundMode::SignalOnCtrlC;
                mode_explicit = true;
                idx += 1;
            }
            _ => break,
        }
    }
    let path = args.get(idx)?.clone();
    idx += 1;

    // Priority: if the next token parses as usize, consume it as priority. Else
    // leave it for argv. This preserves backward compat (`spawn foo 5`) while
    // allowing `spawn foo --help` to pass `--help` as argv[1].
    let priority = match args.get(idx).and_then(|v| v.parse::<usize>().ok()) {
        Some(p) => {
            idx += 1;
            p
        }
        None => DEFAULT_PRIORITY,
    };

    let argv_tail: Vec<String> = args[idx..].to_vec();

    if !mode_explicit {
        mode = infer_foreground_mode(path.as_str());
    }
    Some((path, priority, mode, argv_tail))
}
```

- [ ] **Step 2: Update `SpawnBuiltin::run` to pass the argv tail through.**

In `userspace/shell/src/commands.rs:842-874`, change:

```rust
    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        let Some((path, priority, fg_mode)) = parse_spawn_args(args) else {
            send_with_payload(stdout, TTY_WRITE_LABEL, b"spawn: missing path\n")?;
            return Ok(());
        };
        let spawn = spawn_process(context, path.as_str(), priority)?;
```

to:

```rust
    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        let Some((path, priority, fg_mode, argv_tail)) = parse_spawn_args(args) else {
            send_with_payload(stdout, TTY_WRITE_LABEL, b"spawn: missing path\n")?;
            return Ok(());
        };
        let argv_refs: Vec<&str> = argv_tail.iter().map(|s| s.as_str()).collect();
        let spawn = spawn_process_with_argv(context, path.as_str(), priority, &argv_refs)?;
```

Also update the other `parse_spawn_args` caller at `userspace/shell/src/commands.rs:887` (the `spawnbg` builtin — grep for it to be sure): change to receive four tuple elements and pass `&[]` for argv since `spawnbg` today takes no user-supplied args. Leaving `spawnbg` pattern as:

```rust
        let Some((path, priority, fg_mode, _argv_tail)) = parse_spawn_args(args) else {
            /* ... */
        };
        let spawn = spawn_process_with_argv(context, path.as_str(), priority, &[])?;
```

(`_argv_tail` underscored to silence unused warning. If the user-typed `spawnbg X Y`, we drop Y silently — same as today's behavior. Shell-A.5 rewrites this.)

- [ ] **Step 3: Add the argv-aware `spawn_process_with_argv`, keep `spawn_process` as a thin wrapper.**

In `userspace/shell/src/commands.rs:1191`, rewrite `spawn_process` and add the new variant:

```rust
fn spawn_process(context: &mut CommandContext, name: &str, priority: usize) -> Result<SpawnResult> {
    spawn_process_with_argv(context, name, priority, &[])
}

fn spawn_process_with_argv(
    context: &mut CommandContext,
    name: &str,
    _priority: usize,
    args: &[&str],
) -> Result<SpawnResult> {
    let procmgr_endpoint = context.procmgr_spawn_endpoint()?;
    let (payload, argc) = libcluu::ipc::build_container_run_payload_with_argv(name, args);
    let notify_endpoint = syscall::endpoint_create(process_info().tokens[TOKEN_IPC])?;
    let mut msg = Message::new(PROCMGR_CONTAINER_RUN_LABEL, [0; 6], 4);
    msg.words[0] = payload.len();
    msg.words[1] = notify_endpoint;
    msg.words[2] = 0;            // fdac_offset, unused on this path
    msg.words[3] = argc;         // NEW: argv count
    let mut reply = Message::new(0, [0; 6], 0);
    let _ = debug_print(&format!(
        "shell: container run begin name={} argc={} ep={} notify={}",
        name, argc, procmgr_endpoint, notify_endpoint
    ));
    call_with_payload(procmgr_endpoint, &msg, &payload, &mut reply)?;
    let _ = debug_print(&format!(
        "shell: container run done status={} pid={} stdin={}",
        reply.words[0], reply.words[1], reply.words[4]
    ));
    Ok(SpawnResult {
        procmgr_endpoint,
        notify_endpoint,
        status_word: reply.words[0],
        pid: reply.words[1],
        stdin_endpoint: reply.words[4],
    })
}
```

- [ ] **Step 4: Do the same for `container_run` in the `container` subcommand.**

The `container run X` admin verb at `userspace/shell/src/commands.rs:2771` also calls `build_container_run_payload`. Update it analogously — either call `spawn_process_with_argv` (if it fits the existing block) or compose directly. Keeping it minimal for this plan, pass empty argv; `container run` with user args is out of scope here (can be added in a one-line follow-up if users want it).

At `userspace/shell/src/commands.rs:2779`, change:

```rust
    let payload = build_container_run_payload(name);
```

to (no change to wire bytes — zero-argv is byte-identical via the helper):

```rust
    let (payload, _argc) = libcluu::ipc::build_container_run_payload_with_argv(name, &[]);
```

And change the nearby `Message::new(PROCMGR_CONTAINER_RUN_LABEL, [0; 6], 3)` to `...[0; 6], 4)` with `msg.words[3] = 0` — procmgr needs `msg.tag.words >= 4` to know argc is there (even if zero). Grep for the Message::new call inside `container_run`:

```bash
rg -n "PROCMGR_CONTAINER_RUN_LABEL" userspace/shell/src/commands.rs
```

Update every `PROCMGR_CONTAINER_RUN_LABEL` producer. There should be exactly two (one in `spawn_process_with_argv`, one in `container_run`).

- [ ] **Step 5: Build check.**

Run: `cargo check --manifest-path userspace/shell/Cargo.toml --target triplets/x86_64-cluu-user.json -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem`
Expected: clean.

- [ ] **Step 6: Regression sanity.**

Run: `scripts/harness_suite.sh --case l2_cd_inherit`
Expected: PASS. The zero-argv case (which `spawn pwdprobe` uses) is byte-for-byte identical on the wire and also arrives at procmgr as a 4-word message where `msg.words[3] = 0`. If procmgr's old handler only checks `msg.tag.words >= 3` for the existing fields, it will still decode correctly. Task 5 then teaches it to look at slot 3.

If `l2_cd_inherit` now fails: procmgr is rejecting the 4-word message shape. Roll back the `Message::new` arity bump in `container_run` and `spawn_process_with_argv` — keep `[0; 6], 3)` and only bump to `4` in Task 5 simultaneously with procmgr's extended parser.

- [ ] **Step 7: Commit.**

```bash
git add userspace/shell/src/commands.rs
git commit -m "shell: collect argv tail in spawn/spawnbg and include argc in container_run msg"
```

---

## Task 5: procmgr parses argv from `CONTAINER_RUN` payload

This is the consumer side. `handle_container_run` at `userspace/procmgr/src/main.rs:4175-4228` today discards argv; we teach it to extract `argc` from `msg.words[3]`, slice argv bytes out of the payload, and pass them through `spawn_service_with_env` to `map_process_info_page`.

**Files:**
- Modify: `userspace/procmgr/src/main.rs:4175-4228` (handle_container_run)
- Modify: `userspace/procmgr/src/main.rs:~4175` area for the spawn_service_with_env invocation

- [ ] **Step 1: Read argc from the message.**

Inside `handle_container_run`, just after `split_cwd_trailer(payload)` returns `effective_payload`:

```rust
    let (effective_payload, cwd_bytes) = split_cwd_trailer(payload);

    let fdac_offset = if msg.tag.words >= 3 { msg.words[2] } else { 0 };
    let argc = if msg.tag.words >= 4 { msg.words[3] } else { 0 };
    let param_offset = if msg.tag.words >= 5 { msg.words[3] } else { 0 };
    //                                                    ^ legacy slot
    // NOTE: Old callers used msg.words[3] for "param_offset" (grep for it
    // to confirm). After Plan 2 the shell guarantees `msg.tag.words == 4`
    // and `msg.words[3] = argc`. If some legacy caller uses words==5 with a
    // param_offset, it still works because we check .words >= 5 first.
```

If the existing handler has a `param_offset` read at `msg.words[3]` (Section C.3 of research), preserve it — re-number the new argc slot to avoid collision. **Before starting Task 5, grep `rg -n "msg\\.words\\[3\\]" userspace/procmgr/src/main.rs` and report what's there.** The research report suggested `param_offset` was at slot 3 in an older version; if so, move argc to slot 4 and re-check every shell caller to bump accordingly. This plan assumes slot 3 is the argc home (matching the shell changes in Task 4). If the grep shows otherwise, update both sides.

- [ ] **Step 2: Locate the container name end.**

After the `argc` read, compute where the name ends. When `argc > 0`, the first NUL in the payload is the name/argv boundary; when `argc == 0`, the name runs to the end of `effective_payload` (minus the CWD trailer, which is already stripped):

```rust
    let name_end = if argc > 0 {
        effective_payload
            .iter()
            .position(|b| *b == 0)
            .unwrap_or(effective_payload.len())
    } else if fdac_offset > 0 && fdac_offset <= effective_payload.len() {
        fdac_offset
    } else {
        effective_payload.len()
    };
    let image_name = match core::str::from_utf8(&effective_payload[..name_end]) {
        Ok(s) => s.trim_end_matches('\0').trim(),
        Err(_) => { /* existing error path */ }
    };
```

- [ ] **Step 3: Slice argv bytes.**

Immediately after, slice the argv block: it starts one past the name-terminating NUL (if `argc > 0`) and runs to the end of `effective_payload` (or to `fdac_offset` if FDAC is present).

```rust
    let argv_data = if argc > 0 && name_end + 1 < effective_payload.len() {
        let argv_start = name_end + 1;
        let argv_end = if fdac_offset > argv_start && fdac_offset <= effective_payload.len() {
            fdac_offset
        } else {
            effective_payload.len()
        };
        &effective_payload[argv_start..argv_end]
    } else {
        &[]
    };
```

- [ ] **Step 4: Thread `argv_data` and `argc` through to `spawn_service_with_env`.**

Find the `spawn_service_with_env` call in `handle_container_run` (grep within the function). Today it passes `&[]` and `0` for argv. Change those two to `argv_data` and `argc`:

Before:
```rust
    match self.spawn_service_with_env(
        &image_path,
        priority,
        &[],          // argv_payload
        0,            // argc
        ...
    )
```

After:
```rust
    match self.spawn_service_with_env(
        &image_path,
        priority,
        argv_data,
        argc,
        ...
    )
```

Every other argument stays exactly as today. `spawn_service_with_env` already threads argv through to `map_process_info_page` (confirmed in research Section A.3), which sets `params[PARAM_ARGC]` and `params[PARAM_ARGV_OFFSET]` and copies the bytes into the child's page.

- [ ] **Step 5: Build check.**

Run: `cargo check --manifest-path userspace/procmgr/Cargo.toml --target triplets/x86_64-cluu-user.json -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem`
Expected: clean.

- [ ] **Step 6: Regression sanity.**

Run: `scripts/harness_suite.sh --case l2_cd_inherit`
Expected: PASS. Zero-argv path: name_end falls through the `argc == 0` branch, so name resolution is byte-identical. argv_data is empty, so `map_process_info_page` sees empty argv — matches today's behavior.

- [ ] **Step 7: Commit.**

```bash
git add userspace/procmgr/src/main.rs
git commit -m "procmgr: parse argv from CONTAINER_RUN payload and thread to child info page"
```

---

## Task 6: `argvprobe` Rust binary + `l2_argv` harness case (end-to-end smoke)

Before writing the four real binaries, verify argv plumbing works with a minimal Rust binary that echoes its args via `debug_print`. If this lands green, Tasks 7-14 are mechanically safe.

**Files:**
- Create: `userspace/argvprobe/Cargo.toml`
- Create: `userspace/argvprobe/src/main.rs`
- Create: `containers/argvprobe/Cluufile`
- Modify: `xtask/src/main.rs:2242-2260` (add manifest entry)
- Modify: `scripts/harness_cases.conf`
- Modify: `scripts/harness_case_defaults.sh`
- Modify: `scripts/harness_run.sh`

- [ ] **Step 1: Add the harness case row FIRST (TDD red).**

Append to `scripts/harness_cases.conf`:

```
l2_argv|full|MARKER_MODE=l2_argv TEST_COMMAND_REPEAT=1 RUN_WAIT=15
```

- [ ] **Step 2: Add marker-mode defaults.**

In `scripts/harness_case_defaults.sh`, inside the `case "$MARKER_MODE"` block (alphabetically with other `l2_*`):

```sh
            l2_argv)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="spawn argvprobe hello world"
                ;;
```

- [ ] **Step 3: Add required markers.**

In `scripts/harness_run.sh`, add inside the `case "$MARKER_MODE"` block:

```sh
    l2_argv)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "argvprobe: argc=3"
            "argvprobe: arg0=argvprobe"
            "argvprobe: arg1=hello"
            "argvprobe: arg2=world"
        )
        ;;
```

Note `argc=3` because `argv[0]` is the program name. The shell passes the container name as `argv[0]` by convention (see `build_container_run_payload_with_argv` — the name bytes come first and are the argv[0] source; but actually the shell-side logic must explicitly prepend the name as argv[0] if we want POSIX behavior. **Decision point — check now:** in Task 4 / Task 5 did we arrange for name to become argv[0]?

Re-reading Task 3 Step 2: the payload is `[name_bytes][argv[0]\0]...`. On the procmgr side, name runs from 0 to first NUL; then argv[0] comes from byte `name_end+1`. These are two separate fields; the child's argv[] does NOT include the name.

That makes `argc` in the message equal to the user-supplied count (`["hello", "world"]` → `argc=2`), not `argc=3`. The child's `args()` returns `["hello", "world"]`.

**Adjust the marker list:**

```sh
    l2_argv)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "argvprobe: argc=2"
            "argvprobe: arg0=hello"
            "argvprobe: arg1=world"
        )
        ;;
```

If you want POSIX-style `argv[0] = program_name`, that's a separate decision the shell can make (prepend name before argv in `build_container_run_payload_with_argv`). Defer that for now — keep the wire format minimal: only user-supplied args. The child's `args()` function returns exactly what the user typed after the name.

- [ ] **Step 4: Run the case — expect FAIL (red).**

Run: `scripts/harness_suite.sh --case l2_argv`
Expected: FAIL. `argvprobe` doesn't exist yet; the shell will report "container run failed" or similar. Verify the failure is "image not found" (not a crash).

- [ ] **Step 5: Write the Cargo.toml.**

Create `userspace/argvprobe/Cargo.toml`:

```toml
[package]
name = "cluu-argvprobe"
version = "0.1.0"
edition = "2021"
description = "CLUU argv plumbing smoke test"
authors = ["CLUU Team", "Balazs Valkony"]
license = "MIT"

[dependencies]
libcluu = { path = "../libcluu" }

[[bin]]
name = "argvprobe"
path = "src/main.rs"
```

- [ ] **Step 6: Write the main.**

Create `userspace/argvprobe/src/main.rs`:

```rust
#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = libcluu::args::args();
    let _ = libcluu::debug_print(&format!("argvprobe: argc={}", args.len()));
    for (i, a) in args.iter().enumerate() {
        let _ = libcluu::debug_print(&format!("argvprobe: arg{}={}", i, a));
    }
    0
}
```

- [ ] **Step 7: Write the Cluufile.**

Create `containers/argvprobe/Cluufile`:

```
FROM minimal
PROFILE ipc
BUILD "cargo build --manifest-path userspace/argvprobe/Cargo.toml --target triplets/x86_64-cluu-user.json -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem" target/x86_64-cluu-user/debug/argvprobe.elf /bin/argvprobe
ENTRYPOINT /bin/argvprobe
```

- [ ] **Step 8: Register in xtask manifests list.**

In `xtask/src/main.rs:2242-2260`, add after `"userspace/cat"`:

```rust
    "userspace/argvprobe",
```

Keep alphabetical order within the trailing group if possible; otherwise tail is fine.

- [ ] **Step 9: Full build.**

Run: `cargo xtask build`
Expected: clean build. `target/x86_64-cluu-user/debug/argvprobe.elf` exists.

- [ ] **Step 10: Run the case — expect PASS (green).**

Run: `scripts/harness_suite.sh --case l2_argv`
Expected: PASS.

If FAIL with `argvprobe: argc=0`: the plumbing is broken somewhere. Debug order:
1. Grep procmgr serial log for `procmgr: container run 'argvprobe'` — confirms the request arrived.
2. Add a `debug_print(&format!("procmgr: argv_data={} bytes argc={}", argv_data.len(), argc))` inside `handle_container_run` after Task 5 Step 3's slice. Rebuild and rerun. Expected: `argv_data=12 bytes argc=2` (`"hello\0world\0"` = 12 bytes).
3. If procmgr sees argc=0: the shell didn't send it. Add `debug_print(&format!("shell: spawn_argc={}", argc))` inside `spawn_process_with_argv`. Rebuild and rerun.
4. If procmgr sees argc=2 but argvprobe sees 0: `args::init()` isn't running, or it's reading the wrong params slot. Add `debug_print(&format!("argvprobe: params[6]={} params[7]={}", info.params[6], info.params[7]))` at the very top of argvprobe's main. If both are non-zero, the decoder has a bug; if both are zero, `map_process_info_page` didn't write them.

Remove any added debug prints before committing.

- [ ] **Step 11: Commit.**

```bash
git add userspace/argvprobe/ containers/argvprobe/ xtask/src/main.rs scripts/harness_cases.conf scripts/harness_case_defaults.sh scripts/harness_run.sh
git commit -m "argvprobe: verify argv plumbing end-to-end via l2_argv harness"
```

---

## Task 7: Failing harness case `l2_mkdir` (TDD red)

**Files:**
- Modify: `scripts/harness_cases.conf`
- Modify: `scripts/harness_case_defaults.sh`
- Modify: `scripts/harness_run.sh`

- [ ] **Step 1: Add the case row.**

Append to `scripts/harness_cases.conf`:

```
l2_mkdir|full|MARKER_MODE=l2_mkdir TEST_COMMAND_REPEAT=1 RUN_WAIT=18
```

- [ ] **Step 2: Marker-mode defaults.**

In `scripts/harness_case_defaults.sh`:

```sh
            l2_mkdir)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="spawn mkdir /tmp/a; spawn mkdir -p /tmp/b/c/d"
                ;;
```

- [ ] **Step 3: Required markers.**

In `scripts/harness_run.sh`:

```sh
    l2_mkdir)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "mkdir: ok /tmp/a"
            "mkdir: ok /tmp/b/c/d"
        )
        ;;
```

- [ ] **Step 4: Run case — expect FAIL.**

Run: `scripts/harness_suite.sh --case l2_mkdir`
Expected: FAIL. `mkdir` container doesn't exist; procmgr reports image-not-found.

- [ ] **Step 5: Commit.**

```bash
git add scripts/harness_cases.conf scripts/harness_case_defaults.sh scripts/harness_run.sh
git commit -m "harness: add failing l2_mkdir case"
```

---

## Task 8: `/bin/mkdir` implementation

**Files:**
- Create: `userspace/mkdir/Cargo.toml`, `userspace/mkdir/src/main.rs`
- Create: `containers/mkdir/Cluufile`
- Modify: `xtask/src/main.rs:2242-2260`

- [ ] **Step 1: Cargo.toml.**

Create `userspace/mkdir/Cargo.toml`:

```toml
[package]
name = "cluu-mkdir"
version = "0.1.0"
edition = "2021"
description = "CLUU mkdir utility"
authors = ["CLUU Team", "Balazs Valkony"]
license = "MIT"

[dependencies]
libcluu = { path = "../libcluu" }

[[bin]]
name = "mkdir"
path = "src/main.rs"
```

- [ ] **Step 2: main.rs.**

Create `userspace/mkdir/src/main.rs`:

```rust
#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use libcluu::fs::client::VfsClient;
use libcluu::{debug_print, registry};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = libcluu::args::args();
    let (flags, positional) = parse_flags(&args);
    if positional.is_empty() {
        let _ = debug_print("mkdir: missing operand");
        return 1;
    }

    let Ok(vfs_endpoint) = registry::subscribe_output("vfs", "main") else {
        let _ = debug_print("mkdir: vfs unavailable");
        return 1;
    };
    let client_id = registry::control_endpoint();
    let client = VfsClient::new(vfs_endpoint, client_id);

    let mut exit_code = 0i32;
    for path in &positional {
        let resolved = libcluu::posix::dir::resolve_path(path);
        let result = if flags.p {
            mkdir_p(&client, &resolved)
        } else {
            client.mkdir(&resolved, 0o755).map_err(|e| format!("{:?}", e))
        };
        match result {
            Ok(()) => {
                let _ = debug_print(&format!("mkdir: ok {}", resolved));
            }
            Err(err) => {
                let _ = debug_print(&format!("mkdir: {}: {}", resolved, err));
                exit_code = 1;
            }
        }
    }
    exit_code
}

struct Flags {
    p: bool,
}

fn parse_flags(args: &[String]) -> (Flags, Vec<String>) {
    let mut flags = Flags { p: false };
    let mut positional = Vec::new();
    for arg in args {
        if arg == "-p" {
            flags.p = true;
        } else if arg.starts_with('-') && arg.len() > 1 {
            let _ = debug_print(&format!("mkdir: unknown option '{}'", arg));
        } else {
            positional.push(arg.clone());
        }
    }
    (flags, positional)
}

fn mkdir_p(client: &VfsClient, path: &str) -> Result<(), String> {
    // Walk components; `mkdir` each one, ignoring EEXIST on directories.
    let trimmed = path.trim_start_matches('/');
    if trimmed.is_empty() {
        return Ok(());
    }
    let mut current = String::new();
    for component in trimmed.split('/') {
        if component.is_empty() {
            continue;
        }
        current.push('/');
        current.push_str(component);
        match client.mkdir(&current, 0o755) {
            Ok(()) => {}
            Err(e) => {
                // Distinguish EEXIST-on-dir (OK) vs other errors.
                match client.stat(&current) {
                    Ok(info) if info.mode & 0o170000 == 0o040000 => {
                        // It's already a directory — fine for -p.
                    }
                    _ => return Err(format!("{:?}", e)),
                }
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 3: Cluufile.**

Create `containers/mkdir/Cluufile`:

```
FROM minimal
PROFILE ipc vfs registry
BUILD "cargo build --manifest-path userspace/mkdir/Cargo.toml --target triplets/x86_64-cluu-user.json -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem" target/x86_64-cluu-user/debug/mkdir.elf /bin/mkdir
ENTRYPOINT /bin/mkdir
```

- [ ] **Step 4: xtask manifest list.**

In `xtask/src/main.rs:2242-2260`, add:

```rust
    "userspace/mkdir",
```

- [ ] **Step 5: Build + test.**

Run: `cargo xtask build`
Then: `scripts/harness_suite.sh --case l2_mkdir`
Expected: PASS.

- [ ] **Step 6: Commit.**

```bash
git add userspace/mkdir/ containers/mkdir/ xtask/src/main.rs
git commit -m "mkdir: implement /bin/mkdir with -p flag"
```

---

## Task 9: Failing harness case `l2_rm` (TDD red)

**Files:**
- Modify: `scripts/harness_cases.conf`, `scripts/harness_case_defaults.sh`, `scripts/harness_run.sh`

- [ ] **Step 1: Case row.**

Append to `scripts/harness_cases.conf`:

```
l2_rm|full|MARKER_MODE=l2_rm TEST_COMMAND_REPEAT=1 RUN_WAIT=18
```

- [ ] **Step 2: Marker-mode defaults.**

```sh
            l2_rm)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="spawn mkdir /tmp/rmtest; spawn mkdir /tmp/rmtest/inner; spawn rm -r /tmp/rmtest"
                ;;
```

- [ ] **Step 3: Required markers.**

```sh
    l2_rm)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "rm: ok /tmp/rmtest"
        )
        ;;
```

- [ ] **Step 4: Run case — expect FAIL.**

Run: `scripts/harness_suite.sh --case l2_rm`
Expected: FAIL.

- [ ] **Step 5: Commit.**

```bash
git add scripts/harness_cases.conf scripts/harness_case_defaults.sh scripts/harness_run.sh
git commit -m "harness: add failing l2_rm case"
```

---

## Task 10: `/bin/rm` implementation

**Files:**
- Create: `userspace/rm/Cargo.toml`, `userspace/rm/src/main.rs`
- Create: `containers/rm/Cluufile`
- Modify: `xtask/src/main.rs:2242-2260`

- [ ] **Step 1: Cargo.toml.**

Create `userspace/rm/Cargo.toml`:

```toml
[package]
name = "cluu-rm"
version = "0.1.0"
edition = "2021"
description = "CLUU rm utility"
authors = ["CLUU Team", "Balazs Valkony"]
license = "MIT"

[dependencies]
libcluu = { path = "../libcluu" }

[[bin]]
name = "rm"
path = "src/main.rs"
```

- [ ] **Step 2: main.rs.**

Create `userspace/rm/src/main.rs`:

```rust
#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use libcluu::fs::client::VfsClient;
use libcluu::{debug_print, registry};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = libcluu::args::args();
    let (flags, positional) = parse_flags(&args);
    if positional.is_empty() {
        let _ = debug_print("rm: missing operand");
        return 1;
    }

    // Hard guard: refuse root removal before any processing.
    for arg in &positional {
        let resolved = libcluu::posix::dir::resolve_path(arg);
        if resolved == "/" || resolved.is_empty() {
            let _ = debug_print("rm: refusing to remove root directory");
            return 1;
        }
    }

    let Ok(vfs_endpoint) = registry::subscribe_output("vfs", "main") else {
        let _ = debug_print("rm: vfs unavailable");
        return 1;
    };
    let client_id = registry::control_endpoint();
    let client = VfsClient::new(vfs_endpoint, client_id);

    let mut exit_code = 0i32;
    for path in &positional {
        let resolved = libcluu::posix::dir::resolve_path(path);
        match remove_entry(&client, &resolved, &flags) {
            Ok(()) => {
                let _ = debug_print(&format!("rm: ok {}", resolved));
            }
            Err(err) => {
                let _ = debug_print(&format!("rm: {}: {}", resolved, err));
                exit_code = 1;
            }
        }
    }
    exit_code
}

struct Flags {
    r: bool,
    f: bool,
}

fn parse_flags(args: &[String]) -> (Flags, Vec<String>) {
    let mut flags = Flags { r: false, f: false };
    let mut positional = Vec::new();
    for arg in args {
        if let Some(rest) = arg.strip_prefix('-') {
            if rest.is_empty() {
                positional.push(arg.clone());
                continue;
            }
            for ch in rest.chars() {
                match ch {
                    'r' | 'R' => flags.r = true,
                    'f' => flags.f = true,
                    other => {
                        let _ = debug_print(&format!("rm: unknown option '-{}'", other));
                    }
                }
            }
        } else {
            positional.push(arg.clone());
        }
    }
    (flags, positional)
}

fn remove_entry(client: &VfsClient, path: &str, flags: &Flags) -> Result<(), String> {
    let info = match client.stat(path) {
        Ok(v) => v,
        Err(e) => {
            // `-f` suppresses ENOENT; other errors still surface.
            let s = format!("{:?}", e);
            if flags.f && s.contains("NotFound") {
                return Ok(());
            }
            return Err(s);
        }
    };
    let is_dir = info.mode & 0o170000 == 0o040000;
    if is_dir {
        if !flags.r {
            return Err(String::from("is a directory"));
        }
        remove_tree(client, path)
    } else {
        client.unlink(path).map_err(|e| format!("{:?}", e))
    }
}

fn remove_tree(client: &VfsClient, root: &str) -> Result<(), String> {
    // Post-order iterative removal. Work stack holds dirs pending rmdir.
    let mut pending: Vec<String> = alloc::vec![String::from(root)];
    let mut rmdir_order: Vec<String> = Vec::new();
    while let Some(dir) = pending.pop() {
        rmdir_order.push(dir.clone());
        let entries = client.readdir(&dir).map_err(|e| format!("{:?}", e))?;
        for entry in entries {
            if entry.name == "." || entry.name == ".." {
                continue;
            }
            let child = if dir.ends_with('/') {
                format!("{}{}", dir, entry.name)
            } else {
                format!("{}/{}", dir, entry.name)
            };
            if entry.is_dir {
                pending.push(child);
            } else {
                client.unlink(&child).map_err(|e| format!("{:?}", e))?;
            }
        }
    }
    // rmdir in reverse discovery order (children before parents).
    while let Some(dir) = rmdir_order.pop() {
        client.rmdir(&dir).map_err(|e| format!("{:?}", e))?;
    }
    Ok(())
}
```

- [ ] **Step 3: Cluufile.**

Create `containers/rm/Cluufile`:

```
FROM minimal
PROFILE ipc vfs registry
BUILD "cargo build --manifest-path userspace/rm/Cargo.toml --target triplets/x86_64-cluu-user.json -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem" target/x86_64-cluu-user/debug/rm.elf /bin/rm
ENTRYPOINT /bin/rm
```

- [ ] **Step 4: xtask manifest list.**

Append `"userspace/rm",` in `xtask/src/main.rs:2242-2260`.

- [ ] **Step 5: Build + test.**

Run: `cargo xtask build`
Then: `scripts/harness_suite.sh --case l2_rm`
Expected: PASS.

- [ ] **Step 6: Commit.**

```bash
git add userspace/rm/ containers/rm/ xtask/src/main.rs
git commit -m "rm: implement /bin/rm with -r, -f flags"
```

---

## Task 11: Failing `l2_rm_root_refuse` + verify guard

This tests the spec's "refuse to remove root" hard guard. Uses the binary from Task 10 — no new binary, just new harness case.

**Files:**
- Modify: `scripts/harness_cases.conf`, `scripts/harness_case_defaults.sh`, `scripts/harness_run.sh`

- [ ] **Step 1: Case row.**

```
l2_rm_root_refuse|full|MARKER_MODE=l2_rm_root_refuse TEST_COMMAND_REPEAT=1 RUN_WAIT=15
```

- [ ] **Step 2: Marker-mode defaults.**

```sh
            l2_rm_root_refuse)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="spawn rm -rf /"
                ;;
```

- [ ] **Step 3: Required markers.**

```sh
    l2_rm_root_refuse)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "rm: refusing to remove root directory"
        )
        ;;
```

- [ ] **Step 4: Run case — expect PASS.**

Run: `scripts/harness_suite.sh --case l2_rm_root_refuse`
Expected: PASS. The binary from Task 10 already emits the marker.

If FAIL: `resolve_path("/")` returned something other than `/`. Add a debug print to rm's root-check to inspect `resolved` for each arg. Trailing slashes and `/..` should all normalize to `/`; if not, fix `resolve_path` in libcluu — but most likely it already does.

- [ ] **Step 5: Commit.**

```bash
git add scripts/harness_cases.conf scripts/harness_case_defaults.sh scripts/harness_run.sh
git commit -m "harness: verify rm refuses to remove root directory"
```

---

## Task 12: Failing harness case `l2_cp` (TDD red)

**Files:**
- Modify: `scripts/harness_cases.conf`, `scripts/harness_case_defaults.sh`, `scripts/harness_run.sh`

- [ ] **Step 1: Case row.**

```
l2_cp|full|MARKER_MODE=l2_cp TEST_COMMAND_REPEAT=1 RUN_WAIT=18
```

- [ ] **Step 2: Marker-mode defaults.**

```sh
            l2_cp)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="spawn cp /etc/users.toml /tmp/u"
                ;;
```

- [ ] **Step 3: Required markers.**

```sh
    l2_cp)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "cp: ok /etc/users.toml -> /tmp/u"
        )
        ;;
```

- [ ] **Step 4: Run case — expect FAIL.**

Run: `scripts/harness_suite.sh --case l2_cp`
Expected: FAIL.

- [ ] **Step 5: Commit.**

```bash
git add scripts/harness_cases.conf scripts/harness_case_defaults.sh scripts/harness_run.sh
git commit -m "harness: add failing l2_cp case"
```

---

## Task 13: `/bin/cp` implementation

**Files:**
- Create: `userspace/cp/Cargo.toml`, `userspace/cp/src/main.rs`
- Create: `containers/cp/Cluufile`
- Modify: `xtask/src/main.rs:2242-2260`

- [ ] **Step 1: Cargo.toml.**

Create `userspace/cp/Cargo.toml`:

```toml
[package]
name = "cluu-cp"
version = "0.1.0"
edition = "2021"
description = "CLUU cp utility"
authors = ["CLUU Team", "Balazs Valkony"]
license = "MIT"

[dependencies]
libcluu = { path = "../libcluu" }

[[bin]]
name = "cp"
path = "src/main.rs"
```

- [ ] **Step 2: main.rs.**

Create `userspace/cp/src/main.rs`:

```rust
#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use libcluu::fs::client::VfsClient;
use libcluu::{debug_print, registry};

const COPY_CHUNK: usize = 64 * 1024;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = libcluu::args::args();
    if args.len() < 2 {
        let _ = debug_print("cp: missing operand");
        return 1;
    }

    let Ok(vfs_endpoint) = registry::subscribe_output("vfs", "main") else {
        let _ = debug_print("cp: vfs unavailable");
        return 1;
    };
    let client_id = registry::control_endpoint();
    let client = VfsClient::new(vfs_endpoint, client_id);

    // Last arg is dst; prior args are srcs.
    let (srcs, dst) = args.split_at(args.len() - 1);
    let dst = &dst[0];
    let dst_resolved = libcluu::posix::dir::resolve_path(dst);

    let dst_is_dir = client
        .stat(&dst_resolved)
        .ok()
        .map(|s| s.mode & 0o170000 == 0o040000)
        .unwrap_or(false);

    // cp src1 src2 ... destdir/  — multiple srcs require dst to be dir.
    if srcs.len() > 1 && !dst_is_dir {
        let _ = debug_print(&format!(
            "cp: target '{}' is not a directory",
            dst_resolved
        ));
        return 1;
    }

    let mut exit_code = 0i32;
    for src in srcs {
        let src_resolved = libcluu::posix::dir::resolve_path(src);
        let final_dst = if dst_is_dir {
            let name = src_resolved.rsplit('/').next().unwrap_or(&src_resolved);
            if dst_resolved.ends_with('/') {
                format!("{}{}", dst_resolved, name)
            } else {
                format!("{}/{}", dst_resolved, name)
            }
        } else {
            dst_resolved.clone()
        };
        match copy_one(&client, &src_resolved, &final_dst) {
            Ok(()) => {
                let _ = debug_print(&format!("cp: ok {} -> {}", src_resolved, final_dst));
            }
            Err(err) => {
                let _ = debug_print(&format!(
                    "cp: {} -> {}: {}",
                    src_resolved, final_dst, err
                ));
                exit_code = 1;
            }
        }
    }
    exit_code
}

fn copy_one(client: &VfsClient, src: &str, dst: &str) -> Result<(), String> {
    if src == dst {
        return Err(format!("'{}' and '{}' are the same file", src, dst));
    }

    let src_info = client.stat(src).map_err(|e| format!("{:?}", e))?;
    if src_info.mode & 0o170000 == 0o040000 {
        return Err(String::from("is a directory"));
    }
    let mode = (src_info.mode & 0o777) as usize;

    // Flags: O_WRONLY | O_CREAT | O_TRUNC — use libcluu's POSIX constants if
    // exposed, otherwise fall back to hex. Grep `userspace/libcluu/src/posix/`
    // for O_WRONLY to confirm values.
    const O_RDONLY: usize = 0;
    const O_WRONLY: usize = 1;
    const O_CREAT: usize = 0o100;
    const O_TRUNC: usize = 0o1000;

    let src_file = client
        .open_with(src, O_RDONLY, 0)
        .map_err(|e| format!("open src: {:?}", e))?;
    let dst_file = client
        .open_with(dst, O_WRONLY | O_CREAT | O_TRUNC, mode)
        .map_err(|e| {
            let _ = client.close(src_file);
            format!("open dst: {:?}", e)
        })?;

    let mut offset = 0usize;
    let mut buf = alloc::vec![0u8; COPY_CHUNK];
    let total = src_info.size as usize;
    while offset < total {
        let chunk = COPY_CHUNK.min(total - offset);
        // TODO(shell-a.2): use grant-based zero-copy read once we're confident.
        // For now, read via a heap buffer using the pread-style API. Check
        // libcluu::fs::client for a read-to-buf API; if only read_grant
        // exists, use that + memcpy.
        let read_bytes = match client_read_into(client, src_file, offset, &mut buf[..chunk]) {
            Ok(n) => n,
            Err(e) => {
                let _ = client.close(src_file);
                let _ = client.close(dst_file);
                return Err(format!("read: {:?}", e));
            }
        };
        if read_bytes == 0 {
            break;
        }
        if let Err(e) = client.write(dst_file, offset, &buf[..read_bytes]) {
            let _ = client.close(src_file);
            let _ = client.close(dst_file);
            return Err(format!("write: {:?}", e));
        }
        offset += read_bytes;
    }

    let _ = client.close(src_file);
    let _ = client.close(dst_file);
    Ok(())
}

/// Small adapter — implement a sync read-into-buf on top of the VfsClient
/// API. If `VfsClient` exposes a direct `read(file, offset, buf)` helper, use
/// that and delete this function. Grep `userspace/libcluu/src/fs/client.rs`
/// for `pub fn read` before writing this.
fn client_read_into(
    client: &VfsClient,
    file: libcluu::fs::client::VfsFile,
    offset: usize,
    buf: &mut [u8],
) -> Result<usize, libcluu::error::Error> {
    // If VfsClient has `read(file, offset, len) -> Result<Vec<u8>>`, use it.
    // Otherwise fall back to grant-based read using the caller's own space:
    //   let info = libcluu::boot::process_info();
    //   let space = info.tokens[libcluu::boot::TOKEN_SPACE];
    //   let grant = client.read_grant(file, offset, buf.len(), space, buf.as_ptr() as usize)?;
    //   Ok(grant.len as usize)
    //
    // During planning I did not verify which API exists; the implementer
    // should grep first and pick the simplest one. If neither exists, the
    // implementer should pause and ask — do NOT add a new VFS protocol op.
    let _ = (client, file, offset, buf);
    unimplemented!("pick simplest read API in libcluu::fs::client, see note above")
}
```

**IMPORTANT IMPLEMENTER NOTE:** `client_read_into` is a stub with a directive — before writing it, run:

```bash
rg -n "pub fn read|fn read_grant" userspace/libcluu/src/fs/client.rs
```

to discover the actual API. Research Section A.2 suggested `read_grant` exists; if a simpler `read(file, offset, len) -> Result<Vec<u8>>` exists, use it. If only grant exists, implement grant-based copy with the local space. Do NOT add a new VFS protocol op in this plan.

- [ ] **Step 3: Cluufile.**

Create `containers/cp/Cluufile`:

```
FROM minimal
PROFILE ipc vfs registry
BUILD "cargo build --manifest-path userspace/cp/Cargo.toml --target triplets/x86_64-cluu-user.json -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem" target/x86_64-cluu-user/debug/cp.elf /bin/cp
ENTRYPOINT /bin/cp
```

- [ ] **Step 4: xtask manifest list.**

Append `"userspace/cp",` in `xtask/src/main.rs:2242-2260`.

- [ ] **Step 5: Build + test.**

Run: `cargo xtask build`
Then: `scripts/harness_suite.sh --case l2_cp`
Expected: PASS.

If the build fails inside `client_read_into`: you skipped the pre-check. Go back and grep for the actual API.

- [ ] **Step 6: Commit.**

```bash
git add userspace/cp/ containers/cp/ xtask/src/main.rs
git commit -m "cp: implement /bin/cp with single-file copy and basename-into-dir"
```

---

## Task 14: Failing harness case `l2_mv` (TDD red)

**Files:**
- Modify: `scripts/harness_cases.conf`, `scripts/harness_case_defaults.sh`, `scripts/harness_run.sh`

- [ ] **Step 1: Case row.**

```
l2_mv|full|MARKER_MODE=l2_mv TEST_COMMAND_REPEAT=1 RUN_WAIT=20
```

- [ ] **Step 2: Marker-mode defaults.**

```sh
            l2_mv)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="spawn mkdir /tmp/mv; spawn cp /etc/users.toml /tmp/mv/a; spawn mv /tmp/mv/a /tmp/mv/b"
                ;;
```

- [ ] **Step 3: Required markers.**

```sh
    l2_mv)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "mv: ok /tmp/mv/a -> /tmp/mv/b"
        )
        ;;
```

- [ ] **Step 4: Run case — expect FAIL.**

Run: `scripts/harness_suite.sh --case l2_mv`
Expected: FAIL.

- [ ] **Step 5: Commit.**

```bash
git add scripts/harness_cases.conf scripts/harness_case_defaults.sh scripts/harness_run.sh
git commit -m "harness: add failing l2_mv case"
```

---

## Task 15: `/bin/mv` implementation

**Files:**
- Create: `userspace/mv/Cargo.toml`, `userspace/mv/src/main.rs`
- Create: `containers/mv/Cluufile`
- Modify: `xtask/src/main.rs:2242-2260`

- [ ] **Step 1: Cargo.toml.**

Create `userspace/mv/Cargo.toml`:

```toml
[package]
name = "cluu-mv"
version = "0.1.0"
edition = "2021"
description = "CLUU mv utility"
authors = ["CLUU Team", "Balazs Valkony"]
license = "MIT"

[dependencies]
libcluu = { path = "../libcluu" }

[[bin]]
name = "mv"
path = "src/main.rs"
```

- [ ] **Step 2: main.rs.**

Create `userspace/mv/src/main.rs`:

```rust
#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use libcluu::fs::client::VfsClient;
use libcluu::{debug_print, registry};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = libcluu::args::args();
    if args.len() < 2 {
        let _ = debug_print("mv: missing operand");
        return 1;
    }

    let Ok(vfs_endpoint) = registry::subscribe_output("vfs", "main") else {
        let _ = debug_print("mv: vfs unavailable");
        return 1;
    };
    let client_id = registry::control_endpoint();
    let client = VfsClient::new(vfs_endpoint, client_id);

    let (srcs, dst) = args.split_at(args.len() - 1);
    let dst = &dst[0];
    let dst_resolved = libcluu::posix::dir::resolve_path(dst);

    let dst_is_dir = client
        .stat(&dst_resolved)
        .ok()
        .map(|s| s.mode & 0o170000 == 0o040000)
        .unwrap_or(false);

    if srcs.len() > 1 && !dst_is_dir {
        let _ = debug_print(&format!(
            "mv: target '{}' is not a directory",
            dst_resolved
        ));
        return 1;
    }

    let mut exit_code = 0i32;
    for src in srcs {
        let src_resolved = libcluu::posix::dir::resolve_path(src);
        let final_dst = if dst_is_dir {
            let name = src_resolved.rsplit('/').next().unwrap_or(&src_resolved);
            if dst_resolved.ends_with('/') {
                format!("{}{}", dst_resolved, name)
            } else {
                format!("{}/{}", dst_resolved, name)
            }
        } else {
            dst_resolved.clone()
        };
        match client.rename(&src_resolved, &final_dst) {
            Ok(()) => {
                let _ = debug_print(&format!("mv: ok {} -> {}", src_resolved, final_dst));
            }
            Err(e) => {
                let s = format!("{:?}", e);
                if s.contains("CrossDevice") || s.contains("EXDEV") {
                    let _ = debug_print("mv: cross-device rename not yet supported");
                } else {
                    let _ = debug_print(&format!(
                        "mv: {} -> {}: {}",
                        src_resolved, final_dst, s
                    ));
                }
                exit_code = 1;
            }
        }
    }
    exit_code
}
```

**Note:** The error-variant matching for `CrossDevice` is tentative. Grep `userspace/libcluu/src/error.rs` for the actual `Error` enum variants before finalizing. If `EXDEV` is represented differently, update the `.contains()` check or match on the enum directly.

- [ ] **Step 3: Cluufile.**

Create `containers/mv/Cluufile`:

```
FROM minimal
PROFILE ipc vfs registry
BUILD "cargo build --manifest-path userspace/mv/Cargo.toml --target triplets/x86_64-cluu-user.json -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem" target/x86_64-cluu-user/debug/mv.elf /bin/mv
ENTRYPOINT /bin/mv
```

- [ ] **Step 4: xtask manifest list.**

Append `"userspace/mv",` in `xtask/src/main.rs:2242-2260`.

- [ ] **Step 5: Build + test.**

Run: `cargo xtask build`
Then: `scripts/harness_suite.sh --case l2_mv`
Expected: PASS.

- [ ] **Step 6: Commit.**

```bash
git add userspace/mv/ containers/mv/ xtask/src/main.rs
git commit -m "mv: implement /bin/mv via VFS rename"
```

---

## Task 16: Full harness matrix regression check

- [ ] **Step 1: Run the full suite.**

Run: `scripts/harness_suite.sh`
Expected: all cases green except the three pre-existing known-flakes (`l2_fg`, `l2_stop`, `f10_view_passthrough` — see `project_l2_owner_deny_flaky.md` memory for context).

New cases expected green:
- `l2_argv`, `l2_mkdir`, `l2_rm`, `l2_rm_root_refuse`, `l2_cp`, `l2_mv`.

- [ ] **Step 2: If a previously-green case fails, investigate.**

Most likely suspects:
- `msg.words[3]` slot collision with a different pre-existing `CONTAINER_RUN` payload variant (grep for `PROCMGR_CONTAINER_RUN_LABEL` to enumerate callers).
- Name/argv boundary parser misidentifying a legit name containing bytes that look like a malformed trailer. Unlikely but possible — the `argc > 0` gate should make zero-argv byte-for-byte identical to Plan 1 output.
- Rust `_start` panicking in `args::init()` on a service that boots before procmgr has written argv — notably `init` and `procmgr` themselves, which receive no argv. `params[PARAM_ARGC] == 0` in that case, and the decoder returns empty — OK.

- [ ] **Step 3: No-op commit marker (optional).**

If no regressions, note this in the PR description. No commit needed for a successful rerun.

---

## Self-review

After finishing the plan, verify against the spec:

- [ ] Spec's "New `/bin` binaries" (mkdir/rm/cp/mv) — Tasks 8, 10, 13, 15.
- [ ] Spec's 7 harness cases — Plan 1 shipped `l2_cd`, `l2_cd_inherit`; Plan 2 ships `l2_argv` (new), `l2_mkdir`, `l2_rm`, `l2_rm_root_refuse`, `l2_cp`, `l2_mv`. Total: 7 as of end of Plan 2 (6 new here + 2 from Plan 1 = 8, but `l2_argv` is a plumbing test not in the original spec; remove it only if the spec's count of 7 must be exact — it's worth the extra case for TDD confidence).
- [ ] Spec's "rm hard guard: refuse root" — Task 10 Step 2 root-check at top of main; Task 11 verifies via harness.
- [ ] Spec's "cp refuse same-path self-copy" — Task 13 `copy_one` checks `src == dst` before opening dst.
- [ ] Spec's "mv cross-device returns explicit error" — Task 15 error mapping.
- [ ] Spec's "mkdir -p exempts EEXIST on directory components" — Task 8 `mkdir_p` stat-then-check.
- [ ] Spec's follow-up "two IPC paths need argv" — Tasks 3-5 extend `CONTAINER_RUN` path. `SPAWN` path already had argv via Plan 1's earlier work (never broken).

Out of scope (deferred to later Shell-A.* plans or other phases):
- Bare-command dispatch (Shell-A.5) — users must still type `spawn mkdir /foo`.
- `cp -r` directory recursion.
- Glob expansion.
- `mv` cross-filesystem fallback.
- Path canonicalization tests (`/foo/../../..`) — `resolve_path` behavior assumed correct per Plan 1.

---

## Execution options

Once this plan is committed, choose an execution style:

1. **Subagent-Driven (recommended):** fresh subagent per task, two-stage review between tasks (spec compliance, then code quality), fast iteration.
2. **Inline Execution:** batch execution in this session with checkpoints.
