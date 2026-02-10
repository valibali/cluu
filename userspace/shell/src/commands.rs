//! Builtin command handling for the shell.
//!
//! This module keeps the command execution logic separate from the IO loop and
//! parser wiring, following SOLID separation between parsing, dispatch, and IO.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use libcluu::boot::{TOKEN_REGISTRY, TOKEN_SPACE};
use libcluu::fs::client::VfsClient;
use libcluu::ipc::{
    call, call_with_payload, recv, send_with_payload, send_with_retry, TTY_WRITE_LABEL,
};
use libcluu::registry;
use libcluu::syscall;
use libcluu::types::Message;
use libcluu::{debug_print, process_info, Error, IpcFlags, Result, TOKEN_IPC};

use cluu_lang::ast::{Assign, CmdElem, DqPart, Program, Stmt, Word, WordPart};

const PROCMGR_SPAWN_LABEL: u32 = 2;
const PROCMGR_KILL_LABEL: u32 = 3;
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
        registry.register(Box::new(KillDenyBuiltin));
        registry.register(Box::new(RegistryDenyBuiltin));
        registry.register(Box::new(MapFailBuiltin));
        registry.register(Box::new(MapCopyFailBuiltin));
        registry.register(Box::new(MapErrorBuiltin));
        registry.register(Box::new(Ext2WriteBuiltin));
        registry.register(Box::new(Ext2AppendBuiltin));
        registry.register(Box::new(Ext2MutateBuiltin));
        registry.register(Box::new(Ext2UnlinkBuiltin));
        registry.register(Box::new(CatBuiltin));
        registry.register(Box::new(LsBuiltin));
        registry.register(Box::new(HeapBuiltin));
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
            b"builtins: help, echo, exit, set, unset, env, expr, let, spawn, killdeny, regdeny, mapfail, mapcpfail, maperror, ext2write, ext2append, ext2mutate, ext2unlink, repeat, cat, ls, heap\n",
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
                    send_with_payload(stdout, TTY_WRITE_LABEL, b"repeat: unknown command\n")?;
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
        let notify_endpoint = syscall::endpoint_create(process_info().tokens[TOKEN_IPC])?;
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

struct MapFailBuiltin;

struct KillDenyBuiltin;

struct RegistryDenyBuiltin;

impl BuiltinCommand for KillDenyBuiltin {
    fn name(&self) -> &'static str {
        "killdeny"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        let target_pid = args
            .first()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(2);
        let signal = args
            .get(1)
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(9);
        let procmgr_endpoint = context.procmgr_spawn_endpoint()?;

        let mut msg = Message::new(PROCMGR_KILL_LABEL, [0; 6], 2);
        msg.words[0] = target_pid;
        msg.words[1] = signal;
        call(procmgr_endpoint, &mut msg, IpcFlags::empty())?;

        match parse_status(msg.words[0]) {
            Err(Error::PermissionDenied) => {
                let line = format!("killdeny: PASS permission denied pid={}\n", target_pid);
                send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                let _ = debug_print(line.trim_end());
            }
            Ok(()) => {
                let line = format!("killdeny: FAIL unexpected success pid={}\n", target_pid);
                send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                let _ = debug_print(line.trim_end());
            }
            Err(err) => {
                let line = format!("killdeny: FAIL wrong error {:?} pid={}\n", err, target_pid);
                send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                let _ = debug_print(line.trim_end());
            }
        }
        Ok(())
    }
}

