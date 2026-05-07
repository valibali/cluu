# Phase 4 Plan A — Workspace Cleanup

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reshape the userspace/ directory so that real user-facing programs are visible at a glance and probe/test fixtures hide behind opt-in build. Split the 3,612-line `commands.rs` into focused modules. Delete or relocate ~19 test-only shell builtins.

**Architecture:** (1) Move 11 probe crates from `userspace/<name>/` to `userspace/probes/<name>/`, drop them from workspace `default-members`, add `cargo xtask build-probes`. (2) Mechanical refactor of `userspace/shell/src/commands.rs` into a `commands/` module hierarchy with a builtin registry trait. (3) Remove test-only builtins from registry; replace with probe binaries under `userspace/probes/`.

**Tech Stack:** Rust workspace, `cargo xtask` build orchestration, existing `Builtin` trait pattern in `commands.rs`.

**Spec reference:** `docs/superpowers/specs/2026-05-07-userspace-polish-design.md` §4

---

## File structure

### Created
- `userspace/probes/argvprobe/` (moved from `userspace/argvprobe/`)
- `userspace/probes/blkprobe/` (moved)
- `userspace/probes/cascadeprobe/` (moved)
- `userspace/probes/detachprobe/` (moved)
- `userspace/probes/escalateprobe/` (moved)
- `userspace/probes/mountprobe/` (moved)
- `userspace/probes/nestprobe/` (moved)
- `userspace/probes/suspendprobe/` (moved)
- `userspace/probes/viewprobe/` (moved)
- `userspace/probes/vqprobe/` (moved)
- `userspace/probes/jobchurn/` (extracted from `JobChurnBuiltin`)
- `userspace/probes/jobmix/` (extracted from `JobMixBuiltin`)
- `userspace/probes/killdeny/` (extracted)
- `userspace/probes/regdeny/` (extracted)
- `userspace/probes/mapfail/` (extracted)
- `userspace/probes/mapcopyfail/` (extracted)
- `userspace/probes/maperror/` (extracted)
- `userspace/probes/ext2io/` (consolidates Ext2Write/Append/Mutate/Unlink)
- `userspace/probes/ownerdeny/` (extracted from `Ext2OwnerDenyBuiltin`)
- `userspace/probes/ringio/` (extracted)
- `userspace/probes/vtcrash/` (extracted)
- `userspace/probes/sudotest/` (extracted)
- `userspace/probes/sutest/` (extracted, includes `SuEqualTestBuiltin`)
- `userspace/shell/src/commands/mod.rs` (registry + dispatch)
- `userspace/shell/src/commands/builtins/{cd,echo,env,alias,jobs,history,help,exit}.rs`
- `userspace/shell/src/commands/exec.rs`
- `userspace/shell/src/commands/redirect.rs`
- `userspace/shell/src/commands/completion.rs`

### Modified
- `Cargo.toml` (workspace `members`, `default-members`)
- `xtask/src/main.rs` (new `BuildProbes` subcommand, update `build_userspace` if probes were there)
- `scripts/harness_case_defaults.sh` (update probe paths)
- `scripts/harness_run.sh` (update probe paths in expected output)
- `userspace/shell/src/main.rs` (import path updates)

### Deleted
- `userspace/shell/src/commands.rs` (split into `commands/`)
- `EscalateDenyBuiltin` registration (duplicate of `escalateprobe`)

---

## Stage 1 — Probe relocation (PR 1)

### Task 1.1: Audit current probe build path

**Files:** read-only audit

- [ ] **Step 1: Find how probes get built today**

```bash
grep -rn 'argvprobe\|nestprobe' xtask/ scripts/ tools/ 2>/dev/null
grep -n 'userspace/argvprobe\|argvprobe' Cargo.toml
```

Expected: list of every reference that builds, packs, or invokes probes. Capture into a temporary scratch note (terminal output, no commit).

- [ ] **Step 2: Confirm probes appear in `cargo build --workspace` output**

```bash
cargo metadata --no-deps --format-version 1 | python3 -c "import json,sys; d=json.load(sys.stdin); print('\n'.join(p['name'] for p in d['packages'] if 'probe' in p['name']))"
```

Expected: lists every probe package name. If empty, probes are not workspace members — fix before continuing.

- [ ] **Step 3: Find where probe binaries land on the boot image**

```bash
grep -rn 'argvprobe\|/bin/argvprobe\|probe' xtask/src/main.rs | head -30
```

Expected: identifies the function that copies probe ELFs into initrd. Note the function name; later steps update it.

### Task 1.2: Move probe directories

**Files:**
- Move: `userspace/argvprobe/` → `userspace/probes/argvprobe/`
- Move: `userspace/blkprobe/` → `userspace/probes/blkprobe/`
- Move: `userspace/cascadeprobe/` → `userspace/probes/cascadeprobe/`
- Move: `userspace/detachprobe/` → `userspace/probes/detachprobe/`
- Move: `userspace/escalateprobe/` → `userspace/probes/escalateprobe/`
- Move: `userspace/mountprobe/` → `userspace/probes/mountprobe/`
- Move: `userspace/nestprobe/` → `userspace/probes/nestprobe/`
- Move: `userspace/suspendprobe/` → `userspace/probes/suspendprobe/`
- Move: `userspace/viewprobe/` → `userspace/probes/viewprobe/`
- Move: `userspace/vqprobe/` → `userspace/probes/vqprobe/`

