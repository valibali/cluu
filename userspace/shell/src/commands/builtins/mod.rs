//! Shell builtin command sub-modules.
//!
//! Each file owns a cohesive group of builtins and exposes a `register`
//! function that adds them to a `BuiltinRegistry`.  The top-level
//! `register_all` assembles all groups.

pub mod registry;
pub mod cd;
pub mod echo;
pub mod env;
pub mod alias;
pub mod jobs;
pub mod history;
pub mod help;
pub mod exit;
pub mod arith;
pub mod sudo;
pub mod container;

use registry::BuiltinRegistry;

/// Register every builtin group into `registry`.
pub fn register_all(registry: &mut BuiltinRegistry) {
    cd::register(registry);
    echo::register(registry);
    env::register(registry);
    alias::register(registry);
    jobs::register(registry);
    history::register(registry);
    help::register(registry);
    exit::register(registry);
    arith::register(registry);
    sudo::register(registry);
    container::register(registry);
}
