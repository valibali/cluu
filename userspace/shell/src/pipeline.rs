//! Pipeline executor — turns a multi-command Pipeline AST into spawn calls
//! wired with pipes between stages.
//!
//! See `docs/superpowers/specs/2026-04-27-pipes-design.md` §6.
//! Single-command pipelines stay on the existing single-command path in
//! `commands.rs`. This module owns the multi-command (`a | b | c ...`) case.

use cluu_lang::ast::Pipeline;

use crate::commands::CommandContext;
use libcluu::Result;

/// Walk the parsed `Pipeline` and execute each stage with stdin/stdout
/// wired to a fresh pipe between adjacent commands.
pub struct PipelineExecutor;

impl PipelineExecutor {
    /// Run a multi-command pipeline.
    ///
    /// Caller is responsible for routing single-command pipelines through
    /// the existing single-command path; this entry point accepts any
    /// pipeline length but is a no-op when `commands.len() < 2`.
    ///
    /// Returns the exit status of the LAST command (POSIX default; `set -o
    /// pipefail` is deferred per spec §10.2).
    pub fn run(
        _stdout: usize,
        _context: &mut CommandContext,
        pipeline: &Pipeline,
    ) -> Result<i32> {
        if pipeline.commands.len() < 2 {
            return Ok(0);
        }
        // Filled in by PI19b.
        Ok(0)
    }
}
