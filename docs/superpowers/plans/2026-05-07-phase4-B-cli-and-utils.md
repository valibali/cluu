# Phase 4 Plan B — Shared CLI Parser, Existing Util Upgrades, New Utils

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a shared POSIX-style argument parser in `libcluu`, then upgrade 11 existing coreutils to GNU-close behavior and add 15 new ones.

**Architecture:** New `libcluu::cli` module — single-pass arg parser supporting clustered short flags, long opts, optional/required attachment, `--`, and auto-generated `--help`/`--version`. Every existing util migrates onto it (DRY). New utils ship using it from the start. Stricter exit codes per GNU convention (0/1/2).

**Tech Stack:** Rust `no_std`, `libcluu` POSIX layer, existing harness infrastructure.

**Spec reference:** `docs/superpowers/specs/2026-05-07-userspace-polish-design.md` §5.1, §5.2, §5.3

**Prereq:** Plan A merged (workspace clean). Plan C is independent of B; can run in parallel by another worker. Plan B does NOT depend on Plan C.

---

## File structure

### Created
- `userspace/libcluu/src/cli.rs` (~250 LOC arg parser)
- `userspace/libcluu/tests/cli_test.rs` (host-side unit tests)
- `userspace/env/`, `userspace/sleep/`, `userspace/basename/`, `userspace/dirname/`, `userspace/date/`, `userspace/kill/`, `userspace/printf/`, `userspace/sort/`, `userspace/uniq/`, `userspace/cut/`, `userspace/tr/`, `userspace/find/`, `userspace/which/`, `userspace/du/`, `userspace/stat/` (15 new util crates)

### Modified
- `userspace/libcluu/src/lib.rs` (re-export `cli`)
- `userspace/cat/src/main.rs`, `userspace/cp/src/main.rs`, `userspace/mv/src/main.rs`, `userspace/rm/src/main.rs`, `userspace/mkdir/src/main.rs`, `userspace/touch/src/main.rs`, `userspace/head/src/main.rs`, `userspace/tail/src/main.rs`, `userspace/wc/src/main.rs`, `userspace/grep/src/main.rs`, `userspace/ps/src/main.rs` (GNU-close upgrades onto `cli.rs`)
- `Cargo.toml` (15 new members in default-members; `xtask/src/main.rs` build_userspace list)
- `scripts/harness_cases.conf` (~26 new smoke cases: 15 new + 11 upgraded)

---

## Stage 1 — Shared CLI parser (PR 4)

### Task 1.1: Test scaffolding for cli.rs

**Files:**
- Create: `userspace/libcluu/tests/cli_test.rs`

`libcluu` is `no_std` for target build, but tests run on host. Use the existing test feature pattern.

- [ ] **Step 1: Confirm libcluu has a host-test path**

```bash
grep -n 'cfg(test)\|\[features\]\|\[\[test\]\]' userspace/libcluu/Cargo.toml userspace/libcluu/src/lib.rs | head
```

If `libcluu` does not currently support `cargo test`, add a `host-test` feature gate so the cli module compiles for `std`:

In `userspace/libcluu/Cargo.toml`:

```toml
[features]
default = []
posix = []
host-test = []   # enables std for unit tests on host
```

- [ ] **Step 2: Write failing test for "parse single short flag"**

Create `userspace/libcluu/tests/cli_test.rs`:

```rust
#![cfg(feature = "host-test")]

use cluu_libcluu::cli::{Spec, ArgKind, parse};

#[test]
fn parse_single_short_flag() {
    let spec = Spec::new()
        .flag('a', "all", "include hidden");
    let argv = ["prog", "-a"].iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let parsed = parse(&spec, &argv).unwrap();
    assert!(parsed.is_set("all"));
    assert!(parsed.positional.is_empty());
}
```

- [ ] **Step 3: Run, verify it fails**

```bash
cargo test -p libcluu --features host-test --test cli_test parse_single_short_flag
```

Expected: compile error — `cli` module doesn't exist yet. Fail loud.

### Task 1.2: Implement minimal cli.rs

**Files:**
- Create: `userspace/libcluu/src/cli.rs`
- Modify: `userspace/libcluu/src/lib.rs`

- [ ] **Step 1: Add module to lib.rs**

```rust
pub mod cli;
```

- [ ] **Step 2: Write minimal cli.rs to make first test pass**

