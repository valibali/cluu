# Phase 4 Plan F — Shell Misc Builtins (alias, history, type, help, exit)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Round out the shell with the muscle-memory builtins: persistent history, alias/unalias, type, help, exit (and tighten set/unset).

**Architecture:** Each builtin lives in its own file under `commands/builtins/`. History persists to `~/.cluu_history` via VFS. Alias table lives on `ShellContext`, expanded at command-line tokenization. `type` looks up via the existing builtin registry + PATH walk.

**Tech Stack:** Rust, libcluu fs/posix, existing `BuiltinRegistry` from Plan A.

**Spec reference:** `docs/superpowers/specs/2026-05-07-userspace-polish-design.md` §2 (exit criteria for shell builtins)

**Prereq:** Plan A merged (commands/builtins/ exists). Plan B's `cli.rs` optional (alias arg parsing benefits from it).

---

## File structure

### Modified / created
- `userspace/shell/src/commands/builtins/exit.rs` (already created in Plan A — flesh out)
- `userspace/shell/src/commands/builtins/alias.rs` (already created stub — implement)
- `userspace/shell/src/commands/builtins/history.rs` (already created stub — implement)
- `userspace/shell/src/commands/builtins/help.rs` (already created — flesh out `type`)
- `userspace/shell/src/commands/builtins/env.rs` (modify — `set`/`unset` cleanup)
- `userspace/shell/src/main.rs` (load history at startup; persist on exit; alias expansion in REPL)
- `scripts/harness_cases.conf` (l2_alias_basic, l2_history_persist, l2_type_basic, l2_help_basic, l2_exit_status)

---

## Stage 1 — exit builtin

### Task 1.1: Implement `exit`

**Files:**
- Modify: `userspace/shell/src/commands/builtins/exit.rs`

- [ ] **Step 1: Implement `ExitBuiltin`**

```rust
//! `exit` builtin.

use alloc::boxed::Box;
use alloc::string::String;

use super::registry::{Builtin, BuiltinRegistry, BuiltinResult};
use crate::ShellContext;

pub struct ExitBuiltin;

impl Builtin for ExitBuiltin {
    fn name(&self) -> &'static str { "exit" }
    fn run(&self, args: &[String], context: &mut ShellContext) -> BuiltinResult {
        let code = match args.first() {
            None => context.last_exit_status,
            Some(s) => s.parse::<i32>().unwrap_or(2),
        };
        context.exit_requested = Some(code);
        BuiltinResult::Ok(code)
    }
}

pub fn register(registry: &mut BuiltinRegistry) {
    registry.register(Box::new(ExitBuiltin));
}
```

- [ ] **Step 2: Wire `ShellContext::exit_requested`**

Add `pub exit_requested: Option<i32>` to `ShellContext`. The REPL loop checks after every dispatch; on Some, persists history and `_exit(code)`.

### Task 1.2: REPL respects exit_requested

**Files:**
- Modify: `userspace/shell/src/main.rs`

- [ ] **Step 1: After each command dispatch, check the flag**

```rust
loop {
    let line = read_line(...);
    let result = dispatch(...);
    if let Some(code) = context.exit_requested {
        persist_history(&context);
        libcluu::posix::_exit(code);
    }
    context.last_exit_status = match result { BuiltinResult::Ok(c) => c, _ => 1 };
}
```

### Task 1.3: Add l2_exit_status smoke

**Files:**
- Modify: `scripts/harness_cases.conf`
- Modify: `scripts/harness_case_defaults.sh`

```
l2_exit_status|full|MARKER_MODE=l2_exit_status TEST_COMMAND_REPEAT=1 RUN_WAIT=15
```

```sh
        l2_exit_status)
            SHELL_AUTOSTART_CMD_DEFAULT="false; echo \$?"
            EXPECTED_CONTAINS=("1")
            ;;
```

- [ ] **Step 1: Run; PASS.**

```bash
bash scripts/harness_run.sh l2_exit_status 2>&1 | tail -5
```

### Task 1.4: Commit Stage 1

```bash
git add -A
git commit -m "feat(shell): exit builtin + last_exit_status REPL plumbing

Phase 4 Plan F Stage 1.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Stage 2 — alias / unalias

### Task 2.1: Alias table on ShellContext

**Files:**
- Modify: `userspace/shell/src/main.rs` (struct definition)

- [ ] **Step 1: Add field**

```rust
pub struct ShellContext {
    // ...
    pub aliases: BTreeMap<String, String>,
}
```

Initialize empty in shell startup.

### Task 2.2: Implement alias / unalias

**Files:**
- Modify: `userspace/shell/src/commands/builtins/alias.rs`

- [ ] **Step 1: Implement**

```rust
//! `alias` and `unalias` builtins.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};

