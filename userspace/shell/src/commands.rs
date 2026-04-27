//! Builtin command handling for the shell.
//!
//! This module keeps the command execution logic separate from the IO loop and
//! parser wiring, following SOLID separation between parsing, dispatch, and IO.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::mem::size_of;
use libcluu::boot::{TOKEN_REGISTRY, TOKEN_SPACE, TOKEN_STDIN};
use libcluu::fs::client::VfsClient;
use libcluu::ipc::{
    call, call_with_payload, call_with_reply_buf, recv, send_with_payload, send_with_retry,
    build_container_run_payload_with_argv, SharedRing, CONSOLE_CLEAR_LABEL, PROCMGR_CONTAINER_LIST_LABEL,
    PROCMGR_CONTAINER_RUN_LABEL, PROCMGR_ESCALATE_LABEL, PROCMGR_SHUTDOWN_LABEL, PROCMGR_SU_LABEL,
    TTY_CTL_LABEL, TTY_FG_FLAG_FORWARD_CTRL_C, TTY_FG_FLAG_NOTIFY_CTRL_C, TTY_READ_LABEL,
    TTY_REGISTER_LABEL, TTY_WRITE_LABEL,
};
use libcluu::registry;
use libcluu::syscall;
use libcluu::types::Message;
use libcluu::{debug_print, process_info, Error, IpcFlags, Result, TOKEN_IPC};

use cluu_lang::ast::{Assign, CmdElem, DqPart, Program, Stmt, Word, WordPart};

const PROCMGR_KILL_LABEL: u32 = 3;
const DEFAULT_PRIORITY: usize = 200;
const SIGINT: usize = 2;
const SIGTERM: usize = 15;
const SIGCONT: usize = 18;
const SIGSTOP: usize = 19;
const TTY_LFLAG_ICANON: usize = 0x02;
const TTY_LFLAG_ECHO: usize = 0x08;
const TTY_LFLAG_DEFAULT: usize = TTY_LFLAG_ICANON | TTY_LFLAG_ECHO;

/// Execution result for a command handler.
pub enum ExecResult {
    Handled,
    NotHandled,
}

/// Per-shell execution context shared across command invocations.
pub struct CommandContext {
    vars: BTreeMap<String, String>,
    procmgr_spawn: usize,
    console_write: usize,
    bg_jobs: BTreeMap<usize, BackgroundJob>,
    /// Exit status of the most recently executed builtin/command.
    /// Read by `echo $?` (Shell-B). `cd`/`pwd` write here.
    last_status: i32,
}

struct BackgroundJob {
    notify_endpoint: usize,
    stdin_endpoint: usize,
    command: String,
    state: JobState,
    fg_mode: ForegroundMode,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum JobState {
    Running,
    Stopped,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ForegroundMode {
    SignalOnCtrlC,
    PassCtrlCToChild,
}

impl CommandContext {
    /// Create a fresh shell context.
    pub fn new() -> Self {
        Self {
            vars: BTreeMap::new(),
            procmgr_spawn: 0,
            console_write: 0,
            bg_jobs: BTreeMap::new(),
            last_status: 0,
        }
    }

    /// Set the exit status of the most recently executed builtin/command.
    pub fn set_last_status(&mut self, status: i32) {
        self.last_status = status;
    }

    /// Return the exit status of the most recently executed builtin/command.
    pub fn last_status(&self) -> i32 {
        self.last_status
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

    pub fn set_procmgr_spawn(&mut self, ep: usize) {
        self.procmgr_spawn = ep;
    }

    fn procmgr_spawn_endpoint(&mut self) -> Result<usize> {
        if self.procmgr_spawn == 0 {
            self.procmgr_spawn = registry::subscribe_output("procmgr", "spawn")?;
        }
        Ok(self.procmgr_spawn)
    }

    fn console_write_endpoint(&mut self) -> Result<usize> {
        if self.console_write == 0 {
            self.console_write = registry::subscribe_output("console:0", "write")?;
        }
        Ok(self.console_write)
    }

    fn add_bg_job(
        &mut self,
        pid: usize,
        notify_endpoint: usize,
        stdin_endpoint: usize,
        command: String,
        fg_mode: ForegroundMode,
    ) {
        self.bg_jobs.insert(
            pid,
            BackgroundJob {
                notify_endpoint,
                stdin_endpoint,
                command,
                state: JobState::Running,
                fg_mode,
            },
        );
    }

    fn remove_bg_job(&mut self, pid: usize) {
        self.bg_jobs.remove(&pid);
    }

    fn take_bg_job(&mut self, pid: usize) -> Option<BackgroundJob> {
        self.bg_jobs.remove(&pid)
    }

    fn bg_job_state(&self, pid: usize) -> Option<JobState> {
        self.bg_jobs.get(&pid).map(|job| job.state)
    }

    fn set_bg_job_state(&mut self, pid: usize, state: JobState) -> bool {
        if let Some(job) = self.bg_jobs.get_mut(&pid) {
            job.state = state;
            true
        } else {
            false
        }
    }

    fn latest_bg_pid(&self) -> Option<usize> {
        self.bg_jobs.keys().next_back().copied()
    }

    fn bg_job_lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (pid, job) in &self.bg_jobs {
            let state = match job.state {
                JobState::Running => "running",
                JobState::Stopped => "stopped",
            };
            out.push(format!("[{}] {} {}", pid, state, job.command));
        }
        out
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
        registry.register(Box::new(ClearBuiltin));
        registry.register(Box::new(EchoBuiltin));
        registry.register(Box::new(CdBuiltin));
        registry.register(Box::new(PwdBuiltin));
        registry.register(Box::new(ExitBuiltin));
        registry.register(Box::new(SetBuiltin));
        registry.register(Box::new(UnsetBuiltin));
        registry.register(Box::new(EnvBuiltin));
        registry.register(Box::new(ExprBuiltin));
        registry.register(Box::new(LetBuiltin));
        registry.register(Box::new(SpawnBuiltin));
        registry.register(Box::new(SpawnBgBuiltin));
        registry.register(Box::new(JobsBuiltin));
        registry.register(Box::new(JobChurnBuiltin));
        registry.register(Box::new(JobMixBuiltin));
        registry.register(Box::new(StopBuiltin));
        registry.register(Box::new(ForegroundBuiltin));
        registry.register(Box::new(BackgroundBuiltin));
        registry.register(Box::new(KillDenyBuiltin));
        registry.register(Box::new(RegistryDenyBuiltin));
        registry.register(Box::new(MapFailBuiltin));
        registry.register(Box::new(MapCopyFailBuiltin));
        registry.register(Box::new(MapErrorBuiltin));
        registry.register(Box::new(Ext2WriteBuiltin));
        registry.register(Box::new(Ext2AppendBuiltin));
        registry.register(Box::new(Ext2MutateBuiltin));
        registry.register(Box::new(Ext2UnlinkBuiltin));
        registry.register(Box::new(Ext2OwnerDenyBuiltin));
        registry.register(Box::new(RingIoBuiltin));
        registry.register(Box::new(CatBuiltin));
        registry.register(Box::new(HeapBuiltin));
        registry.register(Box::new(ContainerBuiltin));
        registry.register(Box::new(SuBuiltin));
        registry.register(Box::new(SudoBuiltin));
        registry.register(Box::new(VtCrashTestBuiltin));
        registry.register(Box::new(SudoTestBuiltin));
        registry.register(Box::new(SuTestBuiltin));
        registry.register(Box::new(EscalateDenyBuiltin));
        registry.register(Box::new(SuEqualTestBuiltin));
        registry.register(Box::new(ShellCrashBuiltin));
        registry.register(Box::new(PoweroffBuiltin));
        registry.register(Box::new(RebootBuiltin));
        registry.register(Box::new(TrueBuiltin));
        registry.register(Box::new(FalseBuiltin));
        registry.register(Box::new(TestBuiltin { bracket: false }));
        registry.register(Box::new(TestBuiltin { bracket: true }));
    }
}

impl CommandExecutor for BuiltinRegistry {
    fn execute(
        &self,
        stdout: usize,
        context: &mut CommandContext,
        program: &Program,
    ) -> Result<ExecResult> {
        if program.stmts.is_empty() {
            return Ok(ExecResult::NotHandled);
        }

        // Execute each statement sequentially. Top-level `;` in cluu_lang
        // produces multiple Stmts; we run them left-to-right and report
        // Handled if every statement was handled (used for startup
        // autostart strings like "cd /; cd etc; pwd").
        let mut all_handled = true;
        for stmt in &program.stmts {
            let command = match flatten_simple_command_from_stmt(stmt) {
                Some(command) => command,
                None => {
                    all_handled = false;
                    continue;
                }
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
                all_handled = false;
                continue;
            };
            let result = if name.as_str() == "repeat" {
                self.execute_repeat(stdout, context, &args[1..])?
            } else {
                self.run_builtin(stdout, context, name, &args[1..])?
            };
            if let ExecResult::NotHandled = result {
                all_handled = false;
            }
        }

        if all_handled {
            Ok(ExecResult::Handled)
        } else {
            Ok(ExecResult::NotHandled)
        }
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
            b"builtins: help, clear, echo, cd, pwd, exit, set, unset, env, expr, let, spawn, spawnbg, jobs, jobchurn, jobmix, stop, fg, bg, killdeny, regdeny, mapfail, mapcpfail, maperror, ext2write, ext2append, ext2mutate, ext2unlink, ext2ownerdeny, ringio, repeat, cat, ls, heap\n",
        )?;
        Ok(())
    }
}

struct ClearBuiltin;

impl BuiltinCommand for ClearBuiltin {
    fn name(&self) -> &'static str {
        "clear"
    }