```rust
//! POSIX-style argument parser. Single-pass. Supports:
//! - clustered short flags: `-rfv`
//! - long options: `--all`, `--color=auto`
//! - `--` end-of-options
//! - optional/required arg attachment
//! - auto-generated `--help` / `--version`

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgKind {
    Flag,           // boolean
    RequiredArg,    // -n VALUE / --name=VALUE / --name VALUE
    OptionalArg,    // -nVALUE / --name=VALUE (no separate-token form)
}

#[derive(Debug, Clone)]
pub struct Opt {
    pub short: Option<char>,
    pub long: &'static str,
    pub help: &'static str,
    pub kind: ArgKind,
}

#[derive(Debug, Default, Clone)]
pub struct Spec {
    pub program: &'static str,
    pub version: &'static str,
    pub usage: &'static str,
    opts: Vec<Opt>,
}

impl Spec {
    pub const fn new() -> Self {
        Spec {
            program: "",
            version: "0.1.0",
            usage: "",
            opts: Vec::new(),
        }
    }

    pub fn program(mut self, name: &'static str) -> Self {
        self.program = name;
        self
    }

    pub fn version(mut self, v: &'static str) -> Self {
        self.version = v;
        self
    }

    pub fn usage(mut self, u: &'static str) -> Self {
        self.usage = u;
        self
    }

    pub fn flag(mut self, short: char, long: &'static str, help: &'static str) -> Self {
        self.opts.push(Opt { short: Some(short), long, help, kind: ArgKind::Flag });
        self
    }

    pub fn long_flag(mut self, long: &'static str, help: &'static str) -> Self {
        self.opts.push(Opt { short: None, long, help, kind: ArgKind::Flag });
        self
    }

    pub fn required(mut self, short: char, long: &'static str, help: &'static str) -> Self {
        self.opts.push(Opt { short: Some(short), long, help, kind: ArgKind::RequiredArg });
        self
    }

    pub fn optional(mut self, short: char, long: &'static str, help: &'static str) -> Self {
        self.opts.push(Opt { short: Some(short), long, help, kind: ArgKind::OptionalArg });
        self
    }
}

#[derive(Debug, Default)]
pub struct Parsed {
    flags: BTreeMap<String, bool>,
    values: BTreeMap<String, String>,
    pub positional: Vec<String>,
}

impl Parsed {
    pub fn is_set(&self, long: &str) -> bool {
        *self.flags.get(long).unwrap_or(&false)
    }
    pub fn value(&self, long: &str) -> Option<&str> {
        self.values.get(long).map(|s| s.as_str())
    }
}

#[derive(Debug)]
pub enum CliError {
    UnknownOption(String),
    MissingValue(String),
    HelpRequested,
    VersionRequested,
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            CliError::UnknownOption(s) => write!(f, "unknown option: {}", s),
            CliError::MissingValue(s)  => write!(f, "missing value for: {}", s),
            CliError::HelpRequested    => write!(f, "(help)"),
            CliError::VersionRequested => write!(f, "(version)"),
        }
    }
}

pub fn parse(spec: &Spec, argv: &[String]) -> Result<Parsed, CliError> {
    let mut out = Parsed::default();
    let mut i = 1usize; // skip argv[0]
    let mut after_dd = false;

    while i < argv.len() {
        let a = &argv[i];
        if after_dd {
            out.positional.push(a.clone());
            i += 1;
            continue;
        }
        if a == "--" {
            after_dd = true;
            i += 1;
            continue;
        }
        if a == "--help" {
            return Err(CliError::HelpRequested);
        }
        if a == "--version" {
            return Err(CliError::VersionRequested);
        }
        if let Some(rest) = a.strip_prefix("--") {
            let (name, value) = match rest.find('=') {
                Some(p) => (&rest[..p], Some(&rest[p+1..])),
                None    => (rest, None),
            };
            let opt = spec.opts.iter().find(|o| o.long == name)
                .ok_or_else(|| CliError::UnknownOption(format!("--{}", name)))?;
            match opt.kind {
                ArgKind::Flag => { out.flags.insert(opt.long.to_string(), true); }
                ArgKind::RequiredArg => {
                    let v = match value {
                        Some(v) => v.to_string(),
                        None => {
                            i += 1;
                            argv.get(i).cloned().ok_or_else(|| CliError::MissingValue(format!("--{}", name)))?
                        }
                    };
                    out.values.insert(opt.long.to_string(), v);
                }
                ArgKind::OptionalArg => {
                    if let Some(v) = value {
                        out.values.insert(opt.long.to_string(), v.to_string());
                    } else {
                        out.flags.insert(opt.long.to_string(), true);
                    }
                }
            }
            i += 1;
            continue;
        }
        if let Some(rest) = a.strip_prefix('-') {
            if rest.is_empty() {
                out.positional.push(a.clone());
                i += 1;
                continue;
            }
            let mut chars = rest.chars().peekable();
            while let Some(c) = chars.next() {
                let opt = spec.opts.iter().find(|o| o.short == Some(c))
                    .ok_or_else(|| CliError::UnknownOption(format!("-{}", c)))?;
                match opt.kind {
                    ArgKind::Flag => { out.flags.insert(opt.long.to_string(), true); }
                    ArgKind::RequiredArg => {
                        let v: String = chars.collect();
                        let v = if !v.is_empty() {
                            v
                        } else {
                            i += 1;
                            argv.get(i).cloned().ok_or_else(|| CliError::MissingValue(format!("-{}", c)))?
                        };
                        out.values.insert(opt.long.to_string(), v);
                        break;
                    }
                    ArgKind::OptionalArg => {
                        let v: String = chars.collect();
                        if !v.is_empty() {
                            out.values.insert(opt.long.to_string(), v);
                        } else {
                            out.flags.insert(opt.long.to_string(), true);
                        }
                        break;
                    }
                }
            }
            i += 1;
            continue;
        }
        out.positional.push(a.clone());
        i += 1;
    }
    Ok(out)
}

pub fn render_help(spec: &Spec) -> String {
    let mut s = String::new();
    s.push_str("Usage: ");
    s.push_str(spec.program);
    s.push(' ');
    s.push_str(spec.usage);
    s.push_str("\n\nOptions:\n");
    for o in &spec.opts {
        s.push_str("  ");
        if let Some(c) = o.short {
            s.push('-');
            s.push(c);
            s.push_str(", ");
        } else {
            s.push_str("    ");
        }
        s.push_str("--");
        s.push_str(o.long);
        s.push_str("  ");
        s.push_str(o.help);
        s.push('\n');
    }
    s
}
```

- [ ] **Step 3: Run first test, verify it passes**

```bash
cargo test -p libcluu --features host-test --test cli_test parse_single_short_flag
```