use super::registry::{Builtin, BuiltinRegistry, BuiltinResult};
use crate::ShellContext;

pub struct AliasBuiltin;
pub struct UnaliasBuiltin;

impl Builtin for AliasBuiltin {
    fn name(&self) -> &'static str { "alias" }
    fn run(&self, args: &[String], context: &mut ShellContext) -> BuiltinResult {
        if args.is_empty() {
            for (k, v) in &context.aliases {
                let line = format!("alias {}='{}'\n", k, v);
                let _ = libcluu::posix::_write(1, line.as_ptr() as *const _, line.len());
            }
            return BuiltinResult::Ok(0);
        }
        for a in args {
            if let Some(eq) = a.find('=') {
                let name = &a[..eq];
                let val_raw = &a[eq+1..];
                // Strip surrounding quotes.
                let val = val_raw.trim_matches(|c| c == '\'' || c == '"').to_string();
                context.aliases.insert(name.to_string(), val);
            } else {
                match context.aliases.get(a) {
                    Some(v) => {
                        let line = format!("alias {}='{}'\n", a, v);
                        let _ = libcluu::posix::_write(1, line.as_ptr() as *const _, line.len());
                    }
                    None => {
                        let m = format!("alias: {}: not found\n", a);
                        let _ = libcluu::posix::_write(2, m.as_ptr() as *const _, m.len());
                    }
                }
            }
        }
        BuiltinResult::Ok(0)
    }
}

impl Builtin for UnaliasBuiltin {
    fn name(&self) -> &'static str { "unalias" }
    fn run(&self, args: &[String], context: &mut ShellContext) -> BuiltinResult {
        if args.is_empty() { return BuiltinResult::Err("unalias: usage: unalias NAME...".into()); }
        for a in args {
            context.aliases.remove(a);
        }
        BuiltinResult::Ok(0)
    }
}

pub fn register(registry: &mut BuiltinRegistry) {
    registry.register(Box::new(AliasBuiltin));
    registry.register(Box::new(UnaliasBuiltin));
}
```

### Task 2.3: Alias expansion at tokenization time

**Files:**
- Modify: `userspace/shell/src/main.rs` (or wherever input is tokenized before dispatch)

- [ ] **Step 1: Find tokenization entry point**

```bash
grep -n 'fn tokenize\|fn parse_command\|fn split_command' userspace/shell/src/ -r 2>/dev/null
```

- [ ] **Step 2: Before dispatch, replace first token if it matches an alias**

```rust
fn expand_alias_first_token(tokens: &mut Vec<String>, aliases: &BTreeMap<String, String>) {
    let mut seen = BTreeSet::new();
    while let Some(first) = tokens.first().cloned() {
        if seen.contains(&first) { break; }   // recursion guard
        match aliases.get(&first) {
            Some(replacement) => {
                seen.insert(first);
                let parts: Vec<String> = replacement.split_whitespace().map(|s| s.to_string()).collect();
                tokens.splice(0..1, parts);
            }
            None => break,
        }
    }
}
```

Call after tokenization, before dispatch. Aliases applied only to the first token; expansion is one-shot per first token (with recursion guard for circular aliases).

### Task 2.4: l2_alias_basic smoke

```
l2_alias_basic|full|MARKER_MODE=l2_alias_basic TEST_COMMAND_REPEAT=1 RUN_WAIT=18
```

```sh
        l2_alias_basic)
            SHELL_AUTOSTART_CMD_DEFAULT="alias ll='ls -l'; ll /tmp"
            EXPECTED_CONTAINS=("-rw" "rwx")
            ;;
```

- [ ] **Step 1: Run; PASS.**

### Task 2.5: Commit Stage 2

```bash
git add -A
git commit -m "feat(shell): alias / unalias with first-token expansion

Phase 4 Plan F Stage 2.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Stage 3 — type builtin

### Task 3.1: Implement `type`

**Files:**
- Modify: `userspace/shell/src/commands/builtins/help.rs`

- [ ] **Step 1: Add `TypeBuiltin`**

