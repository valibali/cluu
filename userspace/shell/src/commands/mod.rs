//! Shell command dispatch module hierarchy.
//!
//! The full builtin registry and execution machinery lives here.
//! Stage 2 of Plan A (cont.): commands_old.rs drained and deleted.

pub mod builtins;
pub mod completion;
pub mod exec;
pub mod redirect;

// Re-export the public surface so callers use `crate::commands::X`.
pub use builtins::registry::{
    BuiltinFactory,
    BuiltinRegistry,
    CommandContext,
    CommandExecutor,
    ExecResult,
    WriteSink,
};
pub use exec::spawn_process_with_argv_and_redirs;
pub use redirect::{build_redir_actions, render_word_public};