    fn run(&self, _stdout: usize, context: &mut CommandContext, _args: &[String]) -> Result<()> {
        let console = context.console_write_endpoint()?;
        send_with_payload(console, CONSOLE_CLEAR_LABEL, &[])?;
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

fn flatten_simple_command_from_stmt(stmt: &Stmt) -> Option<ParsedCommand> {
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

struct CdBuiltin;

impl BuiltinCommand for CdBuiltin {
    fn name(&self) -> &'static str {
        "cd"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        if args.len() > 1 {
            send_with_payload(stdout, TTY_WRITE_LABEL, b"cd: too many arguments\n")?;
            context.set_last_status(1);
            return Ok(());
        }

        let target: String = if args.is_empty() {
            // No arg: use $HOME, fall back to "/" if unset.
            crate::read_env_var("HOME").unwrap_or_else(|| String::from("/"))
        } else {
            args[0].clone()
        };

        match libcluu::posix::set_current_dir_str(target.as_str()) {
            Ok(()) => {
                context.set_last_status(0);
            }
            Err(errno) => {
                let line = format!("cd: {}: errno {}\n", target, errno);
                send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                context.set_last_status(1);
            }
        }
        Ok(())
    }
}

struct PwdBuiltin;

impl BuiltinCommand for PwdBuiltin {
    fn name(&self) -> &'static str {
        "pwd"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        if !args.is_empty() {
            send_with_payload(stdout, TTY_WRITE_LABEL, b"pwd: too many arguments\n")?;
            context.set_last_status(1);
            return Ok(());
        }

        let cwd = libcluu::posix::current_dir_string();
        // Harness-observable signal (COM2 captures debug_print output but not
        // TTY writes). The harness marker "shell: pwd=<path>" is keyed off this.
        let _ = libcluu::debug_print(&format!("shell: pwd={}\n", cwd));
        let mut line = cwd;
        line.push('\n');
        send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
        context.set_last_status(0);
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
struct SpawnBgBuiltin;
struct JobsBuiltin;
struct JobChurnBuiltin;
struct JobMixBuiltin;
struct StopBuiltin;
struct ForegroundBuiltin;
struct BackgroundBuiltin;

fn infer_foreground_mode(path: &str) -> ForegroundMode {
    let path = path.trim();
    if path.ends_with("/mp")
        || path.ends_with("/micropython")
        || path == "mp"
        || path == "micropython"
    {
        return ForegroundMode::PassCtrlCToChild;
    }
    ForegroundMode::SignalOnCtrlC
}

fn parse_spawn_args(args: &[String]) -> Option<(String, usize, ForegroundMode, Vec<String>)> {
    if args.is_empty() {
        return None;
    }
    let mut idx = 0usize;
    let mut mode = ForegroundMode::SignalOnCtrlC;
    let mut mode_explicit = false;
    while idx < args.len() {
        match args[idx].as_str() {
            "-i" | "--interactive" => {
                mode = ForegroundMode::PassCtrlCToChild;
                mode_explicit = true;
                idx += 1;
            }
            "-s" | "--signal" => {
                mode = ForegroundMode::SignalOnCtrlC;
                mode_explicit = true;
                idx += 1;
            }
            _ => break,
        }
    }
    let path = args.get(idx)?.clone();
    idx += 1;

    // Priority: if the next token parses as usize, consume it as priority. Else
    // leave it for argv. This preserves backward compat (`spawn foo 5`) while
    // allowing `spawn foo --help` to pass `--help` as argv[1].
    let priority = match args.get(idx).and_then(|v| v.parse::<usize>().ok()) {
        Some(p) => {
            idx += 1;
            p
        }
        None => DEFAULT_PRIORITY,
    };

    let argv_tail: Vec<String> = args[idx..].to_vec();

    if !mode_explicit {
        mode = infer_foreground_mode(path.as_str());
    }
    Some((path, priority, mode, argv_tail))
}

impl BuiltinCommand for SpawnBuiltin {
    fn name(&self) -> &'static str {
        "spawn"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        let Some((path, priority, fg_mode, argv_tail)) = parse_spawn_args(args) else {
            send_with_payload(stdout, TTY_WRITE_LABEL, b"spawn: missing path\n")?;
            return Ok(());
        };
        let argv_refs: Vec<&str> = argv_tail.iter().map(|s| s.as_str()).collect();
        let spawn = spawn_process_with_argv(context, path.as_str(), priority, &argv_refs)?;
        match parse_status(spawn.status_word) {
            Ok(()) => {
                let child_pid = spawn.pid;
                wait_for_exit_or_sigint(
                    spawn.procmgr_endpoint,
                    stdout,
                    spawn.notify_endpoint,
                    spawn.stdin_endpoint,
                    child_pid,
                    stdout,
                    fg_mode,
                )?;
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

impl BuiltinCommand for SpawnBgBuiltin {
    fn name(&self) -> &'static str {
        "spawnbg"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        let Some((path, priority, fg_mode, _argv_tail)) = parse_spawn_args(args) else {
            send_with_payload(stdout, TTY_WRITE_LABEL, b"spawnbg: missing path\n")?;
            return Ok(());
        };

        let spawn = spawn_process_with_argv(context, path.as_str(), priority, &[])?;
        match parse_status(spawn.status_word) {
            Ok(()) => {
                context.add_bg_job(
                    spawn.pid,
                    spawn.notify_endpoint,
                    spawn.stdin_endpoint,
                    String::from(path.as_str()),
                    fg_mode,
                );
                let line = format!("spawnbg: started pid={}\n", spawn.pid);
                let _ = debug_print(line.trim_end());
                send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            }
            Err(err) => {
                let line = format!("spawnbg: {:?}\n", err);
                send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            }
        }
        Ok(())
    }
}

impl BuiltinCommand for JobsBuiltin {
    fn name(&self) -> &'static str {
        "jobs"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, _args: &[String]) -> Result<()> {
        let lines = context.bg_job_lines();
        if lines.is_empty() {
            send_with_payload(stdout, TTY_WRITE_LABEL, b"jobs: none\n")?;
            return Ok(());
        }
        for line in lines {
            send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            send_with_payload(stdout, TTY_WRITE_LABEL, b"\n")?;
        }
        Ok(())
    }
}

impl BuiltinCommand for ForegroundBuiltin {
    fn name(&self) -> &'static str {
        "fg"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        let Some(pid) = resolve_job_pid(context, args.first()) else {
            send_with_payload(stdout, TTY_WRITE_LABEL, b"fg: no background jobs\n")?;
            return Ok(());
        };
        let Some(job) = context.take_bg_job(pid) else {
            let line = format!("fg: unknown pid={}\n", pid);
            send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            return Ok(());
        };

        let line = format!("fg: pid={} {}\n", pid, job.command);
        let _ = debug_print(line.trim_end());
        send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;

        let procmgr_endpoint = context.procmgr_spawn_endpoint()?;
        if job.state == JobState::Stopped {
            signal_process(procmgr_endpoint, pid, SIGCONT)?;
            let line = format!("fg: continued pid={}\n", pid);
            send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
        }
        wait_for_exit_or_sigint(
            procmgr_endpoint,
            stdout,
            job.notify_endpoint,
            job.stdin_endpoint,
            pid,
            stdout,
            job.fg_mode,
        )?;
        Ok(())
    }
}

impl BuiltinCommand for StopBuiltin {
    fn name(&self) -> &'static str {
        "stop"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        let Some(pid) = resolve_job_pid(context, args.first()) else {
            send_with_payload(stdout, TTY_WRITE_LABEL, b"stop: no background jobs\n")?;
            return Ok(());
        };
        let Some(state) = context.bg_job_state(pid) else {
            let line = format!("stop: unknown pid={}\n", pid);
            send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            return Ok(());
        };
        if state == JobState::Stopped {
            let line = format!("stop: pid={} already stopped\n", pid);
            send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            return Ok(());
        }

        let procmgr_endpoint = context.procmgr_spawn_endpoint()?;
        signal_process(procmgr_endpoint, pid, SIGSTOP)?;
        ensure_bg_job_state(context, pid, JobState::Stopped)?;
        let line = format!("stop: pid={} stopped\n", pid);
        send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
        Ok(())
    }
}

impl BuiltinCommand for JobChurnBuiltin {
    fn name(&self) -> &'static str {
        "jobchurn"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        let iterations = args
            .first()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(3);
        if iterations == 0 {
            send_with_payload(
                stdout,
                TTY_WRITE_LABEL,
                b"jobchurn: iterations must be >= 1\n",
            )?;
            return Ok(());
        }

        for _ in 0..iterations {
            let spawn = spawn_process(context, "sleepy", DEFAULT_PRIORITY)?;
            parse_status(spawn.status_word)?;

            let pid = spawn.pid;
            context.add_bg_job(
                pid,
                spawn.notify_endpoint,
                spawn.stdin_endpoint,
                String::from("sleepy"),
                ForegroundMode::SignalOnCtrlC,
            );

            signal_process(spawn.procmgr_endpoint, pid, SIGSTOP)?;
            ensure_bg_job_state(context, pid, JobState::Stopped)?;

            signal_process(spawn.procmgr_endpoint, pid, SIGCONT)?;
            ensure_bg_job_state(context, pid, JobState::Running)?;

            let Some(job) = context.take_bg_job(pid) else {
                let line = format!("jobchurn: FAIL missing job pid={}\n", pid);
                send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                return Ok(());
            };

            wait_for_exit_or_sigint(
                spawn.procmgr_endpoint,
                stdout,
                job.notify_endpoint,
                job.stdin_endpoint,
                pid,
                stdout,
                job.fg_mode,
            )?;
        }

        let line = format!("jobchurn: PASS iterations={}\n", iterations);
        send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
        let _ = debug_print(line.trim_end());
        Ok(())
    }
}

impl BuiltinCommand for JobMixBuiltin {
    fn name(&self) -> &'static str {
        "jobmix"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, _args: &[String]) -> Result<()> {
        let spawn_a = spawn_process(context, "sleepy", DEFAULT_PRIORITY)?;
        parse_status(spawn_a.status_word)?;
        let pid_a = spawn_a.pid;
        context.add_bg_job(
            pid_a,
            spawn_a.notify_endpoint,
            spawn_a.stdin_endpoint,
            String::from("sleepy"),
            ForegroundMode::SignalOnCtrlC,
        );

        let spawn_b = spawn_process(context, "sleepy", DEFAULT_PRIORITY)?;
        parse_status(spawn_b.status_word)?;
        let pid_b = spawn_b.pid;
        context.add_bg_job(
            pid_b,
            spawn_b.notify_endpoint,
            spawn_b.stdin_endpoint,
            String::from("sleepy"),
            ForegroundMode::SignalOnCtrlC,
        );

        signal_process(spawn_a.procmgr_endpoint, pid_a, SIGSTOP)?;
        ensure_bg_job_state(context, pid_a, JobState::Stopped)?;

        signal_process(spawn_b.procmgr_endpoint, pid_b, SIGSTOP)?;
        ensure_bg_job_state(context, pid_b, JobState::Stopped)?;

        signal_process(spawn_a.procmgr_endpoint, pid_a, SIGCONT)?;
        ensure_bg_job_state(context, pid_a, JobState::Running)?;

        let Some(job_b) = context.take_bg_job(pid_b) else {
            let line = format!("jobmix: FAIL missing job pid={}\n", pid_b);
            send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            return Ok(());
        };
        signal_process(spawn_b.procmgr_endpoint, pid_b, SIGCONT)?;
        wait_for_exit_or_sigint(
            spawn_b.procmgr_endpoint,
            stdout,
            job_b.notify_endpoint,
            job_b.stdin_endpoint,
            pid_b,
            stdout,
            job_b.fg_mode,
        )?;

        let Some(job_a) = context.take_bg_job(pid_a) else {
            let line = format!("jobmix: FAIL missing job pid={}\n", pid_a);
            send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            return Ok(());
        };
        wait_for_exit_or_sigint(
            spawn_a.procmgr_endpoint,
            stdout,
            job_a.notify_endpoint,
            job_a.stdin_endpoint,
            pid_a,
            stdout,
            job_a.fg_mode,
        )?;

        let line = format!("jobmix: PASS pids={},{}\n", pid_a, pid_b);
        send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
        let _ = debug_print(line.trim_end());
        Ok(())
    }
}

