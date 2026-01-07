# Repository Guidelines

## Project Structure & Module Organization
- `kernel/` is the no_std microkernel (scheduler, IPC, tokens, IRQs), exporting only the helpers needed by other crates.
- `userspace/` keeps init, shell, VFS, console, drivers, and `userspace/tests/`; each binary lives in its own folder (e.g., `userspace/shell`).
- Workspace helpers: `klibcluu` (utilities), `xtask/` (build/initrd/image orchestration), and `kernel-tests/` (std crate for kernel assertions).
- Assets live in `artwork/`, `bootboot_image/`, and `triplets/`; edit them only when rebuilding images or boot scripts.

## Build, Test, and Development Commands
- `cargo xtask build` compiles kernel, userspace, and the initrd image (dev profile).
- `cargo xtask run` boots QEMU; add `--debug` to pause, let GDB listen on `:1234`, and expose telnet on `:4321`.
- `cargo xtask test` runs the full regression suite, including `kernel-tests`.
- `cargo xtask userspace` or `cargo xtask kernel` rebuild only one layer when you are iterating.
- The `Makefile` targets (`make build/run/test`) wrap these xtask commands for convenience.

## Coding Style & Naming Conventions
- Always run `cargo fmt`; `rust-toolchain.toml` pins `rustfmt` and `clippy` for everyone.
- Follow Rust naming: `snake_case` modules, `CamelCase` types, and `SCREAMING_SNAKE_CASE` constants (e.g., `PAGE_SIZE`).
- Run `cargo clippy` on sensitive paths and keep kernel helpers tight instead of sprawling utilities.

## Testing Guidelines
- `cargo xtask test` is the canonical suite (≈145 tests) for kernel and userspace.
- `kernel-tests/tests/` organizes feature files (`elf_tests.rs`, `mm_tests.rs`, `ipc_tests.rs`); name new files after the subsystem and import from `kernel_tests::cluu_kernel`.
- Userspace helpers in `userspace/tests/` run with their own `cargo test` when only those files change.
- Always log the command you executed (e.g., `cargo test --test mm_tests -- --nocapture`) so reviewers can reproduce it.

## Commit & Pull Request Guidelines
- Use short, present-tense commits such as `Add token derivation test` or `Fix initrd layout`.
- PRs should say whether the change targets kernel, userspace, or tooling, list the commands you ran, and link the related issue if one exists.
- Attach logs or screenshots only for console-facing work; skip binaries and heavy artifacts.
- Note any follow-up work (“needs new capability tests”) so reviewers know what remains.

## Tooling & Configuration Tips
- `rust-toolchain.toml` locks Rust + `rust-src`, `rustfmt`, and `clippy` so everyone shares the same toolset.
- `xtask` invokes `mkbootimg` and BOOTBOOT when producing `cluu.img`, so keep those binaries reachable before running builds.
- Refer to `DEBUG_GUIDE.md` whenever you need GDB/telnet visibility into the boot process.
