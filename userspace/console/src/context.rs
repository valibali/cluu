//! Console service context and registry wiring.
//!
//! This module owns endpoint creation, registry naming, and timer-driven
//! event loop integration so rendering code stays focused on drawing.

use alloc::format;
use libcluu::boot::{process_info, PARAM_CONSOLE_INSTANCE, TOKEN_PROC_CAP};
use libcluu::registry;
use libcluu::{debug_print, syscall, Result};

// Token index for listen endpoint (set by init).
const SVC_TOKEN_LISTEN: usize = 6;

/// Shared console context.
pub struct ConsoleContext {
    pub endpoint: usize,
    pub registry_endpoint: usize,
    #[allow(dead_code)]
    pub instance_id: u64,
}

impl ConsoleContext {
    /// Initialize registry wiring and resolve the listen endpoint.
    pub fn new() -> Result<Self> {
        let info = process_info();
        let proc_cap = info.tokens[TOKEN_PROC_CAP];
        // Prefer a fresh endpoint created from proc_cap so the console can grant
        // send-only tokens to subscribers via the registry.
        let endpoint = if proc_cap != 0 {
            match syscall::endpoint_create(proc_cap) {
                Ok(token) => token,
                Err(_) => info.tokens[SVC_TOKEN_LISTEN],
            }
        } else {
            info.tokens[SVC_TOKEN_LISTEN]
        };

        let instance_id = info.params[PARAM_CONSOLE_INSTANCE] as u64;
        let service_name = format!("console:{}", instance_id);
        registry::init(&service_name)?;
        registry::register_default_outputs()?;
        // Expose the console write input for tty to subscribe to.
        registry::register_output("write", endpoint)?;
        let registry_endpoint = registry::control_endpoint();

        debug_print(&format!("console: instance {} ready", instance_id))?;

        Ok(Self {
            endpoint,
            registry_endpoint,
            instance_id,
        })
    }
}