- [ ] **Step 1: Create probes directory**

```bash
mkdir -p userspace/probes
```

- [ ] **Step 2: Move all probe crates with git mv (preserves history)**

```bash
cd /home/vlb2bp/git/cluu
git mv userspace/argvprobe userspace/probes/argvprobe
git mv userspace/blkprobe userspace/probes/blkprobe
git mv userspace/cascadeprobe userspace/probes/cascadeprobe
git mv userspace/detachprobe userspace/probes/detachprobe
git mv userspace/escalateprobe userspace/probes/escalateprobe
git mv userspace/mountprobe userspace/probes/mountprobe
git mv userspace/nestprobe userspace/probes/nestprobe
git mv userspace/suspendprobe userspace/probes/suspendprobe
git mv userspace/viewprobe userspace/probes/viewprobe
git mv userspace/vqprobe userspace/probes/vqprobe
```

Expected: `git status` shows 10 directory renames, no content changes.

### Task 1.3: Update workspace Cargo.toml

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Update member paths**

Replace each `"userspace/<probe>"` line with `"userspace/probes/<probe>"`. The 10 lines in `members = [ ... ]`:

```toml
    "userspace/probes/argvprobe",
    "userspace/probes/blkprobe",
    "userspace/probes/cascadeprobe",
    "userspace/probes/detachprobe",
    "userspace/probes/escalateprobe",
    "userspace/probes/mountprobe",
    "userspace/probes/nestprobe",
    "userspace/probes/suspendprobe",
    "userspace/probes/viewprobe",
    "userspace/probes/vqprobe",
```

- [ ] **Step 2: Drop probes from default-members**

In `default-members = [ ... ]`, delete every probe entry. The list should retain only kernel + non-probe userspace + tooling.

- [ ] **Step 3: Add probe metadata block at end of Cargo.toml**

```toml
[workspace.metadata.cluu.probes]
crates = [
    "argvprobe",
    "blkprobe",
    "cascadeprobe",
    "detachprobe",
    "escalateprobe",
    "mountprobe",
    "nestprobe",
    "suspendprobe",
    "viewprobe",
    "vqprobe",
]
```

- [ ] **Step 4: Verify workspace parses**

```bash
cargo metadata --format-version 1 > /dev/null
```

Expected: exit 0, no errors. If a path is wrong it errors here.

- [ ] **Step 5: Verify default build excludes probes**

```bash
cargo check --workspace 2>&1 | grep -c 'argvprobe\|probe' || true
```

Expected: 0. Default `cargo check` should not touch probes.

### Task 1.4: Add `xtask build-probes` subcommand

**Files:**
- Modify: `xtask/src/main.rs`

- [ ] **Step 1: Add the subcommand variant**

Locate the `enum Commands` declaration around `xtask/src/main.rs:114` (the `Build {` variant area). Add:

```rust
    /// Build probe/test crates only
    BuildProbes {
        #[arg(long, default_value = "release")]
        profile: String,
    },
    /// Build everything (userspace + kernel + probes)
    BuildAll {
        #[arg(long, default_value = "release")]
        profile: String,
        #[arg(long, value_enum, default_value_t = BuildUi::Rich)]
        ui: BuildUi,
    },
```

- [ ] **Step 2: Wire dispatch**

Locate the `match cli.command { ... }` block around `xtask/src/main.rs:270`. Add arms (full code shown — copy-paste, do not paraphrase):

```rust
        Commands::BuildProbes { profile } => {
            build_probes(&profile)?;
        }
        Commands::BuildAll { profile, ui } => {
            match ui {
                BuildUi::Linear => build_pipeline_linear(&profile),
                BuildUi::Rich => build_pipeline_rich(&profile),
            }?;
            build_probes(&profile)?;
        }
```

- [ ] **Step 3: Implement `build_probes`**

Add this function near `build_userspace` (around line 2238):

