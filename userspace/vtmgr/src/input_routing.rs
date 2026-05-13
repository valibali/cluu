// userspace/vtmgr/src/input_routing.rs
//! Input router. Holds active target; forwards each KBD_EVENT to the
//! caller-resolved endpoint. Updated by `context::switch_vt` after every
//! transition.
//!
//! Future inputd extraction lifts this module ~verbatim; only the
//! registry output name changes from "vtmgr:input" to "inputd:input".

use libcluu::input_routing::RoutingTargetKind;
use libcluu::ipc::send;
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, yield_cpu};
use libcluu::error::Error;
use core::sync::atomic::{AtomicBool, Ordering};

const SEND_RETRY_BOUND: usize = 8;
static FIRST_DROP_LOGGED: AtomicBool = AtomicBool::new(false);

pub struct InputRouter {
    active: RoutingTargetKind,
}

impl InputRouter {
    pub const fn new() -> Self {
        Self { active: RoutingTargetKind::None }
    }

    pub fn set_active(&mut self, target: RoutingTargetKind) {
        if self.active != target {
            let _ = debug_print(&alloc::format!(
                "vtmgr: router target {:?} -> {:?}",
                self.active, target
            ));
            self.active = target;
        }
    }

    /// Forward `msg` to the endpoint resolved from the current active
    /// target. `lookup_endpoint` keeps context.rs internals out of this
    /// module. Returns true if a send was attempted.
    pub fn forward(
        &self,
        msg: &Message,
        lookup_endpoint: impl FnOnce(RoutingTargetKind) -> usize,
    ) -> bool {
        let ep = lookup_endpoint(self.active);
        if ep == 0 {
            return false;
        }
        for _ in 0..SEND_RETRY_BOUND {
            match send(ep, msg, IpcFlags::empty()) {
                Ok(()) => return true,
                Err(Error::WouldBlock) | Err(Error::Busy) => {
                    let _ = yield_cpu();
                    continue;
                }
                Err(_) => return false,
            }
        }
        if !FIRST_DROP_LOGGED.swap(true, Ordering::Relaxed) {
            let _ = debug_print("vtmgr: dropped keystroke (target backlog persistent)");
        }
        false
    }

    /// Modal lock placeholder. Today: allows always.
    pub fn should_allow_switch(&self, _from: u8, _to: u8) -> bool {
        true
    }
}