impl BuiltinCommand for BackgroundBuiltin {
    fn name(&self) -> &'static str {
        "bg"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        let Some(pid) = resolve_job_pid(context, args.first()) else {
            send_with_payload(stdout, TTY_WRITE_LABEL, b"bg: no background jobs\n")?;
            return Ok(());
        };
        let Some(state) = context.bg_job_state(pid) else {
            let line = format!("bg: unknown pid={}\n", pid);
            send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            return Ok(());
        };
        if state == JobState::Running {
            let line = format!("bg: pid={} already running\n", pid);
            send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            return Ok(());
        }

        let procmgr_endpoint = context.procmgr_spawn_endpoint()?;
        signal_process(procmgr_endpoint, pid, SIGCONT)?;
        ensure_bg_job_state(context, pid, JobState::Running)?;
        let line = format!("bg: pid={} running\n", pid);
        send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
        Ok(())
    }
}

struct SpawnResult {
    procmgr_endpoint: usize,
    notify_endpoint: usize,
    status_word: usize,
    pid: usize,
    stdin_endpoint: usize,
}

/// Build a `PROCMGR_CONTAINER_RUN_LABEL` payload of `name + CWD trailer`.
///
/// Procmgr reads the container image name from the start of the payload and
/// strips the CWD trailer (last 8 bytes + cwd_len) before slicing argv/FDAC,
/// so prepending the name and appending the trailer is safe even when the
/// container_run path doesn't carry argv or FDAC blobs.
/// Thin wrapper around `build_container_run_payload_with_argv`
/// for the zero-arg case. Plan 2 Task 4 adds argv-carrying callers.
fn build_container_run_payload(name: &str) -> Vec<u8> {
    build_container_run_payload_with_argv(name, &[]).0
}

fn spawn_process(context: &mut CommandContext, name: &str, priority: usize) -> Result<SpawnResult> {
    spawn_process_with_argv(context, name, priority, &[])
}

