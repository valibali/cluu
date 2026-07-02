//! Job control builtins: jobs, fg, bg, wait, kill.
//!
//! JobTable tracks each pipeline as a single job keyed by pgid.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use libcluu::ipc::PROCMGR_JOB_NOTIFY_LABEL;
use libcluu::posix::jobs::{pg_resume, pg_signal, tty_set_fg};
use libcluu::syscall;
use libcluu::Result;

use super::registry::{BuiltinCommand, BuiltinRegistry, CommandContext, WriteSink};

// ─── JobState ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Running,
    Stopped,
    Done,
}

// ─── Job ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Job {
    pub id: usize,
    pub pgid: usize,
    pub pids: Vec<usize>,
    /// Per-pid notify endpoints (parallel to `pids`).
    pub notify_endpoints: Vec<usize>,
    pub state: JobState,
    pub cmd_line: String,
    pub bg: bool,
    pub last_exit: Option<i32>,
}

// ─── JobTable ────────────────────────────────────────────────────────────────

pub struct JobTable {
    next_id: usize,
    by_id: BTreeMap<usize, Job>,
}

impl JobTable {
    pub const fn new() -> Self {
        Self {
            next_id: 0,
            by_id: BTreeMap::new(),
        }
    }

    pub fn add(
        &mut self,
        pgid: usize,
        pids: Vec<usize>,
        notify_endpoints: Vec<usize>,
        cmd_line: String,
        bg: bool,
    ) -> usize {
        self.next_id += 1;
        let id = self.next_id;
        self.by_id.insert(
            id,
            Job {
                id,
                pgid,
                pids,
                notify_endpoints,
                state: JobState::Running,
                cmd_line,
                bg,
                last_exit: None,
            },
        );
        id
    }

    pub fn get(&self, id: usize) -> Option<&Job> {
        self.by_id.get(&id)
    }

    pub fn get_mut(&mut self, id: usize) -> Option<&mut Job> {
        self.by_id.get_mut(&id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Job> {
        self.by_id.values()
    }

    pub fn most_recent(&self) -> Option<&Job> {
        self.by_id.values().last()
    }

    pub fn by_pgid_mut(&mut self, pgid: usize) -> Option<&mut Job> {
        self.by_id.values_mut().find(|j| j.pgid == pgid)
    }

    pub fn remove_done(&mut self) {
        self.by_id.retain(|_, j| j.state != JobState::Done);
    }
}

// ─── Builtins ────────────────────────────────────────────────────────────────

pub struct JobsBuiltin;
pub struct FgBuiltin;
pub struct BgBuiltin;
pub struct WaitBuiltin;
pub struct KillBuiltin;

impl BuiltinCommand for JobsBuiltin {
    fn name(&self) -> &'static str {
        "jobs"
    }

    fn run_with_sink(
        &self,
        stdout: &WriteSink,
        ctx: &mut CommandContext,
        _args: &[String],
    ) -> Result<()> {
        drain_job_notifications(ctx);
        let recent_id = ctx.jobs.most_recent().map(|j| j.id);
        let mut count = 0usize;
        for j in ctx.jobs.iter() {
            let state_s = match j.state {
                JobState::Running => "Running",
                JobState::Stopped => "Stopped",
                JobState::Done => "Done",
            };
            let plus = if Some(j.id) == recent_id { "+" } else { " " };
            let line = format!("[{}]{} {:8}  {}\n", j.id, plus, state_s, j.cmd_line);
            let _ = libcluu::debug_print(line.trim_end());
            stdout.write_all(line.as_bytes())?;
            count += 1;
        }
        if count == 0 {
            let _ = libcluu::debug_print("jobs: no jobs");
        }
        Ok(())
    }

    fn run(&self, stdout: usize, ctx: &mut CommandContext, args: &[String]) -> Result<()> {
        self.run_with_sink(&WriteSink::Tty(stdout), ctx, args)
    }
}

impl BuiltinCommand for FgBuiltin {
    fn name(&self) -> &'static str {
        "fg"
    }