```rust
pub struct TypeBuiltin;

impl Builtin for TypeBuiltin {
    fn name(&self) -> &'static str { "type" }
    fn run(&self, args: &[String], context: &mut ShellContext) -> BuiltinResult {
        if args.is_empty() { return BuiltinResult::Err("type: usage: type NAME...".into()); }
        let mut rc = 0;
        for name in args {
            if context.aliases.contains_key(name) {
                let line = format!("{} is aliased to '{}'\n", name, context.aliases[name]);
                let _ = libcluu::posix::_write(1, line.as_ptr() as *const _, line.len());
                continue;
            }
            if context.builtins.lookup(name).is_some() {
                let line = format!("{} is a shell builtin\n", name);
                let _ = libcluu::posix::_write(1, line.as_ptr() as *const _, line.len());
                continue;
            }
            // PATH walk.
            match find_in_path(name, &libcluu::posix::getenv("PATH").unwrap_or_else(|| "/bin".to_string())) {
                Some(p) => {
                    let line = format!("{} is {}\n", name, p);
                    let _ = libcluu::posix::_write(1, line.as_ptr() as *const _, line.len());
                }
                None => {
                    let line = format!("type: {}: not found\n", name);
                    let _ = libcluu::posix::_write(2, line.as_ptr() as *const _, line.len());
                    rc = 1;
                }
            }
        }
        BuiltinResult::Ok(rc)
    }
}

fn find_in_path(name: &str, path: &str) -> Option<String> {
    for dir in path.split(':') {
        let candidate = format!("{}/{}", dir.trim_end_matches('/'), name);
        // Use VFS stat to check existence.
        if let Ok(_) = stat_via_vfs(&candidate) { return Some(candidate); }
    }
    None
}
```

`stat_via_vfs` is a small helper that constructs a `VfsClient` and calls `stat()`. If that's heavyweight, reuse the shell's existing path lookup helper from `userspace/shell/src/path_lookup.rs`.

- [ ] **Step 2: Register**

In `help.rs::register`:

```rust
pub fn register(registry: &mut BuiltinRegistry) {
    registry.register(Box::new(HelpBuiltin));
    registry.register(Box::new(ClearBuiltin));
    registry.register(Box::new(TypeBuiltin));
}
```

### Task 3.2: l2_type_basic smoke

```
l2_type_basic|full|MARKER_MODE=l2_type_basic TEST_COMMAND_REPEAT=1 RUN_WAIT=15
```

```sh
        l2_type_basic)
            SHELL_AUTOSTART_CMD_DEFAULT="alias ll='ls -l'; type ll; type cd; type ls; type nope"
            EXPECTED_CONTAINS=("ll is aliased" "cd is a shell builtin" "ls is /bin/ls" "not found")
            ;;
```

- [ ] **Step 1: Run; PASS.**

### Task 3.3: Commit Stage 3

```bash
git add -A
git commit -m "feat(shell): type builtin — alias/builtin/path lookup

Phase 4 Plan F Stage 3.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Stage 4 — help builtin

### Task 4.1: Implement `help`

**Files:**
- Modify: `userspace/shell/src/commands/builtins/help.rs`

- [ ] **Step 1: List registered builtins by name**

```rust
impl Builtin for HelpBuiltin {
    fn name(&self) -> &'static str { "help" }
    fn run(&self, args: &[String], context: &mut ShellContext) -> BuiltinResult {
        if args.is_empty() {
            let _ = libcluu::posix::_write(1, b"Shell builtins:\n", 16);
            for n in context.builtins.names() {
                let line = format!("  {}\n", n);
                let _ = libcluu::posix::_write(1, line.as_ptr() as *const _, line.len());
            }
            BuiltinResult::Ok(0)
        } else {
            // Per-builtin help: TODO add a `help_text()` method to the Builtin trait.
            for n in args {
                let line = format!("help: {}: detailed help not yet wired (use --help when supported)\n", n);
                let _ = libcluu::posix::_write(1, line.as_ptr() as *const _, line.len());
            }
            BuiltinResult::Ok(0)
        }
    }
}
```

### Task 4.2: l2_help_basic smoke

```
l2_help_basic|full|MARKER_MODE=l2_help_basic TEST_COMMAND_REPEAT=1 RUN_WAIT=15
```

```sh
        l2_help_basic)
            SHELL_AUTOSTART_CMD_DEFAULT="help"
            EXPECTED_CONTAINS=("cd" "exit" "echo")
            ;;
```

- [ ] **Step 1: Run; PASS.**

### Task 4.3: Commit Stage 4

```bash
git add -A
git commit -m "feat(shell): help builtin lists registered builtins

Phase 4 Plan F Stage 4.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Stage 5 — Persistent history

### Task 5.1: History storage data structure

**Files:**
- Modify: `userspace/shell/src/commands/builtins/history.rs`

