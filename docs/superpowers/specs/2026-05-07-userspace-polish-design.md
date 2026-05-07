# Phase 4 — Userspace Polish & Coreutils

*Design spec. Brainstormed 2026-05-07.*

*Status: design — not yet planned, not yet implemented.*

---

## 0. Position in roadmap

- **Was**: Phase 4 = network.
- **Now**: Phase 4 = *Userspace Polish & Coreutils*. Network bumped to Phase 5.
- **Phase 3 status**: considered done in practice. Soak test (1000-iteration `cat | grep | head`) was deferred — only single-step pipeline confirmed working. ROADMAP.md still has unchecked boxes; documentation needs to catch up. Phase 4 begins immediately.
- **Implication**: Phase 1's "real pipe execution" exit criterion (which named 3-stage `cat | grep | head`) may have shipped on a 2-stage-only implementation. §7 verifies and fixes.

---

## 1. Goals

1. A first-time tester opens the shell and feels like a real Unix-ish OS.
2. `userspace/` directory reflects what is *user-facing*; probes hide one level deeper, behind opt-in build.
3. The Phase 1 pipe path is reverified end-to-end and remaining gaps are closed.

## 2. Exit criteria

All user-visible. All testable. Phase done when every box is checked.

- [ ] All 11 probes live under `userspace/probes/<name>/`. `cargo xtask build` excludes them. `cargo xtask build-probes` builds them. Harness still runs them.
- [ ] `commands.rs` split into focused modules (≤400 LOC each). Builtin registry uses a trait so new builtins are additive.
- [ ] Test-only shell builtins (~19 of them, see §2.8) removed from the shell registry. Harness invokes probe binaries instead.
- [ ] `ls -l -a -h -R -1 -S -t -r`, color-by-type when stdout is a TTY, columns by default. Backed by extended `VfsStat`.
- [ ] 15 new utils ship: `env`, `sleep`, `basename`, `dirname`, `date`, `kill`, `printf`, `sort`, `uniq`, `cut`, `tr`, `find`, `which`, `du`, `stat`.
- [ ] Existing utils brought GNU-close: `cat`, `cp`, `mv`, `rm`, `mkdir`, `touch`, `head`, `tail`, `wc`, `grep`, `ps`. Common short-flag matrix per §3.2.
- [ ] Shared `libcluu/src/cli.rs` argument parser used by every util.
- [ ] Shell builtins added: `exit`, `alias`/`unalias`, `type`, `help`, `set`/`unset`, persistent `history` (`~/.cluu_history`).
- [ ] **Full POSIX job control** (all userspace, zero kernel commits): `cmd &`, `jobs`, `fg %N`, `bg %N`, `wait`, `kill %N`, real Ctrl-Z → SIGTSTP, real Ctrl-C → SIGINT.
- [ ] Pipe Phase 1 reverify: `l2_pipe_3stage` smoke green; env propagation parity in pipe stages; sequential-vs-multiplexed wait decision documented.
- [ ] Compiler warnings ≤ Phase 3 baseline.
- [ ] `harness_matrix.sh` green end-to-end.
- [ ] `memory/project_phase3_soak_punted.md` retracted/corrected after §5 diagnostic.
- [ ] Phase 4 retrospective added to `docs/ROADMAP.md`.

## 3. Cross-cutting principle: SOLID

Every implementation in Phase 4 follows SOLID:

- **S** — Single Responsibility. Each builtin in its own file. Each util has one job. `cli.rs` only parses args. `ls/format.rs` only renders.
- **O** — Open/Closed. The builtin registry stays open via a trait. New builtin = new file impl `Builtin`, no edit to dispatcher.
- **L** — Liskov. Every util obeys `main() -> i32` with the same exit-code semantics (0 success, 1 minor, 2 usage). No "special case" utils.
- **I** — Interface Segregation. `VfsClient` does not grow into a god trait. `readdir`/`stat`/`open` are separable surfaces; a util pulls only what it needs.
- **D** — Dependency Inversion. Utils depend on `libcluu::fs::traits::*`, not on a concrete `VfsClient`. Tests and probes substitute fakes.

This is repeated in every PR review. `feedbacker-3` workflow checks SOLID compliance as part of the kernel-expert / sw-architect approval before any merge.

---

## 4. Workspace reshape

### 4.1 Probes move

