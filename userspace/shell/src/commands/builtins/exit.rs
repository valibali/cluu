//! `exit`, `poweroff`, and `reboot` builtins.

use alloc::boxed::Box;
use alloc::string::String;

use libcluu::ipc::{send_with_payload, TTY_WRITE_LABEL, PROCMGR_SHUTDOWN_LABEL};
use libcluu::types::Message;
use libcluu::{IpcFlags, Result};
use libcluu::syscall;

use super::registry::CommandContext;
use super::registry::{BuiltinCommand, BuiltinRegistry};

pub fn register(registry: &mut BuiltinRegistry) {
    registry.register(Box::new(ExitBuiltin));
    registry.register(Box::new(PoweroffBuiltin));
    registry.register(Box::new(RebootBuiltin));
}

pub(crate) struct ExitBuiltin;

impl BuiltinCommand for ExitBuiltin {
    fn name(&self) -> &'static str {
        "exit"
    }

    fn run(&self, stdout: usize, _context: &mut CommandContext, _args: &[String]) -> Result<()> {
        let _ = send_with_payload(stdout, TTY_WRITE_LABEL, b"shell: exiting\n");
        syscall::thread_exit(0);
    }
}

pub(crate) struct PoweroffBuiltin;

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

pub(crate) struct RebootBuiltin;

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
