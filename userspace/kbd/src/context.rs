//! Keyboard service context and registry wiring.
//!
//! This module isolates registry interaction and endpoint lifecycle so the
//! main loop can focus on decoding scancodes and emitting events.
//!
//! Multi-VT support: kbd tracks tty endpoints for each VT and routes
//! keyboard events to the active VT's tty. VT switching is delegated
//! to vtmgr via VTMGR_SWITCH_VT_LABEL.
//!
//! On-demand spawning: only VT0 exists at boot. When the user switches to
//! a VT that has no tty endpoint, kbd sends VTMGR_SWITCH_VT_LABEL to
//! vtmgr, which orchestrates VT creation via console and procmgr.

use alloc::format;
use libcluu::boot::{process_info, TOKEN_EXTRA_0, TOKEN_EXTRA_1};
use libcluu::ipc::{send, VTMGR_SWITCH_VT_LABEL};
use libcluu::registry;
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, irq_attach, yield_cpu, Error, Result};

const KEYBOARD_IRQ: usize = 1;
/// Number of virtual terminals supported.
pub const VT_COUNT: usize = 4;

/// Shared state for the keyboard service runtime.
pub struct KbdContext {
    pub endpoint: usize,
    pub registry_endpoint: usize,
    /// Per-VT tty endpoints (0 = not yet subscribed).
    pub tty_endpoints: [usize; VT_COUNT],
    /// Index of the currently active virtual terminal.
    pub active_vt: usize,
    /// Bitmask: bit N set means subscription for tty:N was requested.
    requested_tty: u8,
    /// vtmgr "control" endpoint for VT switch requests.
    vtmgr_endpoint: usize,
    /// Whether we've requested the vtmgr subscription.
    requested_vtmgr: bool,
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
            tty_endpoints: [0; VT_COUNT],
            active_vt: 0,
            requested_tty: 0,
            vtmgr_endpoint: 0,
            requested_vtmgr: false,
        })
    }

    /// Request subscriptions for services we need.
    ///
    /// VT0 tty is always launched by init, so we eagerly subscribe to it.
    /// vtmgr is subscribed for VT switch requests.
    pub fn ensure_subscriptions(&mut self) {
        // Always try to subscribe to VT0 tty (launched by init).
        self.try_subscribe_tty(0);

        // Subscribe to vtmgr's control endpoint for VT switch requests.
        if self.vtmgr_endpoint == 0 && !self.requested_vtmgr {
            if registry::request_subscription("vtmgr", "control").is_ok() {
                self.requested_vtmgr = true;
            }
        }

        // Subscribe to any additional VT ttys that may have been spawned.
        for i in 1..VT_COUNT {
            self.try_subscribe_tty(i);
        }
    }

    /// Try to subscribe to tty:N if not already done.
    fn try_subscribe_tty(&mut self, i: usize) {
        let bit = 1u8 << i;
        if self.tty_endpoints[i] == 0 && (self.requested_tty & bit) == 0 {
            let name = format!("tty:{}", i);
            if registry::request_subscription(&name, "main").is_ok() {
                self.requested_tty |= bit;
            }
        }
    }

    /// Handle registry control messages and update subscriptions.
    ///
    /// Grants for "main" are matched to tty:N, grants for "control" to vtmgr.
    pub fn handle_registry_message(&mut self, msg: &Message, payload: &[u8]) {
        if let Ok(Some(event)) = registry::handle_incoming_message(msg, payload) {
            match event {
                registry::RegistryEvent::Grant { service_name, name, token } => {
                    if name == "main" {
                        // tty grant — use service name to determine VT index.
                        if let Some(idx) = parse_tty_index(&service_name) {
                            if idx < VT_COUNT {
                                self.tty_endpoints[idx] = token;
                                let _ = debug_print(&format!("kbd: tty:{} subscribed", idx));
                            }
                        }
                    } else if name == "control" {
                        self.vtmgr_endpoint = token;
                        let _ = debug_print("kbd: vtmgr control subscribed");
                    }
                }
                registry::RegistryEvent::SubscribeStatus { code } => {
                    if code != 0 {
                        self.requested_tty = 0;
                        self.requested_vtmgr = false;
                    }
                }
            }
        }
    }

    /// Switch to a different virtual terminal.
    ///
    /// Delegates the actual switch to vtmgr. kbd updates its local active_vt
    /// immediately so keyboard events go to the right tty.
    pub fn switch_vt(&mut self, new_vt: usize) {
        if new_vt >= VT_COUNT || new_vt == self.active_vt {
            return;
        }

        // Tell vtmgr to handle the switch.
        if self.vtmgr_endpoint != 0 {
            let msg = Message::new(VTMGR_SWITCH_VT_LABEL, [new_vt, 0, 0, 0, 0, 0], 1);
            let _ = send(self.vtmgr_endpoint, &msg, IpcFlags::empty());
        }

        let old = self.active_vt;
        self.active_vt = new_vt;
        let _ = debug_print(&format!("kbd: vt switch {} -> {}", old, new_vt));
    }

    /// Send a keyboard event to the active VT's tty.
    ///
    /// Falls back to the first available tty if the active one is not ready.
    pub fn send_to_tty(&self, msg: &Message) {
        let ep = self.tty_endpoints[self.active_vt];
        if ep != 0 {
            let _ = send(ep, msg, IpcFlags::empty());
            return;
        }
        // Graceful fallback: try any available tty endpoint.
        for &ep in &self.tty_endpoints {
            if ep != 0 {
                let _ = send(ep, msg, IpcFlags::empty());
                return;
            }
        }
    }

}

/// Parse a VT index from a service name like "tty:0" → Some(0).
fn parse_tty_index(service_name: &str) -> Option<usize> {
    service_name.strip_prefix("tty:").and_then(|s| s.parse().ok())
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