```rust
fn build_probes(profile: &str) -> Result<()> {
    println!("▸ Building probe crates...");

    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(project_root().join("Cargo.toml"))
        .exec()
        .context("Failed to read workspace metadata")?;

    let probes = metadata
        .workspace_metadata
        .get("cluu")
        .and_then(|c| c.get("probes"))
        .and_then(|p| p.get("crates"))
        .and_then(|c| c.as_array())
        .ok_or_else(|| anyhow::anyhow!("Missing [workspace.metadata.cluu.probes] crates list"))?;

    let target_json = project_root().join("triplets/x86_64-cluu-user.json");
    let tmp_dir = project_root().join("tmp");
    fs::create_dir_all(&tmp_dir)?;

    for probe_name in probes {
        let name = probe_name.as_str().ok_or_else(|| anyhow::anyhow!("non-string probe name"))?;
        let crate_path = format!("userspace/probes/{}", name);

        println!("  Building probe {}...", name);

        let mut cmd = Command::new("cargo");
        cmd.current_dir(project_root()).args([
            "build",
            "--manifest-path",
            &format!("{}/Cargo.toml", crate_path),
            "--target",
            target_json.to_str().unwrap(),
            "-Z",
            "build-std=core,alloc",
            "-Z",
            "build-std-features=compiler-builtins-mem",
        ]);
        cmd.env("TMPDIR", tmp_dir.as_os_str());

        if profile == "release" {
            cmd.arg("--release");
        }

        let status = cmd.status().context("Failed to run cargo")?;
        if !status.success() {
            bail!("Failed to build probe {}", name);
        }
    }

    println!("  ✓ Probes built");
    Ok(())
}
```

- [ ] **Step 4: Add `cargo_metadata` dependency**

In `xtask/Cargo.toml`, add to `[dependencies]`:

```toml
cargo_metadata = "0.18"
```

- [ ] **Step 5: Build and verify**

```bash
cargo build -p xtask
cargo xtask build-probes 2>&1 | tee /tmp/probe-build.log
```

Expected: every probe builds, ELFs land under `target/x86_64-cluu-user/release/`.

### Task 1.5: Update image packer to load probes from new path

**Files:**
- Modify: `xtask/src/main.rs` (image packing function — name discovered in Task 1.1 step 3)

- [ ] **Step 1: Replace probe ELF lookup paths**

Wherever the image packer references `target/.../argvprobe`, no change is needed — cargo emits the binary by package name, not path. Verify by:

```bash
grep -n '"argvprobe"\|"nestprobe"\|userspace/[a-z]*probe' xtask/src/main.rs
```

If any string-literal probe paths exist, update them to point at `userspace/probes/<probe>`. Bin output path is unchanged (cargo names by package, not directory).

- [ ] **Step 2: Confirm probe binaries land at `/probes/<name>` in initrd, not `/bin/<name>`**

Spec §4 calls for `target/sysroot/probes/<name>` and on-image path `/probes/<name>`. If image packer puts every binary under `/bin/`, add a special case for probe crates: copy to `/probes/<name>` instead.

In the image-packing function, identify the bin-copy loop and add:

```rust
let probe_names: HashSet<&str> = ["argvprobe", "blkprobe", "cascadeprobe",
    "detachprobe", "escalateprobe", "mountprobe", "nestprobe",
    "suspendprobe", "viewprobe", "vqprobe"]
    .iter().copied().collect();

let dest_dir = if probe_names.contains(bin_name) { "probes" } else { "bin" };
```

Use the `dest_dir` to construct the in-image path.

- [ ] **Step 3: Build full image with probes**

```bash
cargo xtask build-all
```

Expected: build succeeds; image contains `/probes/argvprobe` etc.

### Task 1.6: Update harness scripts for new probe paths

**Files:**
- Modify: `scripts/harness_case_defaults.sh`
- Modify: `scripts/harness_run.sh`

- [ ] **Step 1: Update default-cases probe invocations**

In `scripts/harness_case_defaults.sh` line 37:

```bash
SHELL_AUTOSTART_CMD_DEFAULT="spawn /probes/argvprobe hello world"
```

Lines 104 and 107:

```bash
TEST_COMMAND="container run /probes/nestprobe"
TEST_COMMAND="container run /probes/escalateprobe"
```

- [ ] **Step 2: Update expected-output strings in harness_run.sh**

Lines 731-734:

```bash
            "argvprobe: argc=3"
            "argvprobe: arg0=/probes/argvprobe"
            "argvprobe: arg1=hello"
            "argvprobe: arg2=world"
```

Lines 858 and 865 only contain probe-name prefixes; no change needed.

- [ ] **Step 3: Run a probe-using harness case**

```bash
bash scripts/harness_run.sh l2_argv 2>&1 | tail -30
```

Expected: case passes. If not, fix the probe path mismatch before moving on.

### Task 1.7: Run full harness matrix; commit

- [ ] **Step 1: Run matrix**

```bash
bash scripts/harness_matrix.sh 2>&1 | tee /tmp/matrix-after-A1.log
```

Expected: green. If a case fails, fix before commit.

- [ ] **Step 2: Commit Stage 1**