```
userspace/argvprobe/        →  userspace/probes/argvprobe/
userspace/blkprobe/         →  userspace/probes/blkprobe/
userspace/cascadeprobe/     →  userspace/probes/cascadeprobe/
userspace/detachprobe/      →  userspace/probes/detachprobe/
userspace/escalateprobe/    →  userspace/probes/escalateprobe/
userspace/mountprobe/       →  userspace/probes/mountprobe/
userspace/nestprobe/        →  userspace/probes/nestprobe/
userspace/suspendprobe/     →  userspace/probes/suspendprobe/
userspace/viewprobe/        →  userspace/probes/viewprobe/
userspace/vqprobe/          →  userspace/probes/vqprobe/
```

After the move, `ls userspace/` shows ~25 entries — all real user-facing programs and services.

### 4.2 Workspace `Cargo.toml`

- `members = [ ... ]` paths updated to point under `userspace/probes/`.
- `default-members = [ ... ]` drops every probe. Default `cargo build` no longer compiles them.
- New `[workspace.metadata.cluu.probes]` lists probe crate names so `xtask build-probes` knows what to build.

### 4.3 xtask additions

| Subcommand | Behavior |
|---|---|
| `cargo xtask build` | builds non-probe userspace + kernel (unchanged surface, but excludes probes now → faster) |
| `cargo xtask build-probes` | builds all probes; emits to `target/sysroot/probes/<name>` |
| `cargo xtask build-all` | `build` + `build-probes` |

### 4.4 Image / boot

Goal: probes ship in a separate initrd segment (`probes.tar`) loaded only when the harness asks. Default boot does not carry them.

If the current image loader supports only a single initrd, fall back to embedding probes in the main initrd while keeping workspace separation. The plan step verifies the loader's capability before committing to either path.

### 4.5 Harness wiring

`scripts/harness_*.sh` already invokes probe binaries by name. Path updates only — `/bin/argvprobe` → `/probes/argvprobe`. Existing harness case names unchanged.

### 4.6 `commands.rs` split

```
userspace/shell/src/
├── main.rs              REPL loop, signal install
├── pipeline.rs          existing — multi-cmd pipelines
├── path_lookup.rs       existing
├── shellrc.rs           existing
├── commands/
│   ├── mod.rs           dispatch table, builtin registry (~300 LOC)
│   ├── builtins/
│   │   ├── mod.rs
│   │   ├── cd.rs        cd + pwd
│   │   ├── echo.rs
│   │   ├── env.rs       export, set, unset, env
│   │   ├── alias.rs     alias / unalias
│   │   ├── jobs.rs      jobs, fg, bg, wait, kill %N
│   │   ├── history.rs
│   │   ├── help.rs      help, type
│   │   └── exit.rs
│   ├── exec.rs          single-command spawn, env+arg payload
│   ├── redirect.rs      >, >>, < parsing + wiring
│   ├── completion.rs    tab completion
│   └── line_edit.rs     line editor (if not already split)
```

Each file ≤ ~400 LOC target. Done as one mechanical commit `refactor(shell): split commands.rs into modules — no behavior change`.

### 4.7 Cull test-only builtins

19 of 47 registered builtins are test fixtures. Remove from shell, recreate as probe binaries:

| Builtin | Disposition |
|---|---|
| `JobChurnBuiltin` | → `probes/jobchurn/` |
| `JobMixBuiltin` | → `probes/jobmix/` |
| `KillDenyBuiltin` | → `probes/killdeny/` |
| `RegistryDenyBuiltin` | → `probes/regdeny/` |
| `MapFailBuiltin` | → `probes/mapfail/` |
| `MapCopyFailBuiltin` | → `probes/mapcopyfail/` |
| `MapErrorBuiltin` | → `probes/maperror/` |
| `Ext2WriteBuiltin` | → `probes/ext2io/` (consolidate next 3) |
| `Ext2AppendBuiltin` | → merge into `probes/ext2io/` |
| `Ext2MutateBuiltin` | → merge into `probes/ext2io/` |
| `Ext2UnlinkBuiltin` | → merge into `probes/ext2io/` |
| `Ext2OwnerDenyBuiltin` | → `probes/ownerdeny/` |
| `RingIoBuiltin` | → `probes/ringio/` |
| `VtCrashTestBuiltin` | → `probes/vtcrash/` |
| `SudoTestBuiltin` | → `probes/sudotest/` |
| `SuTestBuiltin` | → `probes/sutest/` |
| `EscalateDenyBuiltin` | delete (`escalateprobe` is the duplicate) |
| `SuEqualTestBuiltin` | merge into `probes/sutest/` |
| `ShellCrashBuiltin` | keep, rename `_shellcrash`, debug-only |