impl BuiltinCommand for RegistryDenyBuiltin {
    fn name(&self) -> &'static str {
        "regdeny"
    }

    fn run(&self, stdout: usize, _context: &mut CommandContext, args: &[String]) -> Result<()> {
        let service = args.first().map_or("tty:0", String::as_str);
        let endpoint = args.get(1).map_or("main", String::as_str);
        let registry_endpoint = process_info().tokens[TOKEN_REGISTRY];
        if registry_endpoint == 0 {
            let line = "regdeny: FAIL missing registry token\n";
            send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            let _ = debug_print(line.trim_end());
            return Ok(());
        }

        let payload = encode_registry_names(service, endpoint);
        let mut req = Message::new(registry::REGISTRY_UNREGISTER_LABEL, [0; 6], 2);
        req.words[0] = payload.len();
        // Reply endpoint field is ignored for unauthorized sender paths, but keep format valid.
        req.words[1] = process_info().tokens[libcluu::boot::TOKEN_STDOUT];
        let header = req.as_bytes();
        let mut buffer = Vec::with_capacity(header.len() + payload.len());
        buffer.extend_from_slice(header);
        buffer.extend_from_slice(&payload);

        match syscall::ipc_send(registry_endpoint, &buffer) {
            Ok(()) => {
                let line = format!(
                    "regdeny: PASS permission denied service={} endpoint={}\n",
                    service, endpoint
                );
                send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                let _ = debug_print(line.trim_end());
            }
            Err(err) => {
                let line = format!(
                    "regdeny: FAIL send error {:?} service={} endpoint={}\n",
                    err, service, endpoint
                );
                send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                let _ = debug_print(line.trim_end());
            }
        }
        Ok(())
    }
}

impl BuiltinCommand for MapFailBuiltin {
    fn name(&self) -> &'static str {
        "mapfail"
    }

    fn run(&self, stdout: usize, _context: &mut CommandContext, args: &[String]) -> Result<()> {
        // Keep this away from known service mappings (e.g. virtio/vfs windows).
        const TEST_BASE: usize = 0x6C00_0000;
        const MAP_TEST_FAILPOINT: usize = 0x8000_0000;
        const MAP_TEST_FAIL_AFTER_SHIFT: usize = 16;
        const MAP_TEST_FAIL_AFTER_MASK: usize = 0xFF;

        let total_pages = args
            .first()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(12);
        let fail_after_raw = args
            .get(1)
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(4);

        if total_pages < 2 {
            let line = "mapfail: FAIL total_pages must be >= 2\n";
            send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            let _ = debug_print("mapfail: FAIL total_pages must be >= 2");
            return Ok(());
        }
        let fail_after = fail_after_raw.clamp(1, total_pages - 1);
        let fail_bits = (fail_after & MAP_TEST_FAIL_AFTER_MASK) << MAP_TEST_FAIL_AFTER_SHIFT;
        let flags = 0x03 | MAP_TEST_FAILPOINT | fail_bits;
        let space_token = process_info().tokens[TOKEN_SPACE];

        let _ = syscall::space_unmap(space_token, TEST_BASE, total_pages);

        let result = syscall::space_map_range(space_token, TEST_BASE, 0, flags, total_pages, 0);
        match result {
            Err(Error::OutOfMemory) => {}
            Ok(pages) => {
                let line = format!("mapfail: FAIL unexpected success pages={}\n", pages);
                send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                let _ = debug_print(line.trim_end());
                let _ = syscall::space_unmap(space_token, TEST_BASE, total_pages);
                return Ok(());
            }
            Err(err) => {
                let line = format!("mapfail: FAIL wrong error {:?}\n", err);
                send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                let _ = debug_print(line.trim_end());
                let _ = syscall::space_unmap(space_token, TEST_BASE, total_pages);
                return Ok(());
            }
        }

        // Verify rollback by remapping the exact same range without failpoint:
        // if rollback is correct, this should map all requested pages.
        let verify_result =
            syscall::space_map_range(space_token, TEST_BASE, 0, 0x03, total_pages, 0);
        match verify_result {
            Ok(mapped) if mapped == total_pages => {}
            Ok(mapped) => {
                let line = format!(
                    "mapfail: FAIL rollback remap short mapped_pages={}\n",
                    mapped
                );
                send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                let _ = debug_print(line.trim_end());
                let _ = syscall::space_unmap(space_token, TEST_BASE, total_pages);
                return Ok(());
            }
            Err(err) => {
                let line = format!("mapfail: FAIL rollback remap error {:?}\n", err);
                send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                let _ = debug_print(line.trim_end());
                let _ = syscall::space_unmap(space_token, TEST_BASE, total_pages);
                return Ok(());
            }
        }

        let line = format!(
            "mapfail: PASS total_pages={} fail_after={}\n",
            total_pages, fail_after
        );
        send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
        let _ = debug_print(line.trim_end());
        let _ = syscall::space_unmap(space_token, TEST_BASE, total_pages);
        Ok(())
    }
}

