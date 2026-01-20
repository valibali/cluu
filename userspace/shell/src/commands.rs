//! Builtin command handling for the shell.
//!
//! This module keeps the command execution logic separate from the IO loop and
//! parser wiring, following SOLID separation between parsing, dispatch, and IO.

use alloc::boxed::Box;
use alloc::format;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use libcluu::boot::TOKEN_SPACE;
use libcluu::fs::client::VfsClient;
use libcluu::ipc::{call_with_payload, recv, send_with_payload, TTY_WRITE_LABEL};
use libcluu::registry;
use libcluu::syscall;
use libcluu::types::Message;
use libcluu::{debug_print, process_info, Error, Result, IpcFlags, TOKEN_PROC_CAP};

use cluu_lang::ast::{Assign, CmdElem, DqPart, Program, Stmt, Word, WordPart};

const PROCMGR_SPAWN_LABEL: u32 = 2;
const DEFAULT_PRIORITY: usize = 200;

/// Execution result for a command handler.
pub enum ExecResult {
    Handled,
    NotHandled,
}

/// Per-shell execution context shared across command invocations.
pub struct CommandContext {
    vars: BTreeMap<String, String>,
    procmgr_spawn: usize,
}

impl CommandContext {
    /// Create a fresh shell context.
    pub fn new() -> Self {
        Self {
            vars: BTreeMap::new(),
            procmgr_spawn: 0,
        }
    }

    /// Set or update a variable in the shell context.
    pub fn set(&mut self, name: &str, value: String) {
        self.vars.insert(name.to_string(), value);
    }

    /// Remove a variable from the shell context.
    pub fn unset(&mut self, name: &str) {
        self.vars.remove(name);
    }

    /// Fetch a variable value, if present.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.vars.get(name).map(|v| v.as_str())
    }

    /// Clone variable entries for display.
    pub fn entries(&self) -> Vec<(String, String)> {
        self.vars
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    fn procmgr_spawn_endpoint(&mut self) -> Result<usize> {
        if self.procmgr_spawn == 0 {
            self.procmgr_spawn = registry::subscribe_output("procmgr", "spawn")?;
        }
        Ok(self.procmgr_spawn)
    }
}

/// Shell command executor abstraction.
pub trait CommandExecutor {
    fn execute(
        &self,
        stdout: usize,
        context: &mut CommandContext,
        program: &Program,
    ) -> Result<ExecResult>;
}

/// A single builtin command implementation.
pub trait BuiltinCommand {
    fn name(&self) -> &'static str;
    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()>;
}

/// Builtin dispatcher that owns the builtin registry.
pub struct BuiltinRegistry {
    builtins: Vec<Box<dyn BuiltinCommand>>,
}

impl BuiltinRegistry {
    /// Create a registry with the default builtins.
    #[allow(dead_code)]
    pub fn new() -> Self {
        let mut registry = Self {
            builtins: Vec::new(),
        };
        DefaultBuiltins.register(&mut registry);
        registry
    }

    /// Add a builtin command to the registry.
    pub fn register(&mut self, builtin: Box<dyn BuiltinCommand>) {
        self.builtins.push(builtin);
    }

    fn find(&self, name: &str) -> Option<&dyn BuiltinCommand> {
        self.builtins
            .iter()
            .map(|b| b.as_ref())
            .find(|b| b.name() == name)
    }
}

/// Provider for injecting builtin commands into a registry.
pub trait BuiltinProvider {
    fn register(&self, registry: &mut BuiltinRegistry);
}

/// Factory that assembles builtin registries from providers.
pub struct BuiltinFactory {
    providers: Vec<Box<dyn BuiltinProvider>>,
}

impl BuiltinFactory {
    /// Create a factory with the default builtin provider installed.
    pub fn new() -> Self {
        let mut factory = Self {
            providers: Vec::new(),
        };
        factory.add_provider(Box::new(DefaultBuiltins));
        factory.add_provider(Box::new(ExternalBuiltinProvider::new()));
        factory
    }

    /// Register a new builtin provider.
    pub fn add_provider(&mut self, provider: Box<dyn BuiltinProvider>) {
        self.providers.push(provider);
    }

    /// Build a registry by applying all providers.
    pub fn build(&self) -> BuiltinRegistry {
        let mut registry = BuiltinRegistry {
            builtins: Vec::new(),
        };
        for provider in &self.providers {
            provider.register(&mut registry);
        }
        registry
    }
}

struct DefaultBuiltins;