Net: ~47 → ~28 registered shell builtins.

`SpawnBuiltin`, `SpawnBgBuiltin`, `StopBuiltin`, `ForegroundBuiltin`, `BackgroundBuiltin` are pre-jobs primitives. Replaced by `&` syntax + `fg`/`bg` builtins from §6. Plan step decides whether to keep them as transition aliases for one phase or hard-cut.

---

## 5. Coreutils

### 5.1 New utils (15)

| Util | LOC budget | Notes |
|---|---|---|
| `env` | 80 | print, run-with |
| `sleep` | 30 | secs only initially; `Ns/m/h` later |
| `basename` | 40 | one path, optional suffix strip |
| `dirname` | 40 | one path |
| `date` | 100 | timeserver-backed |
| `kill` | 80 | by PID and by job spec (`%N`) |
| `printf` | 200 | `%s %d %x %c`; `\n \t \\` |
| `sort` | 200 | `-n`, `-r`, `-u`, `-k` |
| `uniq` | 80 | `-c`, `-d` |
| `cut` | 150 | `-f`, `-d`, `-c` |
| `tr` | 120 | char map, `-d`, `-s` |
| `find` | 250 | `-name`, `-type`, `-print`, depth-first |
| `which` | 50 | path lookup (`PATH`) |
| `du` | 150 | `-s`, `-h`, recursive |
| `stat` | 100 | mirrors `stat(1)` brief |

`tee`, `clear`, `df`, `sync`, `true`, `false` are out of scope this phase. `clear`, `true`, `false` are already shell builtins. `tee` and `sync` are first add-backs if budget allows.

### 5.2 Existing util GNU-close upgrades

| Util | Add |
|---|---|
| `cat` | `-n`, `-b`, `-A`, `-E`, `-T`, `-s` |
| `cp` | `-r`/`-R`, `-i`, `-f`, `-v`, `-p`, `-n` |
| `mv` | `-i`, `-f`, `-n`, `-v` |
| `rm` | `-i`, `-f`, `-v`, `-d` (already has `-r`) |
| `mkdir` | `-p`, `-v`, `-m MODE` |
| `touch` | `-c`, `-a`, `-m`, `-r REF`, `-d STRING` |
| `head` | `-c`, `-q`, `-v`, multi-file headers |
| `tail` | `-c`, `-q`, `-v`, multi-file headers. `-f` follow-mode is feasible (Phase 3 shipped `poll()`); add if PR 7 has budget, otherwise defer to follow-up |
| `wc` | `-l`, `-w`, `-c`, `-m`, `-L`; multi-file totals |
| `grep` | `-i`, `-v`, `-n`, `-c`, `-l`, `-L`, `-r`/`-R`, `-w`, `-x`, `-E`, `-F`, `-q`, `-H`/`-h`, `--color=auto` |
| `ps` | `-e`/`-A`, `-f`, `-l`, `-u USER`, columns: PID PPID TTY STAT TIME CMD |
| `top` | column toggles, sort by CPU/MEM, `q` quit, `k` kill (most already there) |

### 5.3 Shared `libcluu/src/cli.rs`

POSIX-style argument parser. Single source of truth. Features:

- Clustered short flags (`-rfv`)
- Long opts (`--help`, `--version`)
- `--` end-of-options
- Optional / required arg attachment
- Generated `--help` and `--version` for free from a per-util spec
- Stricter exit codes per GNU convention: 0 success, 1 minor problem, 2 usage error

Estimated 250 LOC. Replaces the per-util ad-hoc arg loops that exist today.

### 5.4 ls deep design

#### 5.4.1 VFS protocol bump

Today `VfsStat = { size, mode }`. `ls -l` needs more. Extend (userspace-only change):

