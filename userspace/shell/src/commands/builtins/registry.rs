//! `BuiltinCommand` trait + `BuiltinRegistry` + `BuiltinProvider` trait,
//! `CommandContext`, `ExecResult`, and the `CommandExecutor` trait.
//!
//! Moved from the old monolithic `commands.rs` / `commands_old.rs`.
//! All builtin sub-modules reference this file for the shared traits.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use libcluu::ipc::{send_with_payload, PIPE_DATA_LABEL, PIPE_EOF_LABEL, TTY_WRITE_LABEL};
use libcluu::registry;
use libcluu::Result;

use cluu_lang::ast::{Assign, CmdElem, Program, Stmt, Word};

// ─── Enums & structs ─────────────────────────────────────────────────────────

/// Execution result for a command handler.
pub enum ExecResult {
    Handled,
    NotHandled,
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

pub(crate) struct BackgroundJob {
    pub(crate) notify_endpoint: usize,
    pub(crate) stdin_endpoint: usize,
    pub(crate) command: String,
    pub(crate) state: JobState,
    pub(crate) fg_mode: ForegroundMode,
}

// ─── CommandContext ───────────────────────────────────────────────────────────

// ─── HistoryBuf ───────────────────────────────────────────────────────────────

const HISTORY_CAP: usize = 1000;

/// Ring buffer for shell command history.
pub struct HistoryBuf {
    entries: VecDeque<String>,
}

impl HistoryBuf {
    pub fn new() -> Self {
        Self { entries: VecDeque::new() }
    }

    pub fn push(&mut self, line: String) {
        if line.trim().is_empty() {
            return;
        }
        if self.entries.back().map(|l| l == &line).unwrap_or(false) {
            return;
        }
        if self.entries.len() >= HISTORY_CAP {
            self.entries.pop_front();
        }
        self.entries.push_back(line);
    }

    pub fn iter(&self) -> impl Iterator<Item = &String> {
        self.entries.iter()
    }

