//! Job-control and spawn builtins.
//!
//! Test-only probes culled in Stage 3 — they now live under
//! userspace/probes/ and are invoked via `spawn <name>` or
//! `container run <name>`.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libcluu::ipc::{send_with_payload, TTY_WRITE_LABEL};
use libcluu::{debug_print, Result};

use crate::commands::exec::{
    ensure_bg_job_state, parse_spawn_args, parse_status,
    signal_process, wait_for_exit_or_sigint,
};
use super::registry::{CommandContext, JobState};
use super::registry::{BuiltinCommand, BuiltinRegistry};

pub fn register(registry: &mut BuiltinRegistry) {
    registry.register(Box::new(SpawnBuiltin));
    registry.register(Box::new(SpawnBgBuiltin));
    registry.register(Box::new(JobsBuiltin));
    registry.register(Box::new(StopBuiltin));
    registry.register(Box::new(ForegroundBuiltin));
    registry.register(Box::new(BackgroundBuiltin));
    register_shellcrash(registry);
}

#[cfg(feature = "debug-shellcrash")]
fn register_shellcrash(registry: &mut BuiltinRegistry) {
    registry.register(Box::new(ShellCrashBuiltin));
}

#[cfg(not(feature = "debug-shellcrash"))]
fn register_shellcrash(_registry: &mut BuiltinRegistry) {}

// ---------------------------------------------------------------------------
// spawn
// ---------------------------------------------------------------------------

pub(crate) struct SpawnBuiltin;

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
        let spawn = crate::commands::exec::spawn_process_with_argv(context, path.as_str(), priority, &argv_refs)?;
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

// ---------------------------------------------------------------------------
// spawnbg
// ---------------------------------------------------------------------------

pub(crate) struct SpawnBgBuiltin;

impl BuiltinCommand for SpawnBgBuiltin {
    fn name(&self) -> &'static str {
        "spawnbg"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        let Some((path, priority, fg_mode, _argv_tail)) = parse_spawn_args(args) else {
            send_with_payload(stdout, TTY_WRITE_LABEL, b"spawnbg: missing path\n")?;
            return Ok(());
        };

        let spawn = crate::commands::exec::spawn_process_with_argv(context, path.as_str(), priority, &[])?;
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

// ---------------------------------------------------------------------------
// jobs
// ---------------------------------------------------------------------------

pub(crate) struct JobsBuiltin;

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

// ---------------------------------------------------------------------------
// fg
// ---------------------------------------------------------------------------

pub(crate) struct ForegroundBuiltin;

impl BuiltinCommand for ForegroundBuiltin {
    fn name(&self) -> &'static str {
        "fg"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        let Some(pid) = crate::commands::exec::resolve_job_pid(context, args.first()) else {
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
            signal_process(procmgr_endpoint, pid, 18 /* SIGCONT */)?;
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

// ---------------------------------------------------------------------------
// stop
// ---------------------------------------------------------------------------

pub(crate) struct StopBuiltin;

impl BuiltinCommand for StopBuiltin {
    fn name(&self) -> &'static str {
        "stop"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        let Some(pid) = crate::commands::exec::resolve_job_pid(context, args.first()) else {
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
        signal_process(procmgr_endpoint, pid, 19 /* SIGSTOP */)?;
        ensure_bg_job_state(context, pid, JobState::Stopped)?;
        let line = format!("stop: pid={} stopped\n", pid);
        send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// bg
// ---------------------------------------------------------------------------

pub(crate) struct BackgroundBuiltin;

impl BuiltinCommand for BackgroundBuiltin {
    fn name(&self) -> &'static str {
        "bg"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        let Some(pid) = crate::commands::exec::resolve_job_pid(context, args.first()) else {
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
        signal_process(procmgr_endpoint, pid, 18 /* SIGCONT */)?;
        ensure_bg_job_state(context, pid, JobState::Running)?;
        let line = format!("bg: pid={} running\n", pid);
        send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// shellcrash (debug gate: feature = "debug-shellcrash")
// ---------------------------------------------------------------------------

pub(crate) struct ShellCrashBuiltin;

impl BuiltinCommand for ShellCrashBuiltin {
    fn name(&self) -> &'static str {
        "_shellcrash"
    }

    fn run(&self, stdout: usize, _context: &mut CommandContext, _args: &[String]) -> Result<()> {
        let _ = send_with_payload(stdout, TTY_WRITE_LABEL, b"shellcrash: triggering fault\n");
        let _ = debug_print("shellcrash: triggering null-write fault");
        unsafe { core::ptr::write_volatile(0 as *mut u8, 0); }
        Ok(())
    }
}
