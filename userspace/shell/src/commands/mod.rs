//! Shell command dispatch module hierarchy.
//!
//! The full builtin registry and execution machinery lives here.
//! Stage 2 of Plan A: commands.rs split into focused submodules.

pub mod builtins;
pub mod completion;
pub mod exec;
pub mod redirect;

// Re-export BuiltinRegistry for callers like shellrc.rs.
pub use crate::commands_old::BuiltinRegistry;

// Re-export the remaining public surface from the old monolithic module
// while the migration is in progress.  All items will be removed from
// commands_old once fully migrated into the new sub-files.
pub use crate::commands_old::{
    build_redir_actions,
    poll_background_jobs,
    render_word_public,
    spawn_process_with_argv_and_redirs,
    BuiltinFactory,
    CommandContext,
    CommandExecutor,
    ExecResult,
};
