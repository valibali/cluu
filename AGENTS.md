# Repository Guidelines

## Project Structure & Module Organization
- `kernel/src/` hosts the microkernel proper: scheduler (`sched/`), VMM (`mm/`), syscall handling (`syscall/`), IPC, and the bootstrap/token setup that init relies on (`kernel/src/bootstrap.rs` and `kernel/src/token/`).
- `klibcluu/` is the shared runtime for IRQ-safe logging, crypto, and ELF helpers that both the kernel (`klibcluu::logging`) and `xtask` packaging use.
- `userspace/` contains each service crate (`init`, `procmgr`, `shell`, `hello`, etc.) plus `userspace/libcluu` which exports ELF loaders, rights enums, and syscall wrappers (`userspace/libcluu/src/lib.rs`).
- `kernel-tests/` keeps regression suites closest to the kernel sources, while `xtask/` takes care of initrd/tar image creation and the `make`/`cargo xtask` wrappers that copy binaries into `initrd/sys/`.
- Assets (`artwork/`, `bootboot_image/`) and outputs (`target/`, `tmp/`) should not be edited directly; rely on `xtask` so files stay in sync and you avoid manual cross-device tar errors during image generation.

## Build, Test, and Development Commands
- `make run` / `cargo xtask run` builds everything and boots QEMU; `make run-debug` adds the GDB server (`:1234`) and telnet console (`:4321`) for interactive inspection.
- `cargo xtask build`, `cargo xtask kernel`, and `cargo xtask userspace` let you focus on layers without repeating unrelated work; rerun `cargo xtask build` after touching `xtask/src/main.rs` or the initrd manifest.
- Run `cargo xtask test` (or `make test`) before publishing changes; include the exact command output in your PR to prove the regression suite you exercised.
- Use `cargo fmt`, `cargo clippy`, and `cargo test` locally before pushing, especially when editing shared code such as `userspace/libcluu/src/syscall.rs` or `kernel/src/syscall/handlers.rs`.

## Coding Style & Naming Conventions
- Follow SOLID: keep each module focused (`bootstrap`, `token`, `sched`, `ipc`), prefer trait-based boundaries, and inject dependencies via explicit handles rather than global statics.
- Embrace Rust idioms (snake_case functions, CamelCase types, SCREAMING_SNAKE_CASE consts) and keep helper functions small; document unsafe blocks and verify invariants using `debug_assert!`.
- For kernel code (`#![no_std]`), limit inline assembly to architecture files (`kernel/src/architecture/x86_64/`), reuse `klibcluu::logging` for IRQ-safe traces, and keep syscall stubs consistent with `userspace/libcluu` wrappers.
- Userspace binaries should reuse `libcluu` helpers (ELF parsing, rights masks) instead of duplicating logic; declare service lists (e.g., in `userspace/init/src/main.rs`) rather than hard-coding single launches.

## Testing Guidelines
- `cargo xtask test` is the canonical regression command; annotate any new kernel/user tests under `kernel-tests/` or `userspace/tests/` with the subsystem they cover (e.g., `token`, `scheduler`, `ELF`).
- When touching init/procmgr spawn logic, run `cargo xtask userspace` first so the boot image rebuild (via `xtask`) references the latest binaries; mention which initrd image (`initrd/sys/procmgr`, `initrd/sys/init`, etc.) was refreshed.
- Snapshot the command you executed and its success/failure in PR descriptions so reviewers see your platform+profile (dev/release).

## Commit & Pull Request Guidelines
- Keep commits focused and present-tense (`Implement token derivation`, `Add init service list`). Rebase or merge cleanly onto `develop` and avoid bundling unrelated diffs that could complicate review.
- Mention the layer your change touches (`kernel`, `userspace`, `xtask`) in the PR title/body, list the key commands you ran, and summarize any blockers (missing grant logic, TOKEN derivation, etc.).
- Include documentation updates (`AGENTS.md`, `README.md`, `DEBUG_GUIDE.md`) when behavior exposed to contributors changes; add log snippets or screenshots if the change alters the boot output or new syscall tracing.

## Security, Scheduling & Timing Notes
- Authority is always token-based: `init` starts with the root token emitted from `kernel/src/bootstrap.rs`, derives limited-capability tokens via `kernel/src/token/derive.rs`, and hands them to services so they only get the rights they need (`SPACE_MAP`, `THREAD_CREATE`, `IPC`, `IRQ`, `GRANT`).
- Critical services must yield once to signal readiness so the scheduler (`kernel/src/sched/scheduler.rs`) can transition from INITMODE to NORMALMODE; once NORMALMODE is active the APIC timer (via `kernel/src/architecture/x86_64/interrupts.rs`/`pic.rs`) keeps firing and cannot be paused, so expect unstoppable ticks even when the CPU seems idle.
- Regenerate the initrd with `cargo xtask run` (or `make run`) whenever you change a service binary; avoid manual `tar`/`cp` edits across file systems, because `xtask` already handles cross-device streaming and keeps the filesystem layout consistent.
