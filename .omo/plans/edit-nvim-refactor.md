# edit-nvim-refactor - Work Plan

## TL;DR (For humans)

**What you'll get:** Edit starts fast, handles Ctrl+C without trashing the terminal, survives window resize without visual artifacts, and plugins load via the correct session procmgr. Hungarian QWERTZ keyboard layout works in the shell and all TUI apps.

**Why this approach:** (1) Clear ISIG in `enter_raw()` because CLUU's line discipline checks ISIG before the raw/canonical split — 0x03 is intercepted as SIGINT even in raw mode. (2) Route `lookup_service("procmgr:spawn")` through session-procmgr via `CLUU_SESSION_ID` env var (already set by shell) rather than adding a new param slot — simplest fix that respects existing patterns. (3) Query console dims at top of `Program::run()` loop to detect resize without key events.

**What it will NOT do:** No new kernel syscalls. No new param slots. No runtime ACL. No changes to the capability/visibility model. No changes to root procmgr's spawn logic. No plugin system rework — just fix routing so plugins spawn via session-procmgr.

**Effort:** Medium
**Risk:** Medium — touches terminal raw mode (can break shell) and spawn routing (can break pipes)
**Decisions to sanity-check:** (1) ISIG clearing via PTS tcsetattr only (legacy TTY_CTL path gets TTY_LFLAG_ISIG constant). (2) `lookup_service` fallthrough to registry when session-procmgr lookup fails — preserves backward compat. (3) Ctrl+C in edit = switch to normal mode (not quit).

Your next move: approve, or run a high-accuracy review. Full execution detail follows below.

---

> TL;DR (machine): Medium effort, medium risk. Fix ISIG clearing in enter_raw, route procmgr:spawn via CLUU_SESSION_ID, detect resize in Program::run, add HU keyboard layout. 4 independent bundles, 2 harness gates.

## Scope

### Must have
1. **ISIG fix**: `enter_raw()` clears ISIG so Ctrl+C (0x03) is delivered as a byte in raw mode, not as SIGINT. Edit handles 0x03 as "switch to normal mode."
2. **Spawn routing**: `lookup_service("procmgr:spawn")` routes to `session-procmgr:spawn:{sid}` when `CLUU_SESSION_ID` env var is set, with fallthrough to registry. Fixes plugin spawn (InvalidArgument from root procmgr).
3. **Viewport resize**: `Program::run()` queries console dims (ioctl TIOCGWINSZ) at top of each loop, detects dimension change, clears screen + resets `prev_buffer` on change. Calls `model.on_resize()`.
4. **Hungarian keyboard layout**: Add `HuLayout` to `kbd/src/layout.rs`, wire into kbd context based on a layout config field.

### Must NOT have (guardrails, anti-slop, scope boundaries)
- NO new param slots (PARAM_ROOT_SPAWN_EP abandoned — env var routing is simpler)
- NO changes to `subscribe_output` beyond mirroring the procmgr:spawn virtual routing
- NO changes to root-procmgr spawn logic (only rename registration from "procmgr" to "root-procmgr")
- NO changes to session-procmgr's existing spawn/pipe handlers
- NO plugin system rework (PluginRegistry stays, just fix routing so plugins CAN load)
- NO new syscalls (AGENTS.md §2)
- NO runtime ACL (AGENTS.md §3)
- NO kernel changes (kernel freeze active)
- NO removal of existing plugin code (plugins may work once routing is fixed)
- NO changes to the legacy tty driver (userspace/tty/) — edit runs behind cluuterm PTS

## Verification strategy
> Zero human intervention - all verification is agent-executed.
- Test decision: tests-after + harness verification
- Evidence: `.omo/evidence/task-<N>-edit-nvim-refactor.<ext>`
- Harness singleton: only ONE QEMU instance at a time. No parallel QEMU.
- All markers via `debug_print!` (COM2 serial). `write_fd(1/2, ...)` does NOT reach COM2.

## Execution strategy

### Parallel execution waves

**Wave 1** (independent, parallel):
- Todo 1: ISIG fix in enter_raw (libcluu/src/posix/tty.rs)
- Todo 3: Viewport resize in Program::run (libtui/src/program.rs)
- Todo 5: Hungarian keyboard layout (kbd/src/layout.rs + kbd/src/context.rs)

**Wave 2** (depends on Todo 1):
- Todo 2: Ctrl+C handling in edit (edit/src/normal.rs + edit/src/insert.rs)

**Wave 3** (depends on Todo 4):
- Todo 4: Spawn routing in lookup_service (libcluu/src/registry.rs)

