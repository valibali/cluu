//! Redirect parsing helpers and word rendering.
//!
//! Canonical home of `build_redir_actions`, `render_word`, and
//! `render_word_public`, moved from the old monolithic `commands.rs`.

use alloc::string::String;
use alloc::vec::Vec;

use cluu_lang::ast::{DqPart, Redir, RedirOp, Word, WordPart};
use libcluu::ipc::RedirAction;

use crate::commands::builtins::registry::CommandContext;

/// Expand a single AST `Word` into its string value, substituting shell
/// variables from `context`.
pub(crate) fn render_word(context: &CommandContext, word: &Word) -> String {
    let mut out = String::new();
    for part in &word.parts {
        match part {
            WordPart::Bare(text) => out.push_str(text),
            WordPart::SingleQuoted(text) => out.push_str(text),
            WordPart::DoubleQuoted(parts) => {
                for dq in parts {
                    match dq {
                        DqPart::Text(text) => out.push_str(text),
                        DqPart::Escaped(text) => out.push_str(text),
                        DqPart::Var(name) => out.push_str(context.get(name).unwrap_or("")),
                        DqPart::CmdSub(_) => out.push_str(""),
                    }
                }
            }
            WordPart::Var(name) => out.push_str(context.get(name).unwrap_or("")),
            WordPart::CmdSub(_) => {}
        }
    }
    out
}

/// Public wrapper around `render_word` for use by `pipeline.rs`.
pub fn render_word_public(context: &CommandContext, word: &Word) -> String {
    render_word(context, word)
}

/// Convert AST `Redir` entries into `RedirAction` values for the REDIR trailer.
/// Callers should first check that there are no conflicts with pipe-wired fds.
pub fn build_redir_actions(context: &CommandContext, redirs: &[Redir]) -> Vec<RedirAction> {
    let mut actions = Vec::with_capacity(redirs.len());
    for r in redirs {
        let target = render_word(context, &r.target);
        let (target_fd, flags) = match r.op {
            RedirOp::OutTrunc => (1u8, 1u8),
            RedirOp::OutAppend => (1u8, 2u8),
            RedirOp::In => (0u8, 3u8),
            RedirOp::ErrTrunc => (2u8, 1u8),
        };
        actions.push(RedirAction { target_fd, flags, path: target });
    }
    actions
}