struct MapCopyFailBuiltin;

impl BuiltinCommand for MapCopyFailBuiltin {
    fn name(&self) -> &'static str {
        "mapcpfail"
    }

    fn run(&self, stdout: usize, _context: &mut CommandContext, args: &[String]) -> Result<()> {
        const SOURCE_BASE: usize = 0x7100_0000;
        const TARGET_BASE: usize = 0x7110_0000;
        const SOURCE_PAGES: usize = 2;
        const PAGE_SIZE: usize = 4096;

        let total_pages = args
            .first()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(4);
        if total_pages < 2 {
            let line = "mapcpfail: FAIL total_pages must be >= 2\n";
            send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            let _ = debug_print("mapcpfail: FAIL total_pages must be >= 2");
            return Ok(());
        }

        let space_token = process_info().tokens[TOKEN_SPACE];
        let _ = syscall::space_unmap(space_token, SOURCE_BASE, SOURCE_PAGES);
        let _ = syscall::space_unmap(space_token, TARGET_BASE, total_pages);

        if let Err(err) = syscall::space_map_range(space_token, SOURCE_BASE, 0, 0x03, 1, 0) {
            let line = format!("mapcpfail: FAIL source map error {:?}\n", err);
            send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            let _ = debug_print(line.trim_end());
            let _ = syscall::space_unmap(space_token, SOURCE_BASE, SOURCE_PAGES);
            return Ok(());
        }

        let result = syscall::space_map_range(
            space_token,
            TARGET_BASE,
            SOURCE_BASE,
            0x03,
            total_pages,
            PAGE_SIZE * 2,
        );
        match result {
            Err(Error::InvalidAddress) => {}
            Ok(pages) => {
                let line = format!("mapcpfail: FAIL unexpected success pages={}\n", pages);
                send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                let _ = debug_print(line.trim_end());
                let _ = syscall::space_unmap(space_token, SOURCE_BASE, SOURCE_PAGES);
                let _ = syscall::space_unmap(space_token, TARGET_BASE, total_pages);
                return Ok(());
            }
            Err(err) => {
                let line = format!("mapcpfail: FAIL wrong error {:?}\n", err);
                send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                let _ = debug_print(line.trim_end());
                let _ = syscall::space_unmap(space_token, SOURCE_BASE, SOURCE_PAGES);
                let _ = syscall::space_unmap(space_token, TARGET_BASE, total_pages);
                return Ok(());
            }
        }

        let verify_result =
            syscall::space_map_range(space_token, TARGET_BASE, 0, 0x03, total_pages, 0);
        match verify_result {
            Ok(mapped) if mapped == total_pages => {}
            Ok(mapped) => {
                let line = format!(
                    "mapcpfail: FAIL rollback remap short mapped_pages={}\n",
                    mapped
                );
                send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                let _ = debug_print(line.trim_end());
                let _ = syscall::space_unmap(space_token, SOURCE_BASE, SOURCE_PAGES);
                let _ = syscall::space_unmap(space_token, TARGET_BASE, total_pages);
                return Ok(());
            }
            Err(err) => {
                let line = format!("mapcpfail: FAIL rollback remap error {:?}\n", err);
                send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                let _ = debug_print(line.trim_end());
                let _ = syscall::space_unmap(space_token, SOURCE_BASE, SOURCE_PAGES);
                let _ = syscall::space_unmap(space_token, TARGET_BASE, total_pages);
                return Ok(());
            }
        }

        let line = format!("mapcpfail: PASS total_pages={}\n", total_pages);
        send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
        let _ = debug_print(line.trim_end());
        let _ = syscall::space_unmap(space_token, SOURCE_BASE, SOURCE_PAGES);
        let _ = syscall::space_unmap(space_token, TARGET_BASE, total_pages);
        Ok(())
    }
}

