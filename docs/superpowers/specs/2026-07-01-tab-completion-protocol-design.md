# Tab Completion Protocol — cluuterm ↔ shell

**Status:** Spec (pre-implementation)
**Date:** 2026-07-01
**Author:** Sisyphus
**Supersedes:** retired `compute_path_completion` (deleted in Path A, see `docs/superpowers/plans/2026-05-14-bug-c-shell-stdin-via-fd0.md` Task 7)
**Roadmap:** Phase 1 exit criterion ("the shell feels like a shell"). Harness test `l2_tab_complete` exists in batch H.

---

## 1. Problem

TAB in the cluuterm shell inserts a literal tab character instead of completing. Root cause is two-layered:

1. **`LineDiscipline::feed_byte`** (`userspace/libcluu/src/tty_core/line_discipline.rs:197`) has no TAB branch. TAB (0x09) falls through to the default case (lines 357-378): inserted into `pending_line` + echoed → cursor jumps to next tab stop. This is the cluuterm path.
2. The legacy `handle_byte_canonical` (line 729) *does* produce a `tab_request` (lines 771-784), but (a) cluuterm never calls it — `route_input_byte` → `feed_byte` is the termios-aware path, and (b) the only consumer (`userspace/tty/src/main.rs:341`) explicitly drops it: `let _ = effect.tab_request;`.

Completion needs shell-level knowledge (builtins, PATH, filenames) that the line discipline doesn't have. The old mechanism (shell recv loop on stdin endpoint) was retired by Path A (POSIX `read(0)` unification). The shell now only sees complete lines and cannot react to mid-line TAB.

## 2. Goals / non-goals

**Goals**
- Single TAB completes a unique prefix; appends the completed suffix to the line buffer and echoes it.
- Double TAB (no unique completion) lists candidates on a new line, then redraws the prompt + current input.
- Completion sources: builtin command names, PATH executables, filenames (absolute and relative).
- Works in cluuterm (graphical PTS path). Legacy `userspace/tty/` text-VT service is untouched but the protocol is service-agnostic.
- Does NOT revert Path A — shell main loop stays on POSIX `read(0)`.

**Non-goals (explicitly deferred)**
- Command-specific argument completion (e.g. `cd <TAB>` completing only directories). All words use the same filename fallback.
- Completion inside the legacy `userspace/tty/` service (it has no shell endpoint either; wire later if needed).
- MicroPython REPL completion.
- History-expansion (`!`-prefix) completion.
- Glob expansion in completions.

## 3. Design — synchronous RPC, shell pthread, stateless handler

### 3.1 High-level flow

```
user presses TAB
  ↓
cluuterm feed_byte(0x09)
  ↓ produces LineDiscOutput::TabRequest(pending_line, consecutive_tabs)
cluuterm extracts current word (last whitespace-delimited token up to cursor)
  ↓
cluuterm ipc::call(shell_completion_ep, SHELL_COMPLETE_QUERY_LABEL, {word, tabs})
  ↓ blocks on reply
shell pthread woken by ipc_recv_any on completion_ep
  ↓ computes candidates: builtins ∪ PATH-executables ∪ filenames
  ↓ ipc::reply(CompleteReply { candidates })
cluuterm applies reply:
  - unique prefix beyond typed → append_completion(suffix), echo
  - exactly one candidate → append_completion(rest), echo
  - multiple candidates, single TAB → no-op (wait for 2nd TAB)
  - multiple candidates, double TAB → echo "\n" + candidates + "\n", redraw prompt+line
  - zero candidates → bell (echo 0x07)
```

### 3.2 Why a pthread (not multi-wait on read(0))

The shell main loop blocks in `_read(0)` (POSIX, VFS-backed — `userspace/shell/src/main.rs:224`). fd 0 is not a shell-owned IPC token; its receive endpoint lives in the VFS service. You cannot put "fd-0-readiness" into an `ipc_recv_any` token slice from the shell side without reverting Path A (recv on `info.tokens[TOKEN_STDIN]` directly).

A dedicated pthread sidesteps this: main loop unchanged, completion endpoint handled independently. The handler is **stateless** — it reads cwd (`current_dir_string()`), PATH (`snapshot_env()`), and the VFS endpoint fresh on each query. The only shared data is the `BuiltinRegistry` (immutable after `BuiltinFactory::build()` at startup) and the VFS send token (immutable). **No mutex required.**