impl BuiltinProvider for DefaultBuiltins {
    fn register(&self, registry: &mut BuiltinRegistry) {
        registry.register(Box::new(HelpBuiltin));
        registry.register(Box::new(EchoBuiltin));
        registry.register(Box::new(ExitBuiltin));
        registry.register(Box::new(SetBuiltin));
        registry.register(Box::new(UnsetBuiltin));
        registry.register(Box::new(EnvBuiltin));
        registry.register(Box::new(ExprBuiltin));
        registry.register(Box::new(LetBuiltin));
        registry.register(Box::new(SpawnBuiltin));
        registry.register(Box::new(CatBuiltin));
        registry.register(Box::new(LsBuiltin));
    }
}

impl CommandExecutor for BuiltinRegistry {
    fn execute(
        &self,
        stdout: usize,
        context: &mut CommandContext,
        program: &Program,
    ) -> Result<ExecResult> {
        let command = match flatten_simple_command(program) {
            Some(command) => command,
            None => return Ok(ExecResult::NotHandled),
        };
        for assign in command.assigns {
            let value = render_word(context, &assign.value);
            context.set(&assign.name, value);
        }
        let mut args = Vec::new();
        for elem in command.words {
            args.push(render_word(context, &elem));
        }
        let Some(name) = args.first() else {
            return Ok(ExecResult::NotHandled);
        };
        if name.as_str() == "repeat" {
            return self.execute_repeat(stdout, context, &args[1..]);
        }
        self.run_builtin(stdout, context, name, &args[1..])
    }
}

/// Placeholder provider for IPC-driven builtin registration.
///
/// This is a no-op today, but keeps the extension point explicit so future
/// services can publish builtins over IPC without touching the registry logic.
pub struct ExternalBuiltinProvider;

impl ExternalBuiltinProvider {
    /// Create a placeholder external provider.
    pub fn new() -> Self {
        Self
    }
}

impl BuiltinProvider for ExternalBuiltinProvider {
    fn register(&self, _registry: &mut BuiltinRegistry) {}
}

struct HelpBuiltin;

impl BuiltinCommand for HelpBuiltin {
    fn name(&self) -> &'static str {
        "help"
    }

    fn run(&self, stdout: usize, _context: &mut CommandContext, _args: &[String]) -> Result<()> {
        send_with_payload(
            stdout,
            TTY_WRITE_LABEL,
            b"builtins: help, echo, exit, set, unset, env, expr, let, spawn, repeat, cat, ls\n",
        )?;
        Ok(())
    }
}

struct EchoBuiltin;

impl BuiltinCommand for EchoBuiltin {
    fn name(&self) -> &'static str {
        "echo"
    }

    fn run(&self, stdout: usize, _context: &mut CommandContext, args: &[String]) -> Result<()> {
        let output = join_words(args);
        send_with_payload(stdout, TTY_WRITE_LABEL, output.as_bytes())?;
        send_with_payload(stdout, TTY_WRITE_LABEL, b"\n")?;
        Ok(())
    }
}

struct ExitBuiltin;

impl BuiltinCommand for ExitBuiltin {
    fn name(&self) -> &'static str {
        "exit"
    }

    fn run(&self, stdout: usize, _context: &mut CommandContext, _args: &[String]) -> Result<()> {
        let _ = send_with_payload(stdout, TTY_WRITE_LABEL, b"shell: exiting\n");
        syscall::thread_exit(0);
    }
}

struct ParsedCommand {
    assigns: Vec<Assign>,
    words: Vec<Word>,
}

fn flatten_simple_command(program: &Program) -> Option<ParsedCommand> {
    let stmt = program.stmts.first()?;
    let Stmt::Pipeline(pipeline) = stmt;
    if pipeline.commands.len() != 1 {
        return None;
    }
    let command = &pipeline.commands[0];
    if !command.redirs.is_empty() {
        return None;
    }
    let mut words = Vec::new();
    for elem in &command.elems {
        match elem {
            CmdElem::Word(word) => words.push(word.clone()),
            CmdElem::Subshell(_) => return None,
        }
    }
    Some(ParsedCommand {
        assigns: command.assigns.clone(),
        words,
    })
}