Expected: 1 passed.

### Task 1.3: Test clustered short flags

- [ ] **Step 1: Add failing test**

```rust
#[test]
fn parse_clustered_short_flags() {
    let spec = Spec::new()
        .flag('r', "recursive", "")
        .flag('f', "force", "")
        .flag('v', "verbose", "");
    let argv = ["rm", "-rfv", "file"].iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let p = parse(&spec, &argv).unwrap();
    assert!(p.is_set("recursive"));
    assert!(p.is_set("force"));
    assert!(p.is_set("verbose"));
    assert_eq!(p.positional, vec!["file".to_string()]);
}
```

- [ ] **Step 2: Run; should already PASS** (parser handles this).

```bash
cargo test -p libcluu --features host-test --test cli_test parse_clustered_short_flags
```

If FAIL, fix the implementation in `cli.rs` until it passes.

### Task 1.4: Test required arg attachment

- [ ] **Step 1: Add tests**

```rust
#[test]
fn parse_required_arg_separate() {
    let spec = Spec::new().required('n', "lines", "");
    let argv = ["head", "-n", "5", "file"].iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let p = parse(&spec, &argv).unwrap();
    assert_eq!(p.value("lines"), Some("5"));
    assert_eq!(p.positional, vec!["file".to_string()]);
}

#[test]
fn parse_required_arg_attached() {
    let spec = Spec::new().required('n', "lines", "");
    let argv = ["head", "-n5", "file"].iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let p = parse(&spec, &argv).unwrap();
    assert_eq!(p.value("lines"), Some("5"));
}

#[test]
fn parse_long_required_eq() {
    let spec = Spec::new().required('n', "lines", "");
    let argv = ["head", "--lines=5", "file"].iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let p = parse(&spec, &argv).unwrap();
    assert_eq!(p.value("lines"), Some("5"));
}

#[test]
fn parse_long_required_space() {
    let spec = Spec::new().required('n', "lines", "");
    let argv = ["head", "--lines", "5", "file"].iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let p = parse(&spec, &argv).unwrap();
    assert_eq!(p.value("lines"), Some("5"));
}
```

- [ ] **Step 2: Run all four; PASS**

```bash
cargo test -p libcluu --features host-test --test cli_test parse_required
cargo test -p libcluu --features host-test --test cli_test parse_long_required
```

### Task 1.5: Test `--` end-of-options and unknown-option error

```rust
#[test]
fn parse_double_dash_terminates_options() {
    let spec = Spec::new().flag('v', "verbose", "");
    let argv = ["x", "-v", "--", "-not-a-flag", "file"].iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let p = parse(&spec, &argv).unwrap();
    assert!(p.is_set("verbose"));
    assert_eq!(p.positional, vec!["-not-a-flag".to_string(), "file".to_string()]);
}

#[test]
fn parse_unknown_option_errors() {
    let spec = Spec::new().flag('v', "verbose", "");
    let argv = ["x", "-z"].iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match parse(&spec, &argv) {
        Err(CliError::UnknownOption(_)) => {}
        other => panic!("expected UnknownOption, got {:?}", other),
    }
}

#[test]
fn parse_help_requested() {
    let spec = Spec::new().flag('v', "verbose", "");
    let argv = ["x", "--help"].iter().map(|s| s.to_string()).collect::<Vec<_>>();
    matches!(parse(&spec, &argv), Err(CliError::HelpRequested));
}
```

- [ ] **Step 1: Add the three tests, run, PASS.**

```bash
cargo test -p libcluu --features host-test --test cli_test
```

Expected: 9+ tests PASS.

### Task 1.6: Commit cli.rs PR

- [ ] **Step 1: Run full libcluu host tests**

```bash
cargo test -p libcluu --features host-test
```

PASS.

- [ ] **Step 2: Confirm target build still green**

```bash
cargo xtask build 2>&1 | tail -10
```

PASS.

- [ ] **Step 3: Commit**

```bash
git add userspace/libcluu/src/cli.rs userspace/libcluu/src/lib.rs userspace/libcluu/tests/cli_test.rs userspace/libcluu/Cargo.toml
git commit -m "$(cat <<'EOF'
feat(libcluu): add shared POSIX-style cli argument parser

New libcluu::cli module: single-pass parser supporting clustered
short flags, long options, required/optional arg attachment, --,
auto-generated --help/--version. Covered by host-side unit tests
(host-test feature). Replaces ad-hoc per-util arg loops.

Phase 4 Plan B Stage 1.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Stage 2 — Migrate existing utils onto cli.rs (PR 7, may split if >800 LOC)

### Task 2.1: Migrate `cat` and add `-n -b -A -E -T -s`

**Files:**
- Modify: `userspace/cat/src/main.rs`
- Modify: `scripts/harness_cases.conf` (add `l2_cat_basic`)

- [ ] **Step 1: Sketch new cat skeleton using cli.rs**

```rust
#![no_std]
#![no_main]

