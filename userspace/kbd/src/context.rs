//! Keyboard service context and registry wiring.
//!
//! This module isolates registry interaction and endpoint lifecycle so the
//! main loop can focus on decoding scancodes and emitting events.
//!
//! All keyboard events are forwarded to vtmgr:input; vtmgr owns routing to
//! the active VT's tty or compositor. VT switch combos are sent to
//! vtmgr:control as VTMGR_REQUEST_VT_SWITCH_LABEL requests.

use alloc::format;
use libcluu::boot::{process_info, TOKEN_EXTRA_0, TOKEN_EXTRA_1};
use libcluu::ipc::{send, VTMGR_REQUEST_VT_SWITCH_LABEL};
use libcluu::registry;
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, irq_attach, yield_cpu, Error, Result};

const KEYBOARD_IRQ: usize = 1;
/// Number of virtual terminals supported.
pub const VT_COUNT: usize = 5;

/// Shared state for the keyboard service runtime.
pub struct KbdContext {
    pub endpoint: usize,
    pub registry_endpoint: usize,
    /// vtmgr "input" endpoint — every keystroke goes here.
    pub vtmgr_input_ep: usize,
    /// Whether we've requested the vtmgr:input subscription.
    requested_vtmgr_input: bool,
    /// vtmgr "control" endpoint for VT switch requests.
    pub vtmgr_control_ep: usize,
    /// Whether we've requested the vtmgr:control subscription.
    requested_vtmgr_control: bool,
    /// procmgr "spawn" endpoint for shutdown requests.
    procmgr_endpoint: usize,
    /// Whether we've requested the procmgr subscription.
    requested_procmgr: bool,
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
            vtmgr_input_ep: 0,
            requested_vtmgr_input: false,
            vtmgr_control_ep: 0,
            requested_vtmgr_control: false,
            procmgr_endpoint: 0,
            requested_procmgr: false,
        })
    }

    /// Request subscriptions for services we need.
    pub fn ensure_subscriptions(&mut self) {
        // Subscribe to vtmgr:input — all keystrokes go here.
        if !self.requested_vtmgr_input && self.vtmgr_input_ep == 0 {
            if registry::request_subscription("vtmgr", "input").is_ok() {
                self.requested_vtmgr_input = true;
            }
        }

        // Subscribe to vtmgr:control — VT switch requests go here.
        if !self.requested_vtmgr_control && self.vtmgr_control_ep == 0 {
            if registry::request_subscription("vtmgr", "control").is_ok() {
                self.requested_vtmgr_control = true;
            }
        }

        // Subscribe to procmgr:spawn for shutdown combo.
        if self.procmgr_endpoint == 0 && !self.requested_procmgr {
            if registry::request_subscription("procmgr", "spawn").is_ok() {
                self.requested_procmgr = true;
            }
        }
    }

    /// Handle registry control messages and update subscriptions.
    pub fn handle_registry_message(&mut self, msg: &Message, payload: &[u8]) {
        if let Ok(Some(event)) = registry::handle_incoming_message(msg, payload) {
            match event {
                registry::RegistryEvent::Grant { service_name, name, token } => {
                    if service_name == "vtmgr" && name == "input" {
                        self.vtmgr_input_ep = token;
                        let _ = debug_print("kbd: vtmgr:input subscribed");
                    } else if service_name == "vtmgr" && name == "control" {
                        self.vtmgr_control_ep = token;
                        let _ = debug_print("kbd: vtmgr:control subscribed");
                    } else if service_name == "procmgr" && name == "spawn" {
                        self.procmgr_endpoint = token;
                        let _ = debug_print("kbd: procmgr spawn subscribed");
                    }
                }
                registry::RegistryEvent::SubscribeStatus { code } => {
                    if code != 0 {
                        self.requested_vtmgr_input = false;
                        self.requested_vtmgr_control = false;
                    }
                }
            }
        }
    }

    /// Request a VT switch via vtmgr:control.
    pub fn request_vt_switch(&self, new_vt: usize) {
        if new_vt >= VT_COUNT { return; }
        if self.vtmgr_control_ep == 0 { return; }
        let msg = Message::new(
            VTMGR_REQUEST_VT_SWITCH_LABEL,
            [new_vt, 0, 0, 0, 0, 0],
            1,
        );
        let _ = send(self.vtmgr_control_ep, &msg, IpcFlags::empty());
        let _ = debug_print(&format!("kbd: requested vt switch -> {}", new_vt));
    }

    /// Send a shutdown request to procmgr.
    pub fn send_shutdown(&self) {
        if self.procmgr_endpoint == 0 {
            let _ = debug_print("kbd: shutdown combo but no procmgr endpoint");
            return;
        }
        let _ = debug_print("kbd: Ctrl+Alt+Del — sending shutdown to procmgr");
        let msg = Message::new(libcluu::ipc::PROCMGR_SHUTDOWN_LABEL, [0, 0, 0, 0, 0, 0], 1);
        let _ = send(self.procmgr_endpoint, &msg, IpcFlags::empty());
    }

    /// Send a scroll command.
    ///
    /// TODO(v2): route scroll via vtmgr or query active VT from kernel.
    /// Scroll is non-essential to login flow.
    pub fn send_scroll(&self, _direction: usize) {
        // TODO(v2): route scroll via vtmgr or query active VT from kernel.
        // Scroll is non-essential to login flow.
    }

    /// Forward a keyboard event to vtmgr:input (the single routing point).
    ///
    /// Retries up to 8 times on WouldBlock/Busy before dropping with a
    /// one-shot diagnostic log.
    pub fn send_to_router(&self, msg: &Message) {
        if self.vtmgr_input_ep == 0 { return; }
        for _ in 0..8 {
            match send(self.vtmgr_input_ep, msg, IpcFlags::empty()) {
                Ok(()) => return,
                Err(Error::WouldBlock) | Err(Error::Busy) => {
                    let _ = yield_cpu();
                    continue;
                }
                Err(_) => return,
            }
        }
        static FIRST_DROP_LOGGED: core::sync::atomic::AtomicBool =
            core::sync::atomic::AtomicBool::new(false);
        if !FIRST_DROP_LOGGED.swap(true, core::sync::atomic::Ordering::Relaxed) {
            let _ = debug_print("kbd: dropped keystroke (vtmgr backlog persistent)");
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
