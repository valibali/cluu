//! Registry wiring and event forwarding for usb-input.
//!
//! Subscribes to inputd:input so decoded HID events reach the same
//! routing path as PS/2 kbd/mouse events (inputd → vtmgr → tty/compositor).

use alloc::format;
use libcluu::boot::{process_info, TOKEN_EXTRA_0};
use libcluu::ipc::{send, VTMGR_REQUEST_VT_SWITCH_LABEL, PROCMGR_SHUTDOWN_LABEL};
use libcluu::registry;
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, Error, Result};

pub const VT_COUNT: usize = 5;

pub struct UsbInputContext {
    pub endpoint: usize,
    pub registry_endpoint: usize,
    pub inputd_ep: usize,
    requested_inputd: bool,
    pub vtmgr_control_ep: usize,
    requested_vtmgr_control: bool,
    pub procmgr_spawn_ep: usize,
    requested_procmgr: bool,
}

impl UsbInputContext {
    pub fn new() -> Result<Self> {
        let info = process_info();
        let endpoint = info.tokens[TOKEN_EXTRA_0];

        registry::init("usb-input")?;
        registry::register_default_outputs()?;
        let registry_endpoint = registry::control_endpoint();

        Ok(Self {
            endpoint,
            registry_endpoint,
            inputd_ep: 0,
            requested_inputd: false,
            vtmgr_control_ep: 0,
            requested_vtmgr_control: false,
            procmgr_spawn_ep: 0,
            requested_procmgr: false,
        })
    }

    pub fn ensure_subscriptions(&mut self) {
        if !self.requested_inputd && self.inputd_ep == 0 {
            if registry::request_subscription("inputd", "input").is_ok() {
                self.requested_inputd = true;
            }
        }
        if !self.requested_vtmgr_control && self.vtmgr_control_ep == 0 {
            if registry::request_subscription("vtmgr", "control").is_ok() {
                self.requested_vtmgr_control = true;
            }
        }
        if !self.requested_procmgr && self.procmgr_spawn_ep == 0 {
            if registry::request_subscription("root-procmgr", "spawn").is_ok() {
                self.requested_procmgr = true;
            }
        }
    }

    pub fn handle_registry_message(&mut self, msg: &Message, payload: &[u8]) {
        if let Ok(Some(event)) = registry::handle_incoming_message(msg, payload) {
            match event {
                registry::RegistryEvent::Grant { service_name, name, token } => {
                    if service_name == "inputd" && name == "input" {
                        self.inputd_ep = token;
                        let _ = debug_print("usb-input: inputd:input subscribed");
                    } else if service_name == "vtmgr" && name == "control" {
                        self.vtmgr_control_ep = token;
                        let _ = debug_print("usb-input: vtmgr:control subscribed");
                    } else if service_name == "root-procmgr" && name == "spawn" {
                        self.procmgr_spawn_ep = token;
                        let _ = debug_print("usb-input: procmgr spawn subscribed");
                    }
                }
                registry::RegistryEvent::SubscribeStatus { code } => {
                    if code != 0 {
                        self.requested_inputd = false;
                        self.requested_vtmgr_control = false;
                        self.requested_procmgr = false;
                    }
                }
            }
        }
    }

    pub fn forward(&self, msg: &Message) {
        if self.inputd_ep == 0 {
            return;
        }
        for _ in 0..8 {
            match send(self.inputd_ep, msg, IpcFlags::empty()) {
                Ok(()) => return,
                Err(Error::WouldBlock) | Err(Error::Busy) => {
                    let _ = libcluu::yield_cpu();
                    continue;
                }
                Err(_) => return,
            }
        }
        let _ = debug_print("usb-input: dropped event (inputd backlog)");
    }

    pub fn request_vt_switch(&self, new_vt: usize) {
        if new_vt >= VT_COUNT || self.vtmgr_control_ep == 0 {
            return;
        }
        let msg = Message::new(
            VTMGR_REQUEST_VT_SWITCH_LABEL,
            [new_vt, 0, 0, 0, 0, 0],
            1,
        );
        let _ = send(self.vtmgr_control_ep, &msg, IpcFlags::empty());
        let _ = debug_print(&format!("usb-input: requested vt switch -> {}", new_vt));
    }

    pub fn send_shutdown(&self) {
        if self.procmgr_spawn_ep == 0 {
            let _ = debug_print("usb-input: shutdown combo but no procmgr endpoint");
            return;
        }
        let _ = debug_print("usb-input: Ctrl+Alt+Del — sending shutdown to procmgr");
        let msg = Message::new(PROCMGR_SHUTDOWN_LABEL, [0, 0, 0, 0, 0, 0], 1);
        let _ = send(self.procmgr_spawn_ep, &msg, IpcFlags::empty());
    }
}
