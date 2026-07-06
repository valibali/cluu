use alloc::format;
use libcluu::boot::{process_info, TOKEN_EXTRA_0, TOKEN_EXTRA_1};
use libcluu::ipc::send;
use libcluu::registry;
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, irq_attach, yield_cpu, Error, Result};

const MOUSE_IRQ: usize = 12;

pub struct MouseContext {
    pub endpoint: usize,
    pub registry_endpoint: usize,
    pub vtmgr_input_ep: usize,
    requested_vtmgr_input: bool,
}

impl MouseContext {
    pub fn new() -> Result<Self> {
        let info = process_info();
        let endpoint = info.tokens[TOKEN_EXTRA_0];
        let irq_token = info.tokens[TOKEN_EXTRA_1];

        registry::init("mouse")?;
        registry::register_default_outputs()?;
        let registry_endpoint = registry::control_endpoint();

        debug_print("mouse: ready")?;
        irq_attach(irq_token, endpoint, MOUSE_IRQ)?;
        debug_print("mouse: irq12 attached")?;
        yield_cpu()?;

        Ok(Self {
            endpoint,
            registry_endpoint,
            vtmgr_input_ep: 0,
            requested_vtmgr_input: false,
        })
    }

    pub fn ensure_subscriptions(&mut self) {
        if !self.requested_vtmgr_input && self.vtmgr_input_ep == 0 {
            if registry::request_subscription("inputd", "input").is_ok() {
                self.requested_vtmgr_input = true;
            }
        }
    }

    pub fn handle_registry_message(&mut self, msg: &Message, payload: &[u8]) {
        if let Ok(Some(event)) = registry::handle_incoming_message(msg, payload) {
            match event {
                registry::RegistryEvent::Grant { service_name, name, token } => {
                    if service_name == "inputd" && name == "input" {
                        self.vtmgr_input_ep = token;
                        let _ = debug_print("mouse: inputd:input subscribed");
                    }
                }
                registry::RegistryEvent::SubscribeStatus { code } => {
                    if code != 0 {
                        self.requested_vtmgr_input = false;
                    }
                }
            }
        }
    }

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
        let _ = debug_print("mouse: dropped event (vtmgr backlog)");
    }
}
