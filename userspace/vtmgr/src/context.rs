//! VT Manager context and registry wiring.
//!
//! vtmgr is a pure IPC coordinator. It tracks console and procmgr
//! endpoints, maintains VT lifecycle state, and routes switch/spawn
//! requests between kbd, console, and procmgr.

use alloc::format;
use libcluu::boot::PARAM_TTY_INSTANCE;
use libcluu::boot::{process_info, TOKEN_EXTRA_0};
use libcluu::ipc::{
    send, send_msg_with_payload, CONSOLE_ACTIVATE_LABEL, CONSOLE_CREATE_VT_LABEL,
    CONSOLE_DEACTIVATE_LABEL, CONSOLE_SWITCH_VT_LABEL, PROCMGR_CONTAINER_RUN_LABEL,
};
use libcluu::registry;
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, yield_cpu, Result};

/// Number of virtual terminals supported.
const VT_COUNT: usize = 4;

/// Shared state for the VT manager runtime.
pub struct VtmgrContext {
    /// Listen endpoint for incoming messages (from kbd).
    pub endpoint: usize,
    /// Registry control endpoint for subscription management.
    pub registry_endpoint: usize,
    /// Console "write" endpoint (single console process).
    console_endpoint: usize,
    /// Procmgr "spawn" endpoint for tty spawn requests.
    procmgr_spawn_endpoint: usize,
    /// Currently active VT index.
    active_vt: usize,
    /// Bitmask: bit N = VT N has been created in console.
    vt_created: u8,
    /// Bitmask: bit N = vt:N container spawn was requested from procmgr.
    vt_spawned: u8,
    /// Whether console subscription was requested.
    requested_console: bool,
    /// Whether procmgr spawn subscription was requested.
    requested_procmgr_spawn: bool,
}

impl VtmgrContext {
    pub fn new() -> Result<Self> {
        let info = process_info();
        let endpoint = info.tokens[TOKEN_EXTRA_0];

        registry::init("vtmgr")?;
        registry::register_default_outputs()?;
        // Expose a "control" output for kbd to subscribe to.
        registry::register_output("control", endpoint)?;
        let registry_endpoint = registry::control_endpoint();

        debug_print("vtmgr: ready")?;
        yield_cpu()?;

        Ok(Self {
            endpoint,
            registry_endpoint,
            console_endpoint: 0,
            procmgr_spawn_endpoint: 0,
            active_vt: 0,
            vt_created: 1,  // VT 0 console buffer is created at boot by console:0
            vt_spawned: 0, // vt:0 will be spawned by us once procmgr is available
            requested_console: false,
            requested_procmgr_spawn: false,
        })
    }

    /// Request subscriptions for services we need.
    pub fn ensure_subscriptions(&mut self) {
        // Subscribe to console:0's control endpoint (for sending CREATE_VT / ACTIVATE / DEACTIVATE).
        if self.console_endpoint == 0 && !self.requested_console {
            if registry::request_subscription("console:0", "control").is_ok() {
                self.requested_console = true;
            }
        }

        // Subscribe to procmgr's spawn endpoint (for sending service spawn requests).
        if self.procmgr_spawn_endpoint == 0 && !self.requested_procmgr_spawn {
            if registry::request_subscription("procmgr", "spawn").is_ok() {
                self.requested_procmgr_spawn = true;
            }
        }
    }

    /// Handle registry control messages and update subscriptions.
    pub fn handle_registry_message(&mut self, msg: &Message, payload: &[u8]) {
        if let Ok(Some(event)) = registry::handle_incoming_message(msg, payload) {
            match event {
                registry::RegistryEvent::Grant { service_name: _, name, token } => {
                    if name == "control" {
                        self.console_endpoint = token;
                        let _ = debug_print("vtmgr: console control subscribed");
                    } else if name == "spawn" {
                        self.procmgr_spawn_endpoint = token;
                        let _ = debug_print("vtmgr: procmgr spawn subscribed");
                        // Spawn vt:0 now that we can talk to procmgr.
                        if (self.vt_spawned & 1) == 0 {
                            self.spawn_vt_container(0);
                        }
                    }
                }
                registry::RegistryEvent::SubscribeStatus { code } => {
                    if code != 0 {
                        self.requested_console = false;
                        self.requested_procmgr_spawn = false;
                    }
                }
            }
        }
    }

    /// Switch to a different virtual terminal.
    ///
    /// If the target VT doesn't exist yet, creates it in console and spawns
    /// a vt:N container via procmgr.
    pub fn switch_vt(&mut self, new_vt: usize) {
        if new_vt >= VT_COUNT || new_vt == self.active_vt {
            return;
        }

        // Create VT buffer in console if not yet done.
        let vt_bit = 1u8 << new_vt;
        if (self.vt_created & vt_bit) == 0 {
            self.create_vt(new_vt);
        }

        // Spawn vt:N container if not yet done.
        if (self.vt_spawned & vt_bit) == 0 {
            self.spawn_vt_container(new_vt);
        }

        let old = self.active_vt;

        // Atomic VT switch: single message, no intermediate state.
        if self.console_endpoint != 0 {
            let msg = Message::new(CONSOLE_SWITCH_VT_LABEL, [old, new_vt, 0, 0, 0, 0], 2);
            let _ = send(self.console_endpoint, &msg, IpcFlags::empty());
        }

        self.active_vt = new_vt;
        let _ = debug_print(&format!("vtmgr: vt switch {} -> {}", old, new_vt));
    }

    /// Ask console to create a new VT buffer.
    fn create_vt(&mut self, vt_index: usize) {
        if self.console_endpoint == 0 {
            let _ = debug_print("vtmgr: no console endpoint for create_vt");
            return;
        }
        let msg = Message::new(CONSOLE_CREATE_VT_LABEL, [vt_index, 0, 0, 0, 0, 0], 1);
        let _ = send(self.console_endpoint, &msg, IpcFlags::empty());
        self.vt_created |= 1u8 << vt_index;
        let _ = debug_print(&format!("vtmgr: created vt {}", vt_index));
    }

    /// Ask procmgr to spawn a VT container for the given VT index.
    fn spawn_vt_container(&mut self, vt_index: usize) {
        if self.procmgr_spawn_endpoint == 0 {
            let _ = debug_print("vtmgr: no procmgr spawn endpoint");
            return;
        }

        // Payload: "vt\0" + 1 param override (10 bytes)
        let mut payload = [0u8; 3 + 10];
        payload[0] = b'v';
        payload[1] = b't';
        payload[2] = 0;
        // Param override: PARAM_TTY_INSTANCE = vt_index
        let param_index = (PARAM_TTY_INSTANCE as u16).to_le_bytes();
        let param_value = (vt_index as u64).to_le_bytes();
        payload[3..5].copy_from_slice(&param_index);
        payload[5..13].copy_from_slice(&param_value);

        let msg = Message::new(
            PROCMGR_CONTAINER_RUN_LABEL,
            [payload.len(), 0, 0, 3, 1, 0],
            5,
        );
        let _ = send_msg_with_payload(self.procmgr_spawn_endpoint, &msg, &payload);
        self.vt_spawned |= 1u8 << vt_index;
        let _ = debug_print(&format!("vtmgr: requested vt:{} container", vt_index));
    }
}
