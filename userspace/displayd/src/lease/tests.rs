use alloc::vec::Vec;

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Event {
    PrepareAcquire(LeaseOwner),
    ClearForCompositor,
    PrepareRelease(LeaseOwner),
    CompleteRelease(LeaseOwner),
    RestoreCompositor(LeaseHandle, u64),
}

#[derive(Default)]
struct FakeIo {
    events: Vec<Event>,
    operations: Vec<&'static str>,
    fail_prepare_release: bool,
    fail_complete_release: bool,
    fail_restore_compositor: bool,
}

impl LeaseIo for FakeIo {
    fn clear_for_compositor(&mut self) -> Result<(), Error> {
        self.operations.push("clear");
        self.events.push(Event::ClearForCompositor);
        Ok(())
    }

    fn prepare_acquire(&mut self, lease: LeaseGranted, _request: Option<LeaseAcquire>) -> Result<(), Error> {
        self.events.push(Event::PrepareAcquire(lease.owner));
        if lease.owner == LeaseOwner::Compositor {
            self.operations.push("restore");
        }
        Ok(())
    }

    fn prepare_release(&mut self, lease: LeaseGranted) -> Result<(), Error> {
        self.events.push(Event::PrepareRelease(lease.owner));
        self.operations.push(match lease.owner {
            LeaseOwner::Compositor => "client-unmap-ack",
            LeaseOwner::Fullscreen => "route-prepare",
        });
        if self.fail_prepare_release {
            return Err(Error::LeaseIoFailure);
        }
        Ok(())
    }

    fn complete_release(&mut self, lease: LeaseGranted) -> Result<(), Error> {
        self.events.push(Event::CompleteRelease(lease.owner));
        if lease.owner == LeaseOwner::Fullscreen {
            self.operations.push("client-unmap-ack");
            self.operations.push("gpu-release");
        }
        if self.fail_complete_release {
            Err(Error::LeaseIoFailure)
        } else {
            Ok(())
        }
    }

    fn restore_compositor(
        &mut self,
        lease: LeaseGranted,
        resource_token: u64,
    ) -> Result<(), Error> {
        self.events
            .push(Event::RestoreCompositor(lease.handle, resource_token));
        self.operations.push("restore");
        if self.fail_restore_compositor {
            Err(Error::LeaseIoFailure)
        } else {
            Ok(())
        }
    }
}

#[test]
fn identical_compositor_registration_keeps_original_binding() {
    let registration = CompositorRegistration::new(7, 11, 0x4000, 13);
    let mut binding = None;
    assert_eq!(bind_compositor(&mut binding, registration), Ok(registration));

    assert_eq!(bind_compositor(&mut binding, registration), Ok(registration));
    assert_eq!(binding, Some(registration));
}

#[test]
fn conflicting_compositor_registration_fails_without_mutation() {
    let original = CompositorRegistration::new(7, 11, 0x4000, 13);
    let conflicting = CompositorRegistration::new(17, 19, 0x8000, 23);
    let mut binding = Some(original);

    assert_eq!(
        bind_compositor(&mut binding, conflicting),
        Err(Error::InvalidCapability)
    );
    assert_eq!(binding, Some(original));
}

#[test]
fn compositor_prepare_release_failure_keeps_coordinator_fail_closed() {
    let mut coordinator = LeaseCoordinator::new();
    let mut io = FakeIo { fail_prepare_release: true, ..FakeIo::default() };
    assert!(coordinator.register_compositor(&mut io).is_ok());

    assert_eq!(
        coordinator.acquire_fullscreen(
            &mut io,
            LeaseAcquire { client_space_token: 1, client_target_va: 4096, input_endpoint: 2 },
        ),
        Err(Error::LeaseIoFailure)
    );
    assert!(matches!(
        coordinator.state(),
        LeaseState::Revoking(LeaseGranted { owner: LeaseOwner::Compositor, .. })
    ));
    assert_eq!(coordinator.active_lease(), None);
}

