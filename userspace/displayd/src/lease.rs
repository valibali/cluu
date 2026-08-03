use cluu_wire::display::{Error, LeaseAcquire, LeaseGranted, LeaseHandle, LeaseOwner};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompositorRegistration {
    pub endpoint: usize,
    pub space_token: usize,
    pub target_va: usize,
    pub resource_token: u64,
}

impl CompositorRegistration {
    pub const fn new(
        endpoint: usize,
        space_token: usize,
        target_va: usize,
        resource_token: u64,
    ) -> Self {
        Self {
            endpoint,
            space_token,
            target_va,
            resource_token,
        }
    }
}

pub fn bind_compositor(
    binding: &mut Option<CompositorRegistration>,
    requested: CompositorRegistration,
) -> Result<CompositorRegistration, Error> {
    match *binding {
        Some(current) if current == requested => Ok(current),
        Some(_) => Err(Error::InvalidCapability),
        None => {
            *binding = Some(requested);
            Ok(requested)
        }
    }
}

pub trait LeaseIo {
    fn clear_for_compositor(&mut self) -> Result<(), Error>;
    fn prepare_acquire(
        &mut self,
        lease: LeaseGranted,
        request: Option<LeaseAcquire>,
    ) -> Result<(), Error>;
    fn prepare_release(&mut self, lease: LeaseGranted) -> Result<(), Error>;
    fn complete_release(&mut self, lease: LeaseGranted) -> Result<(), Error>;
    fn restore_compositor(
        &mut self,
        lease: LeaseGranted,
        resource_token: u64,
    ) -> Result<(), Error>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseState {
    Idle,
    Active(LeaseGranted),
    Revoking(LeaseGranted),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseOutcome {
    AwaitingAcknowledgement,
    Released,
    AlreadyReleased,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TerminalLease {
    granted: LeaseGranted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InternalState {
    Idle(Option<TerminalLease>),
    Active(LeaseGranted),
    Revoking(LeaseGranted),
}

pub struct LeaseCoordinator {
    state: InternalState,
    next_lease_id: u64,
    next_generation: u64,
    fullscreen_commit_diag: Option<LeaseHandle>,
    compositor_lease: Option<LeaseGranted>,
}

impl LeaseCoordinator {
    pub const fn new() -> Self {
        Self {
            state: InternalState::Idle(None),
            next_lease_id: 1,
            next_generation: 1,
            fullscreen_commit_diag: None,
            compositor_lease: None,
        }
    }

    pub fn state(&self) -> LeaseState {
        match self.state {
            InternalState::Idle(_) => LeaseState::Idle,
            InternalState::Active(granted) => LeaseState::Active(granted),
            InternalState::Revoking(granted) => LeaseState::Revoking(granted),
        }
    }

    pub fn active_lease(&self) -> Option<LeaseGranted> {
        match self.state {
            InternalState::Active(granted) => Some(granted),
            InternalState::Idle(_) | InternalState::Revoking(_) => None,
        }
    }

    /// Mark first commit for active fullscreen lease for bounded diagnostics.
    pub fn mark_fullscreen_commit_diag(&mut self, handle: LeaseHandle) -> bool {
        if self.fullscreen_commit_diag == Some(handle) {
            return false;
        }
        let is_active_fullscreen = matches!(
            self.state,
            InternalState::Active(granted)
                if granted.owner == LeaseOwner::Fullscreen && granted.handle == handle
        );
        if !is_active_fullscreen {
            return false;
        }
        self.fullscreen_commit_diag = Some(handle);
        true
    }

    pub fn register_compositor<I: LeaseIo>(
        &mut self,
        io: &mut I,
    ) -> Result<LeaseGranted, Error> {
        let granted = self.acquire(io, LeaseOwner::Compositor, None)?;
        self.compositor_lease = Some(granted);
        Ok(granted)
    }

    pub fn acquire_fullscreen<I: LeaseIo>(
        &mut self,
        io: &mut I,
        request: LeaseAcquire,
    ) -> Result<LeaseGranted, Error> {
        if let InternalState::Active(granted) = self.state {
            if granted.owner == LeaseOwner::Compositor {
                self.state = InternalState::Revoking(granted);
                if let Err(error) = io.prepare_release(granted) {
                    return Err(error);
                }
                if let Err(error) = io.complete_release(granted) {
                    self.state = InternalState::Revoking(granted);
                    return Err(error);
                }
                self.state = InternalState::Idle(Some(TerminalLease { granted }));
            }
        }
        self.acquire(io, LeaseOwner::Fullscreen, Some(request))
    }

    pub fn release<I: LeaseIo>(
        &mut self,
        io: &mut I,
        handle: LeaseHandle,
    ) -> Result<ReleaseOutcome, Error> {
        match self.state {
            InternalState::Idle(terminal) => {
                if terminal.is_some_and(|entry| entry.granted.handle == handle) {
                    Ok(ReleaseOutcome::AlreadyReleased)
                } else {
                    Err(Error::StaleLease)
                }
            }
            InternalState::Active(granted) => {
                validate_handle(granted, handle)?;
                self.state = InternalState::Revoking(granted);
                if let Err(error) = io.prepare_release(granted) {
                    return Err(error);
                }
                Ok(ReleaseOutcome::AwaitingAcknowledgement)
            }
            InternalState::Revoking(granted) => {
                validate_handle(granted, handle)?;
                Ok(ReleaseOutcome::AwaitingAcknowledgement)
            }
        }
    }

    pub fn acknowledge_release<I: LeaseIo>(
        &mut self,
        io: &mut I,
        handle: LeaseHandle,
    ) -> Result<ReleaseOutcome, Error> {
        match self.state {
            InternalState::Idle(terminal) => {
                if terminal.is_some_and(|entry| entry.granted.handle == handle) {
                    Ok(ReleaseOutcome::AlreadyReleased)
                } else {
                    Err(Error::StaleLease)
                }
            }
            InternalState::Active(granted) => {
                validate_handle(granted, handle)?;
                Err(Error::ReleaseRequired)
            }
            InternalState::Revoking(granted) => {
                validate_handle(granted, handle)?;
                io.complete_release(granted)?;
                self.state = InternalState::Idle(Some(TerminalLease { granted }));
                Ok(ReleaseOutcome::Released)
            }
        }
    }

    pub fn acknowledge_release_and_restore<I: LeaseIo>(
        &mut self,
        io: &mut I,
        handle: LeaseHandle,
        compositor_resource_token: u64,
    ) -> Result<ReleaseOutcome, Error> {
        let outcome = self.acknowledge_release(io, handle)?;
        let should_restore = matches!(
            self.state,
            InternalState::Idle(Some(terminal))
                if terminal.granted.handle == handle
                    && terminal.granted.owner == LeaseOwner::Fullscreen
        );
        if !should_restore {
            return Ok(outcome);
        }
        if compositor_resource_token == 0 {
            return Err(Error::LeaseIoFailure);
        }
        let compositor = self.compositor_lease.ok_or(Error::LeaseIoFailure)?;
        io.clear_for_compositor()?;
        io.restore_compositor(compositor, compositor_resource_token)?;
        self.state = InternalState::Active(compositor);
        Ok(outcome)
    }

    fn acquire<I: LeaseIo>(
        &mut self,
        io: &mut I,
        owner: LeaseOwner,
        request: Option<LeaseAcquire>,
    ) -> Result<LeaseGranted, Error> {
        match self.state {
            InternalState::Active(_) => Err(Error::FramebufferBusy),
            InternalState::Revoking(_) => Err(Error::LeaseTransitioning),
            InternalState::Idle(Some(terminal))
                if terminal.granted.owner == LeaseOwner::Fullscreen =>
            {
                Err(Error::LeaseTransitioning)
            }
            InternalState::Idle(_) => {
                let handle = self.next_handle()?;
                let granted = LeaseGranted { handle, owner };
                if owner == LeaseOwner::Compositor {
                    io.clear_for_compositor()?;
                }
                io.prepare_acquire(granted, request)?;
                self.state = InternalState::Active(granted);
                Ok(granted)
            }
        }
    }

    fn next_handle(&mut self) -> Result<LeaseHandle, Error> {
        let next_lease_id = self
            .next_lease_id
            .checked_add(1)
            .ok_or(Error::LeaseGenerationExhausted)?;
        let next_generation = self
            .next_generation
            .checked_add(1)
            .ok_or(Error::LeaseGenerationExhausted)?;
        let handle = LeaseHandle {
            lease_id: self.next_lease_id,
            generation: self.next_generation,
        };
        self.next_lease_id = next_lease_id;
        self.next_generation = next_generation;
        Ok(handle)
    }
}

fn validate_handle(granted: LeaseGranted, handle: LeaseHandle) -> Result<(), Error> {
    if granted.handle == handle {
        Ok(())
    } else {
        Err(Error::StaleLease)
    }
}

#[cfg(test)]
mod tests;