pthreads exist (`userspace/libcluu/src/posix/pthread.rs`). The shell already uses `ipc_recv_any` elsewhere (`exec.rs:418` for Ctrl-C/exit multiplexing during foreground spawns), proving the shell process has `TOKEN_IPC` capability.

### 3.3 Why stateless handler is safe

`CommandContext` holds mutable state (history, cwd, jobs). The completion handler does NOT touch `CommandContext`. Instead:
- **cwd**: `libcluu::posix::current_dir_string()` reads the process's current dir via syscall each call — always fresh, no shared state.
- **builtins**: `BuiltinRegistry.builtins` is built once at shell startup (`main.rs:111`) and never mutated. The pthread holds a `&'static`-lifetime reference (or a raw pointer validated once at startup). Read-only iteration is safe.
- **PATH**: `libcluu::posix::snapshot_env()` reads the env table fresh each call.
- **VFS**: a new `VfsClient::new_from_registry(vfs_ep)` per query, or a cached send token (immutable).

No mutation crosses the thread boundary. The main thread may change cwd (via `cd`) between queries — the next query sees the new cwd. That's correct behavior.

## 4. Wire protocol

### 4.1 New label

```rust
// userspace/cluu_wire/src/pts.rs (or a new shell.rs module in cluu_wire)
pub const SHELL_COMPLETE_QUERY_LABEL: u32 = 143;  // next free after PTS_READ_DELIVER_LABEL (142)
```

Single label suffices — the reply rides the reply_token back to cluuterm's call (same convention as `PTS_GET_TERMIOS_LABEL` etc.).

### 4.2 Request payload (postcard, cluuterm → shell)

```rust
#[derive(Serialize, Deserialize)]
pub struct CompleteRequest {
    pub word: String,           // current word (may be empty, may contain slashes)
    pub consecutive_tabs: u8,   // 1 = first TAB, 2 = second consecutive TAB
}
```

IPC words: `words[0]` = payload_len, `words[1]` = `cluu_wire::ABI_VERSION` (matches existing PTS verb convention).

### 4.3 Reply payload (postcard, shell → cluuterm)

```rust
#[derive(Serialize, Deserialize)]
pub struct CompleteReply {
    pub candidates: Vec<String>,    // all matches, unsorted (shell may sort; cluuterm may sort)
    pub common_prefix: String,      // longest common prefix of candidates, beyond `word`
}
```

`common_prefix` is computed shell-side so cluuterm's apply step is trivial. If `candidates.len() == 1`, `common_prefix` is the remaining suffix of that single candidate. If `candidates.is_empty()`, both fields are empty.

### 4.4 Error / timeout behavior

- Shell does not reply within a deadline → cluuterm treats as "no candidates" and bells. Concretely: cluuterm uses `ipc::call_with_reply_buf` with the existing timeout convention. If the call returns `Err(Timeout)` or `Err(WouldBlock)`, echo `0x07` (bell) and continue. **Do not hang the terminal.**
- Shell handler panic → caught by pthread join / process abort policy. For v1, a panic in the completion handler logs via `debug_print` and the thread exits; subsequent TABs get no reply (bell). Restart-on-panic is out of scope.

## 5. Endpoint discovery

### 5.1 Shell registers a named completion endpoint

At shell startup, after `registry::init("shell")` and after `CLUU_SESSION_ID` is known:

```rust
// userspace/shell/src/main.rs — after line 188 (job control init), before main loop
let completion_ep = endpoint_create(info.tokens[TOKEN_IPC])?;
let sid = read_env_var("CLUU_SESSION_ID")
    .and_then(|s| s.parse::<u32>().ok())
    .unwrap_or(0);
let completion_name = format!("completion:{}", sid);
registry::register_output(&completion_name, completion_ep)?;
spawn_completion_pthread(completion_ep);  // blocks on ipc_recv_any, never returns
```

Registry namespace: service="shell" (from `registry::init`), output="completion:<sid>". Full lookup name: `"shell:completion:<sid>"`. Matches existing convention (`"session-procmgr:spawn:<sid>"`).

