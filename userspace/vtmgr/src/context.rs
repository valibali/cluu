//! VT Manager context and registry wiring.
//!
//! vtmgr is a pure IPC coordinator. It tracks console and procmgr
//! endpoints, maintains VT lifecycle state, and routes switch/spawn
//! requests between kbd, console, and procmgr.

use alloc::format;
use libcluu::boot::PARAM_TTY_INSTANCE;
use libcluu::boot::{process_info, TOKEN_EXTRA_0};
use libcluu::ipc::{
    send, send_msg_with_payload, COMP_VT_ACTIVATE_LABEL, COMP_VT_DEACTIVATE_LABEL,
    CONSOLE_ACTIVATE_LABEL, CONSOLE_CREATE_VT_LABEL, CONSOLE_DEACTIVATE_LABEL,
    CONSOLE_SWITCH_VT_LABEL, PROCMGR_CONTAINER_RUN_LABEL,
};
use libcluu::registry;
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, yield_cpu, Result};

/// Number of virtual terminals supported (0-3 = console text VTs, 4 = compositor).
pub const VT_COUNT: usize = 5;

/// VT index reserved for the compositor when no explicit pin has been received yet.
/// This default is overridden as soon as the compositor sends VTMGR_PIN_VT_LABEL.
const DEFAULT_COMPOSITOR_VT: usize = 4;

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
    pub active_vt: usize,
    /// Bitmask: bit N = VT N has been created in console.
    vt_created: u8,
    /// Bitmask: bit N = vt:N container spawn was requested from procmgr.
    vt_spawned: u8,
    /// Whether console subscription was requested.
    requested_console: bool,
    /// Whether procmgr spawn subscription was requested.
    requested_procmgr_spawn: bool,
    /// Compositor "control" endpoint (for COMP_VT_ACTIVATE/DEACTIVATE).
    compositor_control: usize,
    /// Whether compositor control subscription was requested.
    requested_compositor: bool,
    /// VT index pinned to the compositor via VTMGR_PIN_VT_LABEL.
    /// Defaults to DEFAULT_COMPOSITOR_VT; updated when compositor sends the pin message.
    compositor_vt: usize,
    /// Compositor "input" endpoint (for forwarding kbd events to compositor).
    compositor_input_ep: usize,
    /// Per-VT tty "main" endpoints (for forwarding kbd events to each tty).
    tty_main_eps: [usize; VT_COUNT],
    /// Whether compositor:input subscription was requested.
    requested_compositor_input: bool,
    /// Bitmask: bit N = tty:N:main subscription was requested.
    requested_tty_main: u8,
    /// Input router: tracks active target and forwards events.
    pub router: crate::input_routing::InputRouter,
}

impl VtmgrContext {
    pub fn new() -> Result<Self> {
        let info = process_info();
        let endpoint = info.tokens[TOKEN_EXTRA_0];

        registry::init("vtmgr")?;
        registry::register_default_outputs()?;
        // Expose a "control" output for kbd to subscribe to.
        registry::register_output("control", endpoint)?;
        // Input event ingress for the router. kbd subscribes here.
        registry::register_output("input", endpoint)?;
        let registry_endpoint = registry::control_endpoint();

        let ctx = Self {
            endpoint,
            registry_endpoint,
            console_endpoint: 0,
            procmgr_spawn_endpoint: 0,
            active_vt: DEFAULT_COMPOSITOR_VT,
            vt_created: 1,
            vt_spawned: 0,
            requested_console: false,
            requested_procmgr_spawn: false,
            compositor_control: 0,
            requested_compositor: false,
            compositor_vt: DEFAULT_COMPOSITOR_VT,
            compositor_input_ep: 0,
            tty_main_eps: [0; VT_COUNT],
            requested_compositor_input: false,
            requested_tty_main: 0,
            router: crate::input_routing::InputRouter::new(),
        };
        debug_print(&format!(
            "vtmgr: ready active_vt={} compositor_vt={}",
            ctx.active_vt, ctx.compositor_vt
        ))?;
        yield_cpu()?;
        Ok(ctx)
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

        // Subscribe to compositor's control endpoint (for VT activate/deactivate).
        if self.compositor_control == 0 && !self.requested_compositor {
            if registry::request_subscription("compositor", "control").is_ok() {
                self.requested_compositor = true;
            }
        }

        // Subscribe to compositor's input endpoint (for forwarding kbd events).
        if !self.requested_compositor_input && self.compositor_input_ep == 0 {
            if registry::request_subscription("compositor", "input").is_ok() {
                self.requested_compositor_input = true;
            }
        }

        // Subscribe to each tty:N's main endpoint (for forwarding kbd events).
        for vt in 0..VT_COUNT {
            let bit = 1u8 << vt;
            if (self.requested_tty_main & bit) == 0 && self.tty_main_eps[vt] == 0 {
                let svc = format!("tty:{}", vt);
                if registry::request_subscription(&svc, "main").is_ok() {
                    self.requested_tty_main |= bit;
                }
            }
        }
    }