```bash
git add -A
git commit -m "$(cat <<'EOF'
refactor(userspace): move probes under userspace/probes/

Mechanical relocation. 10 probe crates moved from userspace/<name>/ to
userspace/probes/<name>/. Workspace default-members no longer includes
probes; new `cargo xtask build-probes` and `cargo xtask build-all`
subcommands. Image packer places probe binaries at /probes/<name>.
Harness scripts updated.

Phase 4 Plan A Stage 1.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

Expected: commit lands clean, `git status` clean.

---

## Stage 2 — `commands.rs` split (PR 2)

### Task 2.1: Create new module skeleton

**Files:**
- Create: `userspace/shell/src/commands/mod.rs`
- Create: `userspace/shell/src/commands/builtins/mod.rs`
- Create: `userspace/shell/src/commands/builtins/cd.rs`
- Create: `userspace/shell/src/commands/builtins/echo.rs`
- Create: `userspace/shell/src/commands/builtins/env.rs`
- Create: `userspace/shell/src/commands/builtins/alias.rs` (empty stub)
- Create: `userspace/shell/src/commands/builtins/jobs.rs`
- Create: `userspace/shell/src/commands/builtins/history.rs` (empty stub)
- Create: `userspace/shell/src/commands/builtins/help.rs`
- Create: `userspace/shell/src/commands/builtins/exit.rs`
- Create: `userspace/shell/src/commands/exec.rs`
- Create: `userspace/shell/src/commands/redirect.rs`
- Create: `userspace/shell/src/commands/completion.rs`

- [ ] **Step 1: Create empty `commands/mod.rs` with re-exports**

```rust
//! Shell command dispatch and builtins.

pub mod builtins;
pub mod completion;
pub mod exec;
pub mod redirect;

pub use builtins::registry::{Builtin, BuiltinRegistry, BuiltinResult};
```

- [ ] **Step 2: Create `commands/builtins/mod.rs`**

```rust
//! Shell builtin commands.

pub mod registry;
pub mod cd;
pub mod echo;
pub mod env;
pub mod alias;
pub mod jobs;
pub mod history;
pub mod help;
pub mod exit;

pub fn register_all(registry: &mut registry::BuiltinRegistry) {
    cd::register(registry);
    echo::register(registry);
    env::register(registry);
    alias::register(registry);
    jobs::register(registry);
    history::register(registry);
    help::register(registry);
    exit::register(registry);
}
```

- [ ] **Step 3: Create `commands/builtins/registry.rs`**

```rust
//! Builtin trait and registry.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::ShellContext;

pub enum BuiltinResult {
    Ok(i32),
    Err(String),
    NotABuiltin,
}

pub trait Builtin: Send + Sync {
    fn name(&self) -> &'static str;
    fn run(&self, args: &[String], context: &mut ShellContext) -> BuiltinResult;
}

#[derive(Default)]
pub struct BuiltinRegistry {
    by_name: BTreeMap<String, Box<dyn Builtin>>,
}

impl BuiltinRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, builtin: Box<dyn Builtin>) {
        self.by_name.insert(builtin.name().into(), builtin);
    }

    pub fn lookup(&self, name: &str) -> Option<&dyn Builtin> {
        self.by_name.get(name).map(|b| b.as_ref())
    }

    pub fn names(&self) -> Vec<&str> {
        self.by_name.keys().map(|s| s.as_str()).collect()
    }
}
```

### Task 2.2: Move `cd` and `pwd` to dedicated module

**Files:**
- Create: `userspace/shell/src/commands/builtins/cd.rs`

- [ ] **Step 1: Locate current `CdBuiltin` and `PwdBuiltin` structs**

```bash
grep -n 'struct CdBuiltin\|struct PwdBuiltin\|impl Builtin for CdBuiltin\|impl Builtin for PwdBuiltin' userspace/shell/src/commands.rs
```

Capture line ranges into scratch.

- [ ] **Step 2: Move both struct + impl blocks into `cd.rs`**

```rust
//! `cd` and `pwd` builtins — directory navigation.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use super::registry::{Builtin, BuiltinRegistry, BuiltinResult};
use crate::ShellContext;

pub struct CdBuiltin;
pub struct PwdBuiltin;

impl Builtin for CdBuiltin {
    fn name(&self) -> &'static str { "cd" }
    fn run(&self, args: &[String], context: &mut ShellContext) -> BuiltinResult {
        // [PASTE the body of the existing CdBuiltin::run here verbatim]
        unimplemented!()
    }
}

impl Builtin for PwdBuiltin {
    fn name(&self) -> &'static str { "pwd" }
    fn run(&self, args: &[String], context: &mut ShellContext) -> BuiltinResult {
        // [PASTE the body of the existing PwdBuiltin::run here verbatim]
        unimplemented!()
    }
}