### 5.2 cluuterm learns its session_id

**Gap:** `read_own_session_id()` returns `None` unconditionally (`userspace/cluuterm/src/main.rs:197`). `spawn_shell_with_pts` hardcodes `sid: u32 = 1` (line 340). The `VfsRegisterPtsRequest` already carries `session_id: Option<u32>` (cluu_wire/src/pts.rs:242) but cluuterm passes `None`.

**Fix:** cluuterm must obtain its sid. Two options:
- **(A) From spawn envelope.** Pass `CLUU_SESSION_ID` in the cluuterm spawn env (session-procmgr already knows the sid). cluuterm reads it via `process_info()` PARAM_ENVC (same as shell does at `main.rs:169`). Cleanest, no new IPC.
- **(B) From VFS_REGISTER_PTS reply.** Extend `VfsRegisterPtsReply` to echo back the session_id VFS assigned. VFS already tracks per-session PTS overlays (`userspace/vfs/src/pts.rs` register_in_session).

**Choose (A)** — it reuses the existing env-var plumbing and doesn't change the VFS register protocol. session-procmgr's cluuterm spawn (via `spawn_shell_with_pts` equivalent on the cluuterm side) adds `CLUU_SESSION_ID=<sid>` to the env block. cluuterm reads it the same way the shell does.

cluuterm then looks up the shell endpoint lazily on first TAB:

```rust
// cluuterm, on first TabRequest
let shell_ep = registry::lookup_service(&format!("shell:completion:{}", my_sid))?;
// cache shell_ep in Cluuterm struct (new field: shell_completion_ep: Option<usize>)
```

If lookup returns `None` (shell not yet registered, or wrong sid), bell and continue — don't crash. Retry lookup on next TAB.

### 5.3 Concurrent shells / multi-session

Each shell registers under `"shell:completion:<sid>"` with its own sid. cluuterm looks up the one matching its own sid. No collision (the collision that motivated removing `register_default_outputs` at `shell/main.rs:51-53` was for stdin/stdout/stderr/stdlog — those are consumed globally; completion is per-session and namespaced by sid).

## 6. Line discipline changes

### 6.1 New `LineDiscOutput` variant

`userspace/libcluu/src/tty_core/line_discipline.rs`:

```rust
pub enum LineDiscOutput {
    Bytes(Vec<u8>),
    Signal(SignalNum),
    Echo(Vec<u8>),
    Eof,
    Drop,
    TabRequest { line: Vec<u8>, cursor: usize, consecutive_tabs: u8 },  // NEW
}
```

Carry `cursor` so cluuterm can extract the word boundary correctly (word is the token between the last whitespace before cursor and the cursor itself).

### 6.2 TAB branch in `feed_byte`

Insert before the default case (line 357), after the VWERASE branch (line 322):

```rust
if byte == b'\t' {
    // TAB is not a c_cc special char in the default termios; treat as completion request.
    // Do not insert into pending_line; do not echo. Emit TabRequest for the service.
    self.consecutive_tabs = self.consecutive_tabs.saturating_add(1);
    out.push(LineDiscOutput::TabRequest {
        line: self.pending_line.clone(),
        cursor: self.pending_cursor,
        consecutive_tabs: self.consecutive_tabs,
    });
    return out;
}
```

Reset `consecutive_tabs` to 0 at the top of `feed_byte` for any non-TAB byte (mirror the logic in `handle_byte_canonical` lines 732-734).

### 6.3 New `ServiceAction` variant

`userspace/libcluu/src/tty_core/routing.rs`:

```rust
pub enum ServiceAction {
    DeliverBytes(Vec<u8>),
    SignalFgPgrp(SignalNum),
    Echo(Vec<u8>),
    DeliverEof,
    TabRequest { line: Vec<u8>, cursor: usize, consecutive_tabs: u8 },  // NEW
}
```

`translate_output` (routing.rs:24) maps `LineDiscOutput::TabRequest` → `ServiceAction::TabRequest`.

### 6.4 cluuterm handles `ServiceAction::TabRequest`

In `apply_service_actions` (`userspace/cluuterm/src/tty_backend.rs:697`), add an arm:

```rust
ServiceAction::TabRequest { line, cursor, consecutive_tabs } => {
    self.handle_tab_request(line, cursor, consecutive_tabs);
}
```