#[test]
fn explicit_compositor_release_failure_keeps_coordinator_fail_closed() {
    let mut coordinator = LeaseCoordinator::new();
    let mut io = FakeIo::default();
    let compositor = match coordinator.register_compositor(&mut io) {
        Ok(lease) => lease,
        Err(_) => return,
    };
    io.fail_prepare_release = true;

    assert_eq!(
        coordinator.release(&mut io, compositor.handle),
        Err(Error::LeaseIoFailure)
    );
    assert!(matches!(coordinator.state(), LeaseState::Revoking(_)));
    assert_eq!(coordinator.active_lease(), None);
}

#[test]
fn default_compositor_registration_owns_framebuffer() {
    let mut coordinator = LeaseCoordinator::new();
    let mut io = FakeIo::default();
    let granted = coordinator.register_compositor(&mut io);

    assert_eq!(granted.map(|lease| lease.owner), Ok(LeaseOwner::Compositor));
    assert!(matches!(
        coordinator.state(),
        LeaseState::Active(LeaseGranted {
            owner: LeaseOwner::Compositor,
            ..
        })
    ));
    assert_eq!(
        io.events,
        [
            Event::ClearForCompositor,
            Event::PrepareAcquire(LeaseOwner::Compositor),
        ]
    );
}

#[test]
fn fullscreen_acquire_releases_compositor_before_direct_prepare() {
    let mut coordinator = LeaseCoordinator::new();
    let mut io = FakeIo::default();
    assert!(coordinator.register_compositor(&mut io).is_ok());
    let result = coordinator.acquire_fullscreen(&mut io, LeaseAcquire { client_space_token: 1, client_target_va: 4096, input_endpoint: 2 });

    assert_eq!(result.map(|lease| lease.owner), Ok(LeaseOwner::Fullscreen));
    assert!(matches!(
        coordinator.state(),
        LeaseState::Active(LeaseGranted {
            owner: LeaseOwner::Fullscreen,
            ..
        })
    ));
    assert_eq!(
        io.events,
        [
            Event::ClearForCompositor,
            Event::PrepareAcquire(LeaseOwner::Compositor),
            Event::PrepareRelease(LeaseOwner::Compositor),
            Event::CompleteRelease(LeaseOwner::Compositor),
            Event::PrepareAcquire(LeaseOwner::Fullscreen),
        ]
    );
}

#[test]
fn stale_generation_is_rejected_after_new_lease_generation() {
    let mut coordinator = LeaseCoordinator::new();
    let mut io = FakeIo::default();
    let compositor = coordinator.register_compositor(&mut io);
    assert!(compositor.is_ok());
    let compositor = if let Ok(lease) = compositor {
        lease
    } else {
        return;
    };
    assert_eq!(
        coordinator.release(&mut io, compositor.handle),
        Ok(ReleaseOutcome::AwaitingAcknowledgement)
    );
    assert_eq!(
        coordinator.acknowledge_release(&mut io, compositor.handle),
        Ok(ReleaseOutcome::Released)
    );
    let fullscreen = coordinator.acquire_fullscreen(&mut io, LeaseAcquire { client_space_token: 1, client_target_va: 4096, input_endpoint: 2 });
    assert!(fullscreen.is_ok());
    let fullscreen = if let Ok(lease) = fullscreen {
        lease
    } else {
        return;
    };

    assert_ne!(compositor.handle.generation, fullscreen.handle.generation);
    assert_eq!(
        coordinator.release(&mut io, compositor.handle),
        Err(Error::StaleLease)
    );
}

#[test]
fn release_requires_ack_before_next_owner_can_acquire() {
    let mut coordinator = LeaseCoordinator::new();
    let mut io = FakeIo::default();
    let compositor = coordinator.register_compositor(&mut io);
    assert!(compositor.is_ok());
    let compositor = if let Ok(lease) = compositor {
        lease
    } else {
        return;
    };
    assert_eq!(
        coordinator.release(&mut io, compositor.handle),
        Ok(ReleaseOutcome::AwaitingAcknowledgement)
    );
    assert_eq!(
        coordinator.acquire_fullscreen(&mut io, LeaseAcquire { client_space_token: 1, client_target_va: 4096, input_endpoint: 2 }),
        Err(Error::LeaseTransitioning)
    );
    assert_eq!(
        coordinator.acknowledge_release(&mut io, compositor.handle),
        Ok(ReleaseOutcome::Released)
    );
    assert_eq!(
        coordinator.acquire_fullscreen(&mut io, LeaseAcquire { client_space_token: 1, client_target_va: 4096, input_endpoint: 2 }).map(|lease| lease.owner),
        Ok(LeaseOwner::Fullscreen)
    );
    assert_eq!(
        io.events,
        [
            Event::ClearForCompositor,
            Event::PrepareAcquire(LeaseOwner::Compositor),
            Event::PrepareRelease(LeaseOwner::Compositor),
            Event::CompleteRelease(LeaseOwner::Compositor),
            Event::PrepareAcquire(LeaseOwner::Fullscreen),
        ]
    );
}

