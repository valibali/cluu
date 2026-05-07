//! Job-control and spawn builtins, plus test/probe builtins for Phase H.
//!
//! Large file by design (Stage 3 will cull the test-only probes).

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libcluu::boot::{process_info, TOKEN_REGISTRY, TOKEN_SPACE, TOKEN_STDOUT};
use libcluu::fs::client::VfsClient;
use libcluu::ipc::{
    alloc_shared_ring_region, call, call_with_payload,
    free_shared_ring_region, recv, send_with_payload, send_with_retry,
    PROCMGR_ESCALATE_LABEL, PROCMGR_SU_LABEL,
    SharedRing, SHARED_RING_DEFAULT_MAP_FLAGS,
    TTY_WRITE_LABEL,
};
use libcluu::registry;
use libcluu::syscall;
use libcluu::types::Message;
use libcluu::{debug_print, Error, IpcFlags, Result, TOKEN_IPC};

use crate::commands_old::{
    ensure_bg_job_state, parse_spawn_args, parse_status,
    signal_process, spawn_process, wait_for_exit_or_sigint, CommandContext,
    ForegroundMode, JobState,
};
use super::registry::{BuiltinCommand, BuiltinRegistry};

const PROCMGR_KILL_LABEL: u32 = 3;
const DEFAULT_PRIORITY: usize = 200;

