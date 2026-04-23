# Shell-A Plan 1 — cwd plumbing + `cd`/`pwd` builtins

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `cd` and `pwd` work as shell builtins, and make the cwd propagate to child processes across `posix_spawn`.

**Architecture:** The authoritative cwd lives in each process's `libcluu::posix::dir::CWD` static. `posix_spawn` appends a length-prefixed magic trailer to the spawn IPC payload containing the parent's cwd bytes. Procmgr (stateless relay) parses that trailer and writes the bytes into the child's `ProcessInfo` page at two new param slots (`PARAM_CWD_OFFSET` / `PARAM_CWD_LEN`). The child's `libcluu` init seeds its own `CWD` from those params before `main()` runs. Procmgr keeps no cwd state of its own.

**Tech Stack:** Rust (libcluu, procmgr, shell), C (pwdprobe test helper), pest grammar (existing — no changes here), QEMU-based harness.

**Scope boundary:** This plan covers only `cd`/`pwd` and the cwd inheritance plumbing. A follow-up Plan 2 (Shell-A.2) adds the `mkdir`/`rm`/`cp`/`mv` binaries and bare-command dispatch. The spec at `docs/superpowers/specs/2026-04-22-shell-a-design.md` describes both halves.

---

## Background for the implementer

- CLUU has no `fork()`. The only way to create a process is `posix_spawn` (wrapper in `userspace/libcluu/src/posix/process.rs`), which talks to procmgr over IPC.
- Procmgr keeps process-level state only when it has to (exit notifications, view mounts, container membership). The design rule for this project is: **default to in-process state, do not grow procmgr**. See memory `feedback_procmgr_stateless`.
- The shell is DIY Rust + pest grammar in `userspace/shell/` and `crates/cluu_lang/`. Do **not** propose porting dash/ash/bash. See memory `feedback_shell_diy_pest`.
- The `Message` struct carries exactly 6 `usize` words. `words[0]` is always overwritten by the IPC layer with the payload length. `words[1..6]` are all already assigned in the spawn message — that is why we use an end-of-payload magic trailer instead of a word slot.
- The harness is a headless QEMU that captures COM2 serial output. A test passes when every string in its `required_markers` array appears in the serial log. See `scripts/harness_run.sh` around line 573 for existing cases and `scripts/harness_case_defaults.sh` for marker defaults.
- Read the design spec first: `docs/superpowers/specs/2026-04-22-shell-a-design.md`. Do not deviate from its decisions without a note in the plan.

---

## File structure

**Create:**
- `userspace/c-programs/pwdprobe.c` — ~20-line C test helper: calls `getcwd()` and prints the result with a stable marker prefix.
- `containers/pwdprobe/Cluufile` — container manifest for `pwdprobe`.

**Modify:**
- `userspace/libcluu/src/boot.rs` — add `PARAM_CWD_OFFSET` / `PARAM_CWD_LEN` constants, bump `ProcessInfo.params` from `[u64; 10]` to `[u64; 12]`.
- `userspace/libcluu/src/posix/dir.rs` — change `init_cwd()` to seed from ProcessInfo params; add two Rust-friendly helpers (`current_dir_string`, `set_current_dir_str`) for use by the shell.
- `userspace/libcluu/src/posix/process.rs` — append a CWD magic trailer onto the spawn payload.
- `userspace/procmgr/src/main.rs` — parse the CWD trailer in `handle_spawn_message`, thread the cwd bytes through `spawn_service_with_env` → `map_process_info_page`, and write them into the info page.
- `userspace/shell/src/commands.rs` — add `CdBuiltin` and `PwdBuiltin`, plus a `last_status: i32` field on `CommandContext`, register them in `DefaultBuiltins`.
- `userspace/shell/src/commands.rs` (help text) — update `HelpBuiltin` list.
- `xtask/src/main.rs` — register the new `pwdprobe` userspace crate and its container in the build pipeline.
- `scripts/harness_cases.conf` — add `l2_cd` and `l2_cd_inherit` cases.
- `scripts/harness_case_defaults.sh` — add `MARKER_MODE` branches for the two new cases.
- `scripts/harness_run.sh` — add `required_markers` branches for the two new cases.

---

## Wire format reference (read once, come back to it)

**Magic trailer appended to spawn payload by `posix_spawn`:**

```
[... existing payload: path\0 argv env FDAC ...]
[cwd_bytes (0..CWD_MAX)]       <- the parent's current cwd string, no trailing NUL
[u32 cwd_len  LE]              <- length of cwd_bytes above
[u32 CWD_MAGIC LE = 0x20445743] <- ASCII "CWD " (space at end), little-endian
```

`CWD_MAGIC` is written with `u32::to_le_bytes` so the byte order in the payload is `0x43, 0x57, 0x44, 0x20` = ASCII `"CWD "`.

Procmgr detects the trailer by reading the last 4 bytes of payload. If they match `CWD_MAGIC`, it reads the preceding 4 bytes as `cwd_len` and the preceding `cwd_len` bytes as the cwd string. Otherwise no cwd is inherited (child defaults to `/`). `CWD_MAX = 1024` matches the spec.