```rust
pub struct VfsStat {
    pub size: u64,
    pub mode: u32,    // S_IFMT | perms
    pub mtime: u64,   // unix seconds
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub blocks: u64,  // 512-byte units, used by du
}

pub struct VfsDirEntry {
    pub name: String,
    pub stat: VfsStat,  // batched, no extra round trip
}
```

`readdir` returns `(name, stat)` pairs in one round trip, eliminating the N+1 stat pattern. ext2 backend already reads these from the inode.

#### 5.4.2 ls flag matrix

| Flag | Meaning | Implementation |
|---|---|---|
| (none) | columns when stdout is TTY, single-col otherwise | `isatty(1)` check |
| `-1` | force single column | suppress column logic |
| `-l` | long format | `mode size nlink uid gid mtime name` |
| `-a` | include dotfiles | drop the `name.starts_with('.')` filter |
| `-h` | human size in `-l` | size formatter (1.2K, 3.4M) |
| `-R` | recursive | walk subdirs |
| `-S` | sort by size desc | sort comparator |
| `-t` | sort by mtime desc | sort comparator |
| `-r` | reverse sort | reverse after sort |

#### 5.4.3 Color logic

Color when stdout is TTY and `NO_COLOR` env not set. Otherwise off. Same convention as GNU `ls --color=auto`.

- Directory → blue (`\x1b[1;34m`)
- Executable (`mode & 0o111`) → green (`\x1b[1;32m`)
- Symlink → cyan (deferred until VFS exposes link target)
- Regular file → no color

#### 5.4.4 Mode + time rendering

- Mode: `drwxr-xr-x` from `mode` bits. ~30 LOC.
- Time: `Mmm DD HH:MM` if mtime within 6 months, `Mmm DD  YYYY` otherwise. Date math via `date` util's helper.

ls grows from 53 LOC to ~450 LOC.

### 5.5 YAGNI guard

Out of scope: `--si`, `--block-size`, `--time-style=...`, locale-aware sort, regex backrefs in grep, full bash `[[ ]]` test grammar. Plan rejects them if proposed.

---

## 6. Job control (all userspace)

Earlier assumption "needs kernel" was wrong. `InvokeOp::ThreadSuspend`/`ThreadResume` already exist. Job control is pure userspace, zero kernel commits.

### 6.1 Architecture

```
shell                                                       
  JobTable: Vec<Job { id, pgid, state, cmd_line }>          
  builtins: jobs / fg / bg / wait / kill %N                 
  signal handlers: SIGTSTP / SIGINT installed before child  
            │                                                
            │ procmgr IPC: PG_CREATE, PG_ATTACH,            
            │              PG_SUSPEND, PG_RESUME,           
            │              PG_SIGNAL, TTY_SET_FG            
            ▼                                                
procmgr                                                      
  PgTable: HashMap<Pgid, Vec<Pid>>                           
  Pid state machine: Running / Stopped / Continued / Zombie  
  Suspend/Resume → InvokeOp::ThreadSuspend/Resume per tid    
            │                                                
            │ kernel: existing invoke ops, no changes        
            ▼                                                
kernel (untouched)                                           

tty                                                          
  fg_pgid_per_session: HashMap<SessionId, Pgid>              
  Ctrl-C → PROCMGR_PG_SIGNAL(fg_pgid, SIGINT)                
  Ctrl-Z → PROCMGR_PG_SIGNAL(fg_pgid, SIGTSTP)               
```

### 6.2 procmgr owns pgid → tid

procmgr is the single source of truth for pgid lifetime and membership. Shell calls `PROCMGR_PG_CREATE` when spawning a pipeline, gets a fresh pgid. Each stage's spawn payload includes the pgid. procmgr stores pgid per process; on exit, pid removed; pgid garbage-collected when empty.

TTY queries procmgr for `pgid → session` (or caches at session login) when routing signals.

### 6.3 New IPC labels (procmgr)

| Label | From | Purpose |
|---|---|---|
| `PROCMGR_PG_CREATE` | shell | reserve fresh pgid |
| `PROCMGR_PG_ATTACH` | shell | bind pid → pgid (during spawn) |
| `PROCMGR_PG_SIGNAL` | tty, shell | deliver signal to all pids in pgid |
| `PROCMGR_PG_SUSPEND` | shell | suspend whole pgid |
| `PROCMGR_PG_RESUME` | shell | resume whole pgid |
| `PROCMGR_TTY_SET_FG` | shell | set foreground pgid for caller's session |

