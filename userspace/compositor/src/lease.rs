#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseHandle {
    pub lease_id: u64,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseError {
    Unregistered,
    StaleGeneration,
    AlreadySuspended,
    NotSuspended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseState {
    Unregistered,
    Active(LeaseHandle),
    Suspended(LeaseHandle),
}

impl LeaseState {
    pub const fn new() -> Self {
        Self::Unregistered
    }

    pub const fn register(handle: LeaseHandle) -> Self {
        Self::Active(handle)
    }

    pub fn suspend(&mut self, handle: LeaseHandle) -> Result<(), LeaseError> {
        match *self {
            Self::Active(current) if current == handle => {
                *self = Self::Suspended(current);
                Ok(())
            }
            Self::Active(_) => Err(LeaseError::StaleGeneration),
            Self::Suspended(_) => Err(LeaseError::AlreadySuspended),
            Self::Unregistered => Err(LeaseError::Unregistered),
        }
    }

    pub fn resume(&mut self, handle: LeaseHandle) -> Result<(), LeaseError> {
        match *self {
            Self::Suspended(current) if current == handle => {
                *self = Self::Active(current);
                Ok(())
            }
            Self::Suspended(_) => Err(LeaseError::StaleGeneration),
            Self::Active(_) => Err(LeaseError::NotSuspended),
            Self::Unregistered => Err(LeaseError::Unregistered),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LeaseError, LeaseHandle, LeaseState};

    const HANDLE: LeaseHandle = LeaseHandle { lease_id: 7, generation: 11 };
    const STALE: LeaseHandle = LeaseHandle { lease_id: 7, generation: 12 };

    #[test]
    fn suspend_then_resume_accepts_current_generation() {
        let mut state = LeaseState::register(HANDLE);

        assert_eq!(state.suspend(HANDLE), Ok(()));
        assert_eq!(state.resume(HANDLE), Ok(()));
        assert_eq!(state, LeaseState::Active(HANDLE));
    }

    #[test]
    fn stale_generation_cannot_suspend_or_resume() {
        let mut state = LeaseState::register(HANDLE);

        assert_eq!(state.suspend(STALE), Err(LeaseError::StaleGeneration));
        assert_eq!(state.suspend(HANDLE), Ok(()));
        assert_eq!(state.resume(STALE), Err(LeaseError::StaleGeneration));
    }

    #[test]
    fn repeated_suspend_does_not_change_state() {
        let mut state = LeaseState::register(HANDLE);

        assert_eq!(state.suspend(HANDLE), Ok(()));
        assert_eq!(state.suspend(HANDLE), Err(LeaseError::AlreadySuspended));
        assert_eq!(state, LeaseState::Suspended(HANDLE));
    }
}