#[test]
fn failed_release_ack_keeps_coordinator_closed() {
    let mut coordinator = LeaseCoordinator::new();
    let mut io = FakeIo::default();
    let compositor = coordinator.register_compositor(&mut io);
    assert!(compositor.is_ok());
    let compositor = if let Ok(lease) = compositor {
        lease
    } else {
        return;
    };
    assert!(coordinator.release(&mut io, compositor.handle).is_ok());
    io.fail_complete_release = true;

    assert_eq!(
        coordinator.acknowledge_release(&mut io, compositor.handle),
        Err(Error::LeaseIoFailure)
    );
    assert_eq!(
        coordinator.acquire_fullscreen(&mut io, LeaseAcquire { client_space_token: 1, client_target_va: 4096, input_endpoint: 2 }),
        Err(Error::LeaseTransitioning)
    );
    assert!(matches!(coordinator.state(), LeaseState::Revoking(_)));
    assert_eq!(coordinator.active_lease(), None);
}

#[test]
fn missing_fullscreen_ack_keeps_lease_fail_closed() {
    let mut coordinator = LeaseCoordinator::new();
    let mut io = FakeIo::default();
    assert!(coordinator.register_compositor(&mut io).is_ok());
    let fullscreen = coordinator
        .acquire_fullscreen(
            &mut io,
            LeaseAcquire {
                client_space_token: 1,
                client_target_va: 4096,
                input_endpoint: 2,
            },
        )
        .expect("fullscreen lease should acquire");

    assert_eq!(
        coordinator.release(&mut io, fullscreen.handle),
        Ok(ReleaseOutcome::AwaitingAcknowledgement)
    );
    assert_eq!(coordinator.active_lease(), None);
    assert_eq!(
        coordinator.acquire_fullscreen(
            &mut io,
            LeaseAcquire {
                client_space_token: 3,
                client_target_va: 8192,
                input_endpoint: 4,
            },
        ),
        Err(Error::LeaseTransitioning)
    );
    assert!(matches!(coordinator.state(), LeaseState::Revoking(_)));
}

#[test]
fn release_restore_operations_are_serialized_fail_closed() {
    let mut coordinator = LeaseCoordinator::new();
    let mut io = FakeIo::default();
    assert!(coordinator.register_compositor(&mut io).is_ok());
    let fullscreen = coordinator
        .acquire_fullscreen(
            &mut io,
            LeaseAcquire {
                client_space_token: 1,
                client_target_va: 4096,
                input_endpoint: 2,
            },
        )
        .expect("fullscreen lease should acquire");
    io.operations.clear();

    assert_eq!(
        coordinator.release(&mut io, fullscreen.handle),
        Ok(ReleaseOutcome::AwaitingAcknowledgement)
    );
    assert_eq!(io.operations, ["route-prepare"]);
    assert_eq!(
        coordinator.acknowledge_release_and_restore(
            &mut io,
            fullscreen.handle,
            0xA000_0000_0000_0001,
        ),
        Ok(ReleaseOutcome::Released)
    );
    assert_eq!(
        io.operations,
        ["route-prepare", "client-unmap-ack", "gpu-release", "clear", "restore"]
    );
}