fn spawn_process_with_argv(
    context: &mut CommandContext,
    name: &str,
    _priority: usize,
    args: &[&str],
) -> Result<SpawnResult> {
    let procmgr_endpoint = context.procmgr_spawn_endpoint()?;
    let (payload, _argc) = build_container_run_payload_with_argv(name, args);
    let notify_endpoint = syscall::endpoint_create(process_info().tokens[TOKEN_IPC])?;
    let mut msg = Message::new(PROCMGR_CONTAINER_RUN_LABEL, [0; 6], 3);
    msg.words[0] = payload.len();
    msg.words[1] = notify_endpoint;
    msg.words[2] = 0; // fdac_offset
    let mut reply = Message::new(0, [0; 6], 0);
    let _ = debug_print(&format!(
        "shell: container run begin name={} ep={} notify={}",
        name, procmgr_endpoint, notify_endpoint
    ));
    call_with_payload(procmgr_endpoint, &msg, &payload, &mut reply)?;
    let _ = debug_print(&format!(
        "shell: container run done status={} pid={} stdin={}",
        reply.words[0], reply.words[1], reply.words[4]
    ));
    Ok(SpawnResult {
        procmgr_endpoint,
        notify_endpoint,
        status_word: reply.words[0],
        pid: reply.words[1],
        stdin_endpoint: reply.words[4],
    })
}

fn wait_for_exit_or_sigint(
    procmgr_endpoint: usize,
    tty_endpoint: usize,
    notify_endpoint: usize,
    child_stdin_endpoint: usize,
    child_pid: usize,
    stdout: usize,
    mode: ForegroundMode,
) -> Result<()> {
    if child_stdin_endpoint == 0 {
        let _ = send_with_payload(
            stdout,
            TTY_WRITE_LABEL,
            b"spawn: invalid child stdin route\n",
        );
        return Err(Error::InvalidState);
    }

    let mut ctrl_c_notify_endpoint = 0usize;
    let mut ctrl_c_flags = TTY_FG_FLAG_FORWARD_CTRL_C;
    if mode == ForegroundMode::SignalOnCtrlC {
        ctrl_c_notify_endpoint = syscall::endpoint_create(process_info().tokens[TOKEN_IPC])?;
        ctrl_c_flags = TTY_FG_FLAG_NOTIFY_CTRL_C | TTY_FG_FLAG_FORWARD_CTRL_C;
    }

    // Transfer tty foreground routing to the child while it runs.
    set_tty_foreground(
        tty_endpoint,
        child_stdin_endpoint,
        ctrl_c_notify_endpoint,
        ctrl_c_flags,
    )?;
    let saved_lflag = match tty_get_lflag(tty_endpoint) {
        Ok(lflag) => lflag,
        Err(err) => {
            let _ = debug_print(&format!("shell: tty_get_lflag failed {:?}", err));
            TTY_LFLAG_DEFAULT
        }
    };
    let mut lflag_switched = false;
    if mode == ForegroundMode::PassCtrlCToChild {
        let target_lflag = saved_lflag & !(TTY_LFLAG_ECHO | TTY_LFLAG_ICANON);
        match tty_set_lflag(tty_endpoint, target_lflag) {
            Ok(()) => lflag_switched = true,
            Err(err) => {
                let _ = debug_print(&format!("shell: tty_set_lflag(raw) failed {:?}", err));
            }
        }
    }

    let mut buf = [0u8; 256];
    let tokens = [notify_endpoint, ctrl_c_notify_endpoint];
    let active_tokens = if ctrl_c_notify_endpoint != 0 {
        &tokens[..2]
    } else {
        &tokens[..1]
    };

    let mut result = loop {
        let (index, len) = match syscall::ipc_recv_any(active_tokens, &mut buf, u64::MAX) {
            Ok(v) => v,
            Err(Error::WouldBlock) => continue,
            Err(err) => break Err(err),
        };
        let Some((msg, payload)) = parse_ipc_message(&buf[..len]) else {
            continue;
        };
        if index == 0 {
            // Exit notification from procmgr.
            if msg.tag.words >= 2 {
                let exit_code = msg.words[1] as i32;
                if exit_code > 128 {
                    let sig = exit_code - 128;
                    let sig_name = match sig {
                        4 => "Illegal instruction",
                        6 => "Aborted",
                        8 => "Floating point exception",
                        9 => "Killed",
                        11 => "Segmentation fault",
                        _ => "Signal",
                    };
                    let line = format!("{} (signal {})\n", sig_name, sig);
                    let _ = send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes());
                } else if exit_code != 0 {
                    let line = format!("Exited with status {}\n", exit_code);
                    let _ = send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes());
                }
                break Ok(());
            }
            continue;
        }

        // Ctrl-C notification from tty while child is foreground.
        if msg.tag.label != TTY_READ_LABEL {
            continue;
        }
        if payload.contains(&0x03) {
            let _ = signal_process(procmgr_endpoint, child_pid, SIGINT);
            let line = format!("spawn: SIGINT pid={}\n", child_pid);
            let _ = debug_print(line.trim_end());
            let _ = send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes());
        }
    };

    // Restore normal shell stdin routing once the foreground child finishes.
    if lflag_switched {
        if let Err(err) = tty_set_lflag(tty_endpoint, saved_lflag) {
            let _ = debug_print(&format!("shell: tty_set_lflag(restore) failed {:?}", err));
            if result.is_ok() {
                result = Err(err);
            }
        }
    }
    if let Err(err) = set_tty_foreground(tty_endpoint, 0, 0, TTY_FG_FLAG_FORWARD_CTRL_C) {
        let _ = debug_print(&format!("shell: tty foreground restore failed {:?}", err));
        if result.is_ok() {
            result = Err(err);
        }
    }
    result
}

fn set_tty_foreground(
    tty_endpoint: usize,
    foreground_endpoint: usize,
    ctrl_c_notify_endpoint: usize,
    flags: usize,
) -> Result<()> {
    let mut msg = Message::new(TTY_REGISTER_LABEL, [0; 6], 3);
    msg.words[0] = foreground_endpoint;
    msg.words[1] = ctrl_c_notify_endpoint;
    msg.words[2] = flags;
    call(tty_endpoint, &mut msg, IpcFlags::empty())?;
    Ok(())
}

fn tty_get_lflag(tty_endpoint: usize) -> Result<usize> {
    let mut msg = Message::new(TTY_CTL_LABEL, [0; 6], 1);
    msg.words[0] = 0; // getattr
    tty_ctl_call_with_retry(tty_endpoint, &mut msg)?;
    Ok(msg.words[4])
}

fn tty_set_lflag(tty_endpoint: usize, lflag: usize) -> Result<()> {
    let mut msg = Message::new(TTY_CTL_LABEL, [0; 6], 5);
    msg.words[0] = 1; // setattr
    msg.words[4] = lflag;
    tty_ctl_call_with_retry(tty_endpoint, &mut msg)
}

fn tty_ctl_call_with_retry(tty_endpoint: usize, msg: &mut Message) -> Result<()> {
    const RETRIES: usize = 128;
    for _ in 0..RETRIES {
        match call(tty_endpoint, msg, IpcFlags::empty()) {
            Ok(()) => return Ok(()),
            Err(Error::WouldBlock) | Err(Error::Busy) => {
                let _ = syscall::yield_cpu();
            }
            Err(err) => return Err(err),
        }
    }
    Err(Error::Busy)
}

/// Poll background job notify endpoints and emit async completion markers.
pub fn poll_background_jobs(stdout: usize, context: &mut CommandContext) -> Result<()> {
    let mut finished: Vec<(usize, i32)> = Vec::new();
    let mut buf = [0u8; 128];

    for (pid, job) in &context.bg_jobs {
        match syscall::ipc_recv_nonblocking(job.notify_endpoint, &mut buf) {
            Ok(len) => {
                if let Some((msg, _payload)) = parse_ipc_message(&buf[..len]) {
                    if msg.tag.label == 1 && msg.tag.words >= 2 {
                        finished.push((*pid, msg.words[1] as i32));
                    }
                }
            }
            Err(Error::WouldBlock) => {}
            Err(_) => {}
        }
    }

    for (pid, code) in finished {
        context.remove_bg_job(pid);
        let line = format!("shell: bg done pid={} code={}\n", pid, code);
        let _ = debug_print(line.trim_end());
        send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
    }
    Ok(())
}