struct MapErrorBuiltin;

impl BuiltinCommand for MapErrorBuiltin {
    fn name(&self) -> &'static str {
        "maperror"
    }

    fn run(&self, stdout: usize, _context: &mut CommandContext, args: &[String]) -> Result<()> {
        const TARGET_BASE: usize = 0x6E00_0000;
        const MAP_TEST_FAILPOINT: usize = 0x8000_0000;
        const MAP_TEST_FAIL_ON_MAP_STAGE: usize = 0x4000_0000;
        const MAP_TEST_FAIL_AFTER_SHIFT: usize = 16;
        const MAP_TEST_FAIL_AFTER_MASK: usize = 0xFF;

        let total_pages = args
            .first()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(3);
        if total_pages < 2 {
            let line = "maperror: FAIL total_pages must be >= 2\n";
            send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            let _ = debug_print("maperror: FAIL total_pages must be >= 2");
            return Ok(());
        }

        let space_token = process_info().tokens[TOKEN_SPACE];
        let _ = syscall::space_unmap(space_token, TARGET_BASE, total_pages);
        let fail_after = 1usize.min(total_pages - 1);
        let fail_bits = (fail_after & MAP_TEST_FAIL_AFTER_MASK) << MAP_TEST_FAIL_AFTER_SHIFT;
        let flags = 0x03 | MAP_TEST_FAILPOINT | MAP_TEST_FAIL_ON_MAP_STAGE | fail_bits;
        let result = syscall::space_map_range(space_token, TARGET_BASE, 0, flags, total_pages, 0);
        match result {
            Err(Error::OutOfMemory) => {}
            Ok(pages) => {
                let line = format!("maperror: FAIL unexpected success pages={}\n", pages);
                send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                let _ = debug_print(line.trim_end());
                let _ = syscall::space_unmap(space_token, TARGET_BASE, total_pages);
                return Ok(());
            }
            Err(err) => {
                let line = format!("maperror: FAIL wrong error {:?}\n", err);
                send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                let _ = debug_print(line.trim_end());
                let _ = syscall::space_unmap(space_token, TARGET_BASE, total_pages);
                return Ok(());
            }
        }

        let verify_result =
            syscall::space_map_range(space_token, TARGET_BASE, 0, 0x03, total_pages, 0);
        match verify_result {
            Ok(mapped) if mapped == total_pages => {}
            Ok(mapped) => {
                let line = format!(
                    "maperror: FAIL rollback remap short mapped_pages={}\n",
                    mapped
                );
                send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                let _ = debug_print(line.trim_end());
                let _ = syscall::space_unmap(space_token, TARGET_BASE, total_pages);
                return Ok(());
            }
            Err(err) => {
                let line = format!("maperror: FAIL rollback remap error {:?}\n", err);
                send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                let _ = debug_print(line.trim_end());
                let _ = syscall::space_unmap(space_token, TARGET_BASE, total_pages);
                return Ok(());
            }
        }

        let line = format!("maperror: PASS total_pages={}\n", total_pages);
        send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
        let _ = debug_print(line.trim_end());
        let _ = syscall::space_unmap(space_token, TARGET_BASE, total_pages);
        Ok(())
    }
}

struct Ext2WriteBuiltin;

