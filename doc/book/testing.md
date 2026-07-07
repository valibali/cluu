# Testing

CLUU's integration test harness lives in `python/cluu_harness/`. It boots the
real kernel under QEMU, injects keystrokes through the monitor socket, and
validates expected markers in the serial log. The old bash harness
(`scripts/harness_run.sh`) is retired. The Python harness is the canonical
testing tool.

## Setup

```bash
cd python
pip install -e '.[dev]'        # or: uv pip install -e '.[dev]'
```

The `dev` extra pulls pytest and ruff. The harness boots QEMU itself, so
`qemu-system-x86_64` and KVM access are required, same as a manual run.

## Running cases

```bash
python -m cluu_harness --list                    # list registered cases
python -m cluu_harness --list-modes              # list known MARKER_MODEs
python -m cluu_harness --case l2_login --no-build  # run one case, reuse images
python -m cluu_harness --no-build                # run the whole suite
python -m cluu_harness --no-build --stop-on-fail # halt after first failure
python -m cluu_harness --verbose                 # debug-level logging
```

`--no-build` reuses `target/cluu.img` and `target/userdisk.img`. Without it,
the harness rebuilds before each case. Build once with `cargo xtask build`,
then run cases with `--no-build` for fast iteration.

### CLI flags

| Flag | Effect |
|------|--------|
| `--list` | Print registered case names and exit. |
| `--list-modes` | Print known `MARKER_MODE` names and exit. |
| `--case NAME` | Run only the named case. Repeatable. |
| `--no-build` | Skip the build step, reuse existing images. |
| `--marker-mode MODE` | Override `MARKER_MODE` for an ad-hoc run. |
| `--serial-log PATH` | Serial log path (default `/tmp/cluu-serial-com2.log`). |
| `--verbose` / `-v` | Debug-level logging. |
| `--stop-on-fail` | Stop the suite after the first failing case. |

## pytest

Smoke tests run without QEMU and are selected by default. QEMU-booting cases
are marked `slow` and deselected unless asked for.

```bash
pytest -m smoke                # no-QEMU unit tests
pytest -m slow                 # all QEMU cases
pytest -m slow -k l2_ls        # one QEMU case by name
pytest -m "smoke or slow"      # both
```

## Unit tests outside the harness

Most userspace crates are `#![no_std]` and cannot use `cargo test` directly.
Pure-logic modules build with `rustc --test`:

```bash
rustc --edition 2021 --test userspace/tty/src/line_discipline.rs -o /tmp/t && /tmp/t
rustc --edition 2021 --test userspace/procmgr/src/mount_policy.rs -o /tmp/t && /tmp/t
```

The libcluu host-test suite is the exception; it runs under cargo:

```bash
cargo test --manifest-path userspace/libcluu/Cargo.toml --features host-test
```

## MARKER_MODE

Each case has a `MARKER_MODE` that names the set of serial-log strings the run
must produce. Modes are declared in `python/cluu_harness/markers.py` as
`MarkerModeSpec` entries. A mode carries:

- `required_markers`: lines the serial log must contain for the case to pass.
- `fail_marker`: optional regex whose presence flips the case to failed even
  if all required markers appeared (used by failpoint probes that print
  `mapfail: PASS` or `mapfail: FAIL`).
- `category`: drives the post-check (`boot`, `recv`, `leak`, `fairness`,
  `ipc`, `warm_spawn`, `bench`, `generic`).
- `description`: shown by `--list-modes`.

Two markers recur across most modes: `TSC calibrated` (kernel finished early
init) and `[USER] shell: ready` (the shell prompt is up). Boot-only modes need
just the first; shell-driven modes need both.

List every shipped mode with `--list-modes`. The current set covers boot,
recv churn, token audit, leak diagnostics, map failpoints, sender auth,
fairness SLOs, warm-cache spawn, futex, ext2, shell builtins, login, and
compositor. Add new modes by appending a `MarkerModeSpec` to `MARKER_MODES`
in `markers.py`. No other file needs editing for a mode-only addition.

## Adding a case

Two equivalent declarations.

### Decorator (preferred)