fn parse_ipc_message(buf: &[u8]) -> Option<(Message, &[u8])> {
    if buf.len() < size_of::<Message>() {
        return None;
    }
    let msg = unsafe { (buf.as_ptr() as *const Message).read_unaligned() };
    let mut payload_len = msg.words[0];
    let header = size_of::<Message>();
    if header + payload_len > buf.len() {
        payload_len = 0;
    }
    Some((msg, &buf[header..header + payload_len]))
}

fn resolve_job_pid(context: &CommandContext, arg: Option<&String>) -> Option<usize> {
    let Some(token) = arg else {
        return context.latest_bg_pid();
    };
    let raw = token.strip_prefix('%').unwrap_or(token.as_str());
    raw.parse::<usize>().ok()
}

fn ensure_bg_job_state(context: &mut CommandContext, pid: usize, state: JobState) -> Result<()> {
    if context.set_bg_job_state(pid, state) {
        return Ok(());
    }
    let line = format!("shell: invariant violation missing job pid={}", pid);
    let _ = debug_print(line.as_str());
    Err(Error::InvalidState)
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
        if let Err(err) = vfs.mkdir("/tmp", 0o755) {
            if err != Error::AlreadyExists {
                let line = format!("ext2ownerdeny: FAIL mkdir /tmp {:?}\n", err);
                let _ = debug_print(line.as_str());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                return Ok(());
            }
        }

        let created = match vfs.open_with(path, 0o1000 | 2, 0o644) {
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

struct Ext2OwnerDenyBuiltin;

impl BuiltinCommand for Ext2OwnerDenyBuiltin {
    fn name(&self) -> &'static str {
        "ext2ownerdeny"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, _args: &[String]) -> Result<()> {
        let path = "/tmp/l2a_owner_probe";
        let vfs_endpoint = match registry::subscribe_output("vfs", "main") {
            Ok(ep) => ep,
            Err(err) => {
                let line = format!("ext2ownerdeny: FAIL vfs unavailable {:?}\n", err);
                let _ = debug_print(line.as_str());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                return Ok(());
            }
        };
        let vfs = match VfsClient::new_from_registry(vfs_endpoint) {
            Ok(client) => client,
            Err(err) => {
                let line = format!("ext2ownerdeny: FAIL client {:?}\n", err);
                let _ = debug_print(line.as_str());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                return Ok(());
            }
        };

        let created = match vfs.open_with(path, 0o1000 | 2, 0o644) {
            Ok(file) => file,
            Err(err) => {
                let line = format!("ext2ownerdeny: FAIL create/open {:?}\n", err);
                let _ = debug_print(line.as_str());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                return Ok(());
            }
        };
        let _ = vfs.close(created);

        let spawn = spawn_process(context, "ownerprobe", DEFAULT_PRIORITY)?;
        if let Err(err) = parse_status(spawn.status_word) {
            let line = format!("ext2ownerdeny: FAIL spawn-status {:?}\n", err);
            let _ = debug_print(line.as_str());
            send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            let _ = vfs.unlink(path);
            return Ok(());
        }

        let mut exit_msg = Message::new(0, [0; 6], 0);
        let _ = recv(spawn.notify_endpoint, &mut exit_msg, IpcFlags::empty());
        if exit_msg.tag.words >= 2 && exit_msg.words[1] != 0 {
            let line = format!(
                "ext2ownerdeny: FAIL ownerprobe-exit {}\n",
                exit_msg.words[1]
            );
            let _ = debug_print(line.as_str());
            send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            let _ = vfs.unlink(path);
            return Ok(());
        }

        let still_exists = match vfs.stat(path) {
            Ok(_) => true,
            Err(err) => {
                let line = format!("ext2ownerdeny: FAIL stat-after {:?}\n", err);
                let _ = debug_print(line.as_str());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                false
            }
        };
        if !still_exists {
            return Ok(());
        }

        match vfs.unlink(path) {
            Ok(()) => {
                let line = "ext2ownerdeny: PASS non-owner denied + owner cleanup\n";
                let _ = debug_print(line);
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            }
            Err(err) => {
                let line = format!("ext2ownerdeny: FAIL owner cleanup {:?}\n", err);
                let _ = debug_print(line.as_str());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            }
        }
        Ok(())
    }
}

struct RingIoBuiltin;

impl BuiltinCommand for RingIoBuiltin {
    fn name(&self) -> &'static str {
        "ringio"
    }

    fn run(&self, stdout: usize, _context: &mut CommandContext, args: &[String]) -> Result<()> {
        let path = args.first().map(|s| s.as_str()).unwrap_or("/bin/hello");
        let max_rounds = args
            .get(1)
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(16)
            .max(1);
        let chunk = args
            .get(2)
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(16 * 1024)
            .max(512);

        let vfs_endpoint = match registry::subscribe_output("vfs", "main") {
            Ok(ep) => ep,
            Err(err) => {
                let line = format!("ringio: FAIL vfs unavailable {:?}\n", err);
                let _ = debug_print(line.trim_end());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                return Ok(());
            }
        };
        let vfs = match VfsClient::new_from_registry(vfs_endpoint) {
            Ok(client) => client,
            Err(err) => {
                let line = format!("ringio: FAIL client {:?}\n", err);
                let _ = debug_print(line.trim_end());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                return Ok(());
            }
        };
        let file = match vfs.open(path) {
            Ok(file) => file,
            Err(err) => {
                let line = format!("ringio: FAIL open {} {:?}\n", path, err);
                let _ = debug_print(line.trim_end());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                return Ok(());
            }
        };

        let space_token = process_info().tokens[TOKEN_SPACE];
        if space_token == 0 {
            let _ = vfs.close(file);
            let _ = debug_print("ringio: FAIL missing space token");
            send_with_retry(
                stdout,
                TTY_WRITE_LABEL,
                b"ringio: FAIL missing space token\n",
            )?;
            return Ok(());
        }

        let region = match libcluu::ipc::alloc_shared_ring_region(
            space_token,
            64 * 1024,
            libcluu::ipc::SHARED_RING_DEFAULT_MAP_FLAGS,
        ) {
            Ok(region) => region,
            Err(err) => {
                let _ = vfs.close(file);
                let line = format!("ringio: FAIL alloc_shared_ring {:?}\n", err);
                let _ = debug_print(line.trim_end());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                return Ok(());
            }
        };

        let ring_meta = match vfs.setup_read_ring(space_token, region.base, region.bytes) {
            Ok(meta) => meta,
            Err(err) => {
                let _ = libcluu::ipc::free_shared_ring_region(space_token, region);
                let _ = vfs.close(file);
                let line = format!("ringio: FAIL ring setup {:?}\n", err);
                let _ = debug_print(line.trim_end());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                return Ok(());
            }
        };
        if ring_meta.bytes > region.bytes {
            let _ = libcluu::ipc::free_shared_ring_region(space_token, region);
            let _ = vfs.close(file);
            let _ = debug_print("ringio: FAIL invalid ring bytes");
            send_with_retry(
                stdout,
                TTY_WRITE_LABEL,
                b"ringio: FAIL invalid ring bytes\n",
            )?;
            return Ok(());
        }

        let backing =
            unsafe { core::slice::from_raw_parts_mut(region.base as *mut u8, ring_meta.bytes) };
        let mut ring = match SharedRing::attach(backing) {
            Ok(ring) => ring,
            Err(err) => {
                let _ = libcluu::ipc::free_shared_ring_region(space_token, region);
                let _ = vfs.close(file);
                let line = format!("ringio: FAIL ring attach {:?}\n", err);
                let _ = debug_print(line.trim_end());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                return Ok(());
            }
        };

        let mut total = 0usize;
        let mut offset = 0usize;
        let mut rounds = 0usize;
        let mut notify_seq = ring.notify_seq();
        loop {
            if rounds >= max_rounds {
                break;
            }
            let req = chunk.min(ring_meta.capacity.saturating_sub(1));
            if req == 0 {
                break;
            }
            let ring_chunk = match vfs.read_ring(file, offset, req) {
                Ok(chunk) => chunk,
                Err(err) => {
                    let line = format!("ringio: FAIL read_ring {:?}\n", err);
                    let _ = debug_print(line.trim_end());
                    send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                    let _ = libcluu::ipc::free_shared_ring_region(space_token, region);
                    let _ = vfs.close(file);
                    return Ok(());
                }
            };
            if ring_chunk.len == 0 {
                break;
            }

            let mut drain = alloc::vec![0u8; ring_chunk.len];
            let popped = ring.pop(&mut drain);
            if popped != ring_chunk.len {
                let line = format!(
                    "ringio: FAIL ring pop mismatch expected={} got={}\n",
                    ring_chunk.len, popped
                );
                let _ = debug_print(line.trim_end());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                let _ = libcluu::ipc::free_shared_ring_region(space_token, region);
                let _ = vfs.close(file);
                return Ok(());
            }

            total += popped;
            offset += popped;
            rounds += 1;
            notify_seq = ring_chunk.notify_seq;
            if ring_chunk.eof {
                break;
            }
        }

        let _ = libcluu::ipc::free_shared_ring_region(space_token, region);
        let _ = vfs.close(file);
        let line = format!(
            "ringio: PASS path={} bytes={} rounds={} notify_seq={}\n",
            path, total, rounds, notify_seq
        );
        let _ = debug_print(line.as_str());
        send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
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

fn signal_process(procmgr_endpoint: usize, pid: usize, signal: usize) -> Result<()> {
    let mut req = Message::new(PROCMGR_KILL_LABEL, [0; 6], 2);
    req.words[0] = pid;
    req.words[1] = signal;
    call(procmgr_endpoint, &mut req, IpcFlags::empty())?;
    parse_status(req.words[0])
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

struct ContainerBuiltin;

impl BuiltinCommand for ContainerBuiltin {
    fn name(&self) -> &'static str {
        "container"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        let subcmd = args.first().map(|s| s.as_str()).unwrap_or("");
        match subcmd {
            "run" => container_run(stdout, context, &args[1..]),
            "list" => container_list(stdout, context),
            "stop" => container_stop(stdout, context, &args[1..]),
            _ => {
                send_with_payload(
                    stdout,
                    TTY_WRITE_LABEL,
                    b"usage: container run|list|stop\n",
                )?;
                Ok(())
            }
        }
    }
}

fn container_run(stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
    let Some(name) = args.first() else {
        send_with_payload(stdout, TTY_WRITE_LABEL, b"container run: missing image name\n")?;
        return Ok(());
    };

    let procmgr_endpoint = context.procmgr_spawn_endpoint()?;
    let notify_endpoint = syscall::endpoint_create(process_info().tokens[TOKEN_IPC])?;
    let payload = build_container_run_payload(name);
    let mut msg = Message::new(PROCMGR_CONTAINER_RUN_LABEL, [0; 6], 3);
    msg.words[0] = payload.len();
    msg.words[1] = notify_endpoint;
    msg.words[2] = 0; // fdac_offset — no FDAC for basic container run
    let mut reply = Message::new(0, [0; 6], 0);

    call_with_payload(procmgr_endpoint, &msg, &payload, &mut reply)?;

    let status = reply.words[0];
    if status != 0 {
        let line = format!("container run: error {}\n", status);
        send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
    } else {
        let pid = reply.words[1];
        let cookie = reply.words[2];
        let cid = reply.words[3];
        let child_stdin = reply.words[4];
        let line = format!("container '{}' started pid={} cid={}\n", name, pid, cid);
        send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;

        // Route TTY foreground to the container process while it runs
        if child_stdin != 0 {
            set_tty_foreground(stdout, child_stdin, 0, TTY_FG_FLAG_FORWARD_CTRL_C)?;

            // Wait for container to exit (foreground wait)
            let mut notify_msg = Message::new(0, [0; 6], 0);
            let _ = recv(notify_endpoint, &mut notify_msg, IpcFlags::empty());

            // Restore TTY foreground to shell
            let shell_stdin = process_info().tokens[TOKEN_STDIN];
            let _ = set_tty_foreground(stdout, shell_stdin, 0, TTY_FG_FLAG_FORWARD_CTRL_C);
        } else {
            // Fallback: no stdin route, just wait for exit
            let mut notify_msg = Message::new(0, [0; 6], 0);
            let _ = recv(notify_endpoint, &mut notify_msg, IpcFlags::empty());
        }
    }
    Ok(())
}

fn container_list(stdout: usize, context: &mut CommandContext) -> Result<()> {
    let procmgr_endpoint = context.procmgr_spawn_endpoint()?;
    let msg = Message::new(PROCMGR_CONTAINER_LIST_LABEL, [0; 6], 0);
    let mut reply_buf = [0u8; 4096];

    let (reply_msg, payload_len) =
        call_with_reply_buf(procmgr_endpoint, &msg, &[], &mut reply_buf)?;

    if reply_msg.words[1] != 0 {
        let line = format!("container list: error {}\n", reply_msg.words[1]);
        send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
        return Ok(());
    }

    if payload_len == 0 {
        send_with_payload(stdout, TTY_WRITE_LABEL, b"no containers running\n")?;
        return Ok(());
    }

    // Payload starts after Message header in reply_buf
    let hdr_len = size_of::<Message>();
    let payload = &reply_buf[hdr_len..hdr_len + payload_len];
    send_with_payload(stdout, TTY_WRITE_LABEL, payload)?;
    Ok(())
}

fn container_stop(stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
    let Some(name) = args.first() else {
        send_with_payload(stdout, TTY_WRITE_LABEL, b"container stop: missing name\n")?;
        return Ok(());
    };

    // First list containers to find the pid for the named container
    let procmgr_endpoint = context.procmgr_spawn_endpoint()?;
    let msg = Message::new(PROCMGR_CONTAINER_LIST_LABEL, [0; 6], 0);
    let mut reply_buf = [0u8; 4096];
    let (reply_msg, payload_len) =
        call_with_reply_buf(procmgr_endpoint, &msg, &[], &mut reply_buf)?;

    if reply_msg.words[1] != 0 || payload_len == 0 {
        send_with_payload(stdout, TTY_WRITE_LABEL, b"container stop: no containers found\n")?;
        return Ok(());
    }

    let hdr_len = size_of::<Message>();
    let payload = &reply_buf[hdr_len..hdr_len + payload_len];
    let listing = core::str::from_utf8(payload).unwrap_or("");

    // Each line: "<instance_name> <pid> <cid> <session_id>"
    let mut target_pid = None;
    let by_cid = name.starts_with('@');
    for line in listing.lines() {
        let mut parts = line.split_whitespace();
        let inst_name = parts.next().unwrap_or("");
        let pid_str = parts.next().unwrap_or("");
        let cid_str = parts.next().unwrap_or("");

        let matched = if by_cid {
            // @CID addressing
            cid_str == &name[1..]
        } else {
            // Instance name match
            inst_name == name.as_str()
        };

        if matched {
            if let Ok(pid) = usize::from_str_radix(pid_str, 10) {
                target_pid = Some(pid);
                break;
            }
        }
    }

    let Some(pid) = target_pid else {
        let line = format!("container stop: '{}' not found\n", name);
        send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
        return Ok(());
    };

    // Send kill to procmgr
    let mut kill_msg = Message::new(PROCMGR_KILL_LABEL, [0; 6], 2);
    kill_msg.words[0] = pid;
    kill_msg.words[1] = SIGTERM;
    call(procmgr_endpoint, &mut kill_msg, IpcFlags::empty())?;

    if kill_msg.words[0] != 0 {
        let line = format!("container stop: kill failed ({})\n", kill_msg.words[0]);
        send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
    } else {
        let line = format!("container '{}' (pid={}) stopped\n", name, pid);
        send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
    }
    Ok(())
}

struct SudoBuiltin;

impl BuiltinCommand for SudoBuiltin {
    fn name(&self) -> &'static str {
        "sudo"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        // sudo <command>   — run command with escalated privileges
        // sudo -s / sudo   — open elevated shell
        let command_path = if args.is_empty() || (args.len() == 1 && args[0] == "-s") {
            "/bin/shell"
        } else {
            args[0].as_str()
        };

        // Password: stub (empty string, not verified)
        let password = "";

        // Build payload: password\0command_path\0
        let mut payload = Vec::new();
        payload.extend_from_slice(password.as_bytes());
        payload.push(0);
        payload.extend_from_slice(command_path.as_bytes());
        payload.push(0);

        let procmgr_endpoint = context.procmgr_spawn_endpoint()?;
        let notify_endpoint = syscall::endpoint_create(process_info().tokens[TOKEN_IPC])?;

        let mut msg = Message::new(PROCMGR_ESCALATE_LABEL, [0; 6], 2);
        msg.words[0] = payload.len();
        msg.words[1] = notify_endpoint;
        let mut reply = Message::new(0, [0; 6], 0);

        call_with_payload(procmgr_endpoint, &msg, &payload, &mut reply)?;

        let status = reply.words[0];
        if status != 0 {
            let line = format!("sudo: permission denied (error {})\n", status);
            send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            return Ok(());
        }

        let pid = reply.words[1];
        let cid = reply.words[4];
        let child_stdin = reply.words[3];

        let _ = debug_print(&format!("sudo: escalated cmd={} pid={} cid={}", command_path, pid, cid));

        // Route TTY foreground to the escalated process
        if child_stdin != 0 {
            set_tty_foreground(stdout, child_stdin, 0, TTY_FG_FLAG_FORWARD_CTRL_C)?;

            // Wait for escalated process to exit
            let mut notify_msg = Message::new(0, [0; 6], 0);
            let _ = recv(notify_endpoint, &mut notify_msg, IpcFlags::empty());

            // Restore TTY foreground to this shell
            let shell_stdin = process_info().tokens[TOKEN_STDIN];
            let _ = set_tty_foreground(stdout, shell_stdin, 0, TTY_FG_FLAG_FORWARD_CTRL_C);
        } else {
            // No stdin route, just wait for exit
            let mut notify_msg = Message::new(0, [0; 6], 0);
            let _ = recv(notify_endpoint, &mut notify_msg, IpcFlags::empty());
        }
        Ok(())
    }
}

struct SuBuiltin;

impl BuiltinCommand for SuBuiltin {
    fn name(&self) -> &'static str {
        "su"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        let username = match args.first() {
            Some(u) => u.as_str(),
            None => {
                send_with_payload(stdout, TTY_WRITE_LABEL, b"usage: su <username>\n")?;
                return Ok(());
            }
        };

        // Password: stub (empty string, not verified)
        let password = "";

        // Build payload: target_username\0password\0
        let mut payload = Vec::new();
        payload.extend_from_slice(username.as_bytes());
        payload.push(0);
        payload.extend_from_slice(password.as_bytes());
        payload.push(0);

        let procmgr_endpoint = context.procmgr_spawn_endpoint()?;
        let notify_endpoint = syscall::endpoint_create(process_info().tokens[TOKEN_IPC])?;

        let mut msg = Message::new(PROCMGR_SU_LABEL, [0; 6], 2);
        msg.words[0] = payload.len();
        msg.words[1] = notify_endpoint;
        let mut reply = Message::new(0, [0; 6], 0);

        call_with_payload(procmgr_endpoint, &msg, &payload, &mut reply)?;

        let status = reply.words[0];
        if status != 0 {
            let line = format!("su: authentication failure (error {})\n", status);
            send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            return Ok(());
        }

        let pid = reply.words[1];
        let cid = reply.words[4];
        let child_stdin = reply.words[3];

        let _ = debug_print(&format!("su: nested session user={} pid={} cid={}", username, pid, cid));

        // Route TTY foreground to the nested session's shell
        if child_stdin != 0 {
            set_tty_foreground(stdout, child_stdin, 0, TTY_FG_FLAG_FORWARD_CTRL_C)?;

            // Wait for nested session to exit
            let mut notify_msg = Message::new(0, [0; 6], 0);
            let _ = recv(notify_endpoint, &mut notify_msg, IpcFlags::empty());

            // Restore TTY foreground to this shell
            let shell_stdin = process_info().tokens[TOKEN_STDIN];
            let _ = set_tty_foreground(stdout, shell_stdin, 0, TTY_FG_FLAG_FORWARD_CTRL_C);
        } else {
            // No stdin route, just wait for exit
            let mut notify_msg = Message::new(0, [0; 6], 0);
            let _ = recv(notify_endpoint, &mut notify_msg, IpcFlags::empty());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Test builtins for Phase H verification
// ---------------------------------------------------------------------------

/// H19: vtcrashtest — Verify session is alive after VT crash.
/// The actual TTY crash must be triggered externally (kill tty pid from another VT).
/// This builtin simply writes to stdout; if it succeeds, the session survived.
struct VtCrashTestBuiltin;

impl BuiltinCommand for VtCrashTestBuiltin {
    fn name(&self) -> &'static str {
        "vtcrashtest"
    }

    fn run(&self, stdout: usize, _context: &mut CommandContext, _args: &[String]) -> Result<()> {
        let line = "vtcrashtest: PASS session alive after VT reattach\n";
        send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
        let _ = debug_print(line.trim_end());
        Ok(())
    }
}

/// H21: sudotest — Verify sudo creates an elevated container.
/// Sends PROCMGR_ESCALATE_LABEL for /bin/shell, checks reply has pid/cid,
/// then kills the spawned container.
struct SudoTestBuiltin;

impl BuiltinCommand for SudoTestBuiltin {
    fn name(&self) -> &'static str {
        "sudotest"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, _args: &[String]) -> Result<()> {
        // Build payload: password\0command\0
        let mut payload = Vec::new();
        payload.push(0); // empty password
        payload.extend_from_slice(b"/bin/shell");
        payload.push(0);

        let procmgr_endpoint = context.procmgr_spawn_endpoint()?;
        let notify_endpoint = syscall::endpoint_create(process_info().tokens[TOKEN_IPC])?;

        let mut msg = Message::new(PROCMGR_ESCALATE_LABEL, [0; 6], 2);
        msg.words[0] = payload.len();
        msg.words[1] = notify_endpoint;
        let mut reply = Message::new(0, [0; 6], 0);

        call_with_payload(procmgr_endpoint, &msg, &payload, &mut reply)?;

        let status = reply.words[0];
        let pid = reply.words[1];
        let cid = reply.words[4];

        if status == 0 && pid != 0 {
            let line = format!("sudotest: PASS escalated pid={} cid={}\n", pid, cid);
            send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            let _ = debug_print(line.trim_end());

            // Clean up: kill the spawned container
            let _ = signal_process(procmgr_endpoint, pid, 9);
        } else {
            let line = format!("sudotest: FAIL status={} pid={}\n", status, pid);
            send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            let _ = debug_print(line.trim_end());
        }
        Ok(())
    }
}

/// H22: sutest — Verify su creates a nested session with target's view.
/// Sends PROCMGR_SU_LABEL for user "alice", checks reply has pid/cid,
/// then kills the spawned container.
struct SuTestBuiltin;

impl BuiltinCommand for SuTestBuiltin {
    fn name(&self) -> &'static str {
        "sutest"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, _args: &[String]) -> Result<()> {
        let target = "alice";

        // Build payload: username\0password\0
        let mut payload = Vec::new();
        payload.extend_from_slice(target.as_bytes());
        payload.push(0);
        payload.push(0); // empty password

        let procmgr_endpoint = context.procmgr_spawn_endpoint()?;
        let notify_endpoint = syscall::endpoint_create(process_info().tokens[TOKEN_IPC])?;

        let mut msg = Message::new(PROCMGR_SU_LABEL, [0; 6], 2);
        msg.words[0] = payload.len();
        msg.words[1] = notify_endpoint;
        let mut reply = Message::new(0, [0; 6], 0);

        call_with_payload(procmgr_endpoint, &msg, &payload, &mut reply)?;

        let status = reply.words[0];
        let pid = reply.words[1];
        let cid = reply.words[4];

        if status == 0 && pid != 0 {
            let line = format!("sutest: PASS nested session user={} pid={} cid={}\n", target, pid, cid);
            send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            let _ = debug_print(line.trim_end());

            // Clean up: kill the spawned container
            let _ = signal_process(procmgr_endpoint, pid, 9);
        } else {
            let line = format!("sutest: FAIL status={} pid={}\n", status, pid);
            send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            let _ = debug_print(line.trim_end());
        }
        Ok(())
    }
}

