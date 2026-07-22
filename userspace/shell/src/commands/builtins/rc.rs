//! `mount`, `start`, `probe`, `wait` builtins for rc.boot / rc.profile scripts.
//!
//! These provide a Linux/Plan 9-style declarative boot: a shell script
//! sources at boot (or login) drives service start, VFS mount, bus probe,
//! and ordering gates instead of a hardcoded autostart.toml.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::string::ToString;
use alloc::vec::Vec;

use libcluu::debug_print;
use libcluu::ipc::{
    call_with_payload, DRIVERMGR_PROBE_LABEL, PROCMGR_START_IMAGE_LABEL, VFS_MOUNT_LABEL,
};
use libcluu::registry;
use libcluu::types::Message;
use libcluu::yield_cpu;
use libcluu::Result;

use super::registry::{BuiltinCommand, BuiltinRegistry, CommandContext, WriteSink};

const PROBE_TIMEOUT_ITERS: usize = 200;

pub fn register(reg: &mut BuiltinRegistry) {
    reg.register(Box::new(MountBuiltin));
    reg.register(Box::new(StartBuiltin));
    reg.register(Box::new(ProbeBuiltin));
    reg.register(Box::new(WaitBuiltin));
}

struct MountBuiltin;

impl BuiltinCommand for MountBuiltin {
    fn name(&self) -> &'static str {
        "mount"
    }

    fn run(&self, stdout: usize, _ctx: &mut CommandContext, args: &[String]) -> Result<()> {
        if args.len() < 3 {
            write_stdout(stdout, b"usage: mount <path> <service> [endpoint_token]\n")?;
            return Ok(());
        }
        let path = &args[1];
        let service = &args[2];
        let endpoint = if args.len() >= 4 {
            match args[3].parse::<usize>() {
                Ok(ep) => ep,
                Err(_) => {
                    write_stdout(stdout, b"mount: endpoint_token must be a number\n")?;
                    return Ok(());
                }
            }
        } else {
            match registry::subscribe_output(service, "main") {
                Ok(ep) => ep,
                Err(e) => {
                    let msg = format!("mount: service '{}' not registered: {:?}\n", service, e);
                    write_stdout(stdout, msg.as_bytes())?;
                    return Ok(());
                }
            }
        };
        match registry::vfs_mount(path, service, endpoint) {
            Ok(()) => {
                let msg = format!("mount: {} -> {} (ep={})\n", path, service, endpoint);
                write_stdout(stdout, msg.as_bytes())?;
            }
            Err(e) => {
                let msg = format!("mount: failed: {:?}\n", e);
                write_stdout(stdout, msg.as_bytes())?;
            }
        }
        Ok(())
    }
}

struct StartBuiltin;

impl BuiltinCommand for StartBuiltin {
    fn name(&self) -> &'static str {
        "start"
    }

    fn run(&self, stdout: usize, _ctx: &mut CommandContext, args: &[String]) -> Result<()> {
        if args.len() < 2 {
            write_stdout(stdout, b"usage: start <image> [arg ...]\n")?;
            return Ok(());
        }
        let image = &args[1];
        let procmgr_ep = match registry::subscribe_output("procmgr", "spawn") {
            Ok(ep) => ep,
            Err(e) => {
                let msg = format!("start: procmgr not available: {:?}\n", e);
                write_stdout(stdout, msg.as_bytes())?;
                return Ok(());
            }
        };
        let mut payload = Vec::new();
        payload.extend_from_slice(image.as_bytes());
        let mut msg = Message::new(PROCMGR_START_IMAGE_LABEL, [0; 6], 1);
        msg.words[0] = payload.len();
        let mut reply = Message::new(0, [0; 6], 0);
        match call_with_payload(procmgr_ep, &msg, &payload, &mut reply) {
            Ok(()) => {
                let status = reply.words[0];
                let pid = reply.words[1];
                if status == 0 {
                    let m = format!("start: {} pid={}\n", image, pid);
                    write_stdout(stdout, m.as_bytes())?;
                } else {
                    let m = format!("start: {} failed status={}\n", image, status);
                    write_stdout(stdout, m.as_bytes())?;
                }
            }
            Err(e) => {
                let m = format!("start: {} IPC error: {:?}\n", image, e);
                write_stdout(stdout, m.as_bytes())?;
            }
        }
        Ok(())
    }
}

struct ProbeBuiltin;

impl BuiltinCommand for ProbeBuiltin {
    fn name(&self) -> &'static str {
        "probe"
    }

    fn run(&self, stdout: usize, _ctx: &mut CommandContext, args: &[String]) -> Result<()> {
        if args.len() < 2 {
            write_stdout(stdout, b"usage: probe <bus>\n")?;
            return Ok(());
        }
        let bus = &args[1];
        let drivermgr_ep = match registry::subscribe_output("drivermgr", "main") {
            Ok(ep) => ep,
            Err(e) => {
                let m = format!("probe: drivermgr not available: {:?}\n", e);
                write_stdout(stdout, m.as_bytes())?;
                return Ok(());
            }
        };
        let mut msg = Message::new(DRIVERMGR_PROBE_LABEL, [0; 6], 1);
        msg.words[0] = bus.len();
        let mut reply = Message::new(0, [0; 6], 0);
        match call_with_payload(drivermgr_ep, &msg, bus.as_bytes(), &mut reply) {
            Ok(()) => {
                let status = reply.words[0];
                if status == 0 {
                    let m = format!("probe: {} ok\n", bus);
                    write_stdout(stdout, m.as_bytes())?;
                } else {
                    let m = format!("probe: {} failed status={}\n", bus, status);
                    write_stdout(stdout, m.as_bytes())?;
                }
            }
            Err(e) => {
                let m = format!("probe: {} IPC error: {:?}\n", bus, e);
                write_stdout(stdout, m.as_bytes())?;
            }
        }
        Ok(())
    }
}

struct WaitBuiltin;

impl BuiltinCommand for WaitBuiltin {
    fn name(&self) -> &'static str {
        "wait"
    }

    fn run(&self, stdout: usize, _ctx: &mut CommandContext, args: &[String]) -> Result<()> {
        if args.len() < 2 {
            write_stdout(stdout, b"usage: wait <service> [output]\n")?;
            return Ok(());
        }
        let service = &args[1];
        let output = if args.len() >= 3 { args[2].as_str() } else { "main" };
        for _ in 0..PROBE_TIMEOUT_ITERS {
            if let Ok(_ep) = registry::subscribe_output(service, output) {
                let m = format!("wait: {}:{} ready\n", service, output);
                write_stdout(stdout, m.as_bytes())?;
                return Ok(());
            }
            let _ = yield_cpu();
        }
        let m = format!("wait: {}:{} timed out\n", service, output);
        write_stdout(stdout, m.as_bytes())?;
        let _ = debug_print(&format!("rc.wait: timeout for {}:{}", service, output));
        Ok(())
    }
}

fn write_stdout(endpoint: usize, bytes: &[u8]) -> Result<()> {
    let sink = WriteSink::Tty(endpoint);
    sink.write_all(bytes)
}