`handle_tab_request`:
1. Extract `word` = bytes between last whitespace before `cursor` and `cursor`. If cursor is at start or preceded by whitespace, `word` is empty.
2. Look up `shell_completion_ep` (cached, or lookup on first call — see §5.2).
3. If no shell ep: echo `0x07` (bell), return.
4. `ipc::call(shell_ep, SHELL_COMPLETE_QUERY_LABEL, &CompleteRequest{word, consecutive_tabs}, reply_buf)`.
5. On `Err`: echo `0x07`, return.
6. Deserialize `CompleteReply`.
7. Apply:
   - `candidates.is_empty()` → echo `0x07`.
   - `common_prefix` non-empty → `self.pts.line_discipline.append_completion(common_prefix.as_bytes())`; echo the appended bytes (append_completion returns echo bytes — see existing method at line_discipline.rs:957).
   - `common_prefix` empty AND `consecutive_tabs == 1` → no-op (wait for 2nd TAB).
   - `common_prefix` empty AND `consecutive_tabs >= 2` AND candidates non-empty → echo `"\n"`, join candidates with `"  "`, echo `"\n"`, then redraw prompt + current line (CR + clear-to-EOL + prompt + pending_line).
   - `common_prefix` empty AND `consecutive_tabs >= 2` AND candidates empty → echo `0x07`.

### 6.5 `append_completion` already exists

`LineDiscipline::append_completion(&mut self, bytes: &[u8])` at `line_discipline.rs:957` inserts bytes at cursor and advances cursor. It does NOT echo (caller responsibility, per the doc comment at line 954). Reuse it as-is.

## 7. Shell completion handler

### 7.1 New module: `userspace/shell/src/completion.rs`

```rust
//! TAB completion candidate computation. Stateless; safe to call from a pthread.
//!
//! Sources (in priority order for a bare word with no slash):
//!   1. Builtin command names (BuiltinRegistry.builtins)
//!   2. PATH executables (readdir each PATH dir, filter by prefix)
//!   3. (filenames are only tried when the word contains a slash, or when
//!      sources 1+2 yield nothing AND the word could be a relative path)
//!
//! For a word WITH a slash: filename completion only (don't match builtins).

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use crate::commands::BuiltinRegistry;
use libcluu::fs::client::VfsClient;

pub fn complete(word: &str, registry: &BuiltinRegistry, vfs: &VfsClient) -> Vec<String> {
    if word.contains('/') {
        complete_filename(word, vfs)
    } else {
        let mut cands = Vec::new();
        cands.extend(complete_builtins(word, registry));
        cands.extend(complete_path_executables(word, vfs));
        dedup(&mut cands);
        cands
    }
}
```

### 7.2 Builtin source

```rust
fn complete_builtins(word: &str, registry: &BuiltinRegistry) -> Vec<String> {
    registry.builtins.iter()
        .map(|b| b.name())
        .filter(|n| n.starts_with(word))
        .map(|n| n.to_string())
        .collect()
}
```

`BuiltinRegistry.builtins` is `pub(crate)` (registry.rs:332); `BuiltinCommand::name(&self) -> &'static str` (registry.rs:294). Module is in the shell crate → access works.

**Do NOT use `KNOWN_BUILTINS` (help.rs:26-41)** — it is manually synced and known to drift (lists "cat"/"ls" which are external containers, not registered builtins).

### 7.3 PATH-executable source

```rust
fn complete_path_executables(word: &str, vfs: &VfsClient) -> Vec<String> {
    let path_env = read_path_env();  // exec.rs:119, pub(crate) — or move to completion module
    let mut cands = Vec::new();
    for dir in path_env.split(':') {
        if dir.is_empty() { continue; }
        if let Ok(entries) = vfs.readdir(dir) {
            for e in entries {
                if e.name.starts_with(word) {
                    cands.push(e.name);
                }
            }
        }
    }
    dedup(&mut cands);
    cands
}
```

