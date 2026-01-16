# cluu shell

This shell provides a minimal, no-std command loop that reads line-buffered input
from `tty` and dispatches built-in commands. Parsing is optional but enabled by
default using the `cluu_lang` crate.

## Enabling the parser

The parser is enabled by default via the `lang-parser` feature.

- Disable it explicitly:
  - `cargo build -p cluu-shell --no-default-features`

## Builtin command architecture

Builtins are implemented through small traits to keep the IO loop, parsing, and
command execution separate.

- `BuiltinCommand`: a single builtin implementation.
- `BuiltinRegistry`: holds builtin instances and dispatches them.
- `BuiltinProvider`: registers a set of builtins into a registry.
- `BuiltinFactory`: assembles a registry from multiple providers.

### Add a new builtin

1) Implement `BuiltinCommand` in `userspace/shell/src/commands.rs`:

```rust
struct FooBuiltin;

impl BuiltinCommand for FooBuiltin {
    fn name(&self) -> &'static str {
        "foo"
    }

    fn run(&self, stdout: usize, args: &[String]) -> Result<()> {
        // Write output using TTY_WRITE_LABEL.
        send_with_payload(stdout, TTY_WRITE_LABEL, b"foo\n")?;
        Ok(())
    }
}
```

2) Register it in a provider, usually `DefaultBuiltins`:

```rust
impl BuiltinProvider for DefaultBuiltins {
    fn register(&self, registry: &mut BuiltinRegistry) {
        registry.register(Box::new(HelpBuiltin));
        registry.register(Box::new(EchoBuiltin));
        registry.register(Box::new(ExitBuiltin));
        registry.register(Box::new(FooBuiltin));
    }
}
```

### External builtin provider (future)

`ExternalBuiltinProvider` exists as a placeholder for IPC-driven extensions. It
currently registers nothing, but the `BuiltinFactory` always includes it so a
future IPC implementation can publish builtins without changing the registry.

## Parsing and dispatch flow

1) Line buffered input arrives from `tty`.
2) `cluu_lang::parse_program` parses the line into an AST.
3) `BuiltinFactory` builds a registry and dispatches the command.
4) If no builtin handles the input, the shell logs `unsupported command`.

## Builtins included

- `help`: list builtins.
- `echo`: print args.
- `exit`: exit the shell thread.