pub fn register(registry: &mut BuiltinRegistry) {
    registry.register(Box::new(CdBuiltin));
    registry.register(Box::new(PwdBuiltin));
}
```

Replace `unimplemented!()` with the literal body lifted from `commands.rs`. Do not refactor logic. Only the path of the imports may change.

- [ ] **Step 3: Delete the moved structs from `commands.rs`**

Remove the original `CdBuiltin`/`PwdBuiltin` struct + impl blocks. Leave the `register` calls in place for now (they will be replaced in Task 2.10).

- [ ] **Step 4: Build**

```bash
cargo xtask build 2>&1 | tail -20
```

Expected: build green. If symbols not found, fix `use` paths.

### Task 2.3: Move `echo` to dedicated module

Same pattern as Task 2.2.

**Files:**
- Create: `userspace/shell/src/commands/builtins/echo.rs`
- Modify: `userspace/shell/src/commands.rs`

- [ ] **Step 1: Locate `EchoBuiltin`**

```bash
grep -n 'struct EchoBuiltin\|impl Builtin for EchoBuiltin' userspace/shell/src/commands.rs
```

- [ ] **Step 2: Move struct + impl into `echo.rs`**

Use the same template as cd.rs. Paste the lifted body verbatim.

- [ ] **Step 3: Delete from `commands.rs`. Build green.**

```bash
cargo xtask build 2>&1 | tail -20
```

### Task 2.4: Move env-family builtins

**Files:**
- Create: `userspace/shell/src/commands/builtins/env.rs`

`SetBuiltin`, `ExportBuiltin`, `UnsetBuiltin`, `EnvBuiltin` move together — they share env-vector helpers.

- [ ] **Step 1: Locate all four**

```bash
grep -n 'struct SetBuiltin\|struct ExportBuiltin\|struct UnsetBuiltin\|struct EnvBuiltin' userspace/shell/src/commands.rs
```

- [ ] **Step 2: Move all four structs + impls into `env.rs`**

Single file holds all four. `register` registers them all. Body verbatim from `commands.rs`.

- [ ] **Step 3: Move shared helpers**

Any private free function in `commands.rs` used only by these four (e.g. `parse_kv`, `format_env`) moves with them.

- [ ] **Step 4: Build green.**

### Task 2.5: Move jobs primitives (will be replaced in Plan D)

**Files:**
- Create: `userspace/shell/src/commands/builtins/jobs.rs`

`JobsBuiltin`, `SpawnBuiltin`, `SpawnBgBuiltin`, `StopBuiltin`, `ForegroundBuiltin`, `BackgroundBuiltin` move together. These are pre-jobs primitives; Plan D replaces them with real `cmd &` syntax + proper jobs builtins.

- [ ] **Step 1: Move all six structs + impls into `jobs.rs`**

Verbatim. `register` registers all six.

- [ ] **Step 2: Build green.**

### Task 2.6: Move help, exit, and remaining real builtins

**Files:**
- Create: `userspace/shell/src/commands/builtins/help.rs`
- Create: `userspace/shell/src/commands/builtins/exit.rs`

`HelpBuiltin`, `ClearBuiltin`, `ExitBuiltin`, `PoweroffBuiltin`, `RebootBuiltin`, `TrueBuiltin`, `FalseBuiltin`, `TestBuiltin`, `ExprBuiltin`, `LetBuiltin`, `SuBuiltin`, `SudoBuiltin`, `ContainerBuiltin`, `HeapBuiltin`.

For the split, group into existing files where logical:
- `help.rs`: `HelpBuiltin`, `ClearBuiltin`
- `exit.rs`: `ExitBuiltin`, `PoweroffBuiltin`, `RebootBuiltin`
- `env.rs` (already created): `TrueBuiltin`, `FalseBuiltin`, `TestBuiltin` (test/[ is environment-flavored)
- New `commands/builtins/arith.rs`: `ExprBuiltin`, `LetBuiltin`
- New `commands/builtins/sudo.rs`: `SuBuiltin`, `SudoBuiltin`
- New `commands/builtins/container.rs`: `ContainerBuiltin`, `HeapBuiltin`

- [ ] **Step 1: Create the additional files**

```bash
touch userspace/shell/src/commands/builtins/arith.rs
touch userspace/shell/src/commands/builtins/sudo.rs
touch userspace/shell/src/commands/builtins/container.rs
```

Add their `pub mod` lines to `commands/builtins/mod.rs`. Add their `register::register(registry);` calls to `register_all`.

- [ ] **Step 2: Move each builtin into the matching file**

One PR-sub-step per file. After each, run `cargo xtask build`. Don't batch — easier to bisect if a move breaks compile.

- [ ] **Step 3: Build green after each move.**

### Task 2.7: Move single-command exec into `exec.rs`

**Files:**
- Create: `userspace/shell/src/commands/exec.rs`

- [ ] **Step 1: Identify single-command spawn function**

```bash
grep -n 'fn execute_external\|fn spawn_process\|build_container_run_payload\|fn run_external' userspace/shell/src/commands.rs | head
```

- [ ] **Step 2: Move spawn function + helpers into `exec.rs`**

Move only the single-command path. The pipeline path stays in `pipeline.rs`. Mark functions `pub(crate)` if other modules call them.

- [ ] **Step 3: Build green.**

### Task 2.8: Move redirection parsing into `redirect.rs`

**Files:**
- Create: `userspace/shell/src/commands/redirect.rs`

- [ ] **Step 1: Identify redirection helpers**

```bash
grep -n 'parse_redirect\|RedirSpec\|fn open_redirect\|RedirKind' userspace/shell/src/commands.rs | head
```

- [ ] **Step 2: Move them all into `redirect.rs`**

`pub(crate)` what the rest of the module needs.

- [ ] **Step 3: Build green.**

### Task 2.9: Move tab completion into `completion.rs`

**Files:**
- Create: `userspace/shell/src/commands/completion.rs`

- [ ] **Step 1: Identify completion entry point**

```bash
grep -n 'fn complete_path\|fn tab_complete\|completion' userspace/shell/src/commands.rs | head
```

- [ ] **Step 2: Move into `completion.rs`. Build green.**

### Task 2.10: Replace dispatch table with registry

**Files:**
- Modify: `userspace/shell/src/commands.rs` (now mostly empty)
- Modify: `userspace/shell/src/main.rs` (init registry once)

- [ ] **Step 1: In `commands.rs`, replace the giant `match name { ... }` with a registry call**

```rust
pub fn dispatch_builtin(
    name: &str,
    args: &[String],
    context: &mut ShellContext,
) -> BuiltinResult {
    match context.builtins.lookup(name) {
        Some(b) => b.run(args, context),
        None => BuiltinResult::NotABuiltin,
    }
}
```

- [ ] **Step 2: Initialize registry in `main.rs` startup**

In wherever `ShellContext` is constructed (likely `main.rs`):

```rust
let mut builtins = commands::BuiltinRegistry::new();
commands::builtins::register_all(&mut builtins);
let context = ShellContext { /* ... */ builtins, /* ... */ };
```

`ShellContext` gains a `pub builtins: BuiltinRegistry` field.

- [ ] **Step 3: Delete the empty original `commands.rs`**

If everything moved out, `commands.rs` is now redundant since `commands/mod.rs` covers it. Delete:

```bash
git rm userspace/shell/src/commands.rs
```

If a few free functions are still left in `commands.rs`, move them into `commands/mod.rs` instead, then delete `commands.rs`.

- [ ] **Step 4: Build green.**

```bash
cargo xtask build 2>&1 | tail -20
```

### Task 2.11: Run shell-impacting harness cases

- [ ] **Step 1: Run l2_cd, l2_jobs, l2_export, l2_argv, l2_bare_cmd**

```bash
for case in l2_cd l2_jobs l2_export l2_argv l2_bare_cmd; do
    bash scripts/harness_run.sh $case 2>&1 | tail -3