### 6.4 Ctrl-Z signal flow

1. User types Ctrl-Z while `cat | grep x` is foreground.
2. TTY decodes Ctrl-Z, looks up session's `fg_pgid`.
3. TTY → procmgr: `PROCMGR_PG_SIGNAL(fg_pgid, SIGTSTP)`.
4. procmgr: for each pid in pgid, if process has SIGTSTP handler installed → libcluu signal trampoline delivers (existing infra). Else default action → procmgr calls `ThreadSuspend` on every tid of every pid in the group.
5. procmgr → shell: `PROCMGR_JOB_STOPPED(pgid)`.
6. Shell updates JobTable to `STOPPED`, unblocks `wait()`, prints `[1]+  Stopped  cat | grep x`, restores its own foreground.
7. Shell calls `PROCMGR_TTY_SET_FG(shell_pgid)`.

`fg %1` reverses: shell sets `fg_pgid = job1`, sends `PROCMGR_PG_RESUME(pgid1)`, blocks waiting for the next state change.

### 6.5 procmgr per-pid state machine

```
Running ──Suspend──▶ Stopped ──Resume──▶ Running
   │                    │
   └──exit──▶ Zombie ◀──┘
```

`waitpid(WUNTRACED)` returns when pid → Stopped. `WCONTINUED` returns when Stopped → Running.

### 6.6 Risks

- **TTY ↔ shell race on Ctrl-Z**: Ctrl-Z arrives between shell `fg %N` and procmgr resume. Resolution: TTY queues input; shell drains before next prompt; or fg_pgid lookup is serialized in TTY.
- **Background process steals stdin**: bg process tries `read(0)`. Must SIGTTIN itself. Implementation: TTY reads check caller pgid vs fg_pgid; mismatch → reply with an error code that libcluu maps to delivering SIGTTIN.
- **Suspended process holds VFS lock**: stop a process mid-read → VFS handle locked. Deferred risk; mitigation (have procmgr inform VFS to release reads on suspend) bumped to Phase 5+ unless it bites.

### 6.7 Test cases

- `l2_jobs_basic` — `sleep 30 &` → `jobs` shows it → `kill %1` → exits.
- `l2_jobs_ctrlz` — `cat` → Ctrl-Z → `jobs` shows Stopped → `fg` → resumes → Ctrl-D → exits 0.
- `l2_jobs_pipeline` — `yes | head -5` → exit cleanly, no orphan.
- `l2_jobs_bg_to_fg` — `sleep 30 &` → `fg %1` → Ctrl-C → exits 130.
- `l2_jobs_sigint_fg` — `sleep 30` (foreground) → Ctrl-C → exits 130. `sleep 30 &` running same time stays alive.

---

## 7. Pipe Phase 1 reverify

### 7.1 Reality vs Phase 1 closing notes

Phase 1 was marked DONE 2026-04-27 with `cat foo.txt | grep pattern | head -5` listed among exit criteria. User confirms the practical reality: only single-step pipelines were confirmed working at that time. 2-stage may work; 3-stage status uncertain.

Code surface looks complete: `PIPE_DATA_LABEL`/`PIPE_EOF_LABEL` exist (`userspace/libcluu/src/posix/pipe.rs:17-18`), both are used (`pipe.rs:130`, `pipe.rs:146`), and `PipelineExecutor` walks n stages with no 2-stage special case (`pipeline.rs:46+`). So the n-stage code path *exists* — but whether it executes correctly is unverified. `memory/project_phase3_soak_punted.md` may have the right diagnosis (3-stage hangs) even if its mechanism claim ("wire protocol unfinished") is wrong.

Truth comes from a smoke run, not from reading code.

### 7.2 Step zero: diagnostic

Before any other Phase 4 code, run a 3-stage smoke and capture the truth:

```sh
echo -e "alpha\nbeta\ngamma\nalpha\ndelta" > /tmp/in.txt
cat /tmp/in.txt | grep alpha | head -1
# expected stdout: "alpha"
# expected exit:   0
```

Three outcomes:
1. **Works** — memory was stale. Update memory note. Move on.
2. **Hangs** — capture *where*. Identify whether stage 0 doesn't EOF, stage 1 doesn't drain, or stage 2 exits early without flushing.
3. **Crashes** — same triage as case 2.