impl BuiltinCommand for Ext2WriteBuiltin {
    fn name(&self) -> &'static str {
        "ext2write"
    }

    fn run(&self, stdout: usize, _context: &mut CommandContext, args: &[String]) -> Result<()> {
        let path = args.first().map(|s| s.as_str()).unwrap_or("/bin/hello");

        let vfs_endpoint = match registry::subscribe_output("vfs", "main") {
            Ok(ep) => ep,
            Err(err) => {
                let line = format!("ext2write: FAIL vfs unavailable {:?}\n", err);
                let _ = debug_print(line.as_str());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                return Ok(());
            }
        };

        let vfs = match VfsClient::new_from_registry(vfs_endpoint) {
            Ok(client) => client,
            Err(err) => {
                let line = format!("ext2write: FAIL client {:?}\n", err);
                let _ = debug_print(line.as_str());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                return Ok(());
            }
        };

        let file = match vfs.open(path) {
            Ok(file) => file,
            Err(err) => {
                let line = format!("ext2write: FAIL open {} {:?}\n", path, err);
                let _ = debug_print(line.as_str());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                return Ok(());
            }
        };

        // Keep the file valid by writing ELF magic byte at offset 0.
        match vfs.write(file, 0, &[0x7f]) {
            Ok(1) => {
                let line = format!("ext2write: PASS path={}\n", path);
                let _ = debug_print(line.as_str());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            }
            Ok(written) => {
                let line = format!("ext2write: FAIL short-write {}\n", written);
                let _ = debug_print(line.as_str());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            }
            Err(err) => {
                let line = format!("ext2write: FAIL write {:?}\n", err);
                let _ = debug_print(line.as_str());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            }
        }

        let _ = vfs.close(file);
        Ok(())
    }
}

struct Ext2AppendBuiltin;

impl BuiltinCommand for Ext2AppendBuiltin {
    fn name(&self) -> &'static str {
        "ext2append"
    }

    fn run(&self, stdout: usize, _context: &mut CommandContext, args: &[String]) -> Result<()> {
        let path = args.first().map(|s| s.as_str()).unwrap_or("/bin/hello");

        let vfs_endpoint = match registry::subscribe_output("vfs", "main") {
            Ok(ep) => ep,
            Err(err) => {
                let line = format!("ext2append: FAIL vfs unavailable {:?}\n", err);
                let _ = debug_print(line.as_str());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                return Ok(());
            }
        };

        let vfs = match VfsClient::new_from_registry(vfs_endpoint) {
            Ok(client) => client,
            Err(err) => {
                let line = format!("ext2append: FAIL client {:?}\n", err);
                let _ = debug_print(line.as_str());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                return Ok(());
            }
        };

        let file = match vfs.open(path) {
            Ok(file) => file,
            Err(err) => {
                let line = format!("ext2append: FAIL open {} {:?}\n", path, err);
                let _ = debug_print(line.as_str());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                return Ok(());
            }
        };

        let append_offset = file.size;
        match vfs.write(file, append_offset, &[0]) {
            Ok(1) => {
                let line = format!("ext2append: PASS path={} offset={}\n", path, append_offset);
                let _ = debug_print(line.as_str());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            }
            Ok(written) => {
                let line = format!("ext2append: FAIL short-write {}\n", written);
                let _ = debug_print(line.as_str());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            }
            Err(err) => {
                let line = format!("ext2append: FAIL write {:?}\n", err);
                let _ = debug_print(line.as_str());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            }
        }

        let _ = vfs.close(file);
        Ok(())
    }
}

struct Ext2MutateBuiltin;

