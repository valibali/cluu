//! `echo` builtin.

use alloc::boxed::Box;
use alloc::string::String;

use libcluu::ipc::{send_with_payload, TTY_WRITE_LABEL};
use libcluu::Result;

use crate::commands_old::CommandContext;
use super::registry::{BuiltinCommand, BuiltinRegistry};

pub fn register(registry: &mut BuiltinRegistry) {
    registry.register(Box::new(EchoBuiltin));
}

pub(crate) struct EchoBuiltin;

impl BuiltinCommand for EchoBuiltin {
    fn name(&self) -> &'static str {
        "echo"
    }

    fn run(&self, stdout: usize, _context: &mut CommandContext, args: &[String]) -> Result<()> {
        let output = join_words(args);
        send_with_payload(stdout, TTY_WRITE_LABEL, output.as_bytes())?;
        send_with_payload(stdout, TTY_WRITE_LABEL, b"\n")?;
        Ok(())
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