**Wave 4** (verification):
- Todo 6: Full harness verification (all 4 cases)

### Dependency matrix
| Todo | Depends on | Blocks | Can parallelize with |
| --- | --- | --- | --- |
| 1 (ISIG) | — | 2 | 3, 4, 5 |
| 2 (Ctrl+C edit) | 1 | 6 | 3, 4, 5 |
| 3 (Viewport) | — | 6 | 1, 2, 4, 5 |
| 4 (Spawn routing) | — | 6 | 1, 2, 3, 5 |
| 5 (HU layout) | — | 6 | 1, 2, 3, 4 |
| 6 (Harness) | 1, 2, 3, 4, 5 | — | — |

## Todos

- [ ] 1. Clear ISIG in enter_raw() so 0x03 is delivered as a byte in raw mode
  What to do:
  - `userspace/libcluu/src/posix/tty.rs`: In `enter_raw()`, add `TTY_LFLAG_ISIG` to the flags cleared. The PTS fallback path (tcsetattr) already manipulates the full Termios struct — clear `ISIG` bit in `c_lflag`. For the legacy TTY_CTL path, add `pub const TTY_LFLAG_ISIG: u32 = 0x01;` (matching `cluu_wire::pts::ISIG = 0x0001`) and include it in the lflag clear mask.
  - In `leave_raw()` (restore), re-set ISIG (it was in the saved termios).
  Must NOT do: Do NOT change the line discipline's `feed_byte` logic — it already correctly checks ISIG. Do NOT touch the legacy tty driver (userspace/tty/src/main.rs). Do NOT clear ISIG in `Termios::default_pts()` — only in `enter_raw()`.
  Parallelization: Wave 1 | Blocked by: — | Blocks: 2
  References:
  - `userspace/libcluu/src/posix/tty.rs` — `enter_raw()` at line ~98, `leave_raw()`, `SavedTty` struct with `pts_fallback: bool`
  - `userspace/libcluu/src/tty_core/line_discipline.rs:207-242` — `feed_byte()`: checks ISIG before canonical/raw split. Comment: "ISIG translations always come first regardless of canonical mode." VINTR = c_cc[3], default 0x03.
  - `userspace/cluu_wire/src/pts.rs:93` — `ISIG: u32 = 0x0001`
  - `include/sys/termios.h:23` — `#define ISIG 0x0001`
  Acceptance criteria: `cargo build -p libcluu` succeeds. In raw mode, 0x03 is delivered to `read()` as byte 0x03, not converted to SIGINT.
  QA scenarios: Harness `l2_edit_cluuterm` — send Ctrl+C in edit, verify `RAW_003_DELIVERED` marker on COM2. Evidence `.omo/evidence/task-1-edit-nvim-refactor.log`
  Commit: Y | fix(tty): clear ISIG in enter_raw so Ctrl+C is delivered as byte in raw mode

- [ ] 2. Edit handles 0x03 (Ctrl+C) as switch-to-normal-mode
  What to do:
  - `userspace/edit/src/normal.rs`: No change needed (normal mode already ignores 0x03 or treats as no-op).
  - `userspace/edit/src/insert.rs` (or wherever insert mode key handling lives): Add `KeyEvent::Char('\x03')` → set Mode::Normal. Do NOT quit edit on Ctrl+C — switch to normal mode (like Esc).
  - `userspace/edit/src/main.rs`: In `update()`, ensure 0x03 is handled before plugin dispatch.
  Must NOT do: Do NOT quit edit on Ctrl+C. Do NOT send a signal. Do NOT add 0x03 handling in normal mode (it should be a no-op there).
  Parallelization: Wave 2 | Blocked by: 1 | Blocks: 6
  References:
  - `userspace/edit/src/normal.rs:99` — `KeyEvent::Char('i')` sets Mode::Insert
  - `userspace/edit/src/main.rs` — `update()` function, key dispatch
  - `userspace/edit/src/mode.rs` — Mode enum
  Acceptance criteria: `cargo build -p edit` succeeds. In edit insert mode, Ctrl+C switches to normal mode. Terminal is not broken after Ctrl+C.
  QA scenarios: Harness `l2_edit_cluuterm` — enter insert mode (press 'i'), send Ctrl+C, verify `CTRL_C_NORMAL_MODE` marker, verify edit still responds to keys. Evidence `.omo/evidence/task-2-edit-nvim-refactor.log`
  Commit: Y | feat(edit): handle Ctrl+C as switch-to-normal in insert mode