fn render_word(context: &CommandContext, word: &Word) -> String {
    let mut out = String::new();
    for part in &word.parts {
        match part {
            WordPart::Bare(text) => out.push_str(text),
            WordPart::SingleQuoted(text) => out.push_str(text),
            WordPart::DoubleQuoted(parts) => {
                for dq in parts {
                    match dq {
                        DqPart::Text(text) => out.push_str(text),
                        DqPart::Escaped(text) => out.push_str(text),
                        DqPart::Var(name) => out.push_str(context.get(name).unwrap_or("")),
                        DqPart::CmdSub(_) => out.push_str(""),
                    }
                }
            }
            WordPart::Var(name) => out.push_str(context.get(name).unwrap_or("")),
            WordPart::CmdSub(_) => {}
        }
    }
    out
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

impl BuiltinRegistry {
    fn run_builtin(
        &self,
        stdout: usize,
        context: &mut CommandContext,
        name: &str,
        args: &[String],
    ) -> Result<ExecResult> {
        if let Some(builtin) = self.find(name) {
            builtin.run(stdout, context, args)?;
            return Ok(ExecResult::Handled);
        }
        Ok(ExecResult::NotHandled)
    }

    fn execute_repeat(
        &self,
        stdout: usize,
        context: &mut CommandContext,
        args: &[String],
    ) -> Result<ExecResult> {
        let Some(count_token) = args.first() else {
            send_with_payload(stdout, TTY_WRITE_LABEL, b"repeat: missing count\n")?;
            return Ok(ExecResult::Handled);
        };
        let count = match parse_value(context, count_token) {
            Some(value) if value >= 0 => value as usize,
            _ => {
                send_with_payload(stdout, TTY_WRITE_LABEL, b"repeat: invalid count\n")?;
                return Ok(ExecResult::Handled);
            }
        };
        let Some(command_name) = args.get(1) else {
            send_with_payload(stdout, TTY_WRITE_LABEL, b"repeat: missing command\n")?;
            return Ok(ExecResult::Handled);
        };
        let rest = &args[2..];
        for _ in 0..count {
            match self.run_builtin(stdout, context, command_name, rest)? {
                ExecResult::Handled => {}
                ExecResult::NotHandled => {
                    send_with_payload(
                        stdout,
                        TTY_WRITE_LABEL,
                        b"repeat: unknown command\n",
                    )?;
                    break;
                }
            }
        }
        Ok(ExecResult::Handled)
    }
}

struct SetBuiltin;

impl BuiltinCommand for SetBuiltin {
    fn name(&self) -> &'static str {
        "set"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        let Some(name) = args.first() else {
            send_with_payload(stdout, TTY_WRITE_LABEL, b"set: missing name\n")?;
            return Ok(());
        };
        let value = join_words(&args[1..]);
        context.set(name, value);
        Ok(())
    }
}

struct UnsetBuiltin;

impl BuiltinCommand for UnsetBuiltin {
    fn name(&self) -> &'static str {
        "unset"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        let Some(name) = args.first() else {
            send_with_payload(stdout, TTY_WRITE_LABEL, b"unset: missing name\n")?;
            return Ok(());
        };
        context.unset(name);
        Ok(())
    }
}

struct EnvBuiltin;

impl BuiltinCommand for EnvBuiltin {
    fn name(&self) -> &'static str {
        "env"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, _args: &[String]) -> Result<()> {
        for (name, value) in context.entries() {
            let line = format!("{}={}\n", name, value);
            send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
        }
        Ok(())
    }
}

struct ExprBuiltin;

impl BuiltinCommand for ExprBuiltin {
    fn name(&self) -> &'static str {
        "expr"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        let Some((lhs, op, rhs)) = parse_expr_tokens(args) else {
            send_with_payload(stdout, TTY_WRITE_LABEL, b"expr: invalid expression\n")?;
            return Ok(());
        };
        match op.as_str() {
            "+" => arithmetic_op(stdout, context, &[lhs, rhs], |a, b| a + b),
            "-" => arithmetic_op(stdout, context, &[lhs, rhs], |a, b| a - b),
            "*" => arithmetic_op(stdout, context, &[lhs, rhs], |a, b| a * b),
            "/" => div_op(stdout, context, &lhs, &rhs),
            _ => {
                send_with_payload(stdout, TTY_WRITE_LABEL, b"expr: unknown op\n")?;
                Ok(())
            }
        }
    }
}

struct LetBuiltin;

impl BuiltinCommand for LetBuiltin {
    fn name(&self) -> &'static str {
        "let"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        if args.is_empty() {
            send_with_payload(stdout, TTY_WRITE_LABEL, b"let: missing name\n")?;
            return Ok(());
        }

        let mut name = args[0].as_str();
        let mut expr_tokens: Vec<String> = Vec::new();

