//! Keyboard service context and registry wiring.
//!
//! This module isolates registry interaction and endpoint lifecycle so the
//! main loop can focus on decoding scancodes and emitting events.

use libcluu::boot::{process_info, TOKEN_EXTRA_0, TOKEN_EXTRA_1};
use libcluu::ipc::send;
use libcluu::registry;
use libcluu::types::Message;
use libcluu::{debug_print, irq_attach, yield_cpu, Error, Result};

const KEYBOARD_IRQ: usize = 1;

/// Shared state for the keyboard service runtime.
pub struct KbdContext {
    pub endpoint: usize,
    pub registry_endpoint: usize,
    pub tty_endpoint: usize,
    requested_tty: bool,
}

impl KbdContext {
    /// Build the context, initialize registry state, and attach IRQ.
    pub fn new() -> Result<Self> {
        let info = process_info();
        let endpoint = info.tokens[TOKEN_EXTRA_0];
        let irq_token = info.tokens[TOKEN_EXTRA_1];

        registry::init("kbd")?;
        registry::register_default_outputs()?;
        let registry_endpoint = registry::control_endpoint();

        debug_print("kbd: ready")?;
        irq_attach(irq_token, endpoint, KEYBOARD_IRQ)?;
        debug_print("kbd: irq attached")?;
        yield_cpu()?;

        Ok(Self {
            endpoint,
            registry_endpoint,
            tty_endpoint: 0,
            requested_tty: false,
        })
    }

    /// Request a tty subscription if we do not yet have one.
    ///
    /// This keeps the wiring lazy so boot can proceed even if tty is late.
    pub fn ensure_tty_subscription(&mut self) {
        if self.tty_endpoint == 0
            && !self.requested_tty
            && registry::request_subscription("tty:0", "main").is_ok()
        {
            self.requested_tty = true;
        }
    }

    /// Handle registry control messages and update subscriptions.
    ///
    /// This reacts to grant and subscribe status replies on the registry control
    /// endpoint and updates our local state accordingly.
    pub fn handle_registry_message(&mut self, msg: &Message, payload: &[u8]) {
        if let Ok(Some(event)) = registry::handle_incoming_message(msg, payload) {
            match event {
                registry::RegistryEvent::Grant { name, token } => {
                    if name == "main" {
                        self.tty_endpoint = token;
                        let _ = debug_print("kbd: tty subscribed");
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

    /// Send a keyboard event to the tty if subscribed.
    ///
    /// This is a no-op until the registry grants a tty endpoint.
    pub fn send_to_tty(&self, msg: &Message) {
        if self.tty_endpoint != 0 {
            let _ = send(self.tty_endpoint, msg, libcluu::types::IpcFlags::empty());
        }
    }
}

/// Helper for sleeping when the IPC queue is empty or errored.
///
/// This rate-limits error spam and yields to the scheduler.
pub fn idle_on_error(err: Error, saw_error: &mut bool) {
    if err != Error::WouldBlock && !*saw_error {
        *saw_error = true;
        let _ = debug_print("kbd: recv error");
    }
    let _ = yield_cpu();
}