done
```

Expected: all five PASS. If any fail, the split broke a body — bisect by inspecting the new builtin file's `run` body against `git log -p commands.rs`.

- [ ] **Step 2: Commit Stage 2**

```bash
git add -A
git commit -m "$(cat <<'EOF'
refactor(shell): split commands.rs into commands/ module hierarchy

3,612-line commands.rs broken into focused modules: builtins/cd.rs,
builtins/echo.rs, builtins/env.rs, builtins/jobs.rs, builtins/help.rs,
builtins/exit.rs, builtins/arith.rs, builtins/sudo.rs, builtins/container.rs,
exec.rs, redirect.rs, completion.rs. Builtin trait and BuiltinRegistry
in builtins/registry.rs. ShellContext now carries the registry. No
behavior change.

Phase 4 Plan A Stage 2.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Stage 3 — Cull test-only builtins (PR 3)

For each test-only builtin: extract its body into a new probe binary, then remove the registration from the shell.

### Task 3.1: Create `probes/jobchurn/`

**Files:**
- Create: `userspace/probes/jobchurn/Cargo.toml`
- Create: `userspace/probes/jobchurn/src/main.rs`
- Modify: `Cargo.toml` (add member, add to `cluu.probes` metadata)
- Modify: `userspace/shell/src/commands/builtins/jobs.rs` (remove `JobChurnBuiltin`)
- Modify: `userspace/shell/src/commands/builtins/mod.rs` (no change to `register_all` — Builtin not registered)

- [ ] **Step 1: Create Cargo.toml**

```toml
[package]
name = "cluu-jobchurn"
version = "0.1.0"
edition = "2021"
description = "CLUU jobchurn probe"
authors = ["CLUU Team"]
license = "MIT"

[dependencies]
libcluu = { path = "../../libcluu", features = ["posix"] }

[[bin]]
name = "jobchurn"
path = "src/main.rs"
```

- [ ] **Step 2: Create main.rs**

Lift the body of `JobChurnBuiltin::run` from `userspace/shell/src/commands/builtins/jobs.rs`. Wrap in:

```rust
#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::string::String;
use alloc::vec::Vec;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args: Vec<String> = libcluu::args::args();
    // [PASTE: body of original JobChurnBuiltin::run, replacing ShellContext access
    //         with libcluu equivalents — getenv, _write, etc.]
    0
}
```

Concrete substitutions:
- `context.env_get("FOO")` → `libcluu::posix::getenv("FOO")`
- writes to TTY token → `libcluu::posix::_write(1, ...)`
- spawn calls → reuse pattern from `userspace/probes/argvprobe/src/main.rs`

- [ ] **Step 3: Add to workspace**

In `Cargo.toml`:

```toml
    "userspace/probes/jobchurn",
```

In the `[workspace.metadata.cluu.probes]` `crates` list:

```toml
    "jobchurn",
```

- [ ] **Step 4: Remove `JobChurnBuiltin` from shell**