extern crate alloc;
#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use libcluu::cli::{Spec, parse, render_help, CliError};
use libcluu::posix::{_write, open, read, close};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let argv: Vec<String> = libcluu::args::args();
    let spec = Spec::new()
        .program("cat")
        .usage("[OPTION]... [FILE]...")
        .flag('n', "number", "number all output lines")
        .flag('b', "number-nonblank", "number nonempty output lines")
        .flag('A', "show-all", "equivalent to -ET")
        .flag('E', "show-ends", "display $ at end of each line")
        .flag('T', "show-tabs", "display TAB characters as ^I")
        .flag('s', "squeeze-blank", "suppress repeated empty output lines");

    let parsed = match parse(&spec, &argv) {
        Ok(p) => p,
        Err(CliError::HelpRequested)    => { let h = render_help(&spec); let _ = _write(1, h.as_ptr() as *const _, h.len()); return 0; }
        Err(CliError::VersionRequested) => { let v = format!("cat 0.1.0\n"); let _ = _write(1, v.as_ptr() as *const _, v.len()); return 0; }
        Err(e) => { let m = format!("cat: {}\n", e); let _ = _write(2, m.as_ptr() as *const _, m.len()); return 2; }
    };

    let show_all = parsed.is_set("show-all");
    let opts = CatOpts {
        number:     parsed.is_set("number") || parsed.is_set("number-nonblank"),
        nonblank:   parsed.is_set("number-nonblank"),
        show_ends:  show_all || parsed.is_set("show-ends"),
        show_tabs:  show_all || parsed.is_set("show-tabs"),
        squeeze:    parsed.is_set("squeeze-blank"),
    };

    if parsed.positional.is_empty() {
        return cat_fd(0, &opts);
    }
    let mut rc = 0;
    for path in &parsed.positional {
        if path == "-" {
            if cat_fd(0, &opts) != 0 { rc = 1; }
        } else {
            match open_path(path) {
                Ok(fd) => { if cat_fd(fd, &opts) != 0 { rc = 1; } let _ = close(fd); }
                Err(e) => { let m = format!("cat: {}: {:?}\n", path, e); let _ = _write(2, m.as_ptr() as *const _, m.len()); rc = 1; }
            }
        }
    }
    rc
}

struct CatOpts { number: bool, nonblank: bool, show_ends: bool, show_tabs: bool, squeeze: bool }

fn cat_fd(fd: usize, opts: &CatOpts) -> i32 {
    // Read into a buffer; apply line-by-line transformations; write to stdout.
    // [PASTE: existing cat read loop, augmented with the four transforms below]
    let mut buf = [0u8; 4096];
    let mut line_no: u64 = 0;
    let mut prev_blank = false;
    loop {
        let n = unsafe { read(fd, buf.as_mut_ptr() as *mut _, buf.len()) };
        if n <= 0 { break; }
        // For first cut, do non-streaming line scanning per chunk. Real impl
        // must hold a partial-line carry buffer across reads. Engineer fills
        // that in if a smoke fails on multi-chunk inputs.
        let s = core::str::from_utf8(&buf[..n as usize]).unwrap_or("");
        for line in s.split_inclusive('\n') {
            let body = line.trim_end_matches('\n');
            if opts.squeeze && body.is_empty() && prev_blank { continue; }
            prev_blank = body.is_empty();
            if opts.nonblank {
                if !body.is_empty() { line_no += 1; }
            } else if opts.number {
                line_no += 1;
            }
            let mut out = String::new();
            if (opts.number && !opts.nonblank) || (opts.nonblank && !body.is_empty()) {
                out.push_str(&format!("{:>6}\t", line_no));
            }
            for ch in body.chars() {
                if ch == '\t' && opts.show_tabs { out.push_str("^I"); } else { out.push(ch); }
            }
            if opts.show_ends && line.ends_with('\n') {
                out.push('$');
            }
            if line.ends_with('\n') { out.push('\n'); }
            let _ = _write(1, out.as_ptr() as *const _, out.len());
        }
    }
    0
}

fn open_path(path: &str) -> Result<usize, libcluu::Error> {
    open(path, 0, 0).map(|f| f.fd)
}
```

- [ ] **Step 2: Build**

```bash
cargo xtask build 2>&1 | tail -10
```

Green.

- [ ] **Step 3: Add smoke test case**

In `scripts/harness_cases.conf`:

```
l2_cat_basic|full|MARKER_MODE=l2_cat_basic TEST_COMMAND_REPEAT=1 RUN_WAIT=15
```

Add the case body in `scripts/harness_case_defaults.sh`:

```sh
        l2_cat_basic)
            SHELL_AUTOSTART_CMD_DEFAULT="echo -e 'one\ntwo\nthree' > /tmp/cat.in; cat -n /tmp/cat.in"
            EXPECTED_CONTAINS=("     1\tone" "     2\ttwo" "     3\tthree")
            ;;
```

- [ ] **Step 4: Run the smoke**

```bash
bash scripts/harness_run.sh l2_cat_basic 2>&1 | tail -10
```

PASS.

- [ ] **Step 5: Commit**

```bash
git add userspace/cat scripts/harness_cases.conf scripts/harness_case_defaults.sh
git commit -m "feat(cat): GNU-close flags (-n -b -A -E -T -s); migrate to cli.rs

Phase 4 Plan B Stage 2.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

### Task 2.2 — 2.11: Migrate `cp`, `mv`, `rm`, `mkdir`, `touch`, `head`, `tail`, `wc`, `grep`, `ps`

For each util, the recipe is identical:

1. Open `userspace/<util>/src/main.rs`.
2. Replace the ad-hoc arg loop with a `Spec` definition matching the flag matrix in spec §5.2.
3. Map each flag onto the existing util body's behavior.
4. Add a smoke case `l2_<util>_basic` in `scripts/harness_cases.conf` and `scripts/harness_case_defaults.sh`.
5. Run the smoke; PASS.
6. Commit per util.

Below is the per-util Spec to use.

#### 2.2 cp

