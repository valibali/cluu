# Repository Layout

This document defines the canonical CLUU repository structure and naming rules.

## Top-Level

- `kernel/`: microkernel crate.
- `klibcluu/`: shared kernel-side utility crate.
- `userspace/`: userspace services and support crates.
- `tests/kernel/`: kernel test crate (hosted test harness for kernel modules).
- `xtask/`: build orchestration and developer workflows.
- `tools/`: third-party/local build tools used by CLUU (for example `mkbootimg`).
- `docs/`: architecture, audits, active plans, and developer documentation.
- `docs/archive/plans/`: historical/superseded plan documents.
- `scripts/`: automation scripts used by xtask and CI.
- `external/`: source download cache roots; only `external/sources.env` is tracked.
- `target/`, `tmp/`: generated outputs and build caches (never tracked).

## Userspace Layout

- `userspace/libcluu`: shared userspace API crate.
- `userspace/libcluu_syscalls`: syscall static library for C/newlib programs.
- `userspace/c-programs`: C probe/sample programs used for integration checks.
- Service crates stay one-directory-per-service (for example `userspace/shell`, `userspace/tty`, `userspace/procmgr`).

## Naming Rules

- Directory names use `kebab-case` (hyphens), not snake_case.
- Avoid ambiguous names like `misc`, `temp`, `stuff`.
- Keep third-party/tooling directories explicit (`tools/`, `external/`).
- Keep root focused on project entry points (`README.md`, `Cargo.toml`, `Makefile`, `LICENSE`, `AGENTS.md`).

## Cleanliness Rules

- Generated binaries/objects must not be tracked in git.
- Downloaded sources must not be tracked in git.
- `make clean` should reset build artifacts to a repo-clean state for tracked files.
- CI should enforce repository hygiene checks.