In `userspace/shell/src/commands/builtins/jobs.rs`, delete the `struct JobChurnBuiltin` and its `impl Builtin`. Remove the `registry.register(Box::new(JobChurnBuiltin));` line from `register`.

- [ ] **Step 5: Update harness**

```bash
grep -rn 'jobchurn\|JobChurn' scripts/ | head
```

For every match in `harness_*.sh`, replace `jobchurn` (the builtin invocation) with `/probes/jobchurn` (the binary).

- [ ] **Step 6: Build, run l2_jobchurn**

```bash
cargo xtask build-all
bash scripts/harness_run.sh l2_jobchurn 2>&1 | tail -5
```

Expected: PASS.

### Task 3.2: Create `probes/jobmix/`

Same pattern as Task 3.1, target `JobMixBuiltin` and harness case `l2_jobmix`.

- [ ] **Step 1**: Create `userspace/probes/jobmix/Cargo.toml` (template above, name `cluu-jobmix`, bin `jobmix`).
- [ ] **Step 2**: Lift `JobMixBuiltin::run` body into `src/main.rs`.
- [ ] **Step 3**: Workspace member + probes metadata.
- [ ] **Step 4**: Delete `JobMixBuiltin` from shell.
- [ ] **Step 5**: Update harness `jobmix` → `/probes/jobmix`.
- [ ] **Step 6**: `bash scripts/harness_run.sh l2_jobmix` → PASS.

### Task 3.3: Create `probes/killdeny/`

Target `KillDenyBuiltin`. Harness case is whichever security test invokes it (find via `grep -rn 'kill_deny\|killdeny' scripts/`). Same six-step pattern.

### Task 3.4: Create `probes/regdeny/`

Target `RegistryDenyBuiltin`. Same pattern.

### Task 3.5: Create `probes/mapfail/`, `probes/mapcopyfail/`, `probes/maperror/`

Three siblings sharing the m3_* harness cases. Each gets its own crate, same six-step pattern. Harness cases `m3_mapfail`, `m3_mapcopyfail`, `m3_maperror` already exist; only invocation paths change.

### Task 3.6: Create consolidated `probes/ext2io/`

**Files:**
- Create: `userspace/probes/ext2io/Cargo.toml`
- Create: `userspace/probes/ext2io/src/main.rs`

Four builtins (`Ext2WriteBuiltin`, `Ext2AppendBuiltin`, `Ext2MutateBuiltin`, `Ext2UnlinkBuiltin`) consolidate into one binary with subcommand args.

- [ ] **Step 1: Cargo.toml** (template above, name `cluu-ext2io`, bin `ext2io`).

- [ ] **Step 2: main.rs dispatches by argv[1]**

```rust
#![no_std]
#![no_main]
extern crate alloc;
#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::string::String;
use alloc::vec::Vec;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args: Vec<String> = libcluu::args::args();
    if args.len() < 2 {
        let msg = b"usage: ext2io <write|append|mutate|unlink> [args...]\n";
        let _ = libcluu::posix::_write(2, msg.as_ptr() as *const _, msg.len());
        return 2;
    }
    match args[1].as_str() {
        "write"  => run_write(&args[2..]),
        "append" => run_append(&args[2..]),
        "mutate" => run_mutate(&args[2..]),
        "unlink" => run_unlink(&args[2..]),
        _ => {
            let msg = b"ext2io: unknown subcommand\n";
            let _ = libcluu::posix::_write(2, msg.as_ptr() as *const _, msg.len());
            2
        }
    }
}

fn run_write(args: &[String]) -> i32 {
    // [PASTE: Ext2WriteBuiltin::run body, ShellContext writes → libcluu::posix::_write]
    0
}

fn run_append(args: &[String]) -> i32 {
    // [PASTE: Ext2AppendBuiltin::run body]
    0
}

fn run_mutate(args: &[String]) -> i32 {
    // [PASTE: Ext2MutateBuiltin::run body]
    0
}

fn run_unlink(args: &[String]) -> i32 {
    // [PASTE: Ext2UnlinkBuiltin::run body]
    0
}
```

- [ ] **Step 3: Workspace + probes metadata.**
- [ ] **Step 4: Remove all four Ext2*Builtin from shell.**
- [ ] **Step 5: Update harness — `ext2write` → `/probes/ext2io write`, etc.**
- [ ] **Step 6: Run any ext2-touching harness cases. PASS.**

### Task 3.7: Create `probes/ownerdeny/`

Target `Ext2OwnerDenyBuiltin`. Harness case `l2_owner_deny`.

### Task 3.8: Create `probes/ringio/`

Target `RingIoBuiltin`. Find harness case via `grep -rn 'ringio' scripts/`.

### Task 3.9: Create `probes/vtcrash/`

Target `VtCrashTestBuiltin`. Find harness case via grep.

### Task 3.10: Create `probes/sudotest/`

Target `SudoTestBuiltin`. Same pattern.

### Task 3.11: Create `probes/sutest/` (consolidated)

Combines `SuTestBuiltin` and `SuEqualTestBuiltin`. argv[1] = `default` or `equal`.