**ProcessInfo params added for the child (read by `libcluu::posix::dir::init_cwd`):**

```rust
pub const PARAM_CWD_OFFSET: usize = 10;  // byte offset into the 4 KB ProcessInfo page
pub const PARAM_CWD_LEN:    usize = 11;  // length in bytes
```

The `ProcessInfo.params` array grows from `[u64; 10]` to `[u64; 12]`. This changes the `ProcessInfo` struct size. Add a compile-time `size_of` assertion to make sure the page layout still fits inside `PAGE_SIZE` (4096 bytes) with room for argv+env+cwd.

---

## Task 1: Add `last_status` field to `CommandContext` (preparatory)

This field is not read by anything in Plan 1, but `CdBuiltin` will write to it. Shell-B's future `echo $?` will read it. Adding it now costs one line and avoids churn later.

**Files:**
- Modify: `userspace/shell/src/commands.rs:45-50` (CommandContext struct) and `userspace/shell/src/commands.rs:72-81` (constructor).

- [ ] **Step 1: Add the field and initializer.**

In `userspace/shell/src/commands.rs`, change:

```rust
pub struct CommandContext {
    vars: BTreeMap<String, String>,
    procmgr_spawn: usize,
    console_write: usize,
    bg_jobs: BTreeMap<usize, BackgroundJob>,
}
```

to:

```rust
pub struct CommandContext {
    vars: BTreeMap<String, String>,
    procmgr_spawn: usize,
    console_write: usize,
    bg_jobs: BTreeMap<usize, BackgroundJob>,
    /// Exit status of the most recently executed builtin/command.
    /// Read by `echo $?` (Shell-B). `cd`/`pwd` write here.
    last_status: i32,
}
```

And in `impl CommandContext::new`:

```rust
pub fn new() -> Self {
    Self {
        vars: BTreeMap::new(),
        procmgr_spawn: 0,
        console_write: 0,
        bg_jobs: BTreeMap::new(),
        last_status: 0,
    }
}
```

Also add an accessor pair so future builtins can update it:

```rust
pub fn set_last_status(&mut self, status: i32) {
    self.last_status = status;
}

pub fn last_status(&self) -> i32 {
    self.last_status
}
```

- [ ] **Step 2: Build check.**

Run: `cargo check --manifest-path userspace/shell/Cargo.toml --target triplets/x86_64-cluu-user.json -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem`
Expected: clean compile. A warning about unused `last_status` is fine (the field is used next task).

- [ ] **Step 3: Commit.**

```bash
git add userspace/shell/src/commands.rs
git commit -m "shell: add CommandContext.last_status field for upcoming cd/pwd builtins"
```

---

## Task 2: Add Rust-friendly `current_dir` / `set_current_dir` helpers to libcluu

`dir.rs` today only exposes `chdir(*const c_char)` and `getcwd(*mut c_char, size_t)` (the C ABI). The shell is Rust and should not marshal through `CString` when we can expose a direct Rust API.

**Files:**
- Modify: `userspace/libcluu/src/posix/dir.rs` (just after the existing `chdir` function around line 246).

- [ ] **Step 1: Write the helpers.**

Append to `userspace/libcluu/src/posix/dir.rs` at the end of the `getcwd/chdir` section (before the `Helpers` banner at line 248):

```rust
/// Rust-friendly read of the current working directory.
pub fn current_dir_string() -> alloc::string::String {
    let cwd = CWD.lock();
    cwd.as_deref().unwrap_or("/").into()
}

/// Rust-friendly `chdir`. Validates the target via VFS stat and updates CWD.
///
/// Returns `Ok(())` on success, or the POSIX errno value on failure.
pub fn set_current_dir_str(path: &str) -> Result<(), c_int> {
    let resolved = resolve_path(path);

    let vfs_endpoint = match crate::registry::lookup_service("vfs:main") {
        Some(ep) => ep,
        None => return Err(ENOENT),
    };
    let client_id = crate::registry::control_endpoint();
    if client_id == 0 {
        return Err(EINVAL);
    }
    let client = crate::fs::client::VfsClient::new(vfs_endpoint, client_id);
    match client.stat(&resolved) {
        Ok(info) => {
            if info.mode & 0o170000 != 0o040000 {
                return Err(ENOTDIR);
            }
        }
        Err(e) => return Err(crate::errno::from_cluu_error(e)),
    }

    let mut cwd = CWD.lock();
    *cwd = Some(alloc::string::String::from(resolved.as_str()));
    Ok(())
}
```

(`ENOENT`, `ENOTDIR`, `EINVAL` are already imported at the top of the file.)

- [ ] **Step 2: Build check.**

Run: `cargo check --manifest-path userspace/libcluu/Cargo.toml --target triplets/x86_64-cluu-user.json -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem`
Expected: clean compile. These are unused for now; that's fine.

- [ ] **Step 3: Commit.**

```bash
git add userspace/libcluu/src/posix/dir.rs
git commit -m "libcluu: add current_dir_string / set_current_dir_str Rust helpers"
```

---