    /// Handle registry control messages and update subscriptions.
    pub fn handle_registry_message(&mut self, msg: &Message, payload: &[u8]) {
        if let Ok(Some(event)) = registry::handle_incoming_message(msg, payload) {
            match event {
                registry::RegistryEvent::Grant { service_name, name, token } => {
                    if service_name == "compositor" && name == "input" {
                        self.compositor_input_ep = token;
                        let _ = debug_print("vtmgr: compositor input subscribed");
                    } else if let Some(idx) = service_name.strip_prefix("tty:")
                        .and_then(|s| s.parse::<usize>().ok())
                    {
                        if name == "main" && idx < VT_COUNT {
                            self.tty_main_eps[idx] = token;
                            let _ = debug_print(&format!(
                                "vtmgr: tty:{} main subscribed", idx
                            ));
                        }
                    } else if service_name == "compositor" && name == "control" {
                        self.compositor_control = token;
                        let _ = debug_print("vtmgr: compositor control subscribed");
                        if self.active_vt == self.compositor_vt {
                            let msg = Message::new(
                                COMP_VT_ACTIVATE_LABEL,
                                [0; 6], 0,
                            );
                            let _ = send(self.compositor_control, &msg, IpcFlags::empty());
                            let _ = debug_print("vtmgr: boot COMP_VT_ACTIVATE sent");
                        }
                        use libcluu::input_routing::RoutingTargetKind;
                        let target_kind = if self.active_vt == self.compositor_vt {
                            RoutingTargetKind::Compositor
                        } else {
                            RoutingTargetKind::Tty(self.active_vt as u8)
                        };
                        self.router.set_active(target_kind);
                    } else if name == "control" {
                        // console:0 control endpoint
                        self.console_endpoint = token;
                        let _ = debug_print("vtmgr: console control subscribed");
                        if self.active_vt != 0 {
                            let de = Message::new(
                                CONSOLE_DEACTIVATE_LABEL,
                                [0, 0, 0, 0, 0, 0], 1,
                            );
                            let _ = send(self.console_endpoint, &de, IpcFlags::empty());
                            let _ = debug_print("vtmgr: boot CONSOLE_DEACTIVATE(0) sent");
                        }
                        use libcluu::input_routing::RoutingTargetKind;
                        let target_kind = if self.active_vt == self.compositor_vt {
                            RoutingTargetKind::Compositor
                        } else {
                            RoutingTargetKind::Tty(self.active_vt as u8)
                        };
                        self.router.set_active(target_kind);
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
                        self.requested_compositor = false;
                        self.requested_compositor_input = false;
                        self.requested_tty_main = 0;
                    }
                }
            }
        }
    }

    /// Record a named VT pin sent by a service at startup.
    ///
    /// Updates `compositor_vt` and switches immediately if `active_vt` differs.
    /// Unknown service names are silently ignored.
    ///
    /// PIN_VT also acts as the signal that the bound compositor identity may
    /// have changed (e.g. system→user compositor swap at SESSION_LOGIN). The
    /// registry does not re-Grant existing subscribers when an entry is
    /// replaced, so vtmgr's cached `compositor_control` / `compositor_input_ep`
    /// can outlive the process that minted them. Invalidate both caches and
    /// re-request the subscriptions; the Grant handler then re-emits
    /// COMP_VT_ACTIVATE when `active_vt == compositor_vt`, which is exactly
    /// the scenario the no-op `switch_vt` branch below would otherwise miss.
    pub fn handle_pin_vt(&mut self, vt_index: usize, service_name: &str) {
        if service_name == "compositor" && vt_index < VT_COUNT {
            self.compositor_vt = vt_index;
            let _ = debug_print(&format!(
                "vtmgr: compositor pinned to VT{}",
                vt_index
            ));
            self.compositor_control = 0;
            self.requested_compositor = false;
            self.compositor_input_ep = 0;
            self.requested_compositor_input = false;
            if self.active_vt != vt_index {
                self.switch_vt(vt_index);
            }
        }
    }