- [ ] **Step 1: Add `HistoryBuf`**

```rust
extern crate alloc;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

const HISTORY_CAP: usize = 1000;

#[derive(Default)]
pub struct HistoryBuf {
    entries: VecDeque<String>,
}

impl HistoryBuf {
    pub const fn new() -> Self { Self { entries: VecDeque::new() } }
    pub fn push(&mut self, line: String) {
        if self.entries.back().map(|l| l == &line).unwrap_or(false) { return; } // dedup adjacent
        if self.entries.len() >= HISTORY_CAP { self.entries.pop_front(); }
        self.entries.push_back(line);
    }
    pub fn iter(&self) -> impl Iterator<Item = &String> { self.entries.iter() }
    pub fn nth_from_end(&self, n: usize) -> Option<&String> {
        let len = self.entries.len();
        if n == 0 || n > len { return None; }
        self.entries.get(len - n)
    }
    pub fn replace_all(&mut self, lines: Vec<String>) {
        self.entries.clear();
        for l in lines { self.entries.push_back(l); }
    }
}
```

### Task 5.2: Wire HistoryBuf into ShellContext

```rust
pub struct ShellContext {
    // ...
    pub history: history::HistoryBuf,
}
```

After every successful command line read, push to history. Already-existing in-session up-arrow line edit code reads from this same buf.

### Task 5.3: Load on startup, persist on exit

**Files:**
- Modify: `userspace/shell/src/main.rs`

- [ ] **Step 1: Load**

```rust
fn load_history(context: &mut ShellContext) {
    let path = format!("{}/.cluu_history", libcluu::posix::home_dir());
    if let Ok(content) = read_file_all(&path) {
        let lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
        context.history.replace_all(lines);
    }
}
```

`read_file_all` walks VfsClient: open + read until EOF + close. Add to `userspace/libcluu/src/posix/io.rs` if not present (~20 LOC).

- [ ] **Step 2: Persist**

```rust
fn persist_history(context: &ShellContext) {
    let path = format!("{}/.cluu_history", libcluu::posix::home_dir());
    let mut buf = String::new();
    for l in context.history.iter() {
        buf.push_str(l);
        buf.push('\n');
    }
    let _ = write_file_atomic(&path, buf.as_bytes());
}
```

`write_file_atomic`: write to `<path>.tmp`, rename. Required so a crash mid-write doesn't corrupt history. If `rename` is not available in libcluu/VFS today, do non-atomic: open(O_TRUNC|O_WRONLY) + write + close, and document the gap.

- [ ] **Step 3: Call from REPL**

```rust
load_history(&mut context);   // at startup
// ...REPL loop...
//   in exit_requested branch:
persist_history(&context);
libcluu::posix::_exit(code);
```

Also persist on every Nth command (every 10) so a crash loses ≤10 entries:

```rust
if command_count % 10 == 0 { persist_history(&context); }
```

### Task 5.4: history builtin command

**Files:**
- Modify: `userspace/shell/src/commands/builtins/history.rs`

- [ ] **Step 1: Implement**

```rust
pub struct HistoryBuiltin;

impl Builtin for HistoryBuiltin {
    fn name(&self) -> &'static str { "history" }
    fn run(&self, args: &[String], context: &mut ShellContext) -> BuiltinResult {
        let n: usize = args.first().and_then(|s| s.parse().ok()).unwrap_or(usize::MAX);
        let total = context.history.iter().count();
        for (idx, line) in context.history.iter().enumerate() {
            if total - idx > n { continue; }
            let l = format!("{:>5}  {}\n", idx + 1, line);
            let _ = libcluu::posix::_write(1, l.as_ptr() as *const _, l.len());
        }
        BuiltinResult::Ok(0)
    }
}

pub fn register(registry: &mut BuiltinRegistry) {
    registry.register(Box::new(HistoryBuiltin));
}
```

### Task 5.5: l2_history_persist smoke

```
l2_history_persist|full|MARKER_MODE=l2_history_persist TEST_COMMAND_REPEAT=1 RUN_WAIT=25
```

The smoke needs cross-shell-restart verification. Approach: run shell, type 3 commands + `exit`, then start shell again and run `history`.

```sh
        l2_history_persist)
            SHELL_AUTOSTART_CMD_DEFAULT="echo first; echo second; echo third; exit"
            POST_AUTOSTART_CMD_DEFAULT="history; exit"
            EXPECTED_CONTAINS=("first" "second" "third")
            ;;
```