- [ ] 3. Program::run() detects viewport resize and clears screen
  What to do:
  - `userspace/libtui/src/program.rs`: At the top of the run loop (before `model.view()`), call `terminal_size()` via `_ioctl(1, TIOCGWINSZ)` to get current console dims. Compare against `prev_buffer.width/height`. If changed: emit `\x1b[2J` (clear screen), reset `prev_buffer = ScreenBuffer::new(0, 0)`, call `model.on_resize()`.
  - `userspace/libtui/src/program.rs`: Add `extern "C" { fn _ioctl(...) }` and `WinSize` struct (copy from top/src/main.rs or edit/src/mode.rs). Or better: add a `pub fn terminal_size() -> (usize, usize)` to `libtui/src/lib.rs` and call it from Program::run().
  Must NOT do: Do NOT add SIGWINCH handler (previous attempt lost keypresses). Do NOT use `wait_for_data` timeout reads (loses keypresses). Do NOT change the blocking `decode()` read — just query dims at loop top before checking for input. Do NOT remove `on_resize()` from the Model trait.
  Parallelization: Wave 1 | Blocked by: — | Blocks: 6
  References:
  - `userspace/libtui/src/program.rs` — `Program::run()`, `prev_buffer: ScreenBuffer` at line 15, initialized `ScreenBuffer::new(0, 0)` at line 26. Loop at line 45-73. `view = self.model.view()` gets viewport from model.
  - `userspace/libtui/src/lib.rs:33` — Model trait with `on_resize()` default no-op at line 57
  - `userspace/edit/src/mode.rs` — `Viewport::from_console()` uses `_ioctl(1, TIOCGWINSZ)`
  - `userspace/top/src/main.rs:48-66` — `WinSize` struct + `terminal_size()` function (copy this pattern)
  - `userspace/edit/src/main.rs:138-149` — `EditModel::on_resize()` re-queries viewport + recomputes render data
  Acceptance criteria: `cargo build -p libtui` succeeds. On terminal resize, screen clears and re-renders without artifacts.
  QA scenarios: Harness `l2_edit_libtui` — resize terminal mid-edit (send resize keystroke), verify `VIEWPORT_RESIZE_OK` marker, no visual artifacts. Evidence `.omo/evidence/task-3-edit-nvim-refactor.log`
  Commit: Y | fix(libtui): detect viewport resize in Program::run and clear screen

