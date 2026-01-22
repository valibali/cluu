# Contribute to CLUU

Thanks for helping with CLUU. The project favors explicit capabilities, minimalism, and SOLID structure. Keep changes small and make control flow easy to audit.

Quick start:

```
cargo xtask run --debug
```

Common commands:
- `make run-debug` or `cargo xtask run --debug`
- `cargo xtask build`
- `cargo xtask test` (if available)
- `cargo fmt` and `cargo clippy`

Core rules:
- No new syscall numbers; use flags or existing hooks.
- Prefer `Result` over panics, especially in `no_std` paths.
- Keep logging precise (`TRACE` for noise, `INFO` for user-visible events).
- Use the registry for lazy endpoint wiring; pass tokens explicitly.

When contributing:
- Describe the subsystem and intent in commits.
- Include test steps and logs (telnet output) when relevant.
- Keep changes scoped to one behavior.
