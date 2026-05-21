//! `set`, `export`, `unset`, `env`, `true`, `false`, and `test`/`[` builtins.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};

use libcluu::fs::client::VfsClient;

use libcluu::registry;
use libcluu::Result;

use super::registry::CommandContext;
use super::registry::{BuiltinCommand, BuiltinRegistry, WriteSink};

pub fn register(registry: &mut BuiltinRegistry) {
    registry.register(Box::new(SetBuiltin));
    registry.register(Box::new(ExportBuiltin));
    registry.register(Box::new(UnsetBuiltin));
    registry.register(Box::new(EnvBuiltin));
    registry.register(Box::new(TrueBuiltin));
    registry.register(Box::new(FalseBuiltin));
    registry.register(Box::new(TestBuiltin { bracket: false }));
    registry.register(Box::new(TestBuiltin { bracket: true }));
}

// ---------------------------------------------------------------------------
// set
// ---------------------------------------------------------------------------

pub(crate) struct SetBuiltin;

impl BuiltinCommand for SetBuiltin {
    fn name(&self) -> &'static str {
        "set"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        let Some(name) = args.first() else {
            // No args: list all variables (POSIX baseline).
            for (k, v) in context.entries() {
                let line = format!("{}={}\n", k, v);
                crate::write_stdout(line.as_bytes());
            }
            return Ok(());
        };
        // Reject unsupported POSIX option flags with a clear message.
        if name.starts_with('-') || name.starts_with('+') {
            let opt = name.chars().nth(1).unwrap_or('?');
            let line = format!("set: option -{} not supported\n", opt);
            crate::write_stdout(line.as_bytes());
            return Ok(());
        }
        let value = join_words(&args[1..]);
        context.set(name, value);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// unset
// ---------------------------------------------------------------------------

pub(crate) struct UnsetBuiltin;

impl BuiltinCommand for UnsetBuiltin {
    fn name(&self) -> &'static str {
        "unset"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        let Some(name) = args.first() else {
            crate::write_stdout(b"unset: missing name\n");
            return Ok(());
        };
        context.unset(name);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// export
// ---------------------------------------------------------------------------

pub(crate) struct ExportBuiltin;

impl BuiltinCommand for ExportBuiltin {
    fn name(&self) -> &'static str {
        "export"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        if args.is_empty() {
            for (k, v) in context.exported_pairs() {
                let line = format!("export {}={}\n", k, v);
                crate::write_stdout(line.as_bytes());
            }
            return Ok(());
        }
        for arg in args {
            if let Some(eq) = arg.find('=') {
                let name = &arg[..eq];
                let value = &arg[eq + 1..];
                if name.is_empty() {
                    crate::write_stdout(b"export: missing name before '='\n");
                    continue;
                }
                context.set(name, value.to_string());
                context.export_var(name);
            } else {
                context.export_var(arg);
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// env
// ---------------------------------------------------------------------------

pub(crate) struct EnvBuiltin;

impl BuiltinCommand for EnvBuiltin {
    fn name(&self) -> &'static str {
        "env"
    }

    fn run_with_sink(
        &self,
        stdout: &WriteSink,
        context: &mut CommandContext,
        _args: &[String],
    ) -> Result<()> {
        // Process env (envelope-resolved at session-create, inherited via
        // spawn). Shell-local vars override only when also exported.
        let mut seen: alloc::collections::BTreeSet<String> =
            alloc::collections::BTreeSet::new();
        for (k, v) in context.exported_pairs() {
            let line = format!("{}={}\n", k, v);
            stdout.write_all(line.as_bytes())?;
            seen.insert(k);
        }
        for (k, v) in libcluu::posix::snapshot_env() {
            if seen.contains(&k) {
                continue;
            }
            let line = format!("{}={}\n", k, v);
            stdout.write_all(line.as_bytes())?;
        }
        let _ = context;
        Ok(())
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        self.run_with_sink(&WriteSink::Tty(stdout), context, args)
    }
}

// ---------------------------------------------------------------------------
// true / false
// ---------------------------------------------------------------------------

pub(crate) struct TrueBuiltin;

impl BuiltinCommand for TrueBuiltin {
    fn name(&self) -> &'static str {
        "true"
    }

    fn run(&self, _stdout: usize, context: &mut CommandContext, _args: &[String]) -> Result<()> {
        context.set_last_status(0);
        Ok(())
    }
}

pub(crate) struct FalseBuiltin;

impl BuiltinCommand for FalseBuiltin {
    fn name(&self) -> &'static str {
        "false"
    }

    fn run(&self, _stdout: usize, context: &mut CommandContext, _args: &[String]) -> Result<()> {
        context.set_last_status(1);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// test / [
// ---------------------------------------------------------------------------

/// POSIX `test`(1) — file/string/numeric predicate evaluator.
///
/// Registered twice: as `test` and as `[`. When invoked as `[`, the final
/// argument must be `]`; we strip it before parsing.
pub(crate) struct TestBuiltin {
    pub bracket: bool,
}

impl BuiltinCommand for TestBuiltin {
    fn name(&self) -> &'static str {
        if self.bracket {
            "["
        } else {
            "test"
        }
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        let argv: &[String] = if self.bracket {
            match args.last() {
                Some(last) if last == "]" => &args[..args.len() - 1],
                _ => {
                    crate::write_stdout(b"[: missing closing ']'\n");
                    context.set_last_status(2);
                    return Ok(());
                }
            }
        } else {
            args
        };

        if argv.is_empty() {
            context.set_last_status(1);
            return Ok(());
        }

        let mut parser = TestParser::new(argv);
        match parser.parse_expr() {
            Ok(value) => {
                if !parser.at_end() {
                    let line = format!(
                        "{}: extra argument: {}\n",
                        self.name(),
                        parser.peek().unwrap_or("")
                    );
                    crate::write_stdout(line.as_bytes());
                    context.set_last_status(2);
                    return Ok(());
                }
                context.set_last_status(if value { 0 } else { 1 });
            }
            Err(msg) => {
                let line = format!("{}: {}\n", self.name(), msg);
                crate::write_stdout(line.as_bytes());
                context.set_last_status(2);
            }
        }
        Ok(())
    }
}

/// Recursive-descent parser for POSIX `test` expressions.
struct TestParser<'a> {
    args: &'a [String],
    pos: usize,
}

impl<'a> TestParser<'a> {
    fn new(args: &'a [String]) -> Self {
        Self { args, pos: 0 }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.args.len()
    }