    fn run_with_sink(
        &self,
        stdout: &WriteSink,
        ctx: &mut CommandContext,
        args: &[String],
    ) -> Result<()> {
        let id = match parse_jobspec(args, ctx) {
            Ok(i) => i,
            Err(m) => {
                stdout.write_all(format!("fg: {}\n", m).as_bytes())?;
                return Ok(());
            }
        };
        let pgid = match ctx.jobs.get(id).map(|j| j.pgid) {
            Some(p) => p,
            None => {
                stdout.write_all(format!("fg: %{}: no such job\n", id).as_bytes())?;
                return Ok(());
            }
        };
        let cmd_line = ctx.jobs.get(id).map(|j| j.cmd_line.clone()).unwrap_or_default();
        stdout.write_all(format!("{}\n", cmd_line).as_bytes())?;

        let procmgr_ep = match ctx.procmgr_spawn_endpoint() {
            Ok(ep) => ep,
            Err(_) => {
                stdout.write_all(b"fg: procmgr unavailable\n")?;
                return Ok(());
            }
        };

        crate::io::report_err(pg_resume(procmgr_ep, pgid), "pg_resume");
        if ctx.tty_stdout != 0 && ctx.session_id != 0 {
            crate::io::report_err(tty_set_fg(ctx.tty_stdout, ctx.session_id, pgid), "tty_set_fg");
        }
        if let Some(j) = ctx.jobs.get_mut(id) {
            j.state = JobState::Running;
            j.bg = false;
        }

        wait_for_job(id, ctx);

        // Restore shell as TTY foreground.
        if ctx.tty_stdout != 0 && ctx.session_id != 0 && ctx.shell_pgid != 0 {
            crate::io::report_err(
                tty_set_fg(ctx.tty_stdout, ctx.session_id, ctx.shell_pgid),
                "tty_set_fg",
            );
        }
        Ok(())
    }

    fn run(&self, stdout: usize, ctx: &mut CommandContext, args: &[String]) -> Result<()> {
        self.run_with_sink(&WriteSink::Tty(stdout), ctx, args)
    }
}

impl BuiltinCommand for BgBuiltin {
    fn name(&self) -> &'static str {
        "bg"
    }

    fn run_with_sink(
        &self,
        stdout: &WriteSink,
        ctx: &mut CommandContext,
        args: &[String],
    ) -> Result<()> {
        let id = match parse_jobspec(args, ctx) {
            Ok(i) => i,
            Err(m) => {
                stdout.write_all(format!("bg: {}\n", m).as_bytes())?;
                return Ok(());
            }
        };
        let pgid = match ctx.jobs.get(id).map(|j| j.pgid) {
            Some(p) => p,
            None => {
                stdout.write_all(format!("bg: %{}: no such job\n", id).as_bytes())?;
                return Ok(());
            }
        };

        let procmgr_ep = match ctx.procmgr_spawn_endpoint() {
            Ok(ep) => ep,
            Err(_) => {
                stdout.write_all(b"bg: procmgr unavailable\n")?;
                return Ok(());
            }
        };

        crate::io::report_err(pg_resume(procmgr_ep, pgid), "pg_resume");
        let cmd_line = ctx.jobs.get(id).map(|j| j.cmd_line.clone()).unwrap_or_default();
        if let Some(j) = ctx.jobs.get_mut(id) {
            j.state = JobState::Running;
            j.bg = true;
        }
        stdout.write_all(format!("[{}]+ {} &\n", id, cmd_line).as_bytes())?;
        Ok(())
    }

    fn run(&self, stdout: usize, ctx: &mut CommandContext, args: &[String]) -> Result<()> {
        self.run_with_sink(&WriteSink::Tty(stdout), ctx, args)
    }
}

impl BuiltinCommand for WaitBuiltin {
    fn name(&self) -> &'static str {
        "wait"
    }

    fn run_with_sink(
        &self,
        _stdout: &WriteSink,
        ctx: &mut CommandContext,
        args: &[String],
    ) -> Result<()> {
        if args.is_empty() {
            let ids: Vec<usize> = ctx
                .jobs
                .iter()
                .filter(|j| j.state != JobState::Done)
                .map(|j| j.id)
                .collect();
            for id in ids {
                wait_for_job(id, ctx);
            }
        } else if let Ok(id) = parse_jobspec(args, ctx) {
            wait_for_job(id, ctx);
        }
        Ok(())
    }

    fn run(&self, stdout: usize, ctx: &mut CommandContext, args: &[String]) -> Result<()> {
        self.run_with_sink(&WriteSink::Tty(stdout), ctx, args)
    }
}

