//! inputd context — registry wiring + event buffering + VFS registration.

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::format;
use alloc::vec::Vec;
use libcluu::boot::{process_info, TOKEN_EXTRA_0};
use libcluu::ipc::{send, send_msg_with_payload, reply_to_sender_with_payload, VFS_REGISTER_DEV_LABEL};
use libcluu::registry;
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, yield_cpu, Error, Result};

const KBD_BUF_SIZE: usize = 64;
const MOUSE_BUF_SIZE: usize = 64;

pub struct InputdContext {
    pub endpoint: usize,
    pub registry_endpoint: usize,
    vtmgr_input_ep: usize,
    requested_vtmgr_input: bool,
    requested_vfs_register: bool,
    registered_with_vfs: bool,
    vfs_endpoint: usize,
    kbd_buf: VecDeque<Vec<u8>>,
    mouse_buf: VecDeque<Vec<u8>>,
}

impl InputdContext {
    pub fn new() -> Result<Self> {
        let info = process_info();
        let endpoint = info.tokens[TOKEN_EXTRA_0];

        registry::init("inputd")?;
        registry::register_default_outputs()?;
        registry::register_output("input", endpoint)?;
        let registry_endpoint = registry::control_endpoint();

        let ctx = Self {
            endpoint,
            registry_endpoint,
            vtmgr_input_ep: 0,
            requested_vtmgr_input: false,
            requested_vfs_register: false,
            registered_with_vfs: false,
            vfs_endpoint: 0,
            kbd_buf: VecDeque::new(),
            mouse_buf: VecDeque::new(),
        };

        let _ = debug_print("inputd: ready");
        yield_cpu()?;
        Ok(ctx)
    }

    pub fn ensure_subscriptions(&mut self) {
        if !self.requested_vtmgr_input && self.vtmgr_input_ep == 0 {
            if registry::request_subscription("vtmgr", "input").is_ok() {
                self.requested_vtmgr_input = true;
            }
        }

        if !self.requested_vfs_register && self.vfs_endpoint == 0 {
            if registry::request_subscription("vfs", "main").is_ok() {
                self.requested_vfs_register = true;
            }
        }

        if self.vfs_endpoint != 0 && !self.registered_with_vfs {
            self.register_devices_with_vfs();
        }
    }

    pub fn handle_registry_message(&mut self, msg: &Message, payload: &[u8]) {
        match registry::handle_incoming_message(msg, payload) {
            Ok(Some(event)) => {
                match event {
                    registry::RegistryEvent::Grant { service_name, name, token } => {
                        if service_name == "vtmgr" && name == "input" {
                            self.vtmgr_input_ep = token;
                            let _ = debug_print("inputd: vtmgr:input subscribed");
                        } else if service_name == "vfs" && name == "main" {
                            self.vfs_endpoint = token;
                            let _ = debug_print("inputd: vfs:main subscribed");
                        }
                    }
                    registry::RegistryEvent::SubscribeStatus { code } => {
                        if code != 0 {
                            self.requested_vtmgr_input = false;
                            self.requested_vfs_register = false;
                        }
                    }
                }
            }
            Ok(None) => {}
            Err(err) => {
                let _ = debug_print(&format!(
                    "inputd: registry error {:?} label={:#x}",
                    err, msg.tag.label
                ));
            }
        }
    }

    pub fn buffer_kbd(&mut self, data: Vec<u8>) {
        if self.kbd_buf.len() >= KBD_BUF_SIZE {
            self.kbd_buf.pop_front();
        }
        self.kbd_buf.push_back(data);
    }

    pub fn buffer_mouse(&mut self, data: Vec<u8>) {
        if self.mouse_buf.len() >= MOUSE_BUF_SIZE {
            self.mouse_buf.pop_front();
        }
        self.mouse_buf.push_back(data);
    }

    pub fn forward_to_vtmgr(&self, msg: &Message) {
        if self.vtmgr_input_ep == 0 {
            return;
        }
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
    }

    pub fn handle_read_request(&mut self, msg: &Message) {
        let device_kind = msg.words[0];
        let data = match device_kind {
            0 => self.kbd_buf.pop_front(),
            1 => self.mouse_buf.pop_front(),
            _ => None,
        };

        let reply_msg = Message::new(
            libcluu::ipc::DEV_READ_REQUEST_LABEL,
            [data.as_ref().map_or(0, |d| d.len()), 0, 0, 0, 0, 0],
            1,
        );

        match &data {
            Some(d) => {
                let _ = reply_to_sender_with_payload(msg, &reply_msg, d, self.endpoint);
            }
            None => {
                let _ = reply_to_sender_with_payload(msg, &reply_msg, &[], self.endpoint);
            }
        }
    }

    fn register_devices_with_vfs(&mut self) {
        let dev_paths = ["/dev/input/kbd\0", "/dev/input/mouse\0"];
        for (idx, path) in dev_paths.iter().enumerate() {
            let path_bytes = path.as_bytes();
            let msg = Message::new(
                VFS_REGISTER_DEV_LABEL,
                [idx, self.endpoint, path_bytes.len(), 0, 0, 0],
                3,
            );
            let _ = send_msg_with_payload(self.vfs_endpoint, &msg, path_bytes);
        }
        let _ = debug_print("inputd: registered /dev/input/* with VFS");
        self.registered_with_vfs = true;
    }
}