No exec-bit filtering for v1 (matches `path_lookup::resolve` which only stat's for existence, not exec bit). May include non-executable files in PATH dirs; acceptable for v1, refine later.

### 7.4 Filename source

```rust
fn complete_filename(word: &str, vfs: &VfsClient) -> Vec<String> {
    // Split into dir + prefix. "/foo/ba" → dir="/foo", prefix="ba".
    // "ba" (no slash) → dir=cwd, prefix="ba" — but only reached when word has slash
    // per the dispatch in complete(); bare words go through builtin+PATH first.
    let (dir, prefix) = match word.rfind('/') {
        Some(idx) => (&word[..idx], &word[idx+1..]),
        None => ("", word),  // shouldn't happen (caller checks contains '/')
    };
    let dir = if dir.is_empty() { "/" } else { dir };
    let mut cands = Vec::new();
    if let Ok(entries) = vfs.readdir(dir) {
        for e in entries {
            if e.name.starts_with(prefix) {
                // Preserve the dir prefix in the candidate so append_completion
                // extends the full typed path.
                let full = if word.starts_with('/') {
                    format!("{}/{}", dir.trim_end_matches('/'), e.name)
                } else {
                    format!("{}/{}", dir, e.name)
                };
                cands.push(if e.is_dir { format!("{}/", full) } else { full });
            }
        }
    }
    cands
}
```

Directory candidates get a trailing `/` so the next TAB descends into them (standard shell behavior).

### 7.5 `common_prefix` computation

Shell computes it before replying:

```rust
fn longest_common_prefix(cands: &[String]) -> String {
    if cands.is_empty() { return String::new(); }
    let first = cands[0].as_bytes();
    let mut len = first.len();
    for c in &cands[1..] {
        let b = c.as_bytes();
        len = len.min(b.len());
        let mut i = 0;
        while i < len && first[i] == b[i] { i += 1; }
        len = i;
        if len == 0 { break; }
    }
    String::from_utf8_lossy(&first[..len]).into_owned()
}
```

The reply's `common_prefix` is `longest_common_prefix(&candidates)` with the typed `word` stripped from the front (cluuterm appends only the *suffix* beyond what's already typed).

Edge case: single candidate. `common_prefix` = the full candidate minus the typed word.

### 7.6 Pthread entry point

```rust
// userspace/shell/src/completion.rs (or main.rs)
pub fn completion_thread(entry_ep: usize, registry: &'static BuiltinRegistry) {
    let vfs_ep = match registry::lookup_service("vfs:main") {
        Some(ep) => ep,
        None => { let _ = debug_print("completion: no vfs:main, exiting"); return; }
    };
    let mut buf = [0u8; 512];
    loop {
        let control = registry::control_endpoint();
        let tokens = if control != 0 { [entry_ep, control] } else { [entry_ep] };
        match ipc_recv_any(&tokens, &mut buf, u64::MAX) {
            Ok((_idx, len)) => {
                let (msg, payload) = parse_message(&buf[..len]);
                if msg.tag.label == SHELL_COMPLETE_QUERY_LABEL {
                    handle_query(&msg, payload, entry_ep, registry, vfs_ep);
                } else {
                    // Control traffic (grant etc.) — let registry handle it.
                    let _ = registry::handle_incoming_message(&msg, payload);
                }
            }
            Err(_) => { let _ = yield_cpu(); }
        }
    }
}
```

`BuiltinRegistry` must outlive the thread. Build it once at startup, leak it (`Box::leak`) or store as a `static`. The current `main.rs:411` builds a fresh registry per line — that pattern must change to a single long-lived registry for the completion thread to reference. **This is the one structural change to the main loop area:** build `BuiltinRegistry` once before spawning the pthread, pass `&'static` to the thread, and reuse it for per-line execution (clone or share — `BuiltinRegistry` is not `Clone` today; either add `Clone` or share by `&'static`).

### 7.7 Thread spawn

Use `libcluu::posix::pthread::spawn` (verify exact signature in `userspace/libcluu/src/posix/pthread.rs` during implementation). The thread never joins. If pthread spawn is unavailable in this build config, fall back to NOT registering the completion endpoint (TAB bells) — don't break the shell.

## 8. cluuterm session_id plumbing

### 8.1 session-procmgr passes sid in spawn env