    /// Switch to a different virtual terminal.
    ///
    /// The compositor VT index is determined by `compositor_vt` (set via
    /// `VTMGR_PIN_VT_LABEL`, defaulting to `DEFAULT_COMPOSITOR_VT = 4`).
    /// VTs that are not the compositor slot are console-backed text VTs.
    /// On each switch, vtmgr deactivates the old owner and activates the new
    /// one so only one framebuffer writer is live at a time.
    pub fn switch_vt(&mut self, new_vt: usize) {
        if new_vt >= VT_COUNT || new_vt == self.active_vt {
            return;
        }

        let old = self.active_vt;
        let old_is_compositor = old == self.compositor_vt;
        let new_is_compositor = new_vt == self.compositor_vt;

        if old_is_compositor && !new_is_compositor {
            // Compositor → Console: deactivate compositor, reactivate console.
            if self.compositor_control != 0 {
                let msg = Message::new(
                    COMP_VT_DEACTIVATE_LABEL,
                    [0; 6], 0,
                );
                let _ = send(self.compositor_control, &msg, IpcFlags::empty());
            }
            if self.console_endpoint != 0 {
                // Reactivate console at the requested VT index.
                let act = Message::new(
                    CONSOLE_ACTIVATE_LABEL,
                    [new_vt, 0, 0, 0, 0, 0], 1,
                );
                let _ = send(self.console_endpoint, &act, IpcFlags::empty());
                let sw = Message::new(
                    CONSOLE_SWITCH_VT_LABEL,
                    [old, new_vt, 0, 0, 0, 0], 2,
                );
                let _ = send(self.console_endpoint, &sw, IpcFlags::empty());
            }
        } else if !old_is_compositor && new_is_compositor {
            // Console → Compositor: deactivate console, activate compositor.
            if self.console_endpoint != 0 {
                let de = Message::new(
                    CONSOLE_DEACTIVATE_LABEL,
                    [old, 0, 0, 0, 0, 0], 1,
                );
                let _ = send(self.console_endpoint, &de, IpcFlags::empty());
            }
            if self.compositor_control != 0 {
                let msg = Message::new(
                    COMP_VT_ACTIVATE_LABEL,
                    [0; 6], 0,
                );
                let _ = send(self.compositor_control, &msg, IpcFlags::empty());
            }
        } else {
            // Console ↔ Console: create + spawn VT if needed, then switch.
            let vt_bit = 1u8 << new_vt;
            if (self.vt_created & vt_bit) == 0 {
                self.create_vt(new_vt);
            }
            if (self.vt_spawned & vt_bit) == 0 {
                self.spawn_vt_container(new_vt);
            }
            if self.console_endpoint != 0 {
                let msg = Message::new(
                    CONSOLE_SWITCH_VT_LABEL,
                    [old, new_vt, 0, 0, 0, 0], 2,
                );
                let _ = send(self.console_endpoint, &msg, IpcFlags::empty());
            }
        }

        self.active_vt = new_vt;
        use libcluu::input_routing::RoutingTargetKind;
        let target_kind = if new_vt == self.compositor_vt {
            RoutingTargetKind::Compositor
        } else {
            RoutingTargetKind::Tty(new_vt as u8)
        };
        self.router.set_active(target_kind);
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

    pub fn lookup_target_endpoint(&self, kind: libcluu::input_routing::RoutingTargetKind) -> usize {
        use libcluu::input_routing::RoutingTargetKind;
        match kind {
            RoutingTargetKind::None => 0,
            RoutingTargetKind::Compositor => self.compositor_input_ep,
            RoutingTargetKind::Tty(n) => {
                let idx = n as usize;
                if idx < VT_COUNT { self.tty_main_eps[idx] } else { 0 }
            }
        }
    }
}
