# Repository Guidelines

## Project Structure & Module Organization
- `kernel/` contains the no_std microkernel (scheduler, IPC, MM, tokens, IRQ routing) and exposes the helpers userspace services need.
- `userspace/` holds init, procmgr, shell, VFS/console/drivers, and per-binary crates; each binary keeps its own directory (e.g., `userspace/init`, `userspace/shell`).
- Helpers such as `klibcluu/` (common utilities), `xtask/` (build/boot image orchestration), and `kernel-tests/` (std crate for kernel assertions) coordinate tooling.
- Assets to edit only when rebuilding images are in `artwork/`, `bootboot_image/`, and `triplets/`; the initrd layout and boot scripts live under `xtask/boot/`.
- Tests and demonstration data live in `userspace/tests/` and `kernel-tests/tests/`, so keep subsystem-specific test code next to the feature it exercises.

## Build, Test, and Development Commands
- `cargo xtask build` compiles kernel, userspace, and the initrd image in dev profile; rerun after touching `kernel/`, `userspace/`, or `xtask/`.
- `cargo xtask run` boots QEMU with the current image (`--debug` pauses for GDB on `:1234` and enables telnet on `:4321`).
- `cargo xtask test` runs the full regression suite (kernel + userspace + boot logic); specify `--test` flags to limit scope.
- `cargo xtask kernel` / `cargo xtask userspace` rebuild only one layer for quicker iteration; the `Makefile` wraps these (`make build`, `make run`, `make test`).
- `cargo fmt` and `cargo clippy` are required before review-ready changes; they follow the `rust-toolchain.toml` pins so everyone shares the same formatter/linter.

## Coding Style & Naming Conventions
- Use four-space indentation, 100-column soft limit, and keep blocks short; prefer descriptive names over terse abbreviations.
- Follow Rust idioms: `snake_case` modules/functions, `CamelCase` types, and `SCREAMING_SNAKE_CASE` constants (e.g., `PAGE_SIZE`).
- All subsystems must adhere to SOLID principles (single responsibility, small traits, dependency inversion); traits such as `Scheduler`, `Repository`, and `AllocationStrategy` are good role models.
- Comment only when the intent is not obvious; prefer short, descriptive helpers inside `kernel/` to limit the trusted surface.

## Testing Guidelines
- `cargo xtask test` is the master suite (~145 tests); treat it as the final gate before declaring readiness.
- `kernel-tests/tests/` files are organized by subsystem (e.g., `elf_tests.rs`, `mm_tests.rs`, `ipc_tests.rs`); name new files after the feature under test and import from `kernel_tests::cluu_kernel`.
- `userspace/tests/` runs its own `cargo test`; use crate-level test files to validate user-facing services without rebuilding the kernel.
- Always record exactly how you ran the tests (e.g., `cargo test --test mm_tests -- --nocapture`) so reviewers can reproduce the failure path.

## Commit & Pull Request Guidelines
- Keep commits short and present tense (`Add token derive handler`, `Fix initrd layout`); link related issues when available.
- PR descriptions should name the targeted domain (kernel/userspace/tooling), list the commands you executed, and highlight follow-up work (e.g., “needs syscall coverage for grants”).
- Include relevant logs/screenshots for console-focused changes; skip shipping binaries or heavy artifacts.
- Mention unresolved risks (missing tests, required capability rights) so reviewers know what remains.

## Token & Authority Notes
- The kernel mints init’s root token in `kernel/src/bootstrap.rs`; all authority flows through tokens, and every right (READ, WRITE, GRANT, IPC, IRQ, SPACE_MAP, THREAD_CREATE, etc.) is explicit.
- Use `token::derive` to hand procmgr (and subsequent services) a degraded token that limits them to only the rights they need (full threads + necessary space rights + IPC/IRQ + GRANT for zero-copy transfers).
- Once the critical processes spawned by init each yield once (via `sys_yield`) the scheduler transitions from CRITICAL to NORMAL mode and APIC preemption begins, so design helpers to yield after setup.