impl BuiltinCommand for Ext2MutateBuiltin {
    fn name(&self) -> &'static str {
        "ext2mutate"
    }

    fn run(&self, stdout: usize, _context: &mut CommandContext, _args: &[String]) -> Result<()> {
        let vfs_endpoint = match registry::subscribe_output("vfs", "main") {
            Ok(ep) => ep,
            Err(err) => {
                let line = format!("ext2mutate: FAIL vfs unavailable {:?}\n", err);
                let _ = debug_print(line.as_str());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                return Ok(());
            }
        };

        let vfs = match VfsClient::new_from_registry(vfs_endpoint) {
            Ok(client) => client,
            Err(err) => {
                let line = format!("ext2mutate: FAIL client {:?}\n", err);
                let _ = debug_print(line.as_str());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                return Ok(());
            }
        };

        let from = "/l2a_dir";
        let to = "/l2a_dir_renamed";
        let mut op = "mkdir";
        let result = (|| -> Result<()> {
            vfs.mkdir(from, 0o755)?;
            op = "rename";
            vfs.rename(from, to)?;
            op = "rmdir";
            vfs.rmdir(to)?;
            Ok(())
        })();

        match result {
            Ok(()) => {
                let line = "ext2mutate: PASS mkdir+rename+rmdir\n";
                let _ = debug_print(line);
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            }
            Err(err) => {
                let line = format!("ext2mutate: FAIL op={} err={:?}\n", op, err);
                let _ = debug_print(line.as_str());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            }
        }

        Ok(())
    }
}

struct Ext2UnlinkBuiltin;

impl BuiltinCommand for Ext2UnlinkBuiltin {
    fn name(&self) -> &'static str {
        "ext2unlink"
    }

    fn run(&self, stdout: usize, _context: &mut CommandContext, _args: &[String]) -> Result<()> {
        let path = "/l2a_tmp_unlink";
        let vfs_endpoint = match registry::subscribe_output("vfs", "main") {
            Ok(ep) => ep,
            Err(err) => {
                let line = format!("ext2unlink: FAIL vfs unavailable {:?}\n", err);
                let _ = debug_print(line.as_str());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                return Ok(());
            }
        };
        let vfs = match VfsClient::new_from_registry(vfs_endpoint) {
            Ok(client) => client,
            Err(err) => {
                let line = format!("ext2unlink: FAIL client {:?}\n", err);
                let _ = debug_print(line.as_str());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                return Ok(());
            }
        };

        // O_CREAT | O_RDWR
        let created = match vfs.open_with(path, 0o100 | 2, 0o644) {
            Ok(file) => file,
            Err(err) => {
                let line = format!("ext2unlink: FAIL create/open {:?}\n", err);
                let _ = debug_print(line.as_str());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                return Ok(());
            }
        };
        let _ = vfs.close(created);

        if let Err(err) = vfs.unlink(path) {
            let line = format!("ext2unlink: FAIL unlink {:?}\n", err);
            let _ = debug_print(line.as_str());
            send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            return Ok(());
        }

        match vfs.stat(path) {
            Err(Error::NotFound) => {
                let line = "ext2unlink: PASS create+unlink+verify\n";
                let _ = debug_print(line);
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            }
            Err(err) => {
                let line = format!("ext2unlink: FAIL verify {:?}\n", err);
                let _ = debug_print(line.as_str());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            }
            Ok(_) => {
                let line = "ext2unlink: FAIL still-exists\n";
                let _ = debug_print(line);
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            }
        }

        Ok(())
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
    // Procmgr requires absolute paths.
    // If path doesn't start with '/', assume it's in /bin/
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/bin/{}", path)
    }
}

fn encode_registry_names(service: &str, endpoint: &str) -> Vec<u8> {
    let service_bytes = service.as_bytes();
    let endpoint_bytes = endpoint.as_bytes();
    let mut payload = Vec::with_capacity(4 + service_bytes.len() + endpoint_bytes.len());
    payload.extend_from_slice(&(service_bytes.len() as u16).to_le_bytes());
    payload.extend_from_slice(&(endpoint_bytes.len() as u16).to_le_bytes());
    payload.extend_from_slice(service_bytes);
    payload.extend_from_slice(endpoint_bytes);
    payload
}

fn parse_status(raw: usize) -> Result<()> {
    let signed = raw as isize;
    if signed < 0 {
        return Err(Error::from_errno(signed));
    }
    Ok(())
}

