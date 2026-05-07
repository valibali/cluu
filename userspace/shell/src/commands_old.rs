//! Builtin command handling for the shell.
//!
//! This module keeps the command execution logic separate from the IO loop and
//! parser wiring, following SOLID separation between parsing, dispatch, and IO.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::mem::size_of;
use libcluu::fs::client::VfsClient;
use libcluu::ipc::{
    call, call_with_payload, send_with_payload,
    build_container_run_payload_full, RedirAction,
    PROCMGR_CONTAINER_RUN_LABEL,
    TTY_FG_FLAG_FORWARD_CTRL_C, TTY_FG_FLAG_NOTIFY_CTRL_C, TTY_READ_LABEL,
    TTY_REGISTER_LABEL, TTY_WRITE_LABEL,
};
use libcluu::posix::tty::{
    get_lflag as tty_get_lflag, set_lflag as tty_set_lflag,
    TTY_LFLAG_ECHO, TTY_LFLAG_ICANON,
};
use libcluu::registry;
use libcluu::syscall;
use libcluu::types::Message;
use libcluu::{debug_print, process_info, Error, IpcFlags, Result, TOKEN_IPC};

use cluu_lang::ast::{Assign, CmdElem, DqPart, Program, Redir, RedirOp, Stmt, Word, WordPart};

const PROCMGR_KILL_LABEL: u32 = 3;
const SIGINT: usize = 2;
const DEFAULT_PRIORITY: usize = 200;
const TTY_LFLAG_DEFAULT: usize = TTY_LFLAG_ICANON | TTY_LFLAG_ECHO;

/// Execution result for a command handler.
pub enum ExecResult {
    Handled,
    NotHandled,
}

/// Per-shell execution context shared across command invocations.
pub struct CommandContext {
    vars: BTreeMap<String, String>,
    /// Names of vars marked for propagation to spawned children (bash semantics).
    /// `set X=v` puts X into `vars` only; `export X` (or `export X=v`) adds X here.
    /// `unset X` removes X from both.
    exported: BTreeSet<String>,
    procmgr_spawn: usize,
    console_write: usize,
    bg_jobs: BTreeMap<usize, BackgroundJob>,
    /// Exit status of the most recently executed builtin/command.
    /// Read by `echo $?` (Shell-B). `cd`/`pwd` write here.
    last_status: i32,
}

