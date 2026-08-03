// userspace/vtmgr/src/input_routing.rs
//! Input router. Holds active target; forwards each KBD_EVENT to the
//! caller-resolved endpoint. Updated by `context::switch_vt` after every
//! transition.
//!
//! Future inputd extraction lifts this module ~verbatim; only the
//! registry output name changes from "vtmgr:input" to "inputd:input".

use libcluu::input_routing::{
    DirectRoute, DirectRouteError, DirectRouteState, RoutingTargetKind,
};
use libcluu::ipc::send;
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, yield_cpu};
use libcluu::error::Error;
use core::sync::atomic::{AtomicBool, Ordering};

const SEND_RETRY_BOUND: usize = 8;
static FIRST_DROP_LOGGED: AtomicBool = AtomicBool::new(false);

pub struct InputRouter {
    active: RoutingTargetKind,
    direct: DirectRouteState,
    return_pending: Option<u64>,
}

impl InputRouter {
    pub const fn new() -> Self {
        Self { active: RoutingTargetKind::None, direct: DirectRouteState::new(), return_pending: None }
    }

    pub fn set_active(&mut self, target: RoutingTargetKind) {
        if self.direct.active().is_some() || self.direct.pending().is_some() {
            return;
        }
        if self.active != target {
            let _ = debug_print(&alloc::format!(
                "vtmgr: router target {:?} -> {:?}",
                self.active, target
            ));
            self.active = target;
        }
    }

    pub fn prepare_direct(
        &mut self,
        endpoint: usize,
        generation: u64,
    ) -> Result<(), DirectRouteError> {
        self.direct.prepare(DirectRoute { endpoint, generation })
    }

    pub fn commit_direct(&mut self, generation: u64) -> Result<(), DirectRouteError> {
        self.direct.commit(generation)?;
        self.active = RoutingTargetKind::Direct;
        Ok(())
    }

    pub fn abort_direct(&mut self, generation: u64) -> Result<(), DirectRouteError> {
        self.direct.abort(generation)
    }

    pub fn prepare_return(&mut self, generation: u64) -> Result<(), DirectRouteError> {
        if self.direct.active().is_none() {
            return Err(DirectRouteError::NoPendingTransition);
        }
        if self.return_pending.is_some() {
            return Err(DirectRouteError::TransitionBusy);
        }
        self.return_pending = Some(generation);
        Ok(())
    }

    pub fn commit_return(&mut self, generation: u64) -> Result<(), DirectRouteError> {
        if self.return_pending != Some(generation) {
            return Err(DirectRouteError::StaleGeneration);
        }
        self.return_pending = None;
        self.direct = DirectRouteState::new();
        self.active = RoutingTargetKind::Compositor;
        Ok(())
    }

    #[cfg(test)]
    pub const fn active_target(&self) -> RoutingTargetKind {
        self.active
    }

    fn target_endpoint(
        &self,
        lookup_endpoint: impl FnOnce(RoutingTargetKind) -> usize,
    ) -> usize {
        match self.active {
            RoutingTargetKind::Direct => self.direct.active().map_or(0, |route| route.endpoint),
            target => lookup_endpoint(target),
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
        self.forward_with(msg, lookup_endpoint, |ep, message| {
            send(ep, message, IpcFlags::empty())
        })
    }

    pub fn forward_with(
        &self,
        msg: &Message,
        lookup_endpoint: impl FnOnce(RoutingTargetKind) -> usize,
        mut send_message: impl FnMut(usize, &Message) -> Result<(), Error>,
    ) -> bool {
        let ep = self.target_endpoint(lookup_endpoint);
        if ep == 0 {
            return false;
        }
        for _ in 0..SEND_RETRY_BOUND {
            match send_message(ep, msg) {
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
        self.direct.active().is_none() && self.direct.pending().is_none()
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;
    use libcluu::ipc::{KBD_EVENT_LABEL, MOUSE_EVENT_LABEL};

    #[test]
    fn prepare_direct_keeps_legacy_route_until_commit() {
        let mut router = InputRouter::new();
        router.set_active(RoutingTargetKind::Compositor);

        assert_eq!(router.prepare_direct(41, 7), Ok(()));
        assert_eq!(router.active_target(), RoutingTargetKind::Compositor);
        assert!(!router.should_allow_switch(4, 0));

        assert_eq!(router.commit_direct(7), Ok(()));
        assert_eq!(router.active_target(), RoutingTargetKind::Direct);
    }

    #[test]
    fn direct_transition_rejects_stale_and_conflicting_operations() {
        let mut router = InputRouter::new();

        assert_eq!(router.prepare_direct(41, 7), Ok(()));
        assert_eq!(router.prepare_direct(42, 8), Err(DirectRouteError::TransitionBusy));
        assert_eq!(router.commit_direct(8), Err(DirectRouteError::StaleGeneration));
        assert_eq!(router.abort_direct(8), Err(DirectRouteError::StaleGeneration));
        assert_eq!(router.commit_direct(7), Ok(()));
        assert_eq!(router.prepare_direct(42, 8), Err(DirectRouteError::RouteBusy));
    }

    #[test]
    fn abort_direct_leaves_legacy_route_unchanged() {
        let mut router = InputRouter::new();
        router.set_active(RoutingTargetKind::Tty(2));

        assert_eq!(router.prepare_direct(41, 7), Ok(()));
        assert_eq!(router.abort_direct(7), Ok(()));
        assert_eq!(router.active_target(), RoutingTargetKind::Tty(2));
        assert!(router.should_allow_switch(2, 3));
    }

    #[test]
    fn keyboard_and_mouse_forward_to_active_direct_endpoint() {
        let mut router = InputRouter::new();
        assert_eq!(router.prepare_direct(41, 7), Ok(()));
        assert_eq!(router.commit_direct(7), Ok(()));
        let keyboard = Message::new(KBD_EVENT_LABEL, [1, 2, 3, 4, 0, 0], 4);
        let mouse = Message::new(MOUSE_EVENT_LABEL, [5, 6, 7, 0, 0, 0], 3);
        let mut sent = Vec::new();

        assert!(router.forward_with(&keyboard, |_| 99, |endpoint, message| {
            sent.push((endpoint, message.tag.label));
            Ok(())
        }));
        assert!(router.forward_with(&mouse, |_| 99, |endpoint, message| {
            sent.push((endpoint, message.tag.label));
            Ok(())
        }));

        assert_eq!(sent, [(41, KBD_EVENT_LABEL), (41, MOUSE_EVENT_LABEL)]);
    }
}