- [ ] 4. Eliminate ambiguous "procmgr" name — rename to "root-procmgr", route via CLUU_SESSION_ID
  What to do:
  - **Rename root-procmgr registration**: `userspace/root-procmgr/src/main.rs:1103`: change `registry::init("procmgr")` → `registry::init("root-procmgr")`. This makes all outputs `root-procmgr:spawn`, `root-procmgr:session`, `root-procmgr:main`. The name "procmgr" no longer exists in the registry.
  - **Update boot callers** to use `root-procmgr:spawn` instead of `procmgr:spawn`: `compositor/src/window_mgr.rs:496`, `kbd/src/context.rs:83`, `vfs/src/main.rs:326`, and all probes that run as boot processes. Session processes already use `session-procmgr:spawn:{sid}`.
  - **Add virtual routing in `lookup_service`**: `userspace/libcluu/src/registry.rs`: if name == "procmgr:spawn", check `CLUU_SESSION_ID` env var (use existing `libcluu::posix::read_env_var` at `libcluu/src/posix/env.rs:323`). If set and non-empty, return `lookup_service(&format!("session-procmgr:spawn:{}", sid))` — NO fallthrough. If not set, return `lookup_service("root-procmgr:spawn")` — NO fallthrough. The name "procmgr:spawn" is now purely virtual — it never hits the registry.
  - **Add same virtual routing in `subscribe_output`**: mirror the existing `vfs:main` pattern at registry.rs:287. If ("procmgr", "spawn") and `CLUU_SESSION_ID` set → `subscribe_output("session-procmgr", &format!("spawn:{}", sid))`. If not set → `subscribe_output("root-procmgr", "spawn")`. NO fallthrough either way.
  - This closes both escape paths AND eliminates the ambiguous "procmgr" name. Session processes get session-procmgr. Boot processes get root-procmgr. Nobody can reach the wrong one.
  Must NOT do: Do NOT add PARAM_ROOT_SPAWN_EP param slot. Do NOT allow fallthrough to registry for "procmgr:spawn" — it's virtual now. Do NOT change root-procmgr's spawn logic. Do NOT change session-procmgr's existing spawn/pipe handlers. Do NOT add runtime ACL or visibility checks. Do NOT change other service lookups (vfs:main already handled, devmgr/console/kbd/compositor are singletons with no session equivalent — acceptable).
  Parallelization: Wave 1 | Blocked by: — | Blocks: 6
  References:
  - `userspace/root-procmgr/src/main.rs:1103` — `registry::init("procmgr")` → change to `"root-procmgr"`
  - `userspace/libcluu/src/registry.rs:99-106` — `lookup_service()` with existing `vfs:main` short-circuit via `PARAM_SESSION_VFS_EP`
  - `userspace/libcluu/src/registry.rs:287` — `subscribe_output()` with existing `vfs:main` short-circuit (MIRROR THIS PATTERN)
  - `userspace/libcluu/src/spawn.rs:28` — `lookup_service("procmgr:spawn")` (virtual name, routes via env)
  - `userspace/libcluu/src/posix/pipe.rs:29,52` — `lookup_service("procmgr:spawn")` (virtual, routes to session-procmgr)
  - `userspace/libcluu/src/posix/process.rs:79` — `lookup_service("procmgr:spawn")` (virtual, routes)
  - `userspace/edit/src/plugin.rs:96` — `lookup_service("procmgr:spawn")` for plugin pipe creation
  - `userspace/edit/src/plugin.rs:166` — `libcluu::spawn::spawn(env)` for plugin spawn
  - `userspace/libcluu/src/posix/env.rs:323` — `read_env_var()` ALREADY EXISTS (do NOT add new helper, import this)
  - `userspace/shell/src/main.rs:194` — reads `CLUU_SESSION_ID` env var via `libcluu::posix::read_env_var`
  - `userspace/shell/src/commands/exec.rs:271` — existing pattern: `format!("session-procmgr:spawn:{}", context.session_id)`
  - `userspace/shell/src/commands/builtins/registry.rs:203` — `subscribe_output("procmgr", "spawn")` (virtual, routes)
  - `userspace/shell/src/main.rs:109` — `subscribe_output("procmgr", "spawn")` (virtual, routes)
  - `userspace/compositor/src/window_mgr.rs:496` — `lookup_service("procmgr:spawn")` (boot caller, change to `root-procmgr:spawn`)
  - `userspace/kbd/src/context.rs:83` — `subscribe_output("procmgr", "spawn")` (boot caller, change to `root-procmgr`)
  - `userspace/vfs/src/main.rs:326` — `subscribe_output("procmgr", "spawn")` (boot caller, change to `root-procmgr`)
  - `userspace/session-procmgr/src/main.rs:174` — `registry::init("session-procmgr")` (already correct)
  - `userspace/session-procmgr/src/elf_spawn.rs:472` — sets `PARAM_SESSION_VFS_EP` for children (should also set CLUU_SESSION_ID env so grandchildren route correctly)
  - Boot callers that need rename to `root-procmgr:spawn`: `probes/pm_bootstrap_two_pmgr/src/main.rs:43`, `probes/pm_proc_query_all_cap/src/main.rs:62`, `probes/escalateprobe/src/main.rs:27`, `probes/nestprobe/src/main.rs:27`, `probes/ownerdeny/src/main.rs:59`, `probes/cascadeprobe/src/main.rs:27`, `probes/viewprobe/src/main.rs:27`, `probes/jobmix/src/main.rs:59`, `probes/detachprobe/src/main.rs:27`, `probes/sutest/src/main.rs:40,100`, `probes/sudotest/src/main.rs:23`, `probes/killdeny/src/main.rs:39`, `probes/jobchurn/src/main.rs:73`
  Acceptance criteria: `cargo build` succeeds (all crates). `grep -rn '"procmgr:spawn"\|"procmgr"' userspace/ --include=*.rs` returns zero matches outside registry.rs (where the virtual routing lives). Session processes' `lookup_service("procmgr:spawn")` AND `subscribe_output("procmgr", "spawn")` return session-procmgr endpoint. Boot processes' lookups return root-procmgr endpoint. Pipe creation works in sessions (via session-procmgr). Session process CANNOT reach root procmgr.
  QA scenarios: Harness `l2_edit_cluuterm` — edit plugins load (or fail gracefully if edit-plugin image has issues), verify `PLUGIN_SPAWN_OK` or at least no `InvalidArgument` marker. Evidence `.omo/evidence/task-4-edit-nvim-refactor.log`
  Commit: Y | refactor(registry): rename procmgr→root-procmgr, route procmgr:spawn via CLUU_SESSION_ID