impl BuiltinCommand for KillBuiltin {
    fn name(&self) -> &'static str {
        "kill"
    }

    fn run_with_sink(
        &self,
        stdout: &WriteSink,
        ctx: &mut CommandContext,
        args: &[String],
    ) -> Result<()> {
        if args.is_empty() {
            stdout.write_all(b"kill: usage: kill [-s SIG | -SIG] PID|%JOB...\n")?;
            return Ok(());
        }
        let mut signum = 15i32; // SIGTERM
        let mut idx = 0usize;
        if args[0].starts_with('-') && args[0].len() > 1 {
            let s = &args[0][1..];
            if let Ok(n) = s.parse::<i32>() {
                signum = n;
                idx = 1;
            } else {
                match s {
                    "TERM" => { signum = 15; idx = 1; }
                    "INT"  => { signum = 2;  idx = 1; }
                    "STOP" => { signum = 19; idx = 1; }
                    "CONT" => { signum = 18; idx = 1; }
                    "KILL" => { signum = 9;  idx = 1; }
                    "HUP"  => { signum = 1;  idx = 1; }
                    _ => {
                        stdout.write_all(
                            format!("kill: unknown signal {}\n", s).as_bytes(),
                        )?;
                        return Ok(());
                    }
                }
            }
        }

        let procmgr_ep = match ctx.procmgr_spawn_endpoint() {
            Ok(ep) => ep,
            Err(_) => {
                stdout.write_all(b"kill: procmgr unavailable\n")?;
                return Ok(());
            }
        };

        for target in &args[idx..] {
            if let Some(spec) = target.strip_prefix('%') {
                let id: usize = match spec.parse() {
                    Ok(n) => n,
                    Err(_) => {
                        stdout.write_all(b"kill: bad job spec\n")?;
                        continue;
                    }
                };
                let pgid = match ctx.jobs.get(id).map(|j| j.pgid) {
                    Some(p) => p,
                    None => {
                        stdout.write_all(
                            format!("kill: %{}: no such job\n", id).as_bytes(),
                        )?;
                        continue;
                    }
                };
                crate::io::report_err(pg_signal(procmgr_ep, pgid, signum), "pg_signal");
            } else {
                stdout.write_all(
                    format!("kill: numeric PID kill not yet supported (use %N)\n").as_bytes(),
                )?;
            }
        }
        Ok(())
    }

    fn run(&self, stdout: usize, ctx: &mut CommandContext, args: &[String]) -> Result<()> {
        self.run_with_sink(&WriteSink::Tty(stdout), ctx, args)
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn parse_jobspec(
    args: &[String],
    ctx: &CommandContext,
) -> core::result::Result<usize, String> {
    if args.is_empty() {
        return ctx
            .jobs
            .most_recent()
            .map(|j| j.id)
            .ok_or_else(|| "no current job".to_string());
    }
    let s = &args[0];
    if let Some(spec) = s.strip_prefix('%') {
        return spec
            .parse()
            .map_err(|_| format!("bad job spec '%{}'", spec));
    }
    s.parse().map_err(|_| format!("bad job spec '{}'", s))
}

/// Non-blocking drain of all job notify endpoints. Updates the JobTable
/// state for any JOB_NOTIFY messages received.
pub fn drain_job_notifications(ctx: &mut CommandContext) {
    let mut buf = [0u8; 64];
    // Collect (job_id, notify_ep) pairs to avoid borrow issues.
    let jobs_info: Vec<(usize, Vec<usize>)> = ctx
        .jobs
        .iter()
        .map(|j| (j.id, j.notify_endpoints.clone()))
        .collect();

    for (job_id, notify_endpoints) in &jobs_info {
        for &notify_ep in notify_endpoints {
            if notify_ep == 0 {
                continue;
            }
            loop {
                match syscall::ipc_recv_nonblocking(notify_ep, &mut buf) {
                    Ok(len) if len >= 8 => {
                        let label = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
                        if label == PROCMGR_JOB_NOTIFY_LABEL {
                            handle_notify_bytes(&buf, ctx);
                        } else {
                            // PROC_EXIT_LABEL (1) or similar — mark job done.
                            handle_proc_exit_bytes(&buf, *job_id, ctx);
                        }
                    }
                    Ok(_) | Err(_) => break,
                }
            }
        }
    }
}

/// Blocking wait until job `id` transitions to Stopped or Done.
pub fn wait_for_job(id: usize, ctx: &mut CommandContext) {
    // Collect notify endpoints for this job.
    let notify_endpoints: Vec<usize> = ctx
        .jobs
        .get(id)
        .map(|j| j.notify_endpoints.clone())
        .unwrap_or_default();

    let mut buf = [0u8; 64];
    'outer: loop {
        for &notify_ep in &notify_endpoints {
            if notify_ep == 0 {
                continue;
            }
            crate::io::report_err(syscall::ipc_recv(notify_ep, &mut buf), "ipc_recv");
            let label = if buf.len() >= 4 {
                u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]])
            } else {
                0
            };
            if label == PROCMGR_JOB_NOTIFY_LABEL {
                handle_notify_bytes(&buf, ctx);
            } else {
                // PROC_EXIT_LABEL — mark job done.
                handle_proc_exit_bytes(&buf, id, ctx);
            }
            if let Some(j) = ctx.jobs.get(id) {
                if j.state == JobState::Stopped || j.state == JobState::Done {
                    break 'outer;
                }
            } else {
                break 'outer; // job was removed
            }
        }
        // If no endpoints, nothing to wait on.
        if notify_endpoints.is_empty() {
            break;
        }
    }
}