This diagnostic is gating: knowing the real state shapes everything downstream.

### 7.3 Known gap 1 — env propagation in pipe stages

`pipeline.rs:236-240` shows pipe-spawned stages get procmgr `DEFAULT_ENV`, not the shell's env. Single-cmd path propagates via the ENV trailer.

Fix: extract the ENV trailer build from `commands.rs` single-cmd path into a shared argument of `build_container_run_payload_full`. `pipeline.rs` passes the same trailer. Test: `l2_envelope_pipe` — `FOO=bar; echo $FOO | tr a-z A-Z` should print `BAR`.

### 7.4 Known gap 2 — sequential vs multiplexed wait

`pipeline.rs:281-301` waits stages in spawn order. Pathological case (stage 0 = `yes`, stage 2 = `head -1`) keeps stage 0 blocked on EPIPE write until the shell drains. Only a latency cost, not a correctness bug.

Decision: keep sequential wait for Phase 4. Document. Move multiplexed wait via `poll()` to a follow-up phase if soak workload ever exposes a real hang. Phase 3 shipped `poll()`, so the option is technically available now — but bigger fish to fry this phase.

### 7.5 Known gap 3 — Ctrl-C in interactive multi-stage pipeline

Once §6 lands, Ctrl-C in `cat | grep | head` SIGINTs the whole pgid. Pipeline assigns a single pgid; TTY signals the group. Folds naturally into the §6.7 test matrix.

### 7.6 Memory cleanup

After §7.2 diagnostic, edit `memory/project_phase3_soak_punted.md` to reflect current truth. Either retract the file or refine its claims. No more stale memories on this topic.

### 7.7 Risk: kernel exception

If §7.2 reveals a kernel bug (say `ipc_recv` deadlocks on a rights-restricted token under specific timing), that is a freeze-exception. The named-fix rule applies: the kernel commit message must reference the failing test case verbatim, and the fix scope is limited to what the test needs.

---

## 8. Testing & harness

### 8.1 Three layers

| Layer | Purpose | Where |
|---|---|---|
| **Unit** | Pure logic: cli parser, mode-bits renderer, time formatter, sort comparator | `userspace/<util>/src/lib.rs` `#[cfg(test)]` |
| **Smoke** | Single-binary against real VFS, output match | `scripts/harness_cases.conf` `l2_<util>_basic` |
| **Integration** | Multi-binary, pipelines, jobs | `scripts/harness_cases.conf` `l2_<scenario>` |

Unit tests run on host (`cargo test -p <util>`). Smoke + integration run in QEMU via the existing harness.

### 8.2 Smoke test contract

Each new or improved util ships with at least one smoke case:

```sh
# l2_<util>_basic
echo "input" > /tmp/in
<util> [args] /tmp/in > /tmp/out
diff /tmp/out /tmp/expected   # exit 0 = pass
```

A util ships only when its smoke is green.

### 8.3 Phase 4 minimum harness cases

```
l2_ls_long          ls -l populates rows: mode, size, mtime, name
l2_ls_color         ls --color=auto colors dirs; no color when piped
l2_ls_recursive     ls -R walks subdirs
l2_grep_recursive   grep -rn pattern /tmp
l2_pipe_3stage      cat | grep | head smoke (§7.2)
l2_pipe_env         env propagation through pipeline (§7.3)
l2_jobs_basic       sleep & / jobs / kill %1
l2_jobs_ctrlz       cat / Ctrl-Z / fg / Ctrl-D
l2_jobs_pipeline    yes | head pipeline cleanup
l2_history_persist  type N cmds / exit / restart shell / up-arrow N+1 times
l2_alias_basic      alias ll='ls -l' / ll
l2_completion_path  tab on /b<TAB> → /bin/, etc.
```

Plus per-util `l2_<util>_basic` for each new util (15 cases).

### 8.4 Cleanup-stage harness

After §4.1 reshape: rename harness probe invocations from `/bin/<probe>` to `/probes/<probe>`. Single PR. Verify `harness_matrix.sh` green before merging.

---

## 9. Commit cadence

3-day WIP rule applies (ROADMAP §3). Phase 4 is split into ~15 PRs:

