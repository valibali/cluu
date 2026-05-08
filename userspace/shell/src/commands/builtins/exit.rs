//! `exit`, `poweroff`, and `reboot` builtins.

use alloc::boxed::Box;
use alloc::string::String;

use libcluu::ipc::{send_with_payload, TTY_WRITE_LABEL, PROCMGR_SHUTDOWN_LABEL};
use libcluu::types::Message;
use libcluu::{IpcFlags, Result};

use super::registry::CommandContext;
use super::registry::{BuiltinCommand, BuiltinRegistry, WriteSink};

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

    fn run_with_sink(
        &self,
        _stdout: &WriteSink,
        context: &mut CommandContext,
        args: &[String],
    ) -> Result<()> {
        let code = match args.first() {
            None => context.last_status,
            Some(s) => s.parse::<i32>().unwrap_or(2),
        };
        context.exit_requested = Some(code);
        Ok(())
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        self.run_with_sink(&WriteSink::Tty(stdout), context, args)
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