The session-procmgr's cluuterm spawn path (equivalent of `spawn_shell_with_pts` at `cluuterm/src/main.rs:256` — but cluuterm is spawned by session-procmgr, not self-spawned) must inject `CLUU_SESSION_ID=<sid>` into the cluuterm env block. session-procmgr knows the sid (it owns the session).

### 8.2 cluuterm reads sid

Replace `read_own_session_id()` (cluuterm/src/main.rs:197):

```rust
fn read_own_session_id() -> Option<u32> {
    // Read CLUU_SESSION_ID from the ProcessInfo env page (same mechanism
    // the shell uses at shell/src/main.rs:252).
    read_env_var("CLUU_SESSION_ID").and_then(|s| s.parse().ok())
}
```

cluuterm needs a `read_env_var` equivalent — either import from libcluu (if exposed) or inline the same ProcessInfo-page walk the shell does (`shell/src/main.rs:252-287`). Prefer factoring `read_env_var` into `libcluu::boot` or `libcluu::posix::env` so both shell and cluuterm share it.

## 9. Files touched (summary)

| File | Change |
|---|---|
| `userspace/cluu_wire/src/pts.rs` (or new `shell.rs`) | Add `SHELL_COMPLETE_QUERY_LABEL = 143`, `CompleteRequest`, `CompleteReply`. |
| `userspace/libcluu/src/tty_core/line_discipline.rs` | Add `LineDiscOutput::TabRequest` variant; TAB branch in `feed_byte`; reset `consecutive_tabs` on non-TAB. |
| `userspace/libcluu/src/tty_core/routing.rs` | Add `ServiceAction::TabRequest`; extend `translate_output`. |
| `userspace/cluuterm/src/tty_backend.rs` | Handle `ServiceAction::TabRequest` in `apply_service_actions`; new `handle_tab_request` + `shell_completion_ep` cache field. |
| `userspace/cluuterm/src/main.rs` | Implement `read_own_session_id` from env; cache sid. |
| `userspace/shell/src/main.rs` | Create completion endpoint, `register_output("completion:<sid>", ep)`, spawn pthread; build `BuiltinRegistry` once (long-lived). |
| `userspace/shell/src/completion.rs` (NEW) | `complete()`, builtin/PATH/filename sources, `longest_common_prefix`, pthread entry. |
| `userspace/shell/src/commands/builtins/registry.rs` | Expose `BuiltinRegistry.builtins` for read iteration (already `pub(crate)`) OR add `pub fn names(&self) -> impl Iterator<Item = &'static str>`. |
| `userspace/session-procmgr/src/*` | Inject `CLUU_SESSION_ID` into cluuterm spawn env. |
| `userspace/libcluu/src/posix/env.rs` (or `boot.rs`) | Factor out `read_env_var` shared by shell + cluuterm. |
| `userspace/tty/src/main.rs` | (Optional, non-goal for v1) Legacy service stays on `let _ = effect.tab_request;`. No change. |

## 10. Testing

### 10.1 Unit tests (pure logic, no kernel)

- `completion.rs`: `complete("e", ...)` returns `["echo", "exit", "env", ...]` against a stub registry.
- `longest_common_prefix(["echo", "exit", "env"])` → `"e"`.
- `longest_common_prefix(["foobar"])` → `"foobar"`.
- `longest_common_prefix([])` → `""`.
- `feed_byte` TAB test: assert `LineDiscOutput::TabRequest` emitted, `pending_line` unchanged, `consecutive_tabs` increments.
- `feed_byte` non-TAB after TAB: `consecutive_tabs` resets to 0.

Run via `rustc --edition 2021 --test` (matches existing pattern in README §Tests).

### 10.2 Integration (harness)

`l2_tab_complete` already exists in batch H (`docs/superpowers/plans/2026-05-26-autologin-removal-harness-migration.md:146`). Wire it to:
- Type `ec<TAB>` → line becomes `echo ` (trailing space if single match) OR `ech` (common prefix).
- Type `ec<TAB><TAB>` → lists `echo` (and any other `ec*` matches) below the prompt.
- Type `/bin/ls<TAB>` → completes to `/bin/ls ` (single file match).
- Type `/bin/l<TAB><TAB>` → lists `ls`, `ln`, ... (all `/bin/l*` files).

### 10.3 Manual QEMU smoke