fn div_op(stdout: usize, context: &mut CommandContext, lhs: &str, rhs: &str) -> Result<()> {
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
    context.get(token).and_then(|val| val.parse::<i64>().ok())
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

fn is_elf_magic(data: &[u8]) -> bool {
    data.len() >= 4 && data[0] == 0x7f && data[1] == b'E' && data[2] == b'L' && data[3] == b'F'
}

fn write_hexdump(stdout: usize, base_offset: usize, data: &[u8]) -> Result<()> {
    const BYTES_PER_LINE: usize = 16;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut line = [0u8; 96];

    for (line_idx, chunk) in data.chunks(BYTES_PER_LINE).enumerate() {
        let offset = (base_offset + line_idx * BYTES_PER_LINE) as u32;
        let mut idx = 0usize;

        // 8-hex offset
        for shift in (0..32).step_by(4).rev() {
            line[idx] = HEX[((offset >> shift) & 0xF) as usize];
            idx += 1;
        }
        line[idx] = b':';
        idx += 1;
        line[idx] = b' ';
        idx += 1;

        // Hex bytes
        for i in 0..BYTES_PER_LINE {
            if i < chunk.len() {
                let b = chunk[i];
                line[idx] = HEX[(b >> 4) as usize];
                idx += 1;
                line[idx] = HEX[(b & 0xF) as usize];
                idx += 1;
                line[idx] = b' ';
                idx += 1;
            } else {
                line[idx] = b' ';
                line[idx + 1] = b' ';
                line[idx + 2] = b' ';
                idx += 3;
            }
        }

        line[idx] = b' ';
        idx += 1;
        line[idx] = b'|';
        idx += 1;

        for &b in chunk {
            line[idx] = if b.is_ascii_graphic() || b == b' ' {
                b
            } else {
                b'.'
            };
            idx += 1;
        }
        for _ in chunk.len()..BYTES_PER_LINE {
            line[idx] = b' ';
            idx += 1;
        }
        line[idx] = b'|';
        idx += 1;
        line[idx] = b'\n';
        idx += 1;

        send_with_retry(stdout, TTY_WRITE_LABEL, &line[..idx])?;
    }

    Ok(())
}

struct CatBuiltin;

impl BuiltinCommand for CatBuiltin {
    fn name(&self) -> &'static str {
        "cat"
    }

    fn run(&self, stdout: usize, _context: &mut CommandContext, args: &[String]) -> Result<()> {
        let mut hex_requested = false;
        let mut path: Option<&str> = None;
        for arg in args {
            if arg == "-x" {
                hex_requested = true;
            } else if arg == "--grant" {
                // Explicitly request zero-copy grant path (default).
                continue;
            } else if path.is_none() {
                path = Some(arg.as_str());
            }
        }

        let Some(path) = path else {
            send_with_retry(stdout, TTY_WRITE_LABEL, b"cat: missing path\n")?;
            return Ok(());
        };

        // Get VFS endpoint
        let vfs_endpoint = match registry::subscribe_output("vfs", "main") {
            Ok(ep) => ep,
            Err(_) => {
                send_with_retry(stdout, TTY_WRITE_LABEL, b"cat: vfs not available\n")?;
                return Ok(());
            }
        };

        let vfs = match VfsClient::new_from_registry(vfs_endpoint) {
            Ok(c) => c,
            Err(_) => {
                send_with_retry(
                    stdout,
                    TTY_WRITE_LABEL,
                    b"cat: failed to create vfs client\n",
                )?;
                return Ok(());
            }
        };

        // Open the file
        let file = match vfs.open(path) {
            Ok(f) => f,
            Err(e) => {
                let msg = format!("cat: {}: {:?}\n", path, e);
                send_with_retry(stdout, TTY_WRITE_LABEL, msg.as_bytes())?;
                return Ok(());
            }
        };

        if file.size == 0 {
            let _ = vfs.close(file);
            return Ok(());
        }

        // Read using grant with streaming/chunked reads
        let info = process_info();
        let space_token = info.tokens[TOKEN_SPACE];

        // Match VFS grant window size to avoid oversized remote reads.
        const CHUNK_SIZE: usize = 64 * 1024;
        let chunk_size = CHUNK_SIZE.min(file.size);
        let grant_size = (chunk_size + 4095) & !4095; // Page-align

        // Allocate virtual address region for chunks
        let read_buf_base = match libcluu::vspace::VSPACE.lock().alloc(grant_size) {
            Ok(addr) => addr,
            Err(_) => {
                let _ = vfs.close(file);
                send_with_retry(stdout, TTY_WRITE_LABEL, b"cat: out of virtual memory\n")?;
                return Ok(());
            }
        };

        // Stream the file in chunks
        let mut offset = 0;
        let mut last_char = None;
        let mut hex_mode = hex_requested;
        while offset < file.size {
            let remaining = file.size - offset;
            let read_size = remaining.min(CHUNK_SIZE);

            match vfs.read_grant(file, offset, read_size, space_token, read_buf_base) {
                Ok(grant) => {
                    if grant.len == 0 {
                        break;
                    }

                    if grant.len > read_size {
                        let msg = format!(
                            "cat: read size mismatch (requested {}, got {})\n",
                            read_size, grant.len
                        );
                        let _ = send_with_retry(stdout, TTY_WRITE_LABEL, msg.as_bytes());
                        break;
                    }

                    if grant.offset + grant.len > grant_size {
                        let msg = format!(
                            "cat: grant range out of bounds (offset {}, len {})\n",
                            grant.offset, grant.len
                        );
                        let _ = send_with_retry(stdout, TTY_WRITE_LABEL, msg.as_bytes());
                        break;
                    }

                    let addr = grant.base + grant.offset;
                    let data = unsafe { core::slice::from_raw_parts(addr as *const u8, grant.len) };

                    if !hex_mode && offset == 0 && is_elf_magic(data) {
                        hex_mode = true;
                    }

                    if hex_mode {
                        write_hexdump(stdout, offset, data)?;
                    } else {
                        // Output the chunk
                        // IPC_MESSAGE_MAX is now 4096 bytes, so we can send larger chunks
                        // This significantly reduces syscall overhead for large files
                        for chunk in data.chunks(4096) {
                            let _ = send_with_retry(stdout, TTY_WRITE_LABEL, chunk);
                        }

                        // Remember last character for newline check
                        if !data.is_empty() {
                            last_char = Some(data[data.len() - 1]);
                        }
                    }

                    offset += grant.len;
                }
                Err(e) => {
                    let msg = format!("cat: read error at offset {}: {:?}\n", offset, e);
                    send_with_retry(stdout, TTY_WRITE_LABEL, msg.as_bytes())?;
                    break;
                }
            }
        }

        // Add newline if file doesn't end with one (text mode only)
        if !hex_mode {
            if let Some(ch) = last_char {
                if ch != b'\n' {
                    let _ = send_with_retry(stdout, TTY_WRITE_LABEL, b"\n");
                }
            }
        }

        // Free the allocated virtual address region
        let _ = libcluu::vspace::VSPACE
            .lock()
            .free(read_buf_base, grant_size);

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
        let path = args.first().map(|s| s.as_str()).unwrap_or("/");

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
                send_with_payload(
                    stdout,
                    TTY_WRITE_LABEL,
                    b"ls: failed to create vfs client\n",
                )?;
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

struct HeapBuiltin;

impl BuiltinCommand for HeapBuiltin {
    fn name(&self) -> &'static str {
        "heap"
    }

    fn run(&self, stdout: usize, _context: &mut CommandContext, _args: &[String]) -> Result<()> {
        let stats = libcluu::allocator::stats();
        let line = format!(
            "heap: used={} total={} peak={} free={}\n",
            stats.used, stats.total, stats.peak, stats.free
        );
        send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
        Ok(())
    }
}
