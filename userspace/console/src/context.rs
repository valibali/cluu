//! Console service context and registry wiring.
//!
//! This module owns endpoint creation, registry naming, and timer-driven
//! event loop integration so rendering code stays focused on drawing.
//!
//! # Per-VT Endpoints
//!
//! Each console instance creates one endpoint per VT plus a separate control
//! endpoint.  The receiving endpoint index identifies the VT — no
//! sender-reported VT index is needed, eliminating confused-deputy attacks.

use alloc::format;
use libcluu::boot::{process_info, PARAM_CONSOLE_INSTANCE, TOKEN_IPC};
use libcluu::registry;
use libcluu::{debug_print, syscall, Result};

/// Number of virtual terminals supported.
pub const VT_COUNT: usize = 4;

/// Shared console context.
pub struct ConsoleContext {
    /// Per-VT write endpoints: index N = endpoint for VT N.
    pub vt_endpoints: [usize; VT_COUNT],
    /// Separate control endpoint for vtmgr lifecycle commands.
    pub control_endpoint: usize,
    pub registry_endpoint: usize,
    pub instance_id: u64,
    /// TTY endpoint for sending credit refill notifications.
    pub tty_endpoint: usize,
    requested_tty: bool,
    /// Bytes rendered since last credit refill was sent.
    pub bytes_since_refill: usize,
}

impl ConsoleContext {
    /// Initialize registry wiring and create per-VT + control endpoints.
    pub fn new() -> Result<Self> {
        let info = process_info();
        let ipc_cap = info.tokens[TOKEN_IPC];
        let instance_id = info.params[PARAM_CONSOLE_INSTANCE] as u64;
        let service_name = format!("console:{}", instance_id);
        registry::init(&service_name)?;
        registry::register_default_outputs()?;

        // Create per-VT write endpoints.
        let mut vt_endpoints = [0usize; VT_COUNT];
        for i in 0..VT_COUNT {
            vt_endpoints[i] = syscall::endpoint_create(ipc_cap)?;
            let name = format!("vt:{}", i);
            registry::register_output(&name, vt_endpoints[i])?;
        }

        // Legacy "write" output mapped to VT 0 for backward compatibility.
        registry::register_output("write", vt_endpoints[0])?;

        // Separate control endpoint for vtmgr lifecycle commands.
        let control_endpoint = syscall::endpoint_create(ipc_cap)?;
        registry::register_output("control", control_endpoint)?;

        let registry_endpoint = registry::control_endpoint();

        debug_print(&format!("console: instance {} ready", instance_id))?;

        Ok(Self {
            vt_endpoints,
            control_endpoint,
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
                registry::RegistryEvent::Grant { service_name: _, name, token } => {
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