## Task 3: Add harness case `l2_cd` (the test — will fail initially)

TDD first step: the test must fail because `cd` and `pwd` don't exist yet.

**Files:**
- Modify: `scripts/harness_cases.conf`
- Modify: `scripts/harness_case_defaults.sh`
- Modify: `scripts/harness_run.sh`

- [ ] **Step 1: Add the case row.**

Append to `scripts/harness_cases.conf`:

```
l2_cd|full|MARKER_MODE=l2_cd TEST_COMMAND_REPEAT=1 RUN_WAIT=12
```

- [ ] **Step 2: Add marker-mode defaults.**

In `scripts/harness_case_defaults.sh`, add inside the `case "$MARKER_MODE"` block (alphabetically with other `l2_*` entries around line 47):

```sh
            l2_cd)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="cd /etc; pwd"
                ;;
```

`TEST_COMMAND=""` prevents the default `spawn hello` from being injected after the autostart runs.

- [ ] **Step 3: Add required markers.**

In `scripts/harness_run.sh`, add inside `case "$MARKER_MODE"` (alongside the other `l2_*` entries around line 553):

```sh
    l2_cd)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "/etc"
        )
        ;;
```

(The `/etc` marker is what `pwd` will print after `cd /etc` succeeds. Keep this loose — the line ends in a newline but ripgrep-substring is good enough.)

- [ ] **Step 4: Run the case — expect failure.**

Run: `scripts/harness_suite.sh --case l2_cd`
Expected: FAIL. The serial log should show something like `cluu: unsupported command: cd` — the `/etc` marker will be missing.

- [ ] **Step 5: Commit (TDD red step).**

```bash
git add scripts/harness_cases.conf scripts/harness_case_defaults.sh scripts/harness_run.sh
git commit -m "harness: add failing l2_cd case for cd/pwd builtins"
```

---

## Task 4: Implement `CdBuiltin`

**Files:**
- Modify: `userspace/shell/src/commands.rs` (add after the existing builtin implementations; `EnvBuiltin` is a good neighbor to sit next to since both read env vars).

- [ ] **Step 1: Find a good insertion point.**

In `userspace/shell/src/commands.rs`, locate the end of `EnvBuiltin` (search for `impl BuiltinCommand for EnvBuiltin`). Add the new struct and impl after its closing brace. If there's an existing `read_env_var` helper in `userspace/shell/src/main.rs`, use it for the `HOME` lookup; otherwise call `libcluu::posix::getenv` through a `CString`. Grep first:

```bash
rg -n "read_env_var|fn .*env_var|getenv" userspace/shell/src/
```

If a `read_env_var` helper exists in `main.rs`, add a `pub(crate) use crate::read_env_var;` at the top of `commands.rs` if needed (or call via `crate::read_env_var` directly). If not, add one in `main.rs`:

```rust
pub(crate) fn read_env_var(name: &str) -> Option<String> {
    use alloc::ffi::CString;
    let c = CString::new(name).ok()?;
    let ptr = unsafe { libcluu::posix::getenv(c.as_ptr()) };
    if ptr.is_null() {
        return None;
    }
    let mut len = 0;
    unsafe {
        while *ptr.add(len) != 0 { len += 1; }
        core::str::from_utf8(core::slice::from_raw_parts(ptr as *const u8, len))
            .ok()
            .map(String::from)
    }
}
```

- [ ] **Step 2: Write `CdBuiltin`.**

Add to `userspace/shell/src/commands.rs`:

```rust
struct CdBuiltin;

impl BuiltinCommand for CdBuiltin {
    fn name(&self) -> &'static str {
        "cd"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        if args.len() > 1 {
            send_with_payload(stdout, TTY_WRITE_LABEL, b"cd: too many arguments\n")?;
            context.set_last_status(1);
            return Ok(());
        }

        let target: String = if args.is_empty() {
            // No arg: use $HOME, fall back to "/" if unset.
            crate::read_env_var("HOME").unwrap_or_else(|| String::from("/"))
        } else {
            args[0].clone()
        };

        match libcluu::posix::set_current_dir_str(target.as_str()) {
            Ok(()) => {
                context.set_last_status(0);
            }
            Err(errno) => {
                let line = format!("cd: {}: errno {}\n", target, errno);
                send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                context.set_last_status(1);
            }
        }
        Ok(())
    }
}
```

Note the import path: `libcluu::posix::set_current_dir_str` — the function is re-exported via `pub use dir::*` at `userspace/libcluu/src/posix/mod.rs:42`.

- [ ] **Step 3: Register `CdBuiltin` in the default provider.**

In `userspace/shell/src/commands.rs` inside `impl BuiltinProvider for DefaultBuiltins::register` (around line 268), add at the top (so `cd` lands before `spawn`):

```rust
        registry.register(Box::new(CdBuiltin));
```

Place it right after `registry.register(Box::new(EchoBuiltin));` — alphabetical ordering is informal here, but grouping with its sibling `PwdBuiltin` in Task 5 is fine.

- [ ] **Step 4: Build check.**

Run: `cargo check --manifest-path userspace/shell/Cargo.toml --target triplets/x86_64-cluu-user.json -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem`
Expected: clean compile.

