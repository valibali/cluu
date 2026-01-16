# Contributing to CLUU

Thanks for helping improve CLUU. This project values clarity, explicit authority, and minimalism. Keep changes focused and well‑documented.

## Quick Start

```
cargo xtask run --debug
```

Serial console:

```
telnet localhost 4321
```

## Build & Test

- `make run-debug` or `cargo xtask run --debug`
- `cargo xtask build`
- `cargo xtask test` (if available)
- `cargo fmt` and `cargo clippy` for Rust changes

## Code Style

- SOLID!
- No new syscall numbers; use flags or existing hooks.
- Keep kernel `no_std` tidy (no panics, return `Result`).
- Use clear logging levels (`TRACE` for IRQ/sched noise, `INFO` for user‑relevant events).
- Avoid global singletons unless already established.

## IPC & Tokens

- IPC is synchronous rendezvous; avoid hidden work.
- Tokens are the only authority. Always derive and pass explicit rights.
- Endpoint wiring is lazy and runtime‑resolved via the registry.
- Outputs can have multiple subscribers; inputs are owned by the consumer.

## Userspace Services

- `init` launches services in order: `registry`, `procmgr`, `kbd`, `tty`, `console`, `shell`.
- `tty` provides line discipline and routes input/output.
- `console` renders output; keep it scheduled on timer ticks.

## Commit Guidelines

- Keep commits scoped to one behavioral change.
- Use imperative, subsystem‑prefixed subjects (e.g., `tty: add line buffering`).
- Mention testing in the commit body.
- Always raise a PR, and explain what and why your are doing it.

## Reporting Issues

Please include:
- Boot log (telnet output)
- Steps to reproduce
- Expected vs actual behavior

