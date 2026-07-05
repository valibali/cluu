# CLUU Harness

The test harness lives in `python/` and is a Python package (`cluu_harness`).

## Quick start

```bash
cd python
pip install -e '.[dev]'

python -m cluu_harness --list                          # list cases
python -m cluu_harness --case l2_login --no-build      # run one case
python -m cluu_harness --no-build                      # run all
pytest -m smoke                                        # no-QEMU unit tests
pytest -m slow                                         # QEMU cases
pytest -m "smoke or slow"                              # both
```

## Adding a case

Declare it in `python/cluu_harness/catalog.py` via the `@cluu_case`
decorator. If the case needs a new `MARKER_MODE`, add a `MarkerModeSpec`
in `markers.py`. See `python/README.md` for the full guide.

## Env-var compatibility

Every env var the retired bash harness read is also read by the Python
config (`HarnessConfig`). The same `MARKER_MODE=l2_ls RUN_WAIT=45`
invocation translates to:

```bash
MARKER_MODE=l2_ls RUN_WAIT=45 python -m cluu_harness --case l2_ls --no-build
```

## Status

11 of ~120 cases from the retired bash harness have been ported. Add
modes to `markers.py` and cases to `catalog.py` as needed.