- [ ] 5. Add Hungarian QWERTZ keyboard layout
  What to do:
  - `userspace/kbd/src/layout.rs`: Add `HuLayout` struct implementing the same trait as `UsLayout`. Map Hungarian QWERTZ scancodes: key differences from US: Z↔Y swap, ;→ö, '→á, etc. Dead keys (¨, ˜) map to closest ASCII stand-ins (per existing file comment: "we approximate with closest ASCII characters and map dead keys to simple ASCII stand-ins").
  - `userspace/kbd/src/context.rs`: Add layout selection — read a layout config (from env var `CLUU_KBD_LAYOUT` or a config file). Default to `UsLayout`. If `hu`, use `HuLayout`.
  - `userspace/kbd/src/main.rs`: Wire layout selection into kbd init.
  Must NOT do: Do NOT change the US layout. Do NOT add accented Unicode characters — use ASCII stand-ins per existing convention. Do NOT change the kbd IPC protocol. Do NOT add a new syscall for layout switching.
  Parallelization: Wave 1 | Blocked by: — | Blocks: 6
  References:
  - `userspace/kbd/src/layout.rs` — `UsLayout` struct, 291 lines, 18 symbols. File header mentions Hungarian approximation.
  - `userspace/kbd/src/context.rs` — kbd context, scancode dispatch
  - `userspace/kbd/src/main.rs` — kbd init
  - `userspace/cluu_wire/src/pts.rs` — ISIG, ICANON, ECHO constants
  Acceptance criteria: `cargo build -p kbd` succeeds. With `CLUU_KBD_LAYOUT=hu`, pressing physical 'z' key produces 'y' and vice versa. Unit test: `HuLayout::letter_for_scancode(0x2C)` returns 'y' (Z position on QWERTZ).
  QA scenarios: Unit test `rustc --edition 2021 --test userspace/kbd/src/layout.rs -o /tmp/hu_layout && /tmp/hu_layout`. Harness: type 'z' in shell with HU layout, verify 'y' appears. Evidence `.omo/evidence/task-5-edit-nvim-refactor.log`
  Commit: Y | feat(kbd): add Hungarian QWERTZ keyboard layout

- [ ] 6. Full harness verification — all 4 existing cases pass
  What to do:
  - Rebuild all containers: `cargo xtask build`
  - Run harness cases in sequence (NOT parallel — singleton): `l2_login`, `l2_edit_libtui`, `l2_edit_cluuterm`, `l2_sysmon_basic`
  - Add debug markers for new behavior: `RAW_003_DELIVERED` (ISIG fix), `CTRL_C_NORMAL_MODE` (edit Ctrl+C), `VIEWPORT_RESIZE_OK` (resize), `PLUGIN_SPAWN_OK` (routing)
  - Verify no regressions: all 4 cases PASS with same or better timing
  Must NOT do: Do NOT run parallel QEMU instances. Do NOT skip the build. Do NOT mark as passing without serial log evidence.
  Parallelization: Wave 4 | Blocked by: 1, 2, 3, 4, 5 | Blocks: —
  References:
  - `python/cluu_harness/case_defaults.py` — case definitions
  - `python/cluu_harness/markers.py` — marker modes
  - Serial log at `/tmp/cluu-serial-com2.log`
  Acceptance criteria: All 4 harness cases PASS. Serial log shows new markers. No regression in timing.
  QA scenarios: `python3 -m cluu_harness --case l2_login --no-build` etc. Evidence `.omo/evidence/task-6-edit-nvim-refactor.log`
  Commit: N

## Final verification wave
> Runs in parallel after ALL todos. ALL must APPROVE. Surface results and wait for the user's explicit okay before declaring complete.
- [ ] F1. Plan compliance audit — verify all changes match plan scope, no scope creep
- [ ] F2. Code quality review — check for `unwrap`, `as any`, empty catch, style violations
- [ ] F3. Real manual QA — run all 4 harness cases, verify serial logs
- [ ] F4. Scope fidelity — no new syscalls, no param slots, no runtime ACL, no kernel changes

## Commit strategy
- One commit per todo (except todo 6 which is verification only)
- Conventional commits: `fix()`, `feat()`
- Build and test before each commit

## Success criteria
1. Edit survives Ctrl+C in insert mode (switches to normal mode, terminal not broken)
2. Edit survives window resize (no visual artifacts, no stale white cells)
3. Edit plugins spawn via session-procmgr (no InvalidArgument from root procmgr)
4. Hungarian QWERTZ layout works (z↔y swap)
5. All 4 existing harness cases PASS
6. No new syscalls, no kernel changes, no param slots, no runtime ACL