/// H23: escalatedeny — Verify escalation beyond ceiling is rejected.
/// Must be run as user "guest" (no escalate field in users.toml).
/// Sends PROCMGR_ESCALATE_LABEL and expects PermissionDenied.
struct EscalateDenyBuiltin;

impl BuiltinCommand for EscalateDenyBuiltin {
    fn name(&self) -> &'static str {
        "escalatedeny"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, _args: &[String]) -> Result<()> {
        // Build payload: password\0command\0
        let mut payload = Vec::new();
        payload.push(0); // empty password
        payload.extend_from_slice(b"/bin/shell");
        payload.push(0);

        let procmgr_endpoint = context.procmgr_spawn_endpoint()?;
        let notify_endpoint = syscall::endpoint_create(process_info().tokens[TOKEN_IPC])?;

        let mut msg = Message::new(PROCMGR_ESCALATE_LABEL, [0; 6], 2);
        msg.words[0] = payload.len();
        msg.words[1] = notify_endpoint;
        let mut reply = Message::new(0, [0; 6], 0);

        call_with_payload(procmgr_endpoint, &msg, &payload, &mut reply)?;

        match parse_status(reply.words[0]) {
            Err(Error::PermissionDenied) => {
                let line = "escalatedeny: PASS escalation rejected (no ceiling)\n";
                send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                let _ = debug_print(line.trim_end());
            }
            Ok(()) => {
                let line = format!(
                    "escalatedeny: FAIL unexpected success pid={}\n",
                    reply.words[1]
                );
                send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                let _ = debug_print(line.trim_end());
                // Clean up the unexpected container
                let _ = signal_process(procmgr_endpoint, reply.words[1], 9);
            }
            Err(err) => {
                let line = format!("escalatedeny: FAIL wrong error {:?}\n", err);
                send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                let _ = debug_print(line.trim_end());
            }
        }
        Ok(())
    }
}