        if args.len() == 1 {
            if let Some((lhs, rhs)) = args[0].split_once('=') {
                if lhs.is_empty() || rhs.is_empty() {
                    send_with_payload(stdout, TTY_WRITE_LABEL, b"let: expected NAME=EXPR\n")?;
                    return Ok(());
                }
                name = lhs;
                expr_tokens.push(rhs.to_string());
            } else {
                send_with_payload(stdout, TTY_WRITE_LABEL, b"let: missing =\n")?;
                return Ok(());
            }
        } else {
            let Some(eq) = args.get(1) else {
                send_with_payload(stdout, TTY_WRITE_LABEL, b"let: missing =\n")?;
                return Ok(());
            };
            if eq.as_str() != "=" {
                send_with_payload(stdout, TTY_WRITE_LABEL, b"let: expected =\n")?;
                return Ok(());
            }
            expr_tokens.extend_from_slice(&args[2..]);
        }

        let expr_args = expr_tokens.as_slice();
        let Some((lhs, op, rhs)) = parse_expr_tokens(expr_args) else {
            send_with_payload(stdout, TTY_WRITE_LABEL, b"let: invalid expression\n")?;
            return Ok(());
        };
        let value = match op.as_str() {
            "+" => calc_value(context, &lhs, &rhs, |a, b| a + b),
            "-" => calc_value(context, &lhs, &rhs, |a, b| a - b),
            "*" => calc_value(context, &lhs, &rhs, |a, b| a * b),
            "/" => {
                let Some(a) = parse_value(context, &lhs) else {
                    send_with_payload(stdout, TTY_WRITE_LABEL, b"let: invalid lhs\n")?;
                    return Ok(());
                };
                let Some(b) = parse_value(context, &rhs) else {
                    send_with_payload(stdout, TTY_WRITE_LABEL, b"let: invalid rhs\n")?;
                    return Ok(());
                };
                if b == 0 {
                    send_with_payload(stdout, TTY_WRITE_LABEL, b"let: divide by zero\n")?;
                    return Ok(());
                }
                Some((a / b).to_string())
            }
            _ => None,
        };
        match value {
            Some(result) => {
                context.set(name, result);
            }
            None => {
                send_with_payload(stdout, TTY_WRITE_LABEL, b"let: invalid expression\n")?;
            }
        }
        Ok(())
    }
}

struct SpawnBuiltin;

impl BuiltinCommand for SpawnBuiltin {
    fn name(&self) -> &'static str {
        "spawn"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        let Some(path) = args.first() else {
            send_with_payload(stdout, TTY_WRITE_LABEL, b"spawn: missing path\n")?;
            return Ok(());
        };
        let priority = args
            .get(1)
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(DEFAULT_PRIORITY);
        let procmgr_endpoint = context.procmgr_spawn_endpoint()?;
        let initrd_path = normalize_spawn_path(path);
        let payload = initrd_path.as_bytes();
        let notify_endpoint = syscall::endpoint_create(process_info().tokens[TOKEN_PROC_CAP])?;
        let mut msg = Message::new(PROCMGR_SPAWN_LABEL, [0; 6], 4);
        msg.words[0] = payload.len();
        msg.words[1] = priority;
        msg.words[2] = 0;
        msg.words[3] = notify_endpoint;
        let mut reply = Message::new(0, [0; 6], 0);
        call_with_payload(procmgr_endpoint, &msg, payload, &mut reply)?;
        match parse_status(reply.words[0]) {
            Ok(()) => {
                let mut exit_msg = Message::new(0, [0; 6], 0);
                let _ = recv(notify_endpoint, &mut exit_msg, IpcFlags::empty());
                Ok(())
            }
            Err(err) => {
                let line = format!("spawn: {:?}\n", err);
                send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                Ok(())
            }
        }
    }
}

fn arithmetic_op<F>(
    stdout: usize,
    context: &mut CommandContext,
    args: &[String],
    op: F,
) -> Result<()>
where
    F: FnOnce(i64, i64) -> i64,
{
    let Some(a) = parse_value(context, args.first().unwrap_or(&String::new())) else {
        send_with_payload(stdout, TTY_WRITE_LABEL, b"arith: invalid lhs\n")?;
        return Ok(());
    };
    let Some(b) = parse_value(context, args.get(1).unwrap_or(&String::new())) else {
        send_with_payload(stdout, TTY_WRITE_LABEL, b"arith: invalid rhs\n")?;
        return Ok(());
    };
    let result = op(a, b).to_string();
    send_with_payload(stdout, TTY_WRITE_LABEL, result.as_bytes())?;
    send_with_payload(stdout, TTY_WRITE_LABEL, b"\n")?;
    Ok(())
}