```python
from cluu_harness import cluu_case

@cluu_case(
    "l2_my_probe",
    marker_mode="l2_my_probe",     # must exist in markers.MARKER_MODES
    test_command="myprobe",
    run_wait_s=30,
    description="my new probe",
)
class L2MyProbe:
    pass
```

### Programmatic

```python
from cluu_harness import Case, registry

registry.register(Case(
    name="l2_my_probe",
    marker_mode="l2_my_probe",
    test_command="myprobe",
    run_wait_s=30,
))
```

If the case needs a new `MARKER_MODE`, add a `MarkerModeSpec` entry in
`markers.py`. If it needs per-mode defaults (test command, sendkey sequence),
add an entry in `case_defaults.py`. That is the entire workflow. No shell
edits, no second file.

Built-in cases live in `python/cluu_harness/catalog.py`. Out-of-tree cases
can register themselves by importing `cluu_harness` and calling
`registry.register` or using `@cluu_case` from their own module.

## Env-var compatibility

Every env var the retired bash harness read is still read by the Python
config (`HarnessConfig`). A bash invocation and its Python equivalent:

```bash
# bash (retired)
MARKER_MODE=l2_ls RUN_WAIT=45 ./scripts/harness_run.sh --no-build

# Python equivalent
MARKER_MODE=l2_ls RUN_WAIT=45 python -m cluu_harness --case l2_ls --no-build
```

Common env vars:

| Var | Default | Purpose |
|-----|---------|---------|
| `MARKER_MODE` | `legacy_p1` | Mode dispatched through `markers.py`. |
| `RUN_WAIT` | `12` | Seconds to let the guest run before tearing down. Safety bound, not the pass criterion. |
| `SHELL_READY_WAIT` | `45` | Seconds to wait for the shell-ready marker. |
| `SHELL_READY_WAIT_MAX` | `45` | Policy cap on the above. Raise `ALLOW_SLOW_SHELL_WAIT=1` to exceed it for debug sessions. |
| `TEST_COMMAND` | `__AUTO__` | Shell command typed into the guest. `__AUTO__` derives from `case_defaults.py`. |
| `TEST_COMMAND_REPEAT` | `1` | Repeat the test command N times. |
| `KEYSTROKE_COMMANDS` | unset | Extra `sendkey` lines, newline-separated. |
| `KEYSTROKE_COMMANDS_FILE` | unset | File of extra `sendkey` lines. |
| `SERIAL_LOG` | `/tmp/cluu-serial-com2.log` | Serial capture path. |
| `EXPECT_FAULT` | unset | Treat fault patterns as expected, not failures. |
| `REQUIRED_MARKERS` | unset | Override the mode's required markers (newline-separated). |
| `QEMU_EXTRA_ARGS` | unset | Extra QEMU CLI arguments. |

SLO knobs (`MIN_EXIT_COOKIES`, `MAX_DELTA_SPACES`, `MAX_IPC_WAIT_P95_MS`,
etc.) are all read by `HarnessConfig` too. Set them to None or leave unset to
skip the corresponding post-check.

## Sendkey sequences

The harness types into the guest through QEMU's monitor `sendkey` command.
Per-mode defaults live in `case_defaults.py`. The standard root login
sequence is `_CREDS_SENDKEY_ROOT`:

```text
sleep 12
sendkey r  sendkey o  sendkey o  sendkey t  sendkey ret
sleep 2
sendkey r  sendkey o  sendkey o  sendkey t  sendkey ret
```

Three constraints shape this sequence.

1. **The 12-second prefix sleep is load-bearing.** The keyboard IRQ handler
   attaches at roughly 9.4s and the login window appears at 9.8s. Sending
   keys before the handler is wired drops them. Five seconds was the old
   value; it raced against kbd attach and lost intermittently.

2. **Login-modal cases set `sendkey_sequence_nowait=True`.** These cases
   spawn the login modal before any shell, so the harness must not wait for
   a shell-ready marker before sending credentials.