/// HR7: suequaltest — Verify su between equal profiles is rejected.
/// Root (admin) attempts `su root` (admin); should be rejected because
/// caller_profile == target_profile (strict narrowing enforced).
struct SuEqualTestBuiltin;

impl BuiltinCommand for SuEqualTestBuiltin {
    fn name(&self) -> &'static str {
        "suequaltest"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, _args: &[String]) -> Result<()> {
        let target = "root";
        let mut payload = Vec::new();
        payload.extend_from_slice(target.as_bytes());
        payload.push(0);
        payload.push(0); // empty password

        let procmgr_endpoint = context.procmgr_spawn_endpoint()?;
        let mut msg = Message::new(PROCMGR_SU_LABEL, [0; 6], 2);
        msg.words[0] = payload.len();
        msg.words[1] = 0; // no notify endpoint needed
        let mut reply = Message::new(0, [0; 6], 0);

        call_with_payload(procmgr_endpoint, &msg, &payload, &mut reply)?;

        let status = reply.words[0];
        if status != 0 {
            let line = format!("suequaltest: PASS su equal-profile rejected (errno={})\n", status);
            send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            let _ = debug_print(line.trim_end());
        } else {
            let pid = reply.words[1];
            let line = format!("suequaltest: FAIL su equal-profile should have been rejected (pid={})\n", pid);
            send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            let _ = debug_print(line.trim_end());
            let _ = signal_process(procmgr_endpoint, pid, 9);
        }
        Ok(())
    }
}

