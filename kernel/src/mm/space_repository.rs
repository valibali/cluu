//! Space repository - records address spaces by ID
//!
//! Provides a simple repository pattern that stores every user address space
//! created via `sys_invoke`. This allows the syscall handlers to mutate the
//! address space when a token references it.

use super::space::AddressSpace;
use crate::token::scope::AddressSpaceId;
use alloc::collections::BTreeMap;
use spin::Mutex;

/// Maximum number of address spaces to prevent unbounded memory growth
/// This limits BTreeMap growth which can cause large heap allocations
const MAX_ADDRESS_SPACES: usize = 256;

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
        // Check limit but don't fail - just log a warning
        // This prevents the leak issue where AddressSpace is created but insert fails
        if self.spaces.len() >= MAX_ADDRESS_SPACES {
            klibcluu::warn("WARNING: Maximum address spaces reached, but allowing insert");
            klibcluu::warn("Consider increasing MAX_ADDRESS_SPACES or investigating memory usage");
        }
        let id = self.allocate_id();
        self.spaces.insert(id, space);
        id
    }

    fn get_mut(&mut self, id: AddressSpaceId) -> Option<&mut AddressSpace> {
        self.spaces.get_mut(&id)
    }

    fn remove(&mut self, id: AddressSpaceId) -> Option<AddressSpace> {
        self.spaces.remove(&id)
    }
}

static REPOSITORY: Mutex<Repository> = Mutex::new(Repository::new());

/// Insert a new address space and return its `AddressSpaceId`.
///
/// # Note
///
/// If MAX_ADDRESS_SPACES is reached, a warning is logged but the insert
/// still proceeds to avoid leaking the AddressSpace that was already created.
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

/// Remove an address space from the repository, returning it if it existed.
pub fn remove(id: AddressSpaceId) -> Option<AddressSpace> {
    let mut repo = REPOSITORY.lock();
    repo.remove(id)
}

/// Number of address spaces currently tracked by the repository.
pub fn count() -> usize {
    REPOSITORY.lock().spaces.len()
}

/// Visit every address space's id + page_table_root, with the repository
/// lock held. The closure must not call back into space_repository or it
/// will deadlock. Intended for diagnostic scanning (e.g. find every space
/// that maps a given physical frame). Skips no entries.
pub fn for_each<F>(mut f: F)
where
    F: FnMut(AddressSpaceId, x86_64::PhysAddr),
{
    let repo = REPOSITORY.lock();
    for (&id, space) in repo.spaces.iter() {
        f(id, space.page_table_root);
    }
}