3. **The guest keyboard layout is Hungarian QWERTZ.** The ASCII-to-sendkey
   map in `sendkey.py` accounts for the swaps (y/z, 0/backtick, slash on
   shift-6, brackets on AltGr combos). For layout-sensitive characters,
   prefer raw `SENDKEY_SEQUENCE` entries with explicit QEMU key names rather
   than going through the ASCII translator.

Login credentials in the harness are `root` / `root`. The production seed
account in `etc/users.toml` is `admin` / `admin`; the harness uses a separate
root account that the test image enables.

## Serial is a stream

The serial log is a live stream, not a fixed buffer. The harness tails it
with a background thread and returns the instant a marker appears. Timeouts
are safety bounds only. A missing marker after a short `RUN_WAIT` usually
means QEMU was killed mid-boot, not that the feature broke. Lengthen
`RUN_WAIT` before treating a miss as a real failure.

A framebuffer dump via QEMU `pmemsave` is captured before teardown when
`FB_DUMP_OUT` is set, useful for visual regressions in the console or
compositor.

## Status

The Python harness ships a representative subset of the retired bash
harness's roughly 120 modes. The current set covers every category: boot,
recv, leak, failpoint, fairness, spawn, futex, ext2, shell builtins, login,
compositor. Add modes to `markers.py` and cases to `catalog.py` as needed.
The bash harness is retired and should not be invoked for new work.

## Plan lessons — testing & harness

Distilled implementation lessons from harness-related plans. 2-5 lines
each; see the dated plan file for the long form.

### harness-migration-interactive-login (2026-05-26-autologin-removal-harness-migration)

Every `l2_*` harness case migrated from `SHELL_AUTOSTART_CMD_DEFAULT` to
the interactive login flow (compositor → login → cluuterm → shell),
driving credentials and test commands through QEMU sendkey. The
`try_auto_login` shortcut in root-procmgr was deleted. A
`CREDS_SENDKEY_ROOT` helper variable holds the standard
`sleep 5; root ret; sleep 2; root ret` sequence, extracted to avoid
duplication across ~50 cases. The harness already supported
`SENDKEY_SEQUENCE_DEFAULT` (fires unconditionally) and `TYPED_COMMANDS`
(fires after `[USER] shell: ready` marker) — the migration was wiring, not
new primitives.

### probes-out-of-default-build (2026-05-07-phase4-A-workspace-cleanup)

11 probe crates moved under `userspace/probes/` and dropped from
`default-members`. `cargo xtask build` does not compile probes;
`cargo xtask build-probes` builds them; `cargo xtask build-all` builds
both. Image places probes at `/probes/<name>`. Test-only shell builtins (19
of them) extracted into probe binaries invoked via `/probes/<name>` —
shell builtin registry shrinks ~47 → ~28. The lesson: test-only code
shouldn't ship in the production binary.

### diagnostic-first-3-stage-pipe (2026-05-07-phase4-E-pipe-reverify)

The pipe reverify plan was diagnostic-first: add a 3-stage smoke harness
case, run it against the existing executor, capture the exact failure or
success, *then* fix. The 3-stage path worked; the real gap was env
propagation through pipe stages. Fix was lifting the ENV trailer from the
single-cmd path into a shared payload builder reused by `pipeline.rs`.
Don't fix what you haven't reproduced.

### per-task-build-gate (2026-05-19-implementer-brief)

Commit after every task. Per-task gate: `cargo xtask build` clean between
tasks. No multi-day WIP on `develop`. The harness convention: one minimal
marker per task; retry `vt/manifest` flake exactly once. Pre-existing
flakes (`vt/manifest` NotFound; procmgr PF after `map_elf` NotFound) are
NOT this plan's job — if they flap, retry once and continue. Build-only
verification per task unless the task says `HARNESS GATE`.

### shell-misc-builtins-history-persist (2026-05-07-phase4-F-shell-misc-builtins)

History persists to `~/.cluu_history` via VFS. Alias table lives on
`ShellContext`, expanded at command-line tokenization. `type` looks up via
the existing builtin registry + PATH walk. Each builtin lives in its own
file under `commands/builtins/`. The lesson: muscle-memory builtins
(alias, history, type, help, exit) are small but each has a persistence or
expansion quirk that belongs in its own file, not a monolithic
`commands.rs`.
