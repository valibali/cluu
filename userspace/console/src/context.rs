//! Console service context and registry wiring.
//!
//! This module owns endpoint creation, registry naming, and timer-driven
//! event loop integration so rendering code stays focused on drawing.

use alloc::format;
use libcluu::boot::{process_info, PARAM_CONSOLE_INSTANCE, TOKEN_EXTRA_0, TOKEN_IPC};
use libcluu::registry;
use libcluu::{debug_print, syscall, Result};

/// Shared console context.
pub struct ConsoleContext {
    pub endpoint: usize,
    pub registry_endpoint: usize,
    pub instance_id: u64,
    /// TTY endpoint for sending credit refill notifications.
    pub tty_endpoint: usize,
    requested_tty: bool,
    /// Bytes rendered since last credit refill was sent.
    pub bytes_since_refill: usize,
}

impl ConsoleContext {
    /// Initialize registry wiring and resolve the listen endpoint.
    pub fn new() -> Result<Self> {
        let info = process_info();
        let ipc_cap = info.tokens[TOKEN_IPC];
        // Prefer a fresh endpoint created from TOKEN_IPC so the console can grant
        // send-only tokens to subscribers via the registry.
        let endpoint = if ipc_cap != 0 {
            match syscall::endpoint_create(ipc_cap) {
                Ok(token) => token,
                Err(_) => info.tokens[TOKEN_EXTRA_0],
            }
        } else {
            info.tokens[TOKEN_EXTRA_0]
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
            tty_endpoint: 0,
            requested_tty: false,
            bytes_since_refill: 0,
        })
    }

    /// Request TTY subscription if not already done.
    pub fn request_subscriptions(&mut self) {
        if self.tty_endpoint == 0 && !self.requested_tty {
            let tty_name = format!("tty:{}", self.instance_id);
            if registry::request_subscription(&tty_name, "main").is_ok() {
                self.requested_tty = true;
            }
        }
    }

    /// Handle registry grant events.
    pub fn handle_registry_event(&mut self, msg: &libcluu::types::Message, payload: &[u8]) {
        if let Ok(Some(event)) = registry::handle_incoming_message(msg, payload) {
            match event {
                registry::RegistryEvent::Grant { name, token } => {
                    if name == "main" {
                        self.tty_endpoint = token;
                        let _ = debug_print("console: tty subscribed for credit refills");
                    }
                }
                registry::RegistryEvent::SubscribeStatus { code } => {
                    if code != 0 {
                        self.requested_tty = false;
                    }
                }
            }
        }
    }

    /// Record rendered bytes and send a credit refill to TTY when threshold is reached.
    pub fn record_rendered_bytes(&mut self, count: usize) {
        self.bytes_since_refill += count;
        if self.bytes_since_refill >= REFILL_THRESHOLD && self.tty_endpoint != 0 {
            let refill = self.bytes_since_refill;
            self.bytes_since_refill = 0;
            let msg = libcluu::types::Message::new(
                libcluu::ipc::CONSOLE_CREDIT_REFILL_LABEL,
                [refill, 0, 0, 0, 0, 0],
                1,
            );
            let _ = libcluu::ipc::send(
                self.tty_endpoint,
                &msg,
                libcluu::types::IpcFlags::empty(),
            );
        }
    }
}

/// Credit refill threshold: half the TTY credit window (256 * 4 / 2 = 512 bytes).
const REFILL_THRESHOLD: usize = 512;
