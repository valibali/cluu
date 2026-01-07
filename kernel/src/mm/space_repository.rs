//! Space repository - records address spaces by ID
//!
//! Provides a simple repository pattern that stores every user address space
//! created via `sys_invoke`. This allows the syscall handlers to mutate the
//! address space when a token references it.

use super::space::AddressSpace;
use crate::token::scope::AddressSpaceId;
use alloc::collections::BTreeMap;
use spin::Mutex;

struct Repository {
    next_id: u64,
    spaces: BTreeMap<AddressSpaceId, AddressSpace>,
}

impl Repository {
    const fn new() -> Self {
        Self {
            next_id: 1,
            spaces: BTreeMap::new(),
        }
    }

    fn allocate_id(&mut self) -> AddressSpaceId {
        let id = AddressSpaceId::new(self.next_id);
        self.next_id += 1;
        id
    }

    fn insert(&mut self, space: AddressSpace) -> AddressSpaceId {
        let id = self.allocate_id();
        self.spaces.insert(id, space);
        id
    }

    fn get_mut(&mut self, id: AddressSpaceId) -> Option<&mut AddressSpace> {
        self.spaces.get_mut(&id)
    }
}

static REPOSITORY: Mutex<Repository> = Mutex::new(Repository::new());

/// Insert a new address space and return its `AddressSpaceId`.
pub fn insert(space: AddressSpace) -> AddressSpaceId {
    let mut repo = REPOSITORY.lock();
    repo.insert(space)
}

/// Execute a closure with mutable access to a stored address space.
pub fn with_space_mut<F, R>(id: AddressSpaceId, f: F) -> Option<R>
where
    F: FnOnce(&mut AddressSpace) -> R,
{
    let mut repo = REPOSITORY.lock();
    repo.get_mut(id).map(f)
}

/// Execute a closure with immutable access to a stored address space.
pub fn with_space<F, R>(id: AddressSpaceId, f: F) -> Option<R>
where
    F: FnOnce(&AddressSpace) -> R,
{
    let repo = REPOSITORY.lock();
    repo.spaces.get(&id).map(f)
}
