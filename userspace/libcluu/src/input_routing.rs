// userspace/libcluu/src/input_routing.rs
//! Shared types for input-routing IPC.
//!
//! Router today is vtmgr; tomorrow it will be a dedicated inputd.
//! These types live in libcluu so both ends speak the same dialect
//! regardless of which process is the publisher.

#![allow(dead_code)]

/// Where keystrokes should go for the currently-active VT.
///
/// Used internally by the router (vtmgr today) to pick which output
/// send-token to use for an incoming event. NOT serialised on the
/// wire — the router holds the token table directly.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RoutingTargetKind {
    /// No active target yet (boot, quiesce, transition). Router drops events.
    None,
    /// Forward to the compositor's input endpoint.
    Compositor,
    /// Forward to tty:N's main endpoint. N is the VT index (0..=3).
    Tty(u8),
    /// Forward to a session-local fullscreen endpoint.
    Direct,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DirectRoute {
    pub endpoint: usize,
    pub generation: u64,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DirectRouteError {
    InvalidEndpoint,
    TransitionBusy,
    RouteBusy,
    StaleGeneration,
    NoPendingTransition,
}

impl DirectRouteError {
    pub const fn status_code(self) -> usize {
        match self {
            Self::InvalidEndpoint => 22,
            Self::TransitionBusy | Self::RouteBusy => 16,
            Self::StaleGeneration | Self::NoPendingTransition => 116,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DirectRouteState {
    Idle,
    Pending(DirectRoute),
    Active(DirectRoute),
}

impl DirectRouteState {
    pub const fn new() -> Self {
        Self::Idle
    }

    pub const fn active(self) -> Option<DirectRoute> {
        match self {
            Self::Active(route) => Some(route),
            Self::Idle | Self::Pending(_) => None,
        }
    }

    pub const fn pending(self) -> Option<DirectRoute> {
        match self {
            Self::Pending(route) => Some(route),
            Self::Idle | Self::Active(_) => None,
        }
    }

    pub fn prepare(&mut self, route: DirectRoute) -> Result<(), DirectRouteError> {
        if route.endpoint == 0 {
            return Err(DirectRouteError::InvalidEndpoint);
        }
        match self {
            Self::Idle => {
                *self = Self::Pending(route);
                Ok(())
            }
            Self::Pending(_) => Err(DirectRouteError::TransitionBusy),
            Self::Active(_) => Err(DirectRouteError::RouteBusy),
        }
    }

    pub fn commit(&mut self, generation: u64) -> Result<DirectRoute, DirectRouteError> {
        let Self::Pending(route) = *self else {
            return Err(DirectRouteError::NoPendingTransition);
        };
        if route.generation != generation {
            return Err(DirectRouteError::StaleGeneration);
        }
        *self = Self::Active(route);
        Ok(route)
    }

    pub fn abort(&mut self, generation: u64) -> Result<(), DirectRouteError> {
        let Self::Pending(route) = *self else {
            return Err(DirectRouteError::NoPendingTransition);
        };
        if route.generation != generation {
            return Err(DirectRouteError::StaleGeneration);
        }
        *self = Self::Idle;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROUTE: DirectRoute = DirectRoute { endpoint: 41, generation: 7 };

    #[test]
    fn prepare_does_not_change_active_route_before_commit() {
        let mut state = DirectRouteState::new();

        assert_eq!(state.prepare(ROUTE), Ok(()));
        assert_eq!(state.active(), None);
        assert_eq!(state.pending(), Some(ROUTE));
    }

    #[test]
    fn commit_requires_matching_generation_and_activates_atomically() {
        let mut state = DirectRouteState::new();
        assert_eq!(state.prepare(ROUTE), Ok(()));

        assert_eq!(state.commit(8), Err(DirectRouteError::StaleGeneration));
        assert_eq!(state.pending(), Some(ROUTE));
        assert_eq!(state.commit(7), Ok(ROUTE));
        assert_eq!(state.active(), Some(ROUTE));
        assert_eq!(state.pending(), None);
    }

    #[test]
    fn abort_requires_matching_generation_and_restores_idle_state() {
        let mut state = DirectRouteState::new();
        assert_eq!(state.prepare(ROUTE), Ok(()));

        assert_eq!(state.abort(8), Err(DirectRouteError::StaleGeneration));
        assert_eq!(state.pending(), Some(ROUTE));
        assert_eq!(state.abort(7), Ok(()));
        assert_eq!(state, DirectRouteState::Idle);
    }

    #[test]
    fn conflicting_prepare_is_rejected_without_replacing_pending_route() {
        let mut state = DirectRouteState::new();
        assert_eq!(state.prepare(ROUTE), Ok(()));

        assert_eq!(
            state.prepare(DirectRoute { endpoint: 42, generation: 8 }),
            Err(DirectRouteError::TransitionBusy)
        );
        assert_eq!(state.pending(), Some(ROUTE));
    }
}