pub(crate) struct BackgroundJob {
    pub(crate) notify_endpoint: usize,
    pub(crate) stdin_endpoint: usize,
    pub(crate) command: String,
    pub(crate) state: JobState,
    pub(crate) fg_mode: ForegroundMode,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum JobState {
    Running,
    Stopped,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ForegroundMode {
    SignalOnCtrlC,
    PassCtrlCToChild,
}

impl CommandContext {
    /// Create a fresh shell context.
    pub fn new() -> Self {
        Self {
            vars: BTreeMap::new(),
            exported: BTreeSet::new(),
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
    #[allow(dead_code)]
    pub fn last_status(&self) -> i32 {
        self.last_status
    }

    /// Set or update a variable in the shell context.
    pub fn set(&mut self, name: &str, value: String) {
        self.vars.insert(name.to_string(), value);
    }

    /// Remove a variable from the shell context.
    ///
    /// Drops the var from both the local set and the exported set, so that
    /// `unset NAME` purges it from `env` output and from the spawn ENV trailer.
    pub fn unset(&mut self, name: &str) {
        self.vars.remove(name);
        self.exported.remove(name);
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

    /// Mark `name` as exported. The next `spawn` will include this var in the
    /// child's environment (provided it has a value in `vars`).
    pub fn export_var(&mut self, name: &str) {
        self.exported.insert(name.to_string());
    }

    /// Test whether `name` is currently exported.
    #[allow(dead_code)]
    pub fn is_exported(&self, name: &str) -> bool {
        self.exported.contains(name)
    }

    /// Snapshot of (key, value) pairs for every exported var that also has a
    /// value in `vars`. Used by `spawn_process_with_argv_and_redirs` to overlay
    /// shell-local exports on top of the inherited (envelope-resolved) env.
    pub fn exported_pairs(&self) -> Vec<(String, String)> {
        self.exported
            .iter()
            .filter_map(|k| self.vars.get(k).map(|v| (k.clone(), v.clone())))
            .collect()
    }

    pub fn set_procmgr_spawn(&mut self, ep: usize) {
        self.procmgr_spawn = ep;
    }

    pub fn procmgr_spawn_endpoint(&mut self) -> Result<usize> {
        if self.procmgr_spawn == 0 {
            self.procmgr_spawn = registry::subscribe_output("procmgr", "spawn")?;
        }
        Ok(self.procmgr_spawn)
    }

    pub(crate) fn console_write_endpoint(&mut self) -> Result<usize> {
        if self.console_write == 0 {
            self.console_write = registry::subscribe_output("console:0", "write")?;
        }
        Ok(self.console_write)
    }

    pub(crate) fn add_bg_job(
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

    pub(crate) fn remove_bg_job(&mut self, pid: usize) {
        self.bg_jobs.remove(&pid);
    }

    pub(crate) fn take_bg_job(&mut self, pid: usize) -> Option<BackgroundJob> {
        self.bg_jobs.remove(&pid)
    }

    pub(crate) fn bg_job_state(&self, pid: usize) -> Option<JobState> {
        self.bg_jobs.get(&pid).map(|job| job.state)
    }

    pub(crate) fn set_bg_job_state(&mut self, pid: usize, state: JobState) -> bool {
        if let Some(job) = self.bg_jobs.get_mut(&pid) {
            job.state = state;
            true
        } else {
            false
        }
    }

    pub(crate) fn latest_bg_pid(&self) -> Option<usize> {
        self.bg_jobs.keys().next_back().copied()
    }

    pub(crate) fn bg_job_lines(&self) -> Vec<String> {
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
        crate::commands::builtins::register_all(registry);
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
            // Multi-command pipelines (`a | b | c`) and single-command pipelines
            // that carry file redirections are dispatched to the PipelineExecutor.
            // Plain single-command pipelines fall through to the builtin-lookup path.
            let Stmt::Pipeline(pipeline) = stmt;
            let has_redirs = pipeline.commands.iter().any(|c| !c.redirs.is_empty());
            if pipeline.commands.len() >= 2 || has_redirs {
                match crate::pipeline::PipelineExecutor::run(stdout, context, pipeline) {
                    Ok(status) => {
                        context.set_last_status(status);
                    }
                    Err(_e) => {
                        context.set_last_status(1);
                        all_handled = false;
                    }
                }
                continue;
            }

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
            // UE17: if the first word didn't match a builtin and isn't a
            // path-like literal (no `/`), fall through to PATH-based
            // resolution. PATH lookup checks /var/images/<name>/manifest.toml
            // for an installed container image; on hit, dispatch the binary
            // through the same code SpawnBuiltin uses (`spawn <name> args…`).
            // On miss, leave `all_handled` false so the caller emits the
            // "shell: unsupported command" diagnostic.
            let result = if let ExecResult::NotHandled = result {
                if name.as_str() != "repeat" && !name.contains('/') {
                    try_path_dispatch(stdout, context, name, &args[1..])?
                } else {
                    ExecResult::NotHandled
                }
            } else {
                result
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

/// Public wrapper around `render_word` for use by `pipeline.rs`.
pub fn render_word_public(context: &CommandContext, word: &Word) -> String {
    render_word(context, word)
}

/// Convert AST `Redir` entries into `RedirAction` values for the REDIR trailer.
/// Callers should first check that there are no conflicts with pipe-wired fds.
pub fn build_redir_actions(context: &CommandContext, redirs: &[Redir]) -> Vec<RedirAction> {
    let mut actions = Vec::with_capacity(redirs.len());
    for r in redirs {
        let target = render_word(context, &r.target);
        let (target_fd, flags) = match r.op {
            RedirOp::OutTrunc => (1u8, 1u8),
            RedirOp::OutAppend => (1u8, 2u8),
            RedirOp::In => (0u8, 3u8),
            RedirOp::ErrTrunc => (2u8, 1u8),
        };
        actions.push(RedirAction { target_fd, flags, path: target });
    }
    actions
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

pub(crate) fn infer_foreground_mode(path: &str) -> ForegroundMode {
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

pub(crate) fn parse_spawn_args(args: &[String]) -> Option<(String, usize, ForegroundMode, Vec<String>)> {
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

pub struct SpawnResult {
    pub procmgr_endpoint: usize,
    pub notify_endpoint: usize,
    pub status_word: usize,
    pub pid: usize,
    pub stdin_endpoint: usize,
}

/// UE17: bare-command PATH resolution + dispatch.
///
/// Called from `BuiltinRegistry::execute` when the first word didn't
/// match any builtin and isn't a literal path (no `/` in `name`). On
/// hit (i.e. `/var/images/<name>/manifest.toml` exists), dispatch the
/// binary through the same code SpawnBuiltin uses and wait for exit.
/// On miss, return NotHandled so the caller emits "unsupported command".
fn try_path_dispatch(
    stdout: usize,
    context: &mut CommandContext,
    name: &str,
    args: &[String],
) -> Result<ExecResult> {
    let path_env = read_path_env();
    let vfs_endpoint = match registry::subscribe_output("vfs", "main") {
        Ok(ep) => ep,
        Err(_) => return Ok(ExecResult::NotHandled),
    };
    let vfs = match VfsClient::new_from_registry(vfs_endpoint) {
        Ok(v) => v,
        Err(_) => return Ok(ExecResult::NotHandled),
    };
    let Some(resolved_name) = crate::path_lookup::resolve(name, &path_env, &vfs) else {
        return Ok(ExecResult::NotHandled);
    };
    let _ = debug_print(&format!(
        "shell: PATH resolved '{}' -> /var/images/{}",
        name, resolved_name
    ));
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let fg_mode = infer_foreground_mode(resolved_name.as_str());
    let status = spawn_and_wait(
        stdout,
        context,
        resolved_name.as_str(),
        DEFAULT_PRIORITY,
        &arg_refs,
        fg_mode,
    )?;
    context.set_last_status(status);
    Ok(ExecResult::Handled)
}

/// Read $PATH from the process env (envelope-resolved at session-login,
/// optionally overridden by `export PATH=...`). Falls back to a paranoid
/// `/bin:/usr/bin` default if PATH is unset or empty so PATH lookup at
/// least works for the standard installed images.
fn read_path_env() -> String {
    for (k, v) in libcluu::posix::snapshot_env() {
        if k == "PATH" {
            return v;
        }
    }
    String::from("/bin:/usr/bin")
}

/// Spawn `name` with `args`, wait for exit, return the exit code (or
/// `1` on internal error). Shared between `SpawnBuiltin::run` and
/// UE17's PATH-dispatch fall-through.
fn spawn_and_wait(
    stdout: usize,
    context: &mut CommandContext,
    name: &str,
    priority: usize,
    args: &[&str],
    fg_mode: ForegroundMode,
) -> Result<i32> {
    let spawn = spawn_process_with_argv(context, name, priority, args)?;
    match parse_status(spawn.status_word) {
        Ok(()) => {
            wait_for_exit_or_sigint(
                spawn.procmgr_endpoint,
                stdout,
                spawn.notify_endpoint,
                spawn.stdin_endpoint,
                spawn.pid,
                stdout,
                fg_mode,
            )?;
            // wait_for_exit_or_sigint doesn't surface the child's exit
            // code; we treat reaching here as success (status=0). Tighter
            // exit-code threading is a follow-up if `$?` plumbing
            // requires it.
            Ok(0)
        }
        Err(err) => {
            let line = format!("spawn: {:?}\n", err);
            let _ = send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes());
            Ok(1)
        }
    }
}

/// Build a `PROCMGR_CONTAINER_RUN_LABEL` payload of `name + CWD trailer`.
///
/// Procmgr reads the container image name from the start of the payload and
/// strips the CWD trailer (last 8 bytes + cwd_len) before slicing argv/FDAC,
/// so prepending the name and appending the trailer is safe even when the
pub(crate) fn spawn_process(context: &mut CommandContext, name: &str, priority: usize) -> Result<SpawnResult> {
    spawn_process_with_argv(context, name, priority, &[])
}

pub(crate) fn spawn_process_with_argv(
    context: &mut CommandContext,
    name: &str,
    _priority: usize,
    args: &[&str],
) -> Result<SpawnResult> {
    spawn_process_with_argv_and_redirs(context, name, _priority, args, &[])
}

pub fn spawn_process_with_argv_and_redirs(
    context: &mut CommandContext,
    name: &str,
    _priority: usize,
    args: &[&str],
    redirs: &[RedirAction],
) -> Result<SpawnResult> {
    let procmgr_endpoint = context.procmgr_spawn_endpoint()?;

    // Build the child's env: start from the shell's own (envelope-resolved at
    // session-login) env, then overlay any vars marked `export` in this
    // shell. Bash semantics: shell-local `set X=v` does NOT propagate; only
    // `export X` (or a var that was already inherited as exported) does.
    let mut env_pairs: Vec<(String, String)> = libcluu::posix::snapshot_env();
    for (k, v) in context.exported_pairs() {
        if let Some(idx) = env_pairs.iter().position(|(ek, _)| ek == &k) {
            env_pairs[idx].1 = v;
        } else {
            env_pairs.push((k, v));
        }
    }
    let env_refs: Vec<(&str, &str)> = env_pairs
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let (payload, _argc, fdac_offset) =
        build_container_run_payload_full(name, args, &[], redirs, &env_refs);
    let notify_endpoint = syscall::endpoint_create(process_info().tokens[TOKEN_IPC])?;
    let mut msg = Message::new(PROCMGR_CONTAINER_RUN_LABEL, [0; 6], 3);
    msg.words[0] = payload.len();
    msg.words[1] = notify_endpoint;
    msg.words[2] = fdac_offset;
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

pub(crate) fn wait_for_exit_or_sigint(
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

pub(crate) fn set_tty_foreground(
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

pub(crate) fn parse_ipc_message(buf: &[u8]) -> Option<(Message, &[u8])> {
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

pub(crate) fn resolve_job_pid(context: &CommandContext, arg: Option<&String>) -> Option<usize> {
    let Some(token) = arg else {
        return context.latest_bg_pid();
    };
    let raw = token.strip_prefix('%').unwrap_or(token.as_str());
    raw.parse::<usize>().ok()
}

pub(crate) fn ensure_bg_job_state(context: &mut CommandContext, pid: usize, state: JobState) -> Result<()> {
    if context.set_bg_job_state(pid, state) {
        return Ok(());
    }
    let line = format!("shell: invariant violation missing job pid={}", pid);
    let _ = debug_print(line.as_str());
    Err(Error::InvalidState)
}

pub(crate) fn parse_status(raw: usize) -> Result<()> {
    let signed = raw as isize;
    if signed < 0 {
        return Err(Error::from_errno(signed));
    }
    Ok(())
}

pub(crate) fn signal_process(procmgr_endpoint: usize, pid: usize, signal: usize) -> Result<()> {
    let mut req = Message::new(PROCMGR_KILL_LABEL, [0; 6], 2);
    req.words[0] = pid;
    req.words[1] = signal;
    call(procmgr_endpoint, &mut req, IpcFlags::empty())?;
    parse_status(req.words[0])
}

/// Parse a numeric token from args or look it up as a variable name.
fn parse_value(context: &CommandContext, token: &str) -> Option<i64> {
    if let Ok(value) = token.parse::<i64>() {
        return Some(value);
    }
    context.get(token).and_then(|val| val.parse::<i64>().ok())
}
