//! `help`, `clear`, and `type` builtins.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;

use libcluu::fs::client::VfsClient;
use libcluu::registry;
use libcluu::Result;

use super::registry::CommandContext;
use super::registry::{BuiltinCommand, BuiltinRegistry, WriteSink};

pub fn register(registry: &mut BuiltinRegistry) {
    registry.register(Box::new(HelpBuiltin));
    registry.register(Box::new(ClearBuiltin));
    registry.register(Box::new(TypeBuiltin));
}

// ─── Well-known builtin names ─────────────────────────────────────────────────

/// Stable list of all registered builtin names.  Kept in sync with
/// `register_all` in mod.rs.  Used by `type` for O(1) lookup without
/// requiring access to a live registry at query time.
const KNOWN_BUILTINS: &[&str] = &[
    "exit", "poweroff", "reboot",
    "cd", "pwd",
    "echo",
    "set", "export", "unset", "env", "true", "false", "test", "[",
    "alias", "unalias",
    "jobs", "fg", "bg", "kill", "wait",
    "history",
    "help", "clear", "type",
    "expr", "let",
    "repeat",
    "cat", "ls", "heap",
    "su", "sudo",
    "container",
    "sleep",
];

// ─── HelpBuiltin ─────────────────────────────────────────────────────────────

pub(crate) struct HelpBuiltin;

impl BuiltinCommand for HelpBuiltin {
    fn name(&self) -> &'static str {
        "help"
    }

    fn run_with_sink(
        &self,
        stdout: &WriteSink,
        _context: &mut CommandContext,
        _args: &[String],
    ) -> Result<()> {
        let _ = libcluu::debug_print("Shell builtins:");
        stdout.write_all(b"Shell builtins:\n")?;
        let mut line = String::new();
        let mut col = 0usize;
        for name in KNOWN_BUILTINS {
            if col > 0 {
                line.push_str(", ");
            }
            line.push_str(name);
            col += 1;
            if col >= 8 {
                line.push('\n');
                stdout.write_all(line.as_bytes())?;
                line.clear();
                col = 0;
            }
        }
        if !line.is_empty() {
            line.push('\n');
            stdout.write_all(line.as_bytes())?;
        }
        Ok(())
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        self.run_with_sink(&WriteSink::Tty(stdout), context, args)
    }
}

// ─── ClearBuiltin ────────────────────────────────────────────────────────────

pub(crate) struct ClearBuiltin;

impl BuiltinCommand for ClearBuiltin {
    fn name(&self) -> &'static str {
        "clear"
    }

    fn run(&self, _stdout: usize, _context: &mut CommandContext, _args: &[String]) -> Result<()> {
        crate::write_stdout(b"\x1b[H\x1b[2J");
        Ok(())
    }
}

// ─── TypeBuiltin ─────────────────────────────────────────────────────────────

pub(crate) struct TypeBuiltin;

impl BuiltinCommand for TypeBuiltin {
    fn name(&self) -> &'static str {
        "type"
    }

    fn run_with_sink(
        &self,
        stdout: &WriteSink,
        context: &mut CommandContext,
        args: &[String],
    ) -> Result<()> {
        if args.is_empty() {
            stdout.write_all(b"type: usage: type NAME...\n")?;
            return Ok(());
        }
        for name in args {
            // 1. Alias?
            if let Some(v) = context.aliases.get(name.as_str()) {
                let line = format!("{} is aliased to '{}'\n", name, v);
                let _ = libcluu::debug_print(line.trim_end());
                stdout.write_all(line.as_bytes())?;
                continue;
            }
            // 2. Builtin?
            if KNOWN_BUILTINS.iter().any(|b| *b == name.as_str()) {
                let line = format!("{} is a shell builtin\n", name);
                let _ = libcluu::debug_print(line.trim_end());
                stdout.write_all(line.as_bytes())?;
                continue;
            }
            // 3. External — walk PATH.
            let path_env = libcluu::posix::snapshot_env()
                .into_iter()
                .find(|(k, _)| k == "PATH")
                .map(|(_, v)| v)
                .unwrap_or_else(|| String::from("/bin"));
            let mut found = false;
            for dir in path_env.split(':') {
                let candidate = format!("{}/{}", dir.trim_end_matches('/'), name);
                if vfs_path_exists(&candidate) {
                    let line = format!("{} is {}\n", name, candidate);
                    stdout.write_all(line.as_bytes())?;
                    found = true;
                    break;
                }
            }
            if !found {
                let line = format!("type: {}: not found\n", name);
                let _ = libcluu::debug_print(line.trim_end());
                stdout.write_all(line.as_bytes())?;
            }
        }
        Ok(())
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        self.run_with_sink(&WriteSink::Tty(stdout), context, args)
    }
}

fn vfs_path_exists(path: &str) -> bool {
    let Ok(ep) = registry::subscribe_output("vfs", "main") else {
        return false;
    };
    let Ok(client) = VfsClient::new_from_registry(ep) else {
        return false;
    };
    client.stat(path).is_ok()
}