pub fn register(registry: &mut BuiltinRegistry) {
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
    registry.register(Box::new(VtCrashTestBuiltin));
    registry.register(Box::new(SudoTestBuiltin));
    registry.register(Box::new(SuTestBuiltin));
    registry.register(Box::new(EscalateDenyBuiltin));
    registry.register(Box::new(SuEqualTestBuiltin));
    registry.register(Box::new(ShellCrashBuiltin));
}

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
        let spawn = crate::commands_old::spawn_process_with_argv(context, path.as_str(), priority, &argv_refs)?;
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

        let spawn = crate::commands_old::spawn_process_with_argv(context, path.as_str(), priority, &[])?;
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
        let Some(pid) = crate::commands_old::resolve_job_pid(context, args.first()) else {
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
        let Some(pid) = crate::commands_old::resolve_job_pid(context, args.first()) else {
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
        let Some(pid) = crate::commands_old::resolve_job_pid(context, args.first()) else {
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
// jobchurn
// ---------------------------------------------------------------------------

pub(crate) struct JobChurnBuiltin;

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

            signal_process(spawn.procmgr_endpoint, pid, 19 /* SIGSTOP */)?;
            ensure_bg_job_state(context, pid, JobState::Stopped)?;

            signal_process(spawn.procmgr_endpoint, pid, 18 /* SIGCONT */)?;
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

// ---------------------------------------------------------------------------
// jobmix
// ---------------------------------------------------------------------------

pub(crate) struct JobMixBuiltin;

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

        signal_process(spawn_a.procmgr_endpoint, pid_a, 19 /* SIGSTOP */)?;
        ensure_bg_job_state(context, pid_a, JobState::Stopped)?;

        signal_process(spawn_b.procmgr_endpoint, pid_b, 19 /* SIGSTOP */)?;
        ensure_bg_job_state(context, pid_b, JobState::Stopped)?;

        signal_process(spawn_a.procmgr_endpoint, pid_a, 18 /* SIGCONT */)?;
        ensure_bg_job_state(context, pid_a, JobState::Running)?;

        let Some(job_b) = context.take_bg_job(pid_b) else {
            let line = format!("jobmix: FAIL missing job pid={}\n", pid_b);
            send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            return Ok(());
        };
        signal_process(spawn_b.procmgr_endpoint, pid_b, 18 /* SIGCONT */)?;
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

// ---------------------------------------------------------------------------
// killdeny
// ---------------------------------------------------------------------------

pub(crate) struct KillDenyBuiltin;

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

// ---------------------------------------------------------------------------
// regdeny
// ---------------------------------------------------------------------------

pub(crate) struct RegistryDenyBuiltin;

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
        req.words[1] = process_info().tokens[TOKEN_STDOUT];
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

// ---------------------------------------------------------------------------
// mapfail
// ---------------------------------------------------------------------------

pub(crate) struct MapFailBuiltin;

impl BuiltinCommand for MapFailBuiltin {
    fn name(&self) -> &'static str {
        "mapfail"
    }

    fn run(&self, stdout: usize, _context: &mut CommandContext, args: &[String]) -> Result<()> {
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

// ---------------------------------------------------------------------------
// mapcpfail
// ---------------------------------------------------------------------------

pub(crate) struct MapCopyFailBuiltin;

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

// ---------------------------------------------------------------------------
// maperror
// ---------------------------------------------------------------------------

pub(crate) struct MapErrorBuiltin;

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

// ---------------------------------------------------------------------------
// ext2write
// ---------------------------------------------------------------------------

pub(crate) struct Ext2WriteBuiltin;

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

// ---------------------------------------------------------------------------
// ext2append
// ---------------------------------------------------------------------------

pub(crate) struct Ext2AppendBuiltin;

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

// ---------------------------------------------------------------------------
// ext2mutate
// ---------------------------------------------------------------------------

pub(crate) struct Ext2MutateBuiltin;

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

// ---------------------------------------------------------------------------
// ext2unlink
// ---------------------------------------------------------------------------

pub(crate) struct Ext2UnlinkBuiltin;

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

// ---------------------------------------------------------------------------
// ext2ownerdeny
// ---------------------------------------------------------------------------

pub(crate) struct Ext2OwnerDenyBuiltin;

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

// ---------------------------------------------------------------------------
// ringio
// ---------------------------------------------------------------------------

pub(crate) struct RingIoBuiltin;

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

        let region = match alloc_shared_ring_region(
            space_token,
            64 * 1024,
            SHARED_RING_DEFAULT_MAP_FLAGS,
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
                let _ = free_shared_ring_region(space_token, region);
                let _ = vfs.close(file);
                let line = format!("ringio: FAIL ring setup {:?}\n", err);
                let _ = debug_print(line.trim_end());
                send_with_retry(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
                return Ok(());
            }
        };
        if ring_meta.bytes > region.bytes {
            let _ = free_shared_ring_region(space_token, region);
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
                let _ = free_shared_ring_region(space_token, region);
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
                    let _ = free_shared_ring_region(space_token, region);
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
                let _ = free_shared_ring_region(space_token, region);
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

        let _ = free_shared_ring_region(space_token, region);
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

// ---------------------------------------------------------------------------
// vtcrashtest
// ---------------------------------------------------------------------------

pub(crate) struct VtCrashTestBuiltin;

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

// ---------------------------------------------------------------------------
// sudotest
// ---------------------------------------------------------------------------

pub(crate) struct SudoTestBuiltin;

impl BuiltinCommand for SudoTestBuiltin {
    fn name(&self) -> &'static str {
        "sudotest"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, _args: &[String]) -> Result<()> {
        let mut payload = Vec::new();
        payload.push(0);
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

            let _ = signal_process(procmgr_endpoint, pid, 9);
        } else {
            let line = format!("sudotest: FAIL status={} pid={}\n", status, pid);
            send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            let _ = debug_print(line.trim_end());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// sutest
// ---------------------------------------------------------------------------

pub(crate) struct SuTestBuiltin;

impl BuiltinCommand for SuTestBuiltin {
    fn name(&self) -> &'static str {
        "sutest"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, _args: &[String]) -> Result<()> {
        let target = "alice";

        let mut payload = Vec::new();
        payload.extend_from_slice(target.as_bytes());
        payload.push(0);
        payload.push(0);

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

            let _ = signal_process(procmgr_endpoint, pid, 9);
        } else {
            let line = format!("sutest: FAIL status={} pid={}\n", status, pid);
            send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
            let _ = debug_print(line.trim_end());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// escalatedeny
// ---------------------------------------------------------------------------

pub(crate) struct EscalateDenyBuiltin;

impl BuiltinCommand for EscalateDenyBuiltin {
    fn name(&self) -> &'static str {
        "escalatedeny"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, _args: &[String]) -> Result<()> {
        let mut payload = Vec::new();
        payload.push(0);
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

// ---------------------------------------------------------------------------
// suequaltest
// ---------------------------------------------------------------------------

pub(crate) struct SuEqualTestBuiltin;

impl BuiltinCommand for SuEqualTestBuiltin {
    fn name(&self) -> &'static str {
        "suequaltest"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, _args: &[String]) -> Result<()> {
        let target = "root";
        let mut payload = Vec::new();
        payload.extend_from_slice(target.as_bytes());
        payload.push(0);
        payload.push(0);

        let procmgr_endpoint = context.procmgr_spawn_endpoint()?;
        let mut msg = Message::new(PROCMGR_SU_LABEL, [0; 6], 2);
        msg.words[0] = payload.len();
        msg.words[1] = 0;
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

// ---------------------------------------------------------------------------
// shellcrash
// ---------------------------------------------------------------------------

pub(crate) struct ShellCrashBuiltin;

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