Do not commit yet — `PwdBuiltin` follows in the next task.

---

## Task 5: Implement `PwdBuiltin`

**Files:**
- Modify: `userspace/shell/src/commands.rs`

- [ ] **Step 1: Write `PwdBuiltin`.**

Add immediately after `CdBuiltin` (from Task 4):

```rust
struct PwdBuiltin;

impl BuiltinCommand for PwdBuiltin {
    fn name(&self) -> &'static str {
        "pwd"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        if !args.is_empty() {
            send_with_payload(stdout, TTY_WRITE_LABEL, b"pwd: too many arguments\n")?;
            context.set_last_status(1);
            return Ok(());
        }

        let cwd = libcluu::posix::current_dir_string();
        // Harness-observable signal (COM2 captures debug_print output but not
        // TTY writes). The harness marker "shell: pwd=<path>" is keyed off this.
        let _ = libcluu::debug_print(&alloc::format!("shell: pwd={}\n", cwd));
        let mut line = cwd;
        line.push('\n');
        send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
        context.set_last_status(0);
        Ok(())
    }
}
```

- [ ] **Step 2: Register `PwdBuiltin`.**

In `DefaultBuiltins::register`, add right after the `CdBuiltin` line from Task 4:

```rust
        registry.register(Box::new(PwdBuiltin));
```

- [ ] **Step 3: Update help text.**

In `HelpBuiltin::run` at `userspace/shell/src/commands.rs:369`, change the listing string to include `cd, pwd` after `echo`:

```rust
        send_with_payload(
            stdout,
            TTY_WRITE_LABEL,
            b"builtins: help, clear, echo, cd, pwd, exit, set, unset, env, expr, let, spawn, spawnbg, jobs, jobchurn, jobmix, stop, fg, bg, killdeny, regdeny, mapfail, mapcpfail, maperror, ext2write, ext2append, ext2mutate, ext2unlink, ext2ownerdeny, ringio, repeat, cat, ls, heap\n",
        )?;
```

- [ ] **Step 4: Run the `l2_cd` harness — expect PASS now.**

Run: `scripts/harness_suite.sh --case l2_cd`
Expected: PASS. Serial log shows `[USER] shell: ready`, then `cd /etc; pwd` runs, then `/etc` prints on a line.