| PR | Scope |
|---|---|
| 1 | §4.1-4.5 workspace reshape (probes move) |
| 2 | §4.6 `commands.rs` split — pure refactor, zero behavior change |
| 3 | §4.7 cull test-only builtins, add probe binaries |
| 4 | §5.3 `libcluu/src/cli.rs` shared arg parser |
| 5 | §5.4.1 VFS protocol bump: extended `VfsStat` + readdir batching |
| 6 | §5.4 ls full feature set |
| 7 | §5.2 existing util GNU-close upgrades (split if >800 LOC) |
| 8 | §5.1 new utils batch A: `env`, `sleep`, `basename`, `dirname`, `date`, `kill`, `printf`, `which` |
| 9 | §5.1 new utils batch B: `sort`, `uniq`, `cut`, `tr`, `find`, `du`, `stat` |
| 10 | §7 pipe 3-stage verify + env propagation |
| 11 | §6.1-6.5 procmgr pgid table + suspend/resume IPC |
| 12 | §6 TTY fg pgid routing + signal delivery |
| 13 | §6 shell jobs builtins + JobTable |
| 14 | persistent history + alias + type + help builtins |
| 15 | docs pass (this design + per-util `--help` strings) |

Each PR runs through `feedbacker-3` workflow: kernel-expert audit and sw-architect SOLID review before merge.

### 9.1 Feature gates / kill-switches

Where reasonable, sections are feature-gated in `Cargo.toml`:

- `shell/jobs` — disable jobs path if §6 destabilizes
- `vfs/extended-stat` — protocol bump can be reverted if backend lags

Off by default until smoke green; flip default-on, gate removed in next PR. Avoids long-lived dead branches.

---

## 10. Sequencing

### 10.1 Phase 3 status

Phase 3 is considered done in practice — `SpaceDestroy`, `poll()`/`select()`, warning cull, `/proc` H9/H10 counters all shipped. The 1000-iteration soak test was deferred. ROADMAP.md exit-criteria boxes are still unchecked; that's a documentation lag, not a real blocker.

Tail of Phase 3 work that bleeds into Phase 4 prologue:

- Update ROADMAP.md to tick Phase 3 boxes (with explicit "soak deferred to Phase 5+") and add a Phase 3 closing-notes block matching Phase 1/2 style. One small commit.
- Reconcile `memory/project_phase3_soak_punted.md` with this reality.

This is housekeeping, not a Phase 4 deliverable, but it should land on day 0 so the roadmap and the work stay in sync.

### 10.2 Phase 4 starts now

No waiting. The §7.2 pipe diagnostic is the literal first work item, because it tells us whether the 2→3 stage gap is a 1-day fix or a structural problem.

### 10.3 Phase 4 kickoff checklist

- [ ] ROADMAP.md updated: Phase 3 closing notes added; soak explicitly deferred
- [ ] `memory/project_phase3_soak_punted.md` reconciled with current state
- [ ] develop branch clean (no WIP)
- [ ] `harness_matrix.sh` green on develop head
- [ ] §7.2 diagnostic run; outcome documented
- [ ] this design doc committed
- [ ] implementation plan written (writing-plans skill output)
- [ ] PR 1 in flight

### 10.4 Phase 5 (network) downstream effects

- BSD socket API will reuse `libcluu/cli.rs` for `wget`/`ping` arg parsing
- DNS resolver client will reuse the new "rich result + batched fields" idiom from §5.4.1
- Network tools (`ping`, `wget`, `nc`, `curl`-lite) follow SOLID + GNU-close conventions established here

Conventions set in Phase 4 are reused, not re-decided, in Phase 5.

### 10.5 Calendar (loose)

Per roadmap policy, no committed dates. Order-of-magnitude estimate:

- §4 cleanup: 1 week
- §5 utils: 2-3 weeks
- §6 jobs: 2 weeks
- §7 pipe: few days
- §8/§9 polish: 1 week

If actual elapsed exceeds 2× the estimate at any boundary → stop, retro, decide whether to descope or fix process.

---

## 11. Acceptance

Phase done when:

- All §2 exit-criteria boxes checked
- `harness_matrix.sh` green
- Compiler warnings ≤ Phase 3 baseline
- `git log --since=phase4-start --grep=^WIP` empty
- `memory/project_phase3_soak_punted.md` retracted/corrected
- Phase 4 retrospective in `docs/ROADMAP.md` matching Phase 1/2 closing-notes style
