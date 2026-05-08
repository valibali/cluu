//! `alias` / `unalias` builtins.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};

use libcluu::Result;

use super::registry::CommandContext;
use super::registry::{BuiltinCommand, BuiltinRegistry, WriteSink};

pub fn register(registry: &mut BuiltinRegistry) {
    registry.register(Box::new(AliasBuiltin));
    registry.register(Box::new(UnaliasBuiltin));
}

pub(crate) struct AliasBuiltin;

impl BuiltinCommand for AliasBuiltin {
    fn name(&self) -> &'static str {
        "alias"
    }

    fn run_with_sink(
        &self,
        stdout: &WriteSink,
        context: &mut CommandContext,
        args: &[String],
    ) -> Result<()> {
        if args.is_empty() {
            // Print all aliases.
            let pairs: alloc::vec::Vec<_> = context
                .aliases
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            for (k, v) in pairs {
                let line = format!("alias {}='{}'\n", k, v);
                let _ = libcluu::debug_print(line.trim_end());
                stdout.write_all(line.as_bytes())?;
            }
            return Ok(());
        }
        for a in args {
            if let Some(eq) = a.find('=') {
                let name = &a[..eq];
                let val = a[eq + 1..]
                    .trim_matches(|c| c == '\'' || c == '"')
                    .to_string();
                context.aliases.insert(name.to_string(), val);
            } else {
                match context.aliases.get(a.as_str()) {
                    Some(v) => {
                        let line = format!("alias {}='{}'\n", a, v);
                        let _ = libcluu::debug_print(line.trim_end());
                        stdout.write_all(line.as_bytes())?;
                    }
                    None => {
                        let line = format!("alias: {}: not found\n", a);
                        let _ = libcluu::debug_print(line.trim_end());
                        stdout.write_all(line.as_bytes())?;
                    }
                }
            }
        }
        Ok(())
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        self.run_with_sink(&WriteSink::Tty(stdout), context, args)
    }
}

pub(crate) struct UnaliasBuiltin;

impl BuiltinCommand for UnaliasBuiltin {
    fn name(&self) -> &'static str {
        "unalias"
    }

    fn run_with_sink(
        &self,
        stdout: &WriteSink,
        context: &mut CommandContext,
        args: &[String],
    ) -> Result<()> {
        if args.is_empty() {
            stdout.write_all(b"unalias: usage: unalias NAME...\n")?;
            return Ok(());
        }
        for a in args {
            context.aliases.remove(a.as_str());
        }
        Ok(())
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        self.run_with_sink(&WriteSink::Tty(stdout), context, args)
    }
}