If it fails: check the serial log (`/tmp/cluu-serial-com2.log`) for what `cd` or `pwd` printed. Common issues are (a) `HOME` lookup path in `read_env_var`, (b) incorrect use of `current_dir_string` (it already returns a `String`, don't `.to_string()` a `String`).

- [ ] **Step 5: Commit.**

```bash
git add userspace/shell/src/commands.rs userspace/shell/src/main.rs
git commit -m "shell: add cd and pwd builtins"
```

---

## Task 6: Add `PARAM_CWD_OFFSET` / `PARAM_CWD_LEN` and bump `ProcessInfo.params`

**Files:**
- Modify: `userspace/libcluu/src/boot.rs`
- Modify: `userspace/procmgr/src/main.rs` (it hardcodes `PAGE_SIZE` checks around the info page — audit once the struct grows).

- [ ] **Step 1: Add the constants and bump the array.**

In `userspace/libcluu/src/boot.rs`, change the `ProcessInfo` struct at line 51-65:

```rust
#[repr(C)]
pub struct ProcessInfo {
    pub exit_token: usize,
    pub exit_cookie: usize,
    pub pid: usize,
    pub tokens: [usize; 16],
    /// Generic parameters (service-specific data).
    /// Slots 0-9: existing (see PARAM_* constants below).
    /// Slots 10-11: cwd offset / length (Shell-A).
    pub params: [u64; 12],
}
```

And add two constants near the other `PARAM_` definitions (after `PARAM_CAP_PROFILE` at line 201):

```rust
// Current working directory inherited across posix_spawn (Shell-A).
// Byte offset within the 4 KB ProcessInfo page where the cwd bytes live.
pub const PARAM_CWD_OFFSET: usize = 10;
// Length of the cwd bytes. 0 means "no inherited cwd; use /".
pub const PARAM_CWD_LEN: usize = 11;
// Maximum cwd byte length carried across spawn.
pub const CWD_MAX: usize = 1024;
```

- [ ] **Step 2: Add a compile-time size assertion.**

Append near the bottom of `userspace/libcluu/src/boot.rs`:

```rust
const _: () = {
    let size = core::mem::size_of::<ProcessInfo>();
    // 3 * usize + 16 * usize + 12 * u64 on x86_64 = 24 + 128 + 96 = 248 bytes.
    // Page is 4096, so there's ~3.8 KB left for argv/env/cwd payloads. Plenty.
    assert!(size <= 512, "ProcessInfo grew unexpectedly large");
};
```

- [ ] **Step 3: Audit procmgr for any hardcoded `[u64; 10]` references.**

Run: `rg -n "\\[u64; 10\\]|params\\.len\\(\\)" userspace/procmgr/ userspace/libcluu/`
For each hit, change `[u64; 10]` to `[u64; 12]`. The expected locations:
- `userspace/procmgr/src/main.rs` around line 4949 (`let mut params = [0u64; 10];`)

Update `let mut params = [0u64; 12];` and leave the rest of the logic alone (it writes specific indices only).

- [ ] **Step 4: Build check.**

Run: `cargo check --manifest-path userspace/libcluu/Cargo.toml --target triplets/x86_64-cluu-user.json -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem`
Then: `cargo check --manifest-path userspace/procmgr/Cargo.toml --target triplets/x86_64-cluu-user.json -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem`
Both should be clean.

- [ ] **Step 5: Commit.**

```bash
git add userspace/libcluu/src/boot.rs userspace/procmgr/src/main.rs
git commit -m "libcluu: add PARAM_CWD_OFFSET/LEN, bump ProcessInfo.params to [u64; 12]"
```

---

## Task 7: Teach child-side `init_cwd` to read the params

**Files:**
- Modify: `userspace/libcluu/src/posix/dir.rs:160-165` (existing `init_cwd`).

- [ ] **Step 1: Rewrite `init_cwd`.**

Replace the current `init_cwd` in `userspace/libcluu/src/posix/dir.rs`:

```rust
/// Initialize the CWD from ProcessInfo params, falling back to "/" if absent.
/// Called from `__cluu_init` (C programs) and `_start` (Rust programs).
pub fn init_cwd() {
    let mut cwd = CWD.lock();
    if cwd.is_some() {
        return;
    }

    let info = crate::boot::process_info();
    let cwd_offset = info.params[crate::boot::PARAM_CWD_OFFSET] as usize;
    let cwd_len = info.params[crate::boot::PARAM_CWD_LEN] as usize;

    if cwd_len == 0 || cwd_offset == 0 || cwd_len > crate::boot::CWD_MAX {
        *cwd = Some(alloc::string::String::from("/"));
        return;
    }

    let page_base = crate::boot::PROCESS_INFO_ADDR & !(4096 - 1);
    let page_end = page_base + 4096;
    let start = page_base + cwd_offset;
    if start + cwd_len > page_end {
        // Malformed — safer to default than to read past the page.
        *cwd = Some(alloc::string::String::from("/"));
        return;
    }

    let bytes = unsafe { core::slice::from_raw_parts(start as *const u8, cwd_len) };
    match core::str::from_utf8(bytes) {
        Ok(s) if !s.is_empty() && s.starts_with('/') => {
            *cwd = Some(alloc::string::String::from(s));
        }
        _ => {
            *cwd = Some(alloc::string::String::from("/"));
        }
    }
}
```

- [ ] **Step 2: Build check.**

Run: `cargo check --manifest-path userspace/libcluu/Cargo.toml --target triplets/x86_64-cluu-user.json -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem`
Expected: clean.

- [ ] **Step 3: Sanity — existing harness still passes.**

Because parents don't yet emit the trailer, `params[PARAM_CWD_*]` stay zero and every process falls through to `/`. Nothing should change yet. Quick check:

Run: `scripts/harness_suite.sh --case m1_recv`
Expected: PASS.

- [ ] **Step 4: Commit.**

```bash
git add userspace/libcluu/src/posix/dir.rs
git commit -m "libcluu: seed CWD from ProcessInfo PARAM_CWD_* params at init"
```

---

## Task 8: Emit the CWD trailer from `posix_spawn`

**Files:**
- Modify: `userspace/libcluu/src/posix/process.rs` (inside `posix_spawn`, after FDAC serialization, around line 436).

- [ ] **Step 1: Define the magic.**

Near the existing `FDAC_MAGIC` at `userspace/libcluu/src/posix/process.rs:491`, add:

```rust
/// Magic marker for the CWD trailer at the end of the spawn payload.
/// Bytes in little-endian order: 'C','W','D',' ' = 0x43, 0x57, 0x44, 0x20.
const CWD_MAGIC: u32 = 0x2044_5743;
```

- [ ] **Step 2: Append the trailer at the end of payload construction.**

In `posix_spawn` at `userspace/libcluu/src/posix/process.rs:436`, immediately after the `serialize_fd_actions` call and before the `let mut msg = ...` line, add:

```rust
    // Append the CWD magic trailer so procmgr can seed the child's cwd.
    // Layout: [cwd_bytes][u32 cwd_len LE][u32 CWD_MAGIC LE].
    let cwd_string = crate::posix::dir::current_dir_string();
    let cwd_bytes = cwd_string.as_bytes();
    let cwd_len = cwd_bytes.len().min(crate::boot::CWD_MAX);
    payload.extend_from_slice(&cwd_bytes[..cwd_len]);
    payload.extend_from_slice(&(cwd_len as u32).to_le_bytes());
    payload.extend_from_slice(&CWD_MAGIC.to_le_bytes());
```

- [ ] **Step 3: Build check.**

Run: `cargo check --manifest-path userspace/libcluu/Cargo.toml --target triplets/x86_64-cluu-user.json -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem`
Expected: clean.

- [ ] **Step 4: Sanity — existing harness still passes.**

The trailer is unconditionally appended now. Until procmgr learns to read it (Task 9), the trailer is ignored (it's past the end of every existing parser's scan). Verify a spawn-heavy case still works:

Run: `scripts/harness_suite.sh --case m1_recv`
Expected: PASS.

Do not commit yet — producer without consumer is half a feature. Combine with Task 9.

---

## Task 9: Parse the CWD trailer in procmgr and thread it to `map_process_info_page`

**Files:**
- Modify: `userspace/procmgr/src/main.rs` in `handle_spawn_message` (around line 3318-3352) and `map_process_info_page` (around line 4900-5026).

- [ ] **Step 1: Add the magic constant and a tiny parser.**

Near the top of `userspace/procmgr/src/main.rs` (with the other `const FDAC_*` or `SPAWN_*` constants), add:

```rust
/// Must match `CWD_MAGIC` in libcluu::posix::process.
const SPAWN_CWD_MAGIC: u32 = 0x2044_5743; // "CWD "
```

At the bottom of the file near `serialize_fd_actions`-adjacent helpers (or inline in the module where it is called from), add:

```rust
/// Extract the cwd string from the end of a spawn payload.
///
/// Returns `(payload_without_trailer, cwd_bytes)`. If no trailer is present,
/// returns the full payload and an empty byte slice.
fn split_cwd_trailer(payload: &[u8]) -> (&[u8], &[u8]) {
    if payload.len() < 8 {
        return (payload, &[]);
    }
    let magic_pos = payload.len() - 4;
    let magic_bytes: [u8; 4] = match payload[magic_pos..].try_into() {
        Ok(b) => b,
        Err(_) => return (payload, &[]),
    };
    if u32::from_le_bytes(magic_bytes) != SPAWN_CWD_MAGIC {
        return (payload, &[]);
    }

    let len_pos = magic_pos - 4;
    let len_bytes: [u8; 4] = match payload[len_pos..magic_pos].try_into() {
        Ok(b) => b,
        Err(_) => return (payload, &[]),
    };
    let cwd_len = u32::from_le_bytes(len_bytes) as usize;

    if cwd_len > len_pos {
        return (payload, &[]);
    }
    if cwd_len > 1024 {
        // CWD_MAX guardrail — drop obviously malformed trailers.
        return (payload, &[]);
    }

    let cwd_start = len_pos - cwd_len;
    (&payload[..cwd_start], &payload[cwd_start..len_pos])
}
```

- [ ] **Step 2: Use the parser in `handle_spawn_message`.**

In `handle_spawn_message` (around line 3318), replace:

```rust
        let path_nul_end = payload
            .iter()
            .position(|b| *b == 0)
            .unwrap_or(payload.len())
            + 1;
        let argv_data = if argc > 0 && path_nul_end < payload.len() {
            &payload[path_nul_end..]
        } else {
            &[]
        };
        let fdac_data = if fdac_offset > 0 && fdac_offset < payload.len() {
            &payload[fdac_offset..]
        } else {
            &[]
        };
```

with:

```rust
        // Strip the CWD trailer first so argv/fdac slices don't extend into it.
        let (effective_payload, cwd_bytes) = split_cwd_trailer(payload);

        let path_nul_end = effective_payload
            .iter()
            .position(|b| *b == 0)
            .unwrap_or(effective_payload.len())
            + 1;
        let argv_data = if argc > 0 && path_nul_end < effective_payload.len() {
            &effective_payload[path_nul_end..]
        } else {
            &[]
        };
        let fdac_data = if fdac_offset > 0 && fdac_offset < effective_payload.len() {
            &effective_payload[fdac_offset..]
        } else {
            &[]
        };
```

- [ ] **Step 3: Thread `cwd_bytes` through `spawn_service_with_env`.**

In the same block, find the call to `self.spawn_service_with_env(...)`. Add `cwd_bytes` as a new trailing argument (before `Some(&child_view_mounts)`). You will need to update the signature of `spawn_service_with_env` (grep for its definition, around line 3452-3480) to accept `cwd_bytes: &[u8]`, and forward it wherever `spawn_service_with_env` calls `map_process_info_page`.

There are other callers of `spawn_service_with_env` (service spawn, session spawn, container spawn). Pass `&[]` for those sites — they inherit no cwd and the child will default to `/`.

- [ ] **Step 4: Extend `map_process_info_page` to accept and write the cwd.**

At `userspace/procmgr/src/main.rs:4901`, update the signature:

```rust
fn map_process_info_page(
    space_token: usize,
    exit_token: usize,
    // ... existing args ...
    param_overrides: &[(usize, u64)],
    cwd_bytes: &[u8],     // NEW: last argument
) -> Result<()> {
```

Inside the body, **after** the existing argv/env offset computation at line 4964-4979 and **before** the `apply caller-specified param overrides` loop at line 4985, compute and write the cwd slot:

```rust
    // Place cwd bytes in the page AFTER env data. Clamp to CWD_MAX and guard
    // against overflow of the 4 KB page. If it won't fit, silently emit zero
    // length — child falls back to "/".
    let cwd_data_offset = env_data_offset + env_data.len();
    let cwd_clamped_len = cwd_bytes.len().min(1024); // == CWD_MAX
    let cwd_end = cwd_data_offset + cwd_clamped_len;
    let cwd_fits = cwd_clamped_len > 0 && cwd_end <= PAGE_SIZE;

    if cwd_fits {
        params[/* PARAM_CWD_OFFSET */ 10] = cwd_data_offset as u64;
        params[/* PARAM_CWD_LEN */ 11] = cwd_clamped_len as u64;
    }
```

And at the page-writing block near line 5013-5016, after the env write, append:

```rust
    if cwd_fits {
        page[cwd_data_offset..cwd_end].copy_from_slice(&cwd_bytes[..cwd_clamped_len]);
    }
```

Prefer to import `PARAM_CWD_OFFSET` / `PARAM_CWD_LEN` via `use libcluu::boot::{PARAM_CWD_OFFSET, PARAM_CWD_LEN, CWD_MAX};` at the top of the file, then use the names instead of hardcoded `10`/`11`/`1024`. The numeric comments above are just to make the relationship explicit in the diff.

- [ ] **Step 5: Build check.**

Run: `cargo check --manifest-path userspace/procmgr/Cargo.toml --target triplets/x86_64-cluu-user.json -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem`
Expected: clean.

- [ ] **Step 6: Rerun `l2_cd` — still passes (tests builtin-local cwd).**

Run: `scripts/harness_suite.sh --case l2_cd`
Expected: PASS (behavior identical to Task 5's passing state).

- [ ] **Step 7: Rerun a spawn-heavy case — check no regression.**

Run: `scripts/harness_suite.sh --case l2_jobs`
Expected: PASS.

- [ ] **Step 8: Commit.**

```bash
git add userspace/libcluu/src/posix/process.rs userspace/procmgr/src/main.rs
git commit -m "cwd: transport parent cwd via magic trailer + ProcessInfo params"
```

---

## Task 10: Create `pwdprobe.c` test helper

**Files:**
- Create: `userspace/c-programs/pwdprobe.c`

- [ ] **Step 1: Check the conventions of neighbors.**

Before writing, look at an existing C probe to match the include set / stdout conventions. Grep for sibling probes:

```bash
ls userspace/c-programs/
rg -n "getcwd|printf|puts" userspace/c-programs/
```

Match whatever stdout-flushing pattern the others use (CLUU stdout is line-buffered in most cases, but some probes call `fflush(stdout)` explicitly).

- [ ] **Step 2: Write the probe.**

Create `userspace/c-programs/pwdprobe.c`:

```c
#include <stdio.h>
#include <unistd.h>
#include <string.h>

int main(void) {
    char buf[1024];
    if (getcwd(buf, sizeof buf) == NULL) {
        printf("pwdprobe: FAIL getcwd returned NULL\n");
        fflush(stdout);
        return 1;
    }
    printf("pwdprobe: cwd=%s\n", buf);
    fflush(stdout);
    return 0;
}
```

The `pwdprobe: cwd=...` prefix is the marker string the harness will look for.

---

## Task 11: Package `pwdprobe` as a container

**Files:**
- Create: `containers/pwdprobe/Cluufile`
- Modify: `xtask/src/main.rs` at line 3573 (the C-programs build list).

Reference: `containers/envprobe/Cluufile` is the exact shape we want — a pure-C probe with `PROFILE ipc` and an xtask-driven C build.

- [ ] **Step 1: Write `containers/pwdprobe/Cluufile`.**

Create `containers/pwdprobe/Cluufile`:

```
FROM minimal
PROFILE ipc vfs registry
BUILD "cargo xtask build-c pwdprobe userspace/c-programs/pwdprobe.c" target/x86_64-cluu-user/debug/pwdprobe.elf /bin/pwdprobe
ENTRYPOINT /bin/pwdprobe
```

`PROFILE` must include `vfs` (pwdprobe calls `getcwd()` which needs VFS lookups? — actually `getcwd` is purely local state, does not hit VFS) and `registry` (to reach services). If the build succeeds but the probe silently hangs or exits 0 without printing, retry with `PROFILE ipc` only (matching `envprobe`); the extra caps aren't needed for a read of the local CWD string.

- [ ] **Step 2: Register in xtask C-programs list.**

In `xtask/src/main.rs` at line 3573 (the tuple list starting `("hello", "userspace/c-programs/hello.c"),`), add a new entry alphabetically or at the end of the list:

```rust
        ("pwdprobe", "userspace/c-programs/pwdprobe.c"),
```

Double-check there isn't a second list elsewhere that also needs updating:

```bash
rg -n "envprobe|pipeprobe" xtask/src/main.rs
```

Every hit that names a C probe is a candidate for adding `pwdprobe`. In practice the list at line 3561+ is the only spot.

- [ ] **Step 3: Confirm the container is auto-discovered.**

The build system should pick up `containers/pwdprobe/Cluufile` automatically (the `containers/` directory is scanned). Confirm by grepping for how containers are enumerated:

```bash
rg -n "containers/\*|read_dir.*containers|container_dirs|Cluufile" xtask/src/main.rs | head -20
```

If the container list is hardcoded, add `"pwdprobe"` to the list. If it's directory-scanned, no additional change is needed.

- [ ] **Step 4: Full build.**

Run: `cargo xtask build`
Expected: successful build. The artifact `target/x86_64-cluu-user/debug/pwdprobe.elf` appears, and `pwdprobe` shows up in the user-disk container image.

- [ ] **Step 5: Smoke test interactively (optional but fast).**

Run: `scripts/harness_suite.sh --case l2_cd_inherit` (from the next task) won't be possible yet — do it in Task 12. For now, a minimal sanity check is that `cargo xtask build` completes and the ELF exists:

```bash
ls -l target/x86_64-cluu-user/debug/pwdprobe.elf
```

- [ ] **Step 6: Commit.**

```bash
git add userspace/c-programs/pwdprobe.c containers/pwdprobe/Cluufile xtask/src/main.rs
git commit -m "pwdprobe: add C test helper that prints getcwd() result"
```

---

## Task 12: Add harness case `l2_cd_inherit`

**Files:**
- Modify: `scripts/harness_cases.conf`
- Modify: `scripts/harness_case_defaults.sh`
- Modify: `scripts/harness_run.sh`

- [ ] **Step 1: Add the case row.**

Append to `scripts/harness_cases.conf`:

```
l2_cd_inherit|full|MARKER_MODE=l2_cd_inherit TEST_COMMAND_REPEAT=1 RUN_WAIT=15
```

- [ ] **Step 2: Add marker-mode defaults.**

In `scripts/harness_case_defaults.sh`, inside the `case "$MARKER_MODE"` block:

```sh
            l2_cd_inherit)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="cd /tmp; spawn pwdprobe"
                ;;
```

- [ ] **Step 3: Add required markers.**

In `scripts/harness_run.sh`, add a `MARKER_MODE` branch:

```sh
    l2_cd_inherit)
        required_markers=(
            "TSC calibrated"
            "[USER] shell: ready"
            "pwdprobe: cwd=/tmp"
        )
        ;;
```

- [ ] **Step 4: Run the case.**

Run: `scripts/harness_suite.sh --case l2_cd_inherit`
Expected: PASS. Serial log shows `pwdprobe: cwd=/tmp`.

If it fails with `pwdprobe: cwd=/`: the trailer isn't reaching the child. Debug order:
1. Check `git status` that Task 8-9 changes are still present.
2. Add a one-shot `debug_print` in procmgr inside `split_cwd_trailer` logging the detected cwd.
3. Add a one-shot `debug_print` in libcluu's `init_cwd` logging what it read from params.

Remove any debug prints before committing.

- [ ] **Step 5: Commit.**

```bash
git add scripts/harness_cases.conf scripts/harness_case_defaults.sh scripts/harness_run.sh
git commit -m "harness: add l2_cd_inherit (cwd propagates to child via posix_spawn)"
```

---

## Task 13: Full harness matrix regression check

- [ ] **Step 1: Run the full suite.**

Run: `scripts/harness_suite.sh`
Expected: ALL PASS, including the two new cases.

If a previously-green case fails: investigate before moving on. The most likely suspects are:
- `ProcessInfo.params` size growth broke page layout in one of the service spawn paths. Grep for `[u64; 10]` again — there may be a second copy of the shape elsewhere (vfs, console, tty).
- CWD trailer confusing an existing parser. Check `argv_data` / `fdac_data` slicing in `handle_spawn_message` — both must now be derived from `effective_payload`, not `payload`.

- [ ] **Step 2: No-op commit marker (optional).**

If no regressions, there's nothing to commit. Just note in the PR description that the full matrix was re-run.

---

## Self-review

After finishing the plan, walk back through the spec (`docs/superpowers/specs/2026-04-22-shell-a-design.md`) and confirm:

- [ ] `cd` 0/1/≥2 args handled — Task 4.
- [ ] `cd` failure writes to `CommandContext.last_status` — Task 4.
- [ ] `pwd` no-arg, too-many-args handled — Task 5.
- [ ] `PARAM_CWD_OFFSET` / `PARAM_CWD_LEN` added — Task 6.
- [ ] `CWD_MAX = 1024` enforced on both sides (libcluu writer clamps, procmgr parser guards) — Tasks 8, 9.
- [ ] `init_cwd` reads params and defaults to `/` on absence/malformed — Task 7.
- [ ] Harness cases `l2_cd`, `l2_cd_inherit` added — Tasks 3, 12.
- [ ] `pwdprobe` helper added — Task 10.
- [ ] Spec requirements *out of scope for Plan 1* (deferred to Plan 2 / later): `mkdir`, `rm`, `cp`, `mv`, `rm -rf /` guard, cp/mv harness cases. These will be handled in `2026-04-22-shell-a-binaries.md`.

---

## Execution options

Once this plan is committed, choose an execution style:

1. **Subagent-Driven (recommended):** fresh subagent per task, review between tasks, fast iteration.
2. **Inline Execution:** batch execution in this session with checkpoints.