fn normalize_spawn_path(path: &str) -> String {
    if let Some(rel) = path.strip_prefix("/dev/initrd/") {
        return rel.to_string();
    }
    if let Some(rel) = path.strip_prefix("/bin/") {
        return format!("bin/{}", rel);
    }
    if let Some(rel) = path.strip_prefix('/') {
        return rel.to_string();
    }
    if path.contains('/') {
        return path.to_string();
    }
    format!("bin/{}", path)
}

fn parse_status(raw: usize) -> Result<()> {
    let signed = raw as isize;
    if signed < 0 {
        return Err(Error::from_errno(signed));
    }
    Ok(())
}

fn div_op(
    stdout: usize,
    context: &mut CommandContext,
    lhs: &str,
    rhs: &str,
) -> Result<()> {
    let Some(a) = parse_value(context, lhs) else {
        send_with_payload(stdout, TTY_WRITE_LABEL, b"div: invalid lhs\n")?;
        return Ok(());
    };
    let Some(b) = parse_value(context, rhs) else {
        send_with_payload(stdout, TTY_WRITE_LABEL, b"div: invalid rhs\n")?;
        return Ok(());
    };
    if b == 0 {
        send_with_payload(stdout, TTY_WRITE_LABEL, b"div: divide by zero\n")?;
        return Ok(());
    }
    let result = (a / b).to_string();
    send_with_payload(stdout, TTY_WRITE_LABEL, result.as_bytes())?;
    send_with_payload(stdout, TTY_WRITE_LABEL, b"\n")?;
    Ok(())
}

fn calc_value<F>(context: &CommandContext, lhs: &str, rhs: &str, op: F) -> Option<String>
where
    F: FnOnce(i64, i64) -> i64,
{
    let a = parse_value(context, lhs)?;
    let b = parse_value(context, rhs)?;
    Some(op(a, b).to_string())
}

fn parse_value(context: &CommandContext, token: &str) -> Option<i64> {
    if let Ok(value) = token.parse::<i64>() {
        return Some(value);
    }
    context
        .get(token)
        .and_then(|val| val.parse::<i64>().ok())
}

fn parse_expr_tokens(args: &[String]) -> Option<(String, String, String)> {
    if args.len() >= 3 {
        return Some((args[0].clone(), args[1].clone(), args[2].clone()));
    }
    if let Some(token) = args.first() {
        if let Some((lhs, op, rhs)) = split_expr_token(token) {
            return Some((lhs, op, rhs));
        }
    }
    None
}

fn split_expr_token(token: &str) -> Option<(String, String, String)> {
    let mut idx = None;
    for (pos, ch) in token.char_indices() {
        if matches!(ch, '+' | '-' | '*' | '/') {
            idx = Some((pos, ch));
            break;
        }
    }
    let (pos, ch) = idx?;
    let lhs = token.get(0..pos)?.trim();
    let rhs = token.get(pos + ch.len_utf8()..)?.trim();
    if lhs.is_empty() || rhs.is_empty() {
        return None;
    }
    Some((lhs.to_string(), ch.to_string(), rhs.to_string()))
}

struct CatBuiltin;