```rust
let spec = Spec::new().program("cp").usage("[-rfivpn] SOURCE... DEST")
    .flag('r', "recursive", "copy directories recursively")
    .flag('R', "recursive-alias", "alias for -r")
    .flag('i', "interactive", "prompt before overwrite")
    .flag('f', "force", "overwrite existing without prompt")
    .flag('v', "verbose", "explain what is being done")
    .flag('p', "preserve", "preserve mode and mtime")
    .flag('n', "no-clobber", "do not overwrite existing files");
```

Smoke `l2_cp_recursive`:
```sh
SHELL_AUTOSTART_CMD_DEFAULT="mkdir -p /tmp/src/sub; echo a > /tmp/src/a; echo b > /tmp/src/sub/b; cp -r /tmp/src /tmp/dst; ls /tmp/dst/sub"
EXPECTED_CONTAINS=("b")
```

#### 2.3 mv

```rust
let spec = Spec::new().program("mv").usage("[-ifvn] SOURCE... DEST")
    .flag('i', "interactive", "prompt before overwrite")
    .flag('f', "force", "overwrite existing without prompt")
    .flag('n', "no-clobber", "do not overwrite existing files")
    .flag('v', "verbose", "");
```

Smoke covered by existing `l2_mv` if the flag set works; otherwise add `l2_mv_force`.

#### 2.4 rm

```rust
let spec = Spec::new().program("rm").usage("[-rfivd] FILE...")
    .flag('r', "recursive", "")
    .flag('R', "recursive-alias", "")
    .flag('i', "interactive", "")
    .flag('f', "force", "")
    .flag('v', "verbose", "")
    .flag('d', "dir", "remove empty directories");
```

#### 2.5 mkdir

```rust
let spec = Spec::new().program("mkdir").usage("[-pv] [-m MODE] DIR...")
    .flag('p', "parents", "make parent directories as needed")
    .flag('v', "verbose", "")
    .required('m', "mode", "set file mode (octal)");
```

#### 2.6 touch

```rust
let spec = Spec::new().program("touch").usage("[-cam] [-r REF] [-d STRING] FILE...")
    .flag('c', "no-create", "do not create files")
    .flag('a', "atime", "change only access time")
    .flag('m', "mtime", "change only modification time")
    .required('r', "reference", "use REF's times")
    .required('d', "date", "use STRING as time");
```

#### 2.7 head

```rust
let spec = Spec::new().program("head").usage("[-cn N] [-qv] [FILE]...")
    .required('n', "lines", "print N lines (default 10)")
    .required('c', "bytes", "print N bytes")
    .flag('q', "quiet", "never print headers")
    .flag('v', "verbose", "always print headers");
```

Multi-file headers: when `parsed.positional.len() > 1` and not `-q`, before each file print `==> FILENAME <==\n`.

#### 2.8 tail

```rust
let spec = Spec::new().program("tail").usage("[-cn N] [-qvf] [FILE]...")
    .required('n', "lines", "")
    .required('c', "bytes", "")
    .flag('q', "quiet", "")
    .flag('v', "verbose", "")
    .flag('f', "follow", "follow file as it grows (uses poll)");
```

`-f` implementation: open file, read to EOF, then `poll(fd, POLLIN)` in a loop, re-reading on event. If poll wiring not available for plain files, print `tail: -f not supported on this fd type\n` to stderr and exit 1.

#### 2.9 wc

```rust
let spec = Spec::new().program("wc").usage("[-lwcmL] [FILE]...")
    .flag('l', "lines", "")
    .flag('w', "words", "")
    .flag('c', "bytes", "")
    .flag('m', "chars", "")
    .flag('L', "max-line-length", "");
```

If no flags set, default to `lines + words + bytes`. Multi-file totals row at the end.

#### 2.10 grep

```rust
let spec = Spec::new().program("grep").usage("[OPTIONS] PATTERN [FILE]...")
    .flag('i', "ignore-case", "")
    .flag('v', "invert-match", "")
    .flag('n', "line-number", "")
    .flag('c', "count", "")
    .flag('l', "files-with-matches", "")
    .flag('L', "files-without-match", "")
    .flag('r', "recursive", "")
    .flag('R', "dereference-recursive", "")
    .flag('w', "word-regexp", "")
    .flag('x', "line-regexp", "")
    .flag('E', "extended-regexp", "")
    .flag('F', "fixed-strings", "")
    .flag('q', "quiet", "")
    .flag('H', "with-filename", "")
    .flag('h', "no-filename", "")
    .optional(' ', "color", "always|never|auto");
```

`-r` walks directories using the existing `VfsClient::readdir`. `--color=auto` enables only when `isatty(1)` (use `libcluu::posix::isatty`).

#### 2.11 ps

```rust
let spec = Spec::new().program("ps").usage("[-eAfl] [-u USER]")
    .flag('e', "every", "")
    .flag('A', "all-procs", "")
    .flag('f', "full", "")
    .flag('l', "long", "")
    .required('u', "user", "filter by user");
```

Columns from procmgr's `PROCMGR_PROC_QUERY_LABEL` data. For `-f`: `UID PID PPID C STIME TTY TIME CMD`. For `-l`: same plus `S` state column.

### Task 2.12: Run shell-impacting + util harness, commit Stage 2

- [ ] **Step 1: Run all l2_<util>_* cases**

```bash
for c in $(grep -oE 'l2_[a-z_]+_basic' scripts/harness_cases.conf); do
    bash scripts/harness_run.sh $c 2>&1 | tail -3
done
```

Expected: every smoke green.