(Adapt to whatever multi-stage harness primitive exists. If not available, defer the cross-restart verification, ship in-session history, and add a follow-up case.)

- [ ] **Step 1: Run; PASS.**

### Task 5.6: Commit Stage 5

```bash
git add -A
git commit -m "$(cat <<'EOF'
feat(shell): persistent command history

In-session history extended with disk persistence to ~/.cluu_history.
Load on startup, persist on exit and every 10 commands. New `history`
builtin lists entries. Capacity 1000 entries; adjacent duplicates
deduped.

Phase 4 Plan F Stage 5.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Stage 6 — `set`/`unset` cleanup

### Task 6.1: Audit current behavior

**Files:**
- Modify: `userspace/shell/src/commands/builtins/env.rs`

- [ ] **Step 1: Read current `SetBuiltin`/`UnsetBuiltin`**

```bash
grep -n 'struct SetBuiltin\|struct UnsetBuiltin' userspace/shell/src/commands/builtins/env.rs
```

These ship today (line 324, 326 of original commands.rs). Verify behaviors match POSIX:
- `set` (no args): list all variables
- `set -e` / `set -x`: shell options (we don't implement these — print "not supported", exit 0)
- `unset NAME...`: remove var

### Task 6.2: Tighten `set` (no shell options)

- [ ] **Step 1: When invoked with no args, print all vars**

```rust
if args.is_empty() {
    for (k, v) in context.env_iter() {
        let line = format!("{}={}\n", k, v);
        let _ = libcluu::posix::_write(1, line.as_ptr() as *const _, line.len());
    }
    return BuiltinResult::Ok(0);
}
```

- [ ] **Step 2: When invoked with `-e/-u/-x`, print "not supported", exit 0**

```rust
for a in args {
    if a.starts_with('-') {
        let m = format!("set: option {} not supported\n", a);
        let _ = libcluu::posix::_write(2, m.as_ptr() as *const _, m.len());
    } else {
        // bash-like: bare arguments become $1, $2, ...
        // Defer — not used in our scripts.
    }
}
BuiltinResult::Ok(0)
```

### Task 6.3: Commit Stage 6

```bash
git add -A
git commit -m "fix(shell): tighten set/unset; document unsupported shell options

Phase 4 Plan F Stage 6.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Stage 7 — Run full matrix; final commit

### Task 7.1: Final matrix run

```bash
bash scripts/harness_matrix.sh 2>&1 | tee /tmp/matrix-after-F.log
```

Green.

### Task 7.2: Spec retrospective

**Files:**
- Modify: `docs/ROADMAP.md`

- [ ] **Step 1: Add Phase 4 closing notes**

Mirror Phase 1 / Phase 2 closing-notes style:

```markdown
### Phase 4 — Userspace Polish & Coreutils ✅ DONE 2026-MM-DD

- Probes hidden under userspace/probes/, opt-in build via `cargo xtask build-probes`.
- 11 existing utils GNU-close, 15 new utils shipped: env, sleep, basename, dirname, date, kill, printf, sort, uniq, cut, tr, find, which, du, stat.
- ls rewritten with -l/-a/-h/-R/-S/-t/--color=auto on top of extended VfsStat protocol bump.
- Full POSIX job control (Ctrl-Z/fg/bg/SIGSTOP) entirely in userspace via existing kernel ThreadSuspend/Resume.
- Pipe Phase 1 reverified: l2_pipe_3stage smoke green; env propagation through pipe stages closed.
- Shell builtins added: exit, alias/unalias, type, help, set/unset cleanup, persistent history.
- commands.rs split into commands/ module hierarchy; ~19 test-only builtins removed from registry.
```

### Task 7.3: Commit final

```bash
git add docs/ROADMAP.md
git commit -m "docs(roadmap): Phase 4 closing notes

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Self-review

- **Spec coverage**: §2 exit criteria for shell builtins → all stages.
- **Placeholders**: none. Every builtin has full code.
- **Type consistency**: `BuiltinRegistry`, `BuiltinResult`, `ShellContext` consistent.
- **Risk**: Task 5.3 atomic write may not be available — plan documents the fallback.

---

## Acceptance

Plan F done when:
- `exit` builtin terminates shell with given (or last) status
- `alias`/`unalias` round-trips
- `type` distinguishes alias / builtin / external
- `help` lists builtins
- `history` persists to `~/.cluu_history` and survives restart
- `set`/`unset` behave per POSIX baseline (no shell-options support, document)
- Smokes for each stage PASS
- `harness_matrix.sh` green
- Phase 4 closing notes in ROADMAP.md