impl BuiltinCommand for CatBuiltin {
    fn name(&self) -> &'static str {
        "cat"
    }

    fn run(&self, stdout: usize, _context: &mut CommandContext, args: &[String]) -> Result<()> {
        let Some(path) = args.first() else {
            send_with_payload(stdout, TTY_WRITE_LABEL, b"cat: missing path\n")?;
            return Ok(());
        };

        // Get VFS endpoint
        let vfs_endpoint = match registry::subscribe_output("vfs", "main") {
            Ok(ep) => ep,
            Err(_) => {
                send_with_payload(stdout, TTY_WRITE_LABEL, b"cat: vfs not available\n")?;
                return Ok(());
            }
        };

        let vfs = match VfsClient::new_from_registry(vfs_endpoint) {
            Ok(c) => c,
            Err(_) => {
                send_with_payload(stdout, TTY_WRITE_LABEL, b"cat: failed to create vfs client\n")?;
                return Ok(());
            }
        };

        // Open the file
        let file = match vfs.open(path) {
            Ok(f) => f,
            Err(e) => {
                let msg = format!("cat: {}: {:?}\n", path, e);
                send_with_payload(stdout, TTY_WRITE_LABEL, msg.as_bytes())?;
                return Ok(());
            }
        };

        if file.size == 0 {
            let _ = vfs.close(file);
            return Ok(());
        }

        // Read using grant - use address based on fd to avoid conflicts
        let info = process_info();
        let space_token = info.tokens[TOKEN_SPACE];
        // Each fd gets a 16MB region to avoid overlap
        const GRANT_REGION_BASE: usize = 0x50000000;
        const GRANT_REGION_SIZE: usize = 16 * 1024 * 1024;
        let read_buf_base = GRANT_REGION_BASE + (file.fd % 16) * GRANT_REGION_SIZE;

        match vfs.read_grant(file, 0, file.size, space_token, read_buf_base) {
            Ok(grant) => {
                let addr = grant.base + grant.offset;
                let _ = debug_print(&format!("cat: grant addr={:#x} len={}", addr, grant.len));
                let data = unsafe {
                    core::slice::from_raw_parts(addr as *const u8, grant.len)
                };
                // Show first 8 bytes for debugging
                let preview: alloc::vec::Vec<u8> = data.iter().take(8).copied().collect();
                let _ = debug_print(&format!("cat: first 8 bytes = {:02x?}", preview.as_slice()));

                // For large files, test page accessibility and show info
                if grant.len > 4096 {
                    let msg = format!("cat: file is {} bytes, testing page access...\n", grant.len);
                    let _ = send_with_payload(stdout, TTY_WRITE_LABEL, msg.as_bytes());

                    // Test reading one byte from each page
                    let page_size = 4096usize;
                    let num_pages = (grant.len + page_size - 1) / page_size;
                    for i in 0..num_pages {
                        let offset = i * page_size;
                        if offset < grant.len {
                            let _ = data[offset]; // Touch the page
                        }
                        if i % 100 == 0 {
                            let _ = debug_print(&format!("cat: tested page {}/{}", i, num_pages));
                        }
                    }
                    let _ = debug_print(&format!("cat: all {} pages accessible!", num_pages));

                    // Show hex dump of first 64 bytes
                    let hex_preview: alloc::vec::Vec<u8> = data.iter().take(64).copied().collect();
                    let hex_str = format!("First 64 bytes: {:02x?}\n", hex_preview.as_slice());
                    let _ = send_with_payload(stdout, TTY_WRITE_LABEL, hex_str.as_bytes());
                } else {
                    // Print small files in chunks
                    for chunk in data.chunks(256) {
                        let _ = send_with_payload(stdout, TTY_WRITE_LABEL, chunk);
                    }
                }
                // Add newline if file doesn't end with one
                if !data.is_empty() && data[data.len() - 1] != b'\n' {
                    let _ = send_with_payload(stdout, TTY_WRITE_LABEL, b"\n");
                }
            }
            Err(e) => {
                let msg = format!("cat: read error: {:?}\n", e);
                send_with_payload(stdout, TTY_WRITE_LABEL, msg.as_bytes())?;
            }
        }

        let _ = vfs.close(file);
        Ok(())
    }
}

struct LsBuiltin;

impl BuiltinCommand for LsBuiltin {
    fn name(&self) -> &'static str {
        "ls"
    }

    fn run(&self, stdout: usize, _context: &mut CommandContext, args: &[String]) -> Result<()> {
        let path = args.first().map(|s| s.as_str()).unwrap_or("/mnt/disk");

        // Get VFS endpoint
        let vfs_endpoint = match registry::subscribe_output("vfs", "main") {
            Ok(ep) => ep,
            Err(_) => {
                send_with_payload(stdout, TTY_WRITE_LABEL, b"ls: vfs not available\n")?;
                return Ok(());
            }
        };

        let vfs = match VfsClient::new_from_registry(vfs_endpoint) {
            Ok(c) => c,
            Err(_) => {
                send_with_payload(stdout, TTY_WRITE_LABEL, b"ls: failed to create vfs client\n")?;
                return Ok(());
            }
        };

        // Read directory
        match vfs.readdir(path) {
            Ok(entries) => {
                for entry in entries {
                    let suffix = if entry.is_dir { "/" } else { "" };
                    let line = format!("{}{}\n", entry.name, suffix);
                    send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                }
            }
            Err(e) => {
                let msg = format!("ls: {}: {:?}\n", path, e);
                send_with_payload(stdout, TTY_WRITE_LABEL, msg.as_bytes())?;
            }
        }

        Ok(())
    }
}