- [ ] **Step 2: Run full matrix**

```bash
bash scripts/harness_matrix.sh 2>&1 | tee /tmp/matrix-after-B2.log
```

Green.

- [ ] **Step 3: Commit Stage 2 (if not committed per-util)**

If you committed each util separately (recommended), skip this. Otherwise:

```bash
git add -A
git commit -m "feat(coreutils): GNU-close flag sets via shared cli.rs

11 utils migrated: cat, cp, mv, rm, mkdir, touch, head, tail, wc, grep, ps.
Each gets full POSIX short-flag matrix + smoke test l2_<util>_basic.

Phase 4 Plan B Stage 2.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Stage 3 — New utils, batch A (PR 8: env, sleep, basename, dirname, date, kill, printf, which)

For each new util, recipe:
1. `cargo new --bin userspace/<util>` (manually — set `[package].name = "cluu-<util>"`, copy template Cargo.toml from `userspace/cat/Cargo.toml`).
2. Add to workspace `members` and `default-members`.
3. Add to `xtask/src/main.rs::build_userspace` userspace_crates array.
4. Implement `main.rs` using cli.rs.
5. Add smoke case.
6. Build, run smoke, PASS.
7. Commit.

Below: minimum viable spec + main.rs body for each.

### Task 3.1: env

**Files:**
- Create: `userspace/env/Cargo.toml`
- Create: `userspace/env/src/main.rs`

```rust
// main.rs
#![no_std]
#![no_main]
extern crate alloc;
#[allow(unused_imports)]
use libcluu::runtime as _;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use libcluu::cli::{Spec, parse, CliError};
use libcluu::posix::{_write, getenv, environ_iter, execvp};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let argv: Vec<String> = libcluu::args::args();
    let spec = Spec::new().program("env").usage("[NAME=VALUE...] [COMMAND [ARGS]...]")
        .flag('i', "ignore-environment", "start with empty env");
    let parsed = match parse(&spec, &argv) {
        Ok(p) => p,
        Err(CliError::HelpRequested) => { let h = libcluu::cli::render_help(&spec); let _ = _write(1, h.as_ptr() as *const _, h.len()); return 0; }
        Err(e) => { let m = format!("env: {}\n", e); let _ = _write(2, m.as_ptr() as *const _, m.len()); return 2; }
    };

    // Split positional into KEY=VAL assignments and command.
    let mut assignments: Vec<(String, String)> = Vec::new();
    let mut cmd_idx: usize = parsed.positional.len();
    for (idx, p) in parsed.positional.iter().enumerate() {
        if let Some(eq) = p.find('=') {
            if p[..eq].chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                assignments.push((p[..eq].to_string(), p[eq+1..].to_string()));
                continue;
            }
        }
        cmd_idx = idx;
        break;
    }

    if cmd_idx == parsed.positional.len() {
        // No command: print env.
        for (k, v) in environ_iter() {
            let line = format!("{}={}\n", k, v);
            let _ = _write(1, line.as_ptr() as *const _, line.len());
        }
        return 0;
    }

    // Apply assignments, exec the command.
    for (k, v) in &assignments { libcluu::posix::setenv(k, v, true); }
    let prog = &parsed.positional[cmd_idx];
    let rest: Vec<&str> = parsed.positional[cmd_idx..].iter().map(|s| s.as_str()).collect();
    let rc = execvp(prog, &rest);
    rc
}
```

- [ ] **Step 1: Verify `environ_iter`, `setenv`, `execvp` exist in libcluu**

```bash
grep -n 'pub fn environ_iter\|pub fn setenv\|pub fn execvp' userspace/libcluu/src/posix/*.rs
```

If missing, add stubs to `userspace/libcluu/src/posix/process.rs` (env handling) and `userspace/libcluu/src/posix/exec.rs`. Body: read/write process env table; execvp = spawn-and-wait with arg vector. Defer if too deep — alternative is to call procmgr spawn IPC directly.

- [ ] **Step 2: Cargo.toml, workspace, xtask, smoke, commit.**

Smoke `l2_env_basic`:
```sh
SHELL_AUTOSTART_CMD_DEFAULT="FOO=bar env | grep FOO"
EXPECTED_CONTAINS=("FOO=bar")
```

### Task 3.2: sleep

**Files:**
- Create: `userspace/sleep/Cargo.toml`, `src/main.rs`

```rust
#![no_std]
#![no_main]
extern crate alloc;
#[allow(unused_imports)]
use libcluu::runtime as _;
use alloc::format;
use alloc::vec::Vec;
use alloc::string::String;
use libcluu::cli::{Spec, parse, CliError};
use libcluu::posix::{_write, nanosleep};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let argv: Vec<String> = libcluu::args::args();
    let spec = Spec::new().program("sleep").usage("SECONDS");
    let parsed = match parse(&spec, &argv) {
        Ok(p) => p,
        Err(_) => return 2,
    };
    if parsed.positional.is_empty() {
        let m = b"sleep: missing operand\n";
        let _ = _write(2, m.as_ptr() as *const _, m.len());
        return 2;
    }
    let secs: u64 = match parsed.positional[0].parse() {
        Ok(n) => n,
        Err(_) => {
            let m = format!("sleep: invalid time: {}\n", parsed.positional[0]);
            let _ = _write(2, m.as_ptr() as *const _, m.len());
            return 2;
        }
    };
    nanosleep(secs, 0);
    0
}
```

If `nanosleep` not in libcluu, look at how `top` handles delay-loops — reuse the same primitive.

Smoke `l2_sleep_basic`:
```sh
SHELL_AUTOSTART_CMD_DEFAULT="sleep 1; echo done"
EXPECTED_CONTAINS=("done")
```

### Task 3.3: basename

```rust
#![no_std]
#![no_main]
extern crate alloc;
#[allow(unused_imports)]
use libcluu::runtime as _;
use alloc::vec::Vec;
use alloc::string::String;
use libcluu::cli::{Spec, parse};
use libcluu::posix::_write;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let argv: Vec<String> = libcluu::args::args();
    let spec = Spec::new().program("basename").usage("PATH [SUFFIX]");
    let parsed = parse(&spec, &argv).unwrap_or_default();
    if parsed.positional.is_empty() { return 2; }
    let path = &parsed.positional[0];
    let suffix = parsed.positional.get(1).map(|s| s.as_str()).unwrap_or("");
    let trimmed = path.trim_end_matches('/');
    let base = match trimmed.rfind('/') {
        Some(p) => &trimmed[p+1..],
        None    => trimmed,
    };
    let out = if !suffix.is_empty() && base.ends_with(suffix) && base.len() > suffix.len() {
        &base[..base.len() - suffix.len()]
    } else { base };
    let _ = _write(1, out.as_ptr() as *const _, out.len());
    let _ = _write(1, b"\n".as_ptr() as *const _, 1);
    0
}
```

Smoke `l2_basename_basic`:
```sh
SHELL_AUTOSTART_CMD_DEFAULT="basename /etc/users.toml"
EXPECTED_CONTAINS=("users.toml")
```

### Task 3.4: dirname

Same shape as basename, returns the directory part:

```rust
let trimmed = path.trim_end_matches('/');
let dir = match trimmed.rfind('/') {
    Some(0) => "/",
    Some(p) => &trimmed[..p],
    None    => ".",
};
```

Smoke `l2_dirname_basic`:
```sh
SHELL_AUTOSTART_CMD_DEFAULT="dirname /etc/users.toml"
EXPECTED_CONTAINS=("/etc")
```

### Task 3.5: date

Reads timeserver via libcluu, formats per `%Y-%m-%d %H:%M:%S` default.

```rust
let spec = Spec::new().program("date").usage("[-u] [+FORMAT]")
    .flag('u', "utc", "use UTC");
// positional[0] starting with '+' = format string
```

Use `libcluu::time::current_time()` (verify it exists; if not, add it as a thin wrapper around timeserver IPC).

Smoke `l2_date_basic`: assert output starts with `20` (year 20XX).

### Task 3.6: kill

```rust
let spec = Spec::new().program("kill").usage("[-s SIG | -SIG] PID...")
    .required('s', "signal", "signal name or number");
```

Look up signal numbers from `libcluu::signal::*`. Send via procmgr IPC `PROCMGR_PG_SIGNAL` once Plan D lands; for now (Plan B is independent of D), call existing `procmgr` kill primitive.

Smoke `l2_kill_basic`:
```sh
SHELL_AUTOSTART_CMD_DEFAULT="sleep 30 & PID=$!; kill -TERM $PID; wait"
EXPECTED_CONTAINS=("Terminated")
```

### Task 3.7: printf

```rust
let spec = Spec::new().program("printf").usage("FORMAT [ARG]...");
```

Implement a minimal format-string interpreter handling `%s %d %x %c \n \t \\`. Reject unknown specifiers with exit 2.

Smoke `l2_printf_basic`:
```sh
SHELL_AUTOSTART_CMD_DEFAULT='printf "%s=%d\n" foo 42'
EXPECTED_CONTAINS=("foo=42")
```

### Task 3.8: which

Walks `PATH`, prints first match:

```rust
let spec = Spec::new().program("which").usage("COMMAND...")
    .flag('a', "all", "print all matches, not just first");
```

Smoke `l2_which_basic`:
```sh
SHELL_AUTOSTART_CMD_DEFAULT="which ls"
EXPECTED_CONTAINS=("/bin/ls")
```

### Task 3.9: Run all batch-A smokes; commit batch A

```bash
for c in l2_env_basic l2_sleep_basic l2_basename_basic l2_dirname_basic l2_date_basic l2_kill_basic l2_printf_basic l2_which_basic; do
    bash scripts/harness_run.sh $c 2>&1 | tail -3
done
```

PASS all 8.

```bash
git add -A
git commit -m "feat(coreutils): add env, sleep, basename, dirname, date, kill, printf, which

8 new utils built on libcluu::cli. Each ships with l2_<util>_basic smoke.

Phase 4 Plan B Stage 3.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Stage 4 — New utils, batch B (PR 9: sort, uniq, cut, tr, find, du, stat)

Same recipe per util.

### Task 4.1: sort

```rust
let spec = Spec::new().program("sort").usage("[-nrukF] [FILE]...")
    .flag('n', "numeric-sort", "")
    .flag('r', "reverse", "")
    .flag('u', "unique", "")
    .required('k', "key", "field index")
    .required('t', "field-separator", "");
```

Read all lines, sort, emit. For `-n`, parse leading number per line. For `-k N`, split by `-t` separator and use field N as key.

Smoke `l2_sort_basic`:
```sh
SHELL_AUTOSTART_CMD_DEFAULT='printf "c\nb\na\n" | sort'
EXPECTED_CONTAINS=("a\nb\nc")
```

### Task 4.2: uniq

```rust
let spec = Spec::new().program("uniq").usage("[-cd] [FILE]")
    .flag('c', "count", "")
    .flag('d', "repeated", "");
```

Smoke `l2_uniq_basic`:
```sh
SHELL_AUTOSTART_CMD_DEFAULT='printf "a\na\nb\n" | uniq -c'
EXPECTED_CONTAINS=("2 a" "1 b")
```

### Task 4.3: cut

```rust
let spec = Spec::new().program("cut").usage("[-f LIST -d DELIM | -c LIST] [FILE]...")
    .required('f', "fields", "")
    .required('d', "delimiter", "")
    .required('c', "characters", "");
```

`LIST` is comma-separated indices and/or ranges (e.g. `1,3-5`).

Smoke `l2_cut_basic`:
```sh
SHELL_AUTOSTART_CMD_DEFAULT='echo "a:b:c" | cut -d: -f2'
EXPECTED_CONTAINS=("b")
```

### Task 4.4: tr

```rust
let spec = Spec::new().program("tr").usage("[-ds] SET1 [SET2]")
    .flag('d', "delete", "")
    .flag('s', "squeeze-repeats", "");
```

Char-class translation. For first cut, no `[:class:]` shortcuts; just literal characters and `a-z` ranges.

Smoke `l2_tr_basic`:
```sh
SHELL_AUTOSTART_CMD_DEFAULT='echo abc | tr a-z A-Z'
EXPECTED_CONTAINS=("ABC")
```

### Task 4.5: find

```rust
let spec = Spec::new().program("find").usage("PATH [-name PATTERN] [-type f|d] [-print]")
    .required(' ', "name", "filename pattern (glob)")
    .required(' ', "type", "f, d, l")
    .long_flag("print", "print matched paths");
```

Walk DFS via `VfsClient::readdir`. Match `-name` against `entry.name`. `-type f` filters regular files; `-type d` directories.

Note: cli.rs allows long-only flags via `long_flag()`. If `find -name` (no `--`) syntax needed, that's classic find non-POSIX behavior — extend cli.rs to accept long opts with single dash, or pre-process argv. Pre-process is cleaner: walk argv, replace `-name`/`-type`/`-print` with `--name`/`--type`/`--print` before `parse()`.

Smoke `l2_find_basic`:
```sh
SHELL_AUTOSTART_CMD_DEFAULT='mkdir -p /tmp/f; touch /tmp/f/a.txt; find /tmp/f -name "*.txt"'
EXPECTED_CONTAINS=("/tmp/f/a.txt")
```

### Task 4.6: du

```rust
let spec = Spec::new().program("du").usage("[-sh] PATH...")
    .flag('s', "summarize", "")
    .flag('h', "human-readable", "");
```

Walk directory tree, sum `stat.blocks` per VFS extended stat (Plan C dependency). If Plan C hasn't landed, fall back to summing `stat.size` and document.

Smoke `l2_du_basic`:
```sh
SHELL_AUTOSTART_CMD_DEFAULT='du -s /etc'
EXPECTED_CONTAINS=("/etc")
```

### Task 4.7: stat

```rust
let spec = Spec::new().program("stat").usage("[-c FORMAT] FILE...")
    .required('c', "format", "format string");
```

Default output (no `-c`):
```
  File: /etc/users.toml
  Size: 312       Blocks: 1          IO Block: 4096   regular file
Access: (0644/-rw-r--r--)  Uid: (0)  Gid: (0)
Modify: 2026-05-07 19:00:00
```

Smoke `l2_stat_basic`:
```sh
SHELL_AUTOSTART_CMD_DEFAULT='stat /etc/users.toml | head -1'
EXPECTED_CONTAINS=("File:" "users.toml")
```

### Task 4.8: Run batch-B smokes; commit

```bash
for c in l2_sort_basic l2_uniq_basic l2_cut_basic l2_tr_basic l2_find_basic l2_du_basic l2_stat_basic; do
    bash scripts/harness_run.sh $c 2>&1 | tail -3
done
```

PASS all 7.

```bash
git add -A
git commit -m "feat(coreutils): add sort, uniq, cut, tr, find, du, stat

7 new utils round out Plan B's coreutils target. Each smoke-tested.

Phase 4 Plan B Stage 4.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Self-review

- **Spec coverage**: §5.1 → Stages 3+4. §5.2 → Stage 2. §5.3 → Stage 1. §5.5 YAGNI list (no `--si`, `--block-size`, locale-aware, regex backrefs) preserved by the per-util specs.
- **Placeholders**: each util has full Spec + smoke. Where existing util body is reused verbatim (e.g. cat read loop) the plan says "[PASTE: existing X read loop]" — concrete copy instruction.
- **Type consistency**: `Spec`/`Parsed`/`CliError` consistent across all utils. `_write`, `getenv`, `setenv`, `nanosleep`, `execvp` consistent across new utils — Task 3.1 step 1 verifies they exist.
- **Risk**: Several libcluu primitives (`environ_iter`, `setenv`, `execvp`, `nanosleep`, `current_time`, `isatty`) may not all exist. Each task's first step verifies; if missing, add a thin wrapper (5-30 LOC each) or pre-emptively bundle them into a "libcluu prep" task before Stage 3.

---

## Acceptance

Plan B done when:
- `libcluu::cli` parser exists with 9+ unit tests passing
- 11 existing utils migrated onto cli.rs with GNU-close flag matrices
- 15 new utils ship: env, sleep, basename, dirname, date, kill, printf, which, sort, uniq, cut, tr, find, du, stat
- Each new/upgraded util has an `l2_<util>_basic` smoke that PASSES
- `harness_matrix.sh` green
