use alloc::boxed::Box;
use alloc::string::String;

use libcluu::Result;

use super::registry::CommandContext;
use super::registry::{BuiltinCommand, BuiltinRegistry, WriteSink};

pub fn register(registry: &mut BuiltinRegistry) {
    registry.register(Box::new(DprintBuiltin));
}

pub(crate) struct DprintBuiltin;

impl BuiltinCommand for DprintBuiltin {
    fn name(&self) -> &'static str {
        "dprint"
    }

    fn run_with_sink(
        &self,
        _stdout: &WriteSink,
        _context: &mut CommandContext,
        args: &[String],
    ) -> Result<()> {
        let mut out = String::new();
        for (idx, word) in args.iter().enumerate() {
            if idx != 0 {
                out.push(' ');
            }
            out.push_str(word);
        }
        libcluu::debug_print(&out)?;
        Ok(())
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        self.run_with_sink(&WriteSink::Tty(stdout), context, args)
    }
}
