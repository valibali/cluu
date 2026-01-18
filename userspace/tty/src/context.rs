//! TTY runtime context and registry wiring.
//!
//! This module owns endpoint creation, registry subscription state, and
//! buffered output so the main loop can focus on routing and discipline.

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;
use libcluu::boot::{process_info, PARAM_TTY_INSTANCE, TOKEN_PROC_CAP};
use libcluu::ipc::{send_with_payload, CONSOLE_WRITE_LABEL};
use libcluu::registry;
use libcluu::{debug_print, yield_cpu, Result};

// Token indices (set by init).
const SVC_TOKEN_LISTEN: usize = 7;
const CONSOLE_MAX_PAYLOAD: usize = 256;

/// TTY context shared by the main loop.
pub struct TtyContext {
    pub endpoint: usize,
    pub registry_endpoint: usize,
    pub console_endpoint: usize,
    pub shell_stdin: usize,
    requested_console: bool,
    requested_shell: bool,
    pending_console_output: Vec<u8>,
}

impl TtyContext {
    /// Initialize registry wiring and select the listen endpoint.
    pub fn new() -> Result<Self> {
        let info = process_info();
        let proc_cap = info.tokens[TOKEN_PROC_CAP];
        // Prefer a fresh endpoint created from proc_cap so tty can grant send-only
        // tokens to subscribers via the registry.
        let endpoint = if proc_cap != 0 {
            match libcluu::syscall::endpoint_create(proc_cap) {
                Ok(token) => token,
                Err(_) => info.tokens[SVC_TOKEN_LISTEN],
            }
        } else {
            info.tokens[SVC_TOKEN_LISTEN]
        };

        let instance_id = info.params[PARAM_TTY_INSTANCE] as u64;
        let service_name = format!("tty:{}", instance_id);
        registry::init(&service_name)?;
        registry::register_default_outputs()?;
        // Expose the tty input for kbd/shell subscriptions.
        registry::register_output("main", endpoint)?;
        let registry_endpoint = registry::control_endpoint();

        debug_print(&format!(
            "tty: endpoint {} registry {}",
            endpoint, registry_endpoint
        ))?;
        debug_print("tty: ready")?;
        yield_cpu()?;

        Ok(Self {
            endpoint,
            registry_endpoint,
            console_endpoint: 0,
            shell_stdin: 0,
            requested_console: false,
            requested_shell: false,
            pending_console_output: Vec::new(),
        })
    }

    /// Request console and shell subscriptions if they are missing.
    pub fn request_subscriptions(&mut self) {
        if self.console_endpoint == 0
            && !self.requested_console
            && registry::request_subscription("console:0", "write").is_ok()
        {
            self.requested_console = true;
        }
        if self.shell_stdin == 0
            && !self.requested_shell
            && registry::request_subscription("shell", "stdin").is_ok()
        {
            self.requested_shell = true;
        }
    }

    /// Handle registry control traffic and update subscriptions.
    pub fn handle_registry_event(&mut self, msg: &libcluu::types::Message, payload: &[u8]) {
        if let Ok(Some(event)) = registry::handle_incoming_message(msg, payload) {
            match event {
                registry::RegistryEvent::Grant { name, token } => {
                    if name == "write" {
                        self.console_endpoint = token;
                        let _ = debug_print("tty: console subscribed");
                        self.flush_pending_console();
                    } else if name == "stdin" {
                        self.shell_stdin = token;
                        let _ = debug_print("tty: shell stdin subscribed");
                    }
                }
                registry::RegistryEvent::SubscribeStatus { code } => {
                    if code != 0 {
                        self.requested_console = false;
                        self.requested_shell = false;
                    }
                }
            }
        }
    }

    /// Forward output to the console or buffer it until the console is ready.
    pub fn forward_to_console(&mut self, payload: &[u8]) {
        if self.console_endpoint != 0 {
            self.send_to_console(payload);
        } else if self.pending_console_output.len() + payload.len() <= 2048 {
            // Keep a small buffer so early shell output is not lost.
            self.pending_console_output.extend_from_slice(payload);
        }
    }

    /// Flush any pending console output once the console is subscribed.
    pub fn flush_pending_console(&mut self) {
        if self.console_endpoint == 0 || self.pending_console_output.is_empty() {
            return;
        }
        self.send_to_console(&self.pending_console_output);
        self.pending_console_output.clear();
    }

    fn send_to_console(&self, payload: &[u8]) {
        for chunk in payload.chunks(CONSOLE_MAX_PAYLOAD) {
            let _ = send_with_payload(self.console_endpoint, CONSOLE_WRITE_LABEL, chunk);
        }
    }
}
