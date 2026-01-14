# Repository Guidelines

## Project Structure & Module Organization
- `kernel/` holds the microkernel core (`sched`, `ipc`, `mm`, `syscall`, architecture bootstrapping, device helpers like `keyboard.rs`).  
- `klibcluu/` hosts reusable kernel helpers (crypto, logging); `userspace/libcluu/` mirrors shared syscall/boot/right definitions that are consumed by every userspace service (init, procmgr, console, tty, shell, kbd, etc.).  
- `userspace/` contains per-binary crates: `console`, `procmgr`, `shell`, `tty`, `kbd`, `ramfs`, etc., plus integration programs (`hello`, `cat`).  
- Build/run helpers live in `xtask/`, and images/initrd helpers in `mkbootimg` targets.

## Build, Test, and Development Commands
- `cargo xtask run --debug` – drives the full build/flash/QEMU run in debug mode; it rebuilds kernel, userspace binaries, creates initrd and disk image, then launches QEMU with logging/telnet hints.  
- `cargo xtask test` (if available) runs targeted unit/integration suites; otherwise rely on `cargo test` within individual crates.  
- `make run-debug` wraps the `xtask` invocation and is what CI uses; it also emits QEMU startup info and telnet/GDB connection hints.  
- `cargo fmt` and `cargo clippy` should be run for any Rust changes; prefer workspace commands so all crates stay synchronized.

## Coding Style & Naming Conventions
- Rust sources default to 4 spaces (use `cargo fmt`). Keep `no_std` kernels tidy (avoid panics, prefer `Result` + `Error`).  
- Syscall handlers live under `kernel/src/syscall/**` and log via `klibcluu::trace/info/warn` with the `sys_` or `invoke_` naming scheme (e.g., `invoke_space_create`).  
- Userspace services follow a declarative configuration (init’s `Service` list) and use the shared `libcluu` syscall wrappers.  
- Prioritize SOLID separation: kernel scheduling, console rendering, and IPC/keyboard drivers live in distinct modules, with traits or clear APIs (e.g., scheduler tick/priority table vs. console cursor blinking vs. kbd IRQ attach).
- SOLID printiples shall be followed! Traits everywhere and well-known architectural patterns shall be used!  
- Use descriptive logging levels (`TRACE` for scheduler/IRQ noise, `INFO` for user-relevant events like “init: console ready”); avoid introducing redundant syscall variants (keep `sys_recv` blocking/nonblocking via flags, not new numbers).

## Testing Guidelines
- Run the full image with `cargo xtask run --debug` to ensure init boots, critical services (`console`, `kbd`, `tty`, `procmgr`) register, and the shell renders with keyboard input.  
- Inspect telnet output for `[USER]` traces from console, procmgr, shell to confirm IPC flow (console now wakes on timer interrupts, blinking cursor, receives tty input).  
- Use QEMU’s Trace logs to verify scheduler/timer behavior (idle path hlt loop, timer interrupts triggering `scheduler.tick`, console scheduling).  
- Simulate API flows via `hello` or new shell tests when expanding service contracts (e.g., process exit notification via IPC cookies).

## Commit & Pull Request Guidelines
- Keep commits scoped to a single behavioral change (e.g., “Implement console timer wake-up” or “Wire init → procmgr tokens”).  
- Reference the relevant subsystem (scheduler/console/keyboard) in the title/body, use the imperative mood.  
- Describe testing steps (e.g., “make run-debug” or “cargo xtask run --debug”). Mention QEMU/telnet/verifiers used.  
- PRs should outline the control flow impact (e.g., init still spawns console/kbd/tty/procmgr before NORMALMODE, scheduler tick drives the idle hlt path) and include traces or log patterns if applicable.

## Notes & Agent Instructions
- Console and shell rendering now rely on timer-driven scheduling (priority + aging) rather than IPC wake-ups, so ensure any future IRQ or scheduling tweaks keep timer interrupts unblocked.  
- procmgr owns lower-right processes; it receives derived tokens from init and handles exit notifications via IPC cookies. Maintain that parent-child IPC contract (console/tty forward keystrokes, shell uses tty/stdout handles).  
- When touching syscall_entry.asm or scheduler wiring, remember the only context switches should come from `yield_now()` or timer/IRQ dispatch; avoid extra cache spinlocks.  
- No new syscall numbers; rely on flag-based behaviors (sys_recv) and existing handler hooks.  
- Keep the console cursor blinking by scheduling the console thread even without IPC updates—timer interrupt drives the cursor FSM.