    fn peek(&self) -> Option<&'a str> {
        self.args.get(self.pos).map(|s| s.as_str())
    }

    fn peek_at(&self, offset: usize) -> Option<&'a str> {
        self.args.get(self.pos + offset).map(|s| s.as_str())
    }

    fn advance(&mut self) -> Option<&'a str> {
        let val = self.peek();
        if val.is_some() {
            self.pos += 1;
        }
        val
    }

    fn parse_expr(&mut self) -> core::result::Result<bool, String> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> core::result::Result<bool, String> {
        let mut left = self.parse_and()?;
        while self.peek() == Some("-o") {
            self.advance();
            let right = self.parse_and()?;
            left = left || right;
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> core::result::Result<bool, String> {
        let mut left = self.parse_unary()?;
        while self.peek() == Some("-a") {
            self.advance();
            let right = self.parse_unary()?;
            left = left && right;
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> core::result::Result<bool, String> {
        if self.peek() == Some("!") && self.args.len() - self.pos > 1 {
            self.advance();
            let inner = self.parse_unary()?;
            return Ok(!inner);
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> core::result::Result<bool, String> {
        if self.peek() == Some("(") {
            self.advance();
            let inner = self.parse_expr()?;
            if self.peek() != Some(")") {
                return Err(String::from("missing ')'"));
            }
            self.advance();
            return Ok(inner);
        }

        let remaining = self.args.len() - self.pos;
        if remaining == 0 {
            return Err(String::from("expected expression"));
        }

        if remaining >= 2 {
            if let Some(tok) = self.peek() {
                if is_unary_op(tok) {
                    let op = tok;
                    self.advance();
                    let arg = self.advance().unwrap();
                    return Ok(eval_unary(op, arg));
                }
            }
        }

        if remaining >= 3 {
            if let Some(op) = self.peek_at(1) {
                if is_binary_op(op) {
                    let lhs = self.advance().unwrap();
                    let _ = self.advance();
                    let rhs = self.advance().unwrap();
                    return eval_binary(lhs, op, rhs);
                }
            }
        }

        let word = self.advance().unwrap();
        Ok(!word.is_empty())
    }
}

fn is_unary_op(s: &str) -> bool {
    matches!(
        s,
        "-e" | "-f" | "-d" | "-r" | "-w" | "-x" | "-s" | "-L" | "-h" | "-z" | "-n"
    )
}

fn is_binary_op(s: &str) -> bool {
    matches!(
        s,
        "=" | "!=" | "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge"
    )
}

fn eval_unary(op: &str, arg: &str) -> bool {
    match op {
        "-z" => arg.is_empty(),
        "-n" => !arg.is_empty(),
        "-e" | "-f" | "-d" | "-r" | "-w" | "-x" | "-s" | "-L" | "-h" => {
            let stat = match stat_path(arg) {
                Some(s) => s,
                None => return false,
            };
            let mode = stat.mode as u32;
            const S_IFMT: u32 = 0o170000;
            const S_IFREG: u32 = 0o100000;
            const S_IFDIR: u32 = 0o040000;
            const S_IFLNK: u32 = 0o120000;
            match op {
                "-e" => true,
                "-f" => (mode & S_IFMT) == S_IFREG,
                "-d" => (mode & S_IFMT) == S_IFDIR,
                "-L" | "-h" => (mode & S_IFMT) == S_IFLNK,
                "-r" => (mode & 0o444) != 0,
                "-w" => (mode & 0o222) != 0,
                "-x" => (mode & 0o111) != 0,
                "-s" => stat.size > 0,
                _ => false,
            }
        }
        _ => false,
    }
}

fn eval_binary(lhs: &str, op: &str, rhs: &str) -> core::result::Result<bool, String> {
    match op {
        "=" => Ok(lhs == rhs),
        "!=" => Ok(lhs != rhs),
        "-eq" | "-ne" | "-lt" | "-le" | "-gt" | "-ge" => {
            let l = parse_int(lhs)?;
            let r = parse_int(rhs)?;
            Ok(match op {
                "-eq" => l == r,
                "-ne" => l != r,
                "-lt" => l < r,
                "-le" => l <= r,
                "-gt" => l > r,
                "-ge" => l >= r,
                _ => unreachable!(),
            })
        }
        _ => Err(format!("unknown operator: {}", op)),
    }
}

fn parse_int(s: &str) -> core::result::Result<i64, String> {
    s.parse::<i64>()
        .map_err(|_| format!("integer expression expected: {}", s))
}

fn stat_path(path: &str) -> Option<libcluu::fs::client::VfsStat> {
    let vfs_endpoint = registry::subscribe_output("vfs", "main").ok()?;
    let vfs = VfsClient::new_from_registry(vfs_endpoint).ok()?;
    let resolved = libcluu::posix::resolve_path(path);
    vfs.stat(&resolved).ok()
}

fn join_words(words: &[String]) -> String {
    let mut out = String::new();
    for (idx, word) in words.iter().enumerate() {
        if idx != 0 {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}
