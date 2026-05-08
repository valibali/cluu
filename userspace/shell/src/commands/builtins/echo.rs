//! `echo` builtin.

use alloc::boxed::Box;
use alloc::string::String;

use libcluu::Result;

use super::registry::CommandContext;
use super::registry::{BuiltinCommand, BuiltinRegistry, WriteSink};

pub fn register(registry: &mut BuiltinRegistry) {
    registry.register(Box::new(EchoBuiltin));
}

pub(crate) struct EchoBuiltin;

impl BuiltinCommand for EchoBuiltin {
    fn name(&self) -> &'static str {
        "echo"
    }

    fn run_with_sink(
        &self,
        stdout: &WriteSink,
        _context: &mut CommandContext,
        args: &[String],
    ) -> Result<()> {
        let output = join_words(args);
        stdout.write_all(output.as_bytes())?;
        stdout.write_all(b"\n")?;
        Ok(())
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        self.run_with_sink(&WriteSink::Tty(stdout), context, args)
    }
}

fn join_words(words: &[String]) -> String {
    let mut out = String::new();
    for (idx, word) in words.iter().enumerate() {
        if idx != 0 {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}
