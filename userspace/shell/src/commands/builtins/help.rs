//! `help` and `clear` builtins.

use alloc::boxed::Box;
use alloc::string::String;

use libcluu::ipc::{send_with_payload, CONSOLE_CLEAR_LABEL};
use libcluu::Result;

use super::registry::CommandContext;
use super::registry::{BuiltinCommand, BuiltinRegistry, WriteSink};

pub fn register(registry: &mut BuiltinRegistry) {
    registry.register(Box::new(HelpBuiltin));
    registry.register(Box::new(ClearBuiltin));
}

pub(crate) struct HelpBuiltin;

impl BuiltinCommand for HelpBuiltin {
    fn name(&self) -> &'static str {
        "help"
    }

    fn run_with_sink(
        &self,
        stdout: &WriteSink,
        _context: &mut CommandContext,
        _args: &[String],
    ) -> Result<()> {
        stdout.write_all(
            b"builtins: help, clear, echo, cd, pwd, exit, set, unset, env, expr, let, spawn, spawnbg, jobs, jobchurn, jobmix, stop, fg, bg, killdeny, regdeny, mapfail, mapcpfail, maperror, ext2write, ext2append, ext2mutate, ext2unlink, ext2ownerdeny, ringio, repeat, cat, ls, heap\n",
        )?;
        Ok(())
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        self.run_with_sink(&WriteSink::Tty(stdout), context, args)
    }
}

pub(crate) struct ClearBuiltin;

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