#[test]
fn fullscreen_release_restores_original_compositor_handle() {
    let mut coordinator = LeaseCoordinator::new();
    let mut io = FakeIo::default();
    let compositor = match coordinator.register_compositor(&mut io) {
        Ok(lease) => lease,
        Err(_) => return,
    };
    let fullscreen = match coordinator.acquire_fullscreen(
        &mut io,
        LeaseAcquire {
            client_space_token: 1,
            client_target_va: 4096,
            input_endpoint: 2,
        },
    ) {
        Ok(lease) => lease,
        Err(_) => return,
    };

    assert!(coordinator.release(&mut io, fullscreen.handle).is_ok());
    assert_eq!(
        coordinator.acknowledge_release_and_restore(
            &mut io,
            fullscreen.handle,
            0xA000_0000_0000_0001,
        ),
        Ok(ReleaseOutcome::Released)
    );
    assert_eq!(coordinator.active_lease(), Some(compositor));
}

#[test]
fn compositor_restore_uses_surface_resource_token_not_lease_id() {
    const RESOURCE_TOKEN: u64 = 0xA000_0000_0000_0001;

    let mut coordinator = LeaseCoordinator::new();
    let mut io = FakeIo::default();
    let compositor = match coordinator.register_compositor(&mut io) {
        Ok(lease) => lease,
        Err(_) => return,
    };
    let fullscreen = match coordinator.acquire_fullscreen(
        &mut io,
        LeaseAcquire {
            client_space_token: 1,
            client_target_va: 4096,
            input_endpoint: 2,
        },
    ) {
        Ok(lease) => lease,
        Err(_) => return,
    };

    assert_ne!(RESOURCE_TOKEN, compositor.handle.lease_id);
    assert!(coordinator.release(&mut io, fullscreen.handle).is_ok());
    assert!(coordinator
        .acknowledge_release_and_restore(&mut io, fullscreen.handle, RESOURCE_TOKEN)
        .is_ok());
    assert!(io.events.contains(&Event::RestoreCompositor(
        compositor.handle,
        RESOURCE_TOKEN,
    )));
}

#[test]
fn failed_compositor_restore_is_fail_closed_and_retryable() {
    const RESOURCE_TOKEN: u64 = 0xA000_0000_0000_0001;

    let mut coordinator = LeaseCoordinator::new();
    let mut io = FakeIo::default();
    let compositor = match coordinator.register_compositor(&mut io) {
        Ok(lease) => lease,
        Err(_) => return,
    };
    let fullscreen = match coordinator.acquire_fullscreen(
        &mut io,
        LeaseAcquire {
            client_space_token: 1,
            client_target_va: 4096,
            input_endpoint: 2,
        },
    ) {
        Ok(lease) => lease,
        Err(_) => return,
    };
    assert!(coordinator.release(&mut io, fullscreen.handle).is_ok());
    io.fail_restore_compositor = true;

    assert_eq!(
        coordinator.acknowledge_release_and_restore(
            &mut io,
            fullscreen.handle,
            RESOURCE_TOKEN,
        ),
        Err(Error::LeaseIoFailure)
    );
    assert_eq!(coordinator.active_lease(), None);

    io.fail_restore_compositor = false;
    assert_eq!(
        coordinator.acknowledge_release_and_restore(
            &mut io,
            fullscreen.handle,
            RESOURCE_TOKEN,
        ),
        Ok(ReleaseOutcome::AlreadyReleased)
    );
    assert_eq!(coordinator.active_lease(), Some(compositor));
}

#[test]
fn terminal_release_is_idempotent_without_repeating_io() {
    let mut coordinator = LeaseCoordinator::new();
    let mut io = FakeIo::default();
    let compositor = coordinator.register_compositor(&mut io);
    assert!(compositor.is_ok());
    let compositor = if let Ok(lease) = compositor {
        lease
    } else {
        return;
    };
    assert!(coordinator.release(&mut io, compositor.handle).is_ok());
    assert!(coordinator.acknowledge_release(&mut io, compositor.handle).is_ok());
    let event_count = io.events.len();

    assert_eq!(
        coordinator.release(&mut io, compositor.handle),
        Ok(ReleaseOutcome::AlreadyReleased)
    );
    assert_eq!(
        coordinator.acknowledge_release(&mut io, compositor.handle),
        Ok(ReleaseOutcome::AlreadyReleased)
    );
    assert_eq!(io.events.len(), event_count);
}