```sh
# in cluuterm shell after login:
ec<TAB>          # → echo
ec<TAB><TAB>     # → lists echo + env + exit + ...
ls /bi<TAB>      # → ls /bin/
ls /bin/l<TAB>   # → ls /bin/ln  (or lists if multiple)
cd /et<TAB>      # → cd /etc/
```

## 11. Risks / open questions

- **pthread availability in the shell build.** ~~The shell is `#![no_std]` + `no_main`. pthreads require the posix shim + a thread-capable kernel config. If `pthread::spawn` is unavailable, the completion endpoint can't be served.~~ **VERIFIED (2026-07-01):** pthreads work. The `futexrace` C program (`PROFILE ipc spawn`) successfully calls ThreadCreate on TOKEN_SPACE; the shell has `ipc spawn registry vfs` (superset). `pthread_create` at `libcluu/src/posix/pthread.rs:338` is linked via the shell's `libcluu/posix` feature.
- **Registry control-endpoint grant race (verified, low-risk).** The completion pthread includes `registry::control_endpoint()` in its `ipc_recv_any` token set (`completion.rs:170-171`). If a `REGISTRY_GRANT_DELIVER_LABEL` (0x106 — a subscriber's grant arriving) lands while the main thread is also blocked in `wait_for_grant` on the same control endpoint, the kernel delivers it to one thread nondeterministically. If the completion thread wins, it discards the `RegistryEvent::Grant` (`completion.rs:175` `let _ = ...`) — the main thread's `subscribe_output` would hang. **Mitigation (in place):** all startup subscriptions (`procmgr:spawn` at `main.rs:90`, `vfs:main` at `main.rs:113`, `register_output("completion:<sid>")` at `main.rs:212`) complete BEFORE the pthread spawns (`main.rs:228`). The race only affects runtime `subscribe_output` calls, which are rare in the shell (only triggered by specific builtins). cluuterm's subscription to `shell:completion:<sid>` triggers a producer-side `GRANT_REQUEST` (0x105), which `handle_incoming_message` → `handle_grant_request` (`registry.rs:363→434`) correctly mints + replies to regardless of which thread receives it. **Hardening follow-up (deferred):** give the completion thread its own dedicated endpoint for registry control traffic, or have the completion thread forward `Grant` events to the main thread via a flag/endpoint instead of discarding.
- **`BuiltinRegistry` lifetime.** Current per-line rebuild (`main.rs:411`) must change to a single long-lived build. `BuiltinRegistry` holds `Vec<Box<dyn BuiltinCommand>>` — not `Clone` cheaply. Share by `&'static` (leak) or refactor to `Arc`. Verify builtins are truly immutable after build (no late registration).
- **Registry grant timing.** `register_output` is synchronous and blocks until committed (`registry.rs:169`). If the registry is slow, shell startup blocks. Acceptable — already the pattern for other services.
- **cluuterm blocking on completion RPC.** While cluuterm waits for the shell reply, it cannot process compositor INPUT_FORWARD (keystrokes queue in the compositor). If the shell hangs, cluuterm appears frozen. **Mitigation:** cluuterm uses a bounded timeout on `call_with_reply_buf`; on timeout, bell + continue. Document the timeout (e.g. 2 seconds).
- **Large candidate lists.** A PATH dir with hundreds of entries → large `CompleteReply`. Postcard handles it; the 512-byte recv buffer in the pthread must grow to match the reply. Use a 4KB buffer or dynamic.
- **Word boundary edge cases.** Mid-line TAB (cursor not at end) must complete the word under the cursor, not the end of the line. `append_completion` inserts at cursor — correct. But the redraw-after-list path must restore cursor position. Verify with a test.
- **Concurrent TAB presses.** User mashes TAB while a previous query is in flight. cluuterm is single-threaded over its input loop — it blocks on the first call, so the second TAB queues in the compositor and is processed after the first reply. Acceptable.

## 12. Out of scope (follow-ups)

- Argument-aware completion (cd completes dirs only, etc.).
- Completion in the legacy `userspace/tty/` text-VT service.
- MicroPython REPL completion.
- Exec-bit filtering for PATH completion.
- Completion of shell variables (`$<TAB>`), aliases, history expansion.
- Async (non-blocking) completion RPC so cluuterm never blocks on the shell.