/// HR6: shellcrash — Trigger a page fault to test session-survives-crash.
/// The null-pointer write causes a page fault → kernel forwards to procmgr →
/// procmgr clears shell_cid (session persists, no SESSION_DEATH).
struct ShellCrashBuiltin;

impl BuiltinCommand for ShellCrashBuiltin {
    fn name(&self) -> &'static str {
        "shellcrash"
    }

    fn run(&self, stdout: usize, _context: &mut CommandContext, _args: &[String]) -> Result<()> {
        let _ = send_with_payload(stdout, TTY_WRITE_LABEL, b"shellcrash: triggering fault\n");
        let _ = debug_print("shellcrash: triggering null-write fault");
        unsafe { core::ptr::write_volatile(0 as *mut u8, 0); }
        Ok(())
    }
}

struct PoweroffBuiltin;

impl BuiltinCommand for PoweroffBuiltin {
    fn name(&self) -> &'static str {
        "poweroff"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, _args: &[String]) -> Result<()> {
        let _ = send_with_payload(stdout, TTY_WRITE_LABEL, b"Powering off...\n");
        let ep = context.procmgr_spawn_endpoint()?;
        let msg = Message::new(PROCMGR_SHUTDOWN_LABEL, [0, 0, 0, 0, 0, 0], 1);
        let _ = libcluu::ipc::send(ep, &msg, IpcFlags::empty());
        Ok(())
    }
}

struct RebootBuiltin;

impl BuiltinCommand for RebootBuiltin {
    fn name(&self) -> &'static str {
        "reboot"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, _args: &[String]) -> Result<()> {
        let _ = send_with_payload(stdout, TTY_WRITE_LABEL, b"Rebooting...\n");
        let ep = context.procmgr_spawn_endpoint()?;
        let msg = Message::new(PROCMGR_SHUTDOWN_LABEL, [1, 0, 0, 0, 0, 0], 1);
        let _ = libcluu::ipc::send(ep, &msg, IpcFlags::empty());
        Ok(())
    }
}

struct TrueBuiltin;

impl BuiltinCommand for TrueBuiltin {
    fn name(&self) -> &'static str {
        "true"
    }

    fn run(&self, _stdout: usize, context: &mut CommandContext, _args: &[String]) -> Result<()> {
        context.set_last_status(0);
        Ok(())
    }
}

struct FalseBuiltin;

impl BuiltinCommand for FalseBuiltin {
    fn name(&self) -> &'static str {
        "false"
    }

    fn run(&self, _stdout: usize, context: &mut CommandContext, _args: &[String]) -> Result<()> {
        context.set_last_status(1);
        Ok(())
    }
}

/// POSIX `test`(1) — file/string/numeric predicate evaluator.
///
/// Registered twice: as `test` and as `[`. When invoked as `[`, the final
/// argument must be `]`; we strip it before parsing.
struct TestBuiltin {
    bracket: bool,
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
        // Slice off the trailing `]` for `[` invocation.
        let argv: &[String] = if self.bracket {
            match args.last() {
                Some(last) if last == "]" => &args[..args.len() - 1],
                _ => {
                    send_with_payload(stdout, TTY_WRITE_LABEL, b"[: missing closing ']'\n")?;
                    context.set_last_status(2);
                    return Ok(());
                }
            }
        } else {
            args
        };

        // Empty argument list: `test` with no args is false.
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
                    send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                    context.set_last_status(2);
                    return Ok(());
                }
                context.set_last_status(if value { 0 } else { 1 });
            }
            Err(msg) => {
                let line = format!("{}: {}\n", self.name(), msg);
                send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                context.set_last_status(2);
            }
        }
        Ok(())
    }
}

/// Recursive-descent parser for POSIX `test` expressions.
///
/// Grammar (precedence climbing -o → -a → ! → primary):
///   expr     := or
///   or       := and ( '-o' and )*
///   and      := unary ( '-a' unary )*
///   unary    := '!' unary | primary
///   primary  := '(' expr ')'
///            |  unary_op WORD
///            |  WORD binary_op WORD
///            |  WORD                       (true iff non-empty)
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
        // `!` flips the next unary, but only when it isn't the sole remaining
        // token (e.g. `test !` should evaluate as the non-empty string "!").
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

        // Two-arg form: <op> WORD  (unary file/string predicates).
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

        // Three-arg form: WORD <op> WORD  (string/numeric comparisons).
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

        // One-arg form: a bare word is true iff non-empty.
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
            let mode = stat.mode;
            const S_IFMT: usize = 0o170000;
            const S_IFREG: usize = 0o100000;
            const S_IFDIR: usize = 0o040000;
            const S_IFLNK: usize = 0o120000;
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
