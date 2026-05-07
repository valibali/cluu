//! `cd` and `pwd` builtins.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;

use libcluu::ipc::{send_with_payload, TTY_WRITE_LABEL};
use libcluu::Result;

use super::registry::CommandContext;
use super::registry::{BuiltinCommand, BuiltinRegistry};

pub fn register(registry: &mut BuiltinRegistry) {
    registry.register(Box::new(CdBuiltin));
    registry.register(Box::new(PwdBuiltin));
}

pub(crate) struct CdBuiltin;

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

pub(crate) struct PwdBuiltin;

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
        let _ = libcluu::debug_print(&format!("shell: pwd={}\n", cwd));
        let mut line = cwd;
        line.push('\n');
        send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
        context.set_last_status(0);
        Ok(())
    }
}
