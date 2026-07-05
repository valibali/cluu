# CLUU gen2 test harness (Python)

Event/log-capture-based QEMU integration test harness for the CLUU
microkernel. Python port of `scripts/harness_run.sh` et al., designed
for extensibility.

## Why a Python port?

The bash harness works but:

* Adding a case means editing three files (`harness_cases.conf`,
  `harness_run.sh`'s marker `case`, `harness_case_defaults.sh`).
* Marker polling is `grep -Fq` every 0.5s — timing-based, not event-based.
* SLO metric parsing is awk — hard to extend or unit-test.
* No structured result objects; pass/fail is exit-code only.

The gen2 harness fixes all four:

* **One declaration per case** via `@cluu_case` decorator or `Case` dataclass.
* **Event-driven serial matching** — a background thread tails the log
  and returns the instant a marker appears. Timeouts are safety bounds
  only, never the pass/fail criterion.
* **Python metric extraction** — unit-testable, no awk.
* **Structured `CaseResult` / `SuiteResult`** — programmatic consumers
  welcome.
* **pytest integration** — cases auto-parametrize as `slow`-marked tests.

## Layout

```
python/
├── pyproject.toml
├── README.md
├── conftest.py                    # pytest fixtures + sys.path bootstrap
└── cluu_harness/
    ├── __init__.py                # public re-exports
    ├── __main__.py                # `python -m cluu_harness`
    ├── cli.py                     # CLI: --list / --case / --no-build
    ├── config.py                  # HarnessConfig (env-var defaults)
    ├── sendkey.py                 # HU QWERTZ char→sendkey map
    ├── monitor.py                 # QEMU HMP monitor (unix socket)
    ├── serial_stream.py           # ★ event-driven tail + wait_for
    ├── qemu.py                    # build + launch + lifecycle + FB dump
    ├── gdb.py                     # manual / auto-continue / script attach
    ├── markers.py                 # MARKER_MODE → required markers
    ├── case_defaults.py           # per-mode TEST_COMMAND + sendkey seq
    ├── metrics.py                 # SLO extraction + post-checks
    ├── cases.py                   # Case dataclass + @cluu_case registry
    ├── suite.py                   # run_case / run_suite
    └── tests/
        └── test_harness_cases.py  # registered cases + smoke tests
```

## Quick start

```bash
cd python

# Install in editable mode with dev extras (pytest + ruff):
uv pip install -e '.[dev]'         # or: pip install -e '.[dev]'

# List registered cases:
python -m cluu_harness --list

# Run one case (boots QEMU):
python -m cluu_harness --case l2_ls --no-build

# Run the whole suite:
python -m cluu_harness --no-build
```

## Declaring a new case

Two equivalent ways:

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
`markers.py`. If it needs per-mode defaults (test command, sendkey
sequence), add an entry in `case_defaults.py`. That's it — no shell
edits.

## pytest

Smoke tests (no QEMU) run by default:

```bash
pytest -m smoke
```

QEMU-booting cases are marked `slow` and deselected by default:

```bash
pytest -m slow               # run all QEMU cases
pytest -m slow -k l2_ls      # run one
pytest -m "smoke or slow"    # run both
```

## Env-var compatibility

Every env var the bash harness reads is also read by the Python
config (`HarnessConfig`). The same `MARKER_MODE=l2_ls RUN_WAIT=45
./scripts/harness_run.sh --no-build` invocation translates to:

```bash
MARKER_MODE=l2_ls RUN_WAIT=45 python -m cluu_harness --case l2_ls --no-build
```

## Knowledge-vault references

* `cluu-harness-serial-is-streaming` — serial log is a live stream;
  short `RUN_WAIT` only means QEMU was killed mid-boot.
* `cluu-sendkey-nowait-for-login-cases` — login-modal cases set
  `sendkey_sequence_nowait=True`.
* `cluu-harness-sendkey-sleep-must-match-boot` — credential sequence
  sleeps 12s before keys (kbd attaches at ~9.4s).
* `cluu-hu-keyboard-layout-mangles-escapes` — HU QWERTZ swaps y↔z, 0→`,
  /→shift-6, etc. Use raw `sendkey_sequence` for layout-sensitive chars.
* `cluu-harness-fb-dump-testing` — FB dump via `pmemsave` before kill.

## Status

Coexists with the bash harness — same QEMU, same images, same env vars.
Not a replacement (yet); the bash harness still has ~120 MARKER_MODEs
this port covers a representative subset of. Add modes to
`markers.py` as needed.
