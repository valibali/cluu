//! `cd` and `pwd` builtins.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;


use libcluu::Result;

use super::registry::CommandContext;
use super::registry::{BuiltinCommand, BuiltinRegistry, WriteSink};

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
            crate::write_stdout(b"cd: too many arguments\n");
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
                let cwd = libcluu::posix::current_dir_string();
                context.set("PWD", cwd);
                context.export_var("PWD");
                context.set_last_status(0);
            }
            Err(errno) => {
                let line = format!("cd: {}: errno {}\n", target, errno);
                crate::write_stdout(line.as_bytes());
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

    fn run_with_sink(
        &self,
        stdout: &WriteSink,
        context: &mut CommandContext,
        args: &[String],
    ) -> Result<()> {
        if !args.is_empty() {
            stdout.write_all(b"pwd: too many arguments\n")?;
            context.set_last_status(1);
            return Ok(());
        }

        let cwd = libcluu::posix::current_dir_string();
        let _ = libcluu::debug_print(&format!("shell: pwd={}\n", cwd));
        let mut line = cwd;
        line.push('\n');
        stdout.write_all(line.as_bytes())?;
        context.set_last_status(0);
        Ok(())
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        self.run_with_sink(&WriteSink::Tty(stdout), context, args)
    }
}