/// Parse a JOB_NOTIFY message and update JobTable.
///
/// Message layout (PROCMGR_JOB_NOTIFY_LABEL):
///   bytes 0-3:   label (u32 LE)
///   bytes 4-7:   tag fields (words count etc.)
///   bytes 8-15:  words[0] = pgid
///   bytes 16-23: words[1] = pid
///   bytes 24-31: words[2] = state (1=Stopped, 2=Continued, 3=Exited)
///   bytes 32-39: words[3] = exit_code
fn handle_notify_bytes(buf: &[u8], ctx: &mut CommandContext) {
    if buf.len() < 40 {
        return;
    }
    let pgid = usize::from_le_bytes(buf[8..16].try_into().unwrap_or([0; 8]));
    let _pid = usize::from_le_bytes(buf[16..24].try_into().unwrap_or([0; 8]));
    let state = usize::from_le_bytes(buf[24..32].try_into().unwrap_or([0; 8])) as u32;
    let exit_code =
        usize::from_le_bytes(buf[32..40].try_into().unwrap_or([0; 8])) as i32;

    if let Some(j) = ctx.jobs.by_pgid_mut(pgid) {
        match state {
            1 => j.state = JobState::Stopped,
            2 => j.state = JobState::Running,
            3 => {
                j.state = JobState::Done;
                j.last_exit = Some(exit_code);
            }
            _ => {}
        }
    }
}

/// Handle a PROC_EXIT_LABEL (label=1) notification for job `job_id`.
fn handle_proc_exit_bytes(buf: &[u8], job_id: usize, ctx: &mut CommandContext) {
    // Exit code is at words[1] = bytes 16-23.
    let exit_code = if buf.len() >= 24 {
        let bytes: [u8; 8] = buf[16..24].try_into().unwrap_or([0; 8]);
        i64::from_le_bytes(bytes) as i32
    } else {
        0
    };
    if let Some(j) = ctx.jobs.get_mut(job_id) {
        j.state = JobState::Done;
        j.last_exit = Some(exit_code);
    }
}

/// Print `[N]+ Done  cmd` lines for all completed bg jobs, then remove them.
pub fn reap_done_jobs(stdout: usize, ctx: &mut CommandContext) {
    let mut done_ids: Vec<(usize, String)> = Vec::new();
    for j in ctx.jobs.iter() {
        if j.state == JobState::Done && j.bg {
            done_ids.push((j.id, j.cmd_line.clone()));
        }
    }
    for (id, cmd) in done_ids {
        let line = format!("[{}]+  Done\t{}\n", id, cmd);
        crate::write_stdout(line.as_bytes());
    }
    ctx.jobs.remove_done();
}

// ─── shellcrash (debug gate) ──────────────────────────────────────────────────

pub(crate) struct ShellCrashBuiltin;

impl BuiltinCommand for ShellCrashBuiltin {
    fn name(&self) -> &'static str {
        "_shellcrash"
    }

    fn run(
        &self,
        stdout: usize,
        _context: &mut CommandContext,
        _args: &[String],
    ) -> Result<()> {
        crate::write_stdout(b"shellcrash: triggering fault\n");
        let _ = libcluu::debug_print("shellcrash: triggering null-write fault");
        unsafe {
            core::ptr::write_volatile(0 as *mut u8, 0);
        }
        Ok(())
    }
}

// ─── Registration ─────────────────────────────────────────────────────────────

#[cfg(feature = "debug-shellcrash")]
fn register_shellcrash(registry: &mut BuiltinRegistry) {
    registry.register(Box::new(ShellCrashBuiltin));
}

#[cfg(not(feature = "debug-shellcrash"))]
fn register_shellcrash(_registry: &mut BuiltinRegistry) {}

pub fn register(registry: &mut BuiltinRegistry) {
    registry.register(Box::new(JobsBuiltin));
    registry.register(Box::new(FgBuiltin));
    registry.register(Box::new(BgBuiltin));
    registry.register(Box::new(WaitBuiltin));
    registry.register(Box::new(KillBuiltin));
    register_shellcrash(registry);
}