    pub fn replace_all(&mut self, lines: Vec<String>) {
        self.entries.clear();
        for l in lines {
            if !l.trim().is_empty() {
                self.entries.push_back(l);
            }
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

// ─── CommandContext ───────────────────────────────────────────────────────────

/// Per-shell execution context shared across command invocations.
pub struct CommandContext {
    pub(crate) vars: BTreeMap<String, String>,
    /// Names of vars marked for propagation to spawned children (bash semantics).
    /// `set X=v` puts X into `vars` only; `export X` (or `export X=v`) adds X here.
    /// `unset X` removes X from both.
    pub(crate) exported: BTreeSet<String>,
    pub(crate) procmgr_spawn: usize,
    pub(crate) console_write: usize,
    pub(crate) bg_jobs: BTreeMap<usize, BackgroundJob>,
    /// Exit status of the most recently executed builtin/command.
    /// Read by `echo $?` (Shell-B). `cd`/`pwd` write here.
    pub(crate) last_status: i32,
    // ── Job control (Phase 4 Plan D Stage 3) ─────────────────────────────────
    /// Real job table replacing the primitive bg_jobs map.
    pub jobs: crate::commands::builtins::jobs::JobTable,
    /// Shell's own process-group id (created at startup).
    pub shell_pgid: usize,
    /// Session id for TTY fg-pgid calls.
    pub session_id: usize,
    /// TTY endpoint (same as stdout token — used for tty_set_fg).
    pub tty_stdout: usize,
    // ── Plan F additions ─────────────────────────────────────────────────────
    /// If set, the REPL will call _exit(code) after the current command.
    pub exit_requested: Option<i32>,
    /// Shell alias table. Keys are alias names, values are expansion strings.
    pub aliases: BTreeMap<String, String>,
    /// Command history ring buffer.
    pub history: HistoryBuf,
    /// Number of commands executed since last history save.
    pub(crate) cmd_count: usize,
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
            jobs: crate::commands::builtins::jobs::JobTable::new(),
            shell_pgid: 0,
            session_id: 0,
            tty_stdout: 0,
            exit_requested: None,
            aliases: BTreeMap::new(),
            history: HistoryBuf::new(),
            cmd_count: 0,
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

// ─── WriteSink ────────────────────────────────────────────────────────────────

/// Describes where a builtin should send its output.
///
/// The shell builds one of these per stage of a pipeline — Tty for the last
/// stage (or no pipe at all), Pipe for any stage feeding another command,
/// Capture for builtin + file-redir (output buffered, caller flushes to VFS).
#[derive(Clone, Copy)]
pub enum WriteSink {
    Tty(usize),
    Pipe(usize),
    /// File-redir for builtins. Holds a raw pointer to a `Vec<u8>` owned by
    /// the caller's stack frame; bytes are appended on `write_all`. The
    /// caller flushes to VFS after the builtin returns. Lifetime safety
    /// rests on the caller keeping the Vec alive across the builtin call.
    Capture(*mut alloc::vec::Vec<u8>),
    /// Reserved — direct file token. Unused for now; Capture is the path.
    File(usize),
}

// SAFETY: the raw pointer is only ever dereferenced from the same thread
// that owns the Vec. WriteSink isn't sent across threads.
unsafe impl Send for WriteSink {}
unsafe impl Sync for WriteSink {}

impl WriteSink {
    /// Write `bytes` to the sink.
    ///
    /// **Wire format note**: Tty/File use full Message+payload framing
    /// because TTY/file services parse Message structures. Pipe uses the
    /// raw 4-byte-LE-label-prefix format that libcluu/posix/pipe.rs
    /// `read_pipe` expects on the consumer side; sending a full Message
    /// here would make the consumer read the Message header bytes as
    /// data, producing garbage characters before the actual payload.
    pub fn write_all(&self, bytes: &[u8]) -> Result<()> {
        match self {
            WriteSink::Tty(tok) => send_with_payload(*tok, TTY_WRITE_LABEL, bytes),
            WriteSink::Pipe(tok) => {
                use alloc::vec::Vec;
                let mut buf: Vec<u8> = Vec::with_capacity(4 + bytes.len());
                buf.extend_from_slice(&PIPE_DATA_LABEL.to_le_bytes());
                buf.extend_from_slice(bytes);
                libcluu::syscall::ipc_send(*tok, &buf).map(|_| ())
            }
            WriteSink::Capture(ptr) => {
                // SAFETY: caller keeps the Vec alive for the builtin call.
                unsafe { (**ptr).extend_from_slice(bytes); }
                Ok(())
            }
            WriteSink::File(tok) => send_with_payload(*tok, TTY_WRITE_LABEL, bytes),
        }
    }

    /// Close the sink. For Pipe sends PIPE_EOF_LABEL so the downstream stage
    /// sees EOF. For Tty/File this is a no-op.
    pub fn close(&self) {
        if let WriteSink::Pipe(tok) = self {
            let eof = PIPE_EOF_LABEL.to_le_bytes();
            let _ = libcluu::syscall::ipc_send(*tok, &eof);
        }
    }
}

// ─── Traits ───────────────────────────────────────────────────────────────────

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

    /// Run the builtin with an explicit output sink. Builtins that support
    /// piped output override this. The default adapts to the legacy `run`
    /// signature when the sink is Tty; Pipe/File fall back to an error.
    fn run_with_sink(
        &self,
        stdout: &WriteSink,
        context: &mut CommandContext,
        args: &[String],
    ) -> Result<()> {
        match stdout {
            WriteSink::Tty(tok) => self.run(*tok, context, args),
            _ => {
                let m = format!(
                    "shell: builtin '{}' does not support redirected/piped output\n",
                    self.name()
                );
                let _ = stdout.write_all(m.as_bytes());
                Ok(())
            }
        }
    }

    /// Legacy entry point. Existing builtins implement this. Newer builtins
    /// MAY override `run_with_sink` directly instead.
    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()>;
}

/// Provider for injecting builtin commands into a registry.
pub trait BuiltinProvider {
    fn register(&self, registry: &mut BuiltinRegistry);
}

// ─── Registry ─────────────────────────────────────────────────────────────────

/// Builtin dispatcher that owns the builtin registry.
pub struct BuiltinRegistry {
    pub(crate) builtins: Vec<Box<dyn BuiltinCommand>>,
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

    pub fn find(&self, name: &str) -> Option<&dyn BuiltinCommand> {
        self.builtins
            .iter()
            .map(|b| b.as_ref())
            .find(|b| b.name() == name)
    }

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
        // produces multiple Stmts; `&&` and `||` produce stmts whose
        // prev_connector field controls short-circuit. We run left-to-right.
        let mut all_handled = true;
        for stmt in &program.stmts {
            let Stmt::Pipeline(pipeline) = stmt;

            // Short-circuit based on the connector that joins this pipeline
            // to the previous one.
            match pipeline.prev_connector {
                cluu_lang::ast::Connector::Always => {}
                cluu_lang::ast::Connector::AndIf => {
                    if context.last_status != 0 {
                        continue; // skip; previous command failed
                    }
                }
                cluu_lang::ast::Connector::OrIf => {
                    if context.last_status == 0 {
                        continue; // skip; previous command succeeded
                    }
                }
            }

            let has_redirs = pipeline.commands.iter().any(|c| !c.redirs.is_empty());
            // Route background pipelines, multi-stage pipelines, and
            // single-command pipelines with file redirections through the
            // PipelineExecutor which handles pgid creation and job table.
            if pipeline.commands.len() >= 2 || has_redirs || pipeline.bg {
                match crate::pipeline::PipelineExecutor::run(stdout, context, pipeline, self) {
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
                let value = crate::commands::redirect::render_word(context, &assign.value);
                context.set(&assign.name, value);
            }
            let mut args = Vec::new();
            for elem in command.words {
                args.push(crate::commands::redirect::render_word(context, &elem));
            }
            // Alias expansion: expand the first token repeatedly until stable
            // (recursion-guarded by a seen set to break alias→alias chains).
            expand_alias_first_token(&mut args, context);
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
                    crate::commands::exec::try_path_dispatch(stdout, context, name, &args[1..])?
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

// ─── Factory ──────────────────────────────────────────────────────────────────

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

// ─── Default providers ────────────────────────────────────────────────────────

struct DefaultBuiltins;

impl BuiltinProvider for DefaultBuiltins {
    fn register(&self, registry: &mut BuiltinRegistry) {
        crate::commands::builtins::register_all(registry);
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

// ─── Private helpers ──────────────────────────────────────────────────────────

/// Expand the first token of `args` against `context.aliases`, iterating until
/// the first token no longer matches any alias or it forms a cycle.
fn expand_alias_first_token(args: &mut Vec<String>, context: &CommandContext) {
    let mut seen = BTreeSet::new();
    loop {
        let first = match args.first() {
            Some(f) => f.clone(),
            None => break,
        };
        if seen.contains(&first) {
            break;
        }
        match context.aliases.get(&first) {
            Some(replacement) => {
                seen.insert(first);
                let parts: Vec<String> = replacement
                    .split_whitespace()
                    .map(|s| s.to_string())
                    .collect();
                if parts.is_empty() {
                    break;
                }
                // Replace the first element with the expansion parts;
                // keep the rest of args after the splice.
                args.splice(0..1, parts);
            }
            None => break,
        }
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

/// Parse a numeric token from args or look it up as a variable name.
fn parse_value(context: &CommandContext, token: &str) -> Option<i64> {
    if let Ok(value) = token.parse::<i64>() {
        return Some(value);
    }
    context.get(token).and_then(|val| val.parse::<i64>().ok())
}