### Task 3.12: Delete `EscalateDenyBuiltin` (duplicate)

`escalateprobe` already exists. Just delete:

- [ ] **Step 1: Find and delete `EscalateDenyBuiltin`**

```bash
grep -n 'EscalateDenyBuiltin' userspace/shell/src/commands/builtins/*.rs
```

Remove struct, impl, and registration line.

- [ ] **Step 2: Update any harness case that invoked `escalate_deny` to call `/probes/escalateprobe`** (if not already).

### Task 3.13: Rename `ShellCrashBuiltin` → `_shellcrash`, debug-only

**Files:**
- Modify: shell builtin file containing `ShellCrashBuiltin`

- [ ] **Step 1: Change the name**

```rust
fn name(&self) -> &'static str { "_shellcrash" }
```

- [ ] **Step 2: Gate behind cfg**

```rust
#[cfg(feature = "debug-shellcrash")]
pub fn register(registry: &mut BuiltinRegistry) {
    registry.register(Box::new(ShellCrashBuiltin));
}
#[cfg(not(feature = "debug-shellcrash"))]
pub fn register(_registry: &mut BuiltinRegistry) {}
```

In `userspace/shell/Cargo.toml`:

```toml
[features]
default = []
debug-shellcrash = []
```

- [ ] **Step 3: Verify default build does not register it.**

```bash
cargo xtask build
echo '_shellcrash' | <invoke shell somehow>
```

A simpler check — grep the built binary or run a `type _shellcrash` in shell once jobs+type land. For now, just confirm the cfg compiles:

```bash
cargo build -p cluu-shell --target triplets/x86_64-cluu-user.json -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem
cargo build -p cluu-shell --features debug-shellcrash --target triplets/x86_64-cluu-user.json -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem
```

Both succeed.

### Task 3.14: Audit for stale `Spawn*`/`Stop`/`Foreground`/`Background` primitives

Spec §4.7: these are pre-jobs primitives. They will be replaced in Plan D. For Plan A, leave them registered so harness cases that use them keep passing. Plan D removes them.

Add a comment in `commands/builtins/jobs.rs`:

```rust
// TODO(Phase4-Plan-D): SpawnBuiltin / SpawnBgBuiltin / StopBuiltin /
// ForegroundBuiltin / BackgroundBuiltin are pre-jobs primitives. They are
// replaced by `cmd &` syntax + proper jobs builtins in Plan D. Do not
// extend them.
```

### Task 3.15: Run full harness, commit Stage 3

- [ ] **Step 1: Run matrix**

```bash
bash scripts/harness_matrix.sh 2>&1 | tee /tmp/matrix-after-A3.log
```

Expected: green. Specifically:
- Old test-only-builtin invocation paths replaced everywhere
- Probe binaries land at `/probes/<name>`
- All `m3_*` and `l2_*` cases that touched test-only builtins still PASS

- [ ] **Step 2: Commit**

```bash
git add -A
git commit -m "$(cat <<'EOF'
refactor(shell): cull test-only builtins; relocate as probe binaries

Removes 19 test-only builtins from the shell registry. Each is rebuilt
as a probe binary under userspace/probes/, invoked by harness scripts
via /probes/<name> instead of as a shell builtin. EscalateDenyBuiltin
deleted (duplicate of escalateprobe). ShellCrashBuiltin renamed
_shellcrash and gated behind debug-shellcrash feature.

Shell builtin registry shrinks ~47 → ~28.

Phase 4 Plan A Stage 3.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Self-review

- **Spec coverage**: §4.1 (probe move) → Stage 1. §4.2 (Cargo.toml) → Task 1.3. §4.3 (xtask) → Task 1.4. §4.4 (image) → Task 1.5. §4.5 (harness) → Task 1.6. §4.6 (commands.rs split) → Stage 2. §4.7 (cull) → Stage 3.
- **Placeholders**: `[PASTE the body of...]` markers are intentional — body bodies live in current code, lifted verbatim. Engineer copies; placeholder is concrete instruction. Acceptable.
- **Type consistency**: `BuiltinRegistry`/`Builtin`/`BuiltinResult`/`ShellContext` consistent across tasks.
- **Risk**: Task 1.1 audit may reveal probes are built by a path this plan didn't anticipate. If so, write down what it is and adjust Task 1.5 accordingly before proceeding.

---

## Acceptance

Plan A done when:
- `userspace/probes/` houses 10 original probes + ~10 extracted probe binaries (jobchurn, jobmix, killdeny, regdeny, mapfail, mapcopyfail, maperror, ext2io, ownerdeny, ringio, vtcrash, sudotest, sutest)
- `cargo xtask build` does not compile probes
- `cargo xtask build-probes` builds them
- `cargo xtask build-all` builds both
- Image places probes at `/probes/<name>`
- `userspace/shell/src/commands.rs` deleted; `commands/` module hierarchy in place; every file ≤ ~400 LOC
- `harness_matrix.sh` green
- Shell builtin registry has ~28 entries (down from ~47)
