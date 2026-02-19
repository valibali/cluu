//! Thread Repository
//!
//! This module implements storage and management of threads using
//! the Repository pattern.
//!
//! # Design Pattern
//!
//! The ThreadRepository follows the Repository pattern:
//! - Abstracts storage implementation details
//! - Provides CRUD operations for threads
//! - Uses BTreeMap for O(log n) operations with ordered iteration
//!
//! # Thread Lifecycle
//!
//! 1. Insert new thread (Init state)
//! 2. Retrieve and modify thread
//! 3. Remove when dead
//!
//! # Example
//!
//! ```rust,no_run
//! use crate::sched::{ThreadRepository, Thread, ThreadId};
//!
//! let mut repo = ThreadRepository::new();
//!
//! // Insert a thread
//! let thread_id = repo.insert(thread)?;
//!
//! // Retrieve thread
//! if let Some(thread) = repo.get_mut(thread_id) {
//!     thread.make_ready();
//! }
//!
//! // Remove thread
//! repo.remove(thread_id);
//! ```

use crate::error::Error;
use crate::sched::thread::{Thread, ThreadId};
use alloc::collections::BTreeMap;

/// Thread Repository
///
/// Stores and manages threads using a BTreeMap for efficient
/// lookup and ordered iteration.
///
/// # Invariants
///
/// - Thread IDs are unique
/// - Thread IDs are never reused (monotonically increasing)
/// - No two threads can have the same ID
pub struct ThreadRepository {
    /// Storage for threads (ThreadId -> Thread)
    threads: BTreeMap<ThreadId, Thread>,

    /// Next thread ID to assign
    next_id: u64,
}

impl ThreadRepository {
    /// Create a new empty repository
    pub fn new() -> Self {
        Self {
            threads: BTreeMap::new(),
            next_id: 1, // Start from 1 (0 reserved for kernel/idle)
        }
    }

    /// Allocate a new thread ID
    ///
    /// Thread IDs are never reused to avoid confusion.
    pub fn alloc_id(&mut self) -> ThreadId {
        let id = ThreadId::new(self.next_id);
        self.next_id += 1;
        id
    }

    /// Insert a thread into the repository
    ///
    /// The thread must have a unique ID. If a thread with that ID already
    /// exists, returns an error.
    ///
    /// # Arguments
    ///
    /// * `thread` - Thread to insert
    ///
    /// # Returns
    ///
    /// * `Ok(ThreadId)` - ID of inserted thread
    /// * `Err(Error)` - If thread ID already exists
    pub fn insert(&mut self, thread: Thread) -> Result<ThreadId, Error> {
        let id = thread.id;

        if self.threads.contains_key(&id) {
            return Err(Error::AlreadyExists);
        }

        self.threads.insert(id, thread);
        Ok(id)
    }

    /// Get a thread by ID (immutable)
    ///
    /// # Returns
    ///
    /// * `Some(&Thread)` - If thread exists
    /// * `None` - If thread doesn't exist
    pub fn get(&self, id: ThreadId) -> Option<&Thread> {
        self.threads.get(&id)
    }

    /// Get a thread by ID (mutable)
    ///
    /// # Returns
    ///
    /// * `Some(&mut Thread)` - If thread exists
    /// * `None` - If thread doesn't exist
    pub fn get_mut(&mut self, id: ThreadId) -> Option<&mut Thread> {
        self.threads.get_mut(&id)
    }

    /// Remove a thread from the repository
    ///
    /// # Returns
    ///
    /// * `Some(Thread)` - The removed thread
    /// * `None` - If thread doesn't exist
    pub fn remove(&mut self, id: ThreadId) -> Option<Thread> {
        self.threads.remove(&id)
    }

    /// Check if a thread exists
    pub fn contains(&self, id: ThreadId) -> bool {
        self.threads.contains_key(&id)
    }

    /// Get the number of threads
    pub fn len(&self) -> usize {
        self.threads.len()
    }

    /// Check if repository is empty
    pub fn is_empty(&self) -> bool {
        self.threads.is_empty()
    }

    /// Iterate over all threads (immutable)
    pub fn iter(&self) -> impl Iterator<Item = (&ThreadId, &Thread)> {
        self.threads.iter()
    }

    /// Iterate over all threads (mutable)
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&ThreadId, &mut Thread)> {
        self.threads.iter_mut()
    }

    /// Clear all threads from the repository
    pub fn clear(&mut self) {
        self.threads.clear();
    }
}

impl Default for ThreadRepository {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sched::thread::{Priority, ThreadFlags};
    use alloc::vec;
    use alloc::vec::Vec;
    use x86_64::{PhysAddr, VirtAddr};

    fn create_test_thread(id: ThreadId) -> Thread {
        Thread::new(
            id,
            PhysAddr::new(0x1000),
            VirtAddr::new(0x400000),
            VirtAddr::new(0x7ff00000),
            Priority::DEFAULT,
            ThreadFlags::empty(),
        )
    }

    #[test]
    fn test_repository_new() {
        let repo = ThreadRepository::new();
        assert_eq!(repo.len(), 0);
        assert!(repo.is_empty());
    }

    #[test]
    fn test_alloc_id() {
        let mut repo = ThreadRepository::new();

        let id1 = repo.alloc_id();
        let id2 = repo.alloc_id();
        let id3 = repo.alloc_id();

        assert_eq!(id1, ThreadId::new(1));
        assert_eq!(id2, ThreadId::new(2));
        assert_eq!(id3, ThreadId::new(3));
    }

    #[test]
    fn test_insert_and_get() {
        let mut repo = ThreadRepository::new();
        let thread = create_test_thread(ThreadId::new(1));

        let id = repo.insert(thread).expect("insert failed");
        assert_eq!(id, ThreadId::new(1));
        assert_eq!(repo.len(), 1);

        let retrieved = repo.get(id).expect("get failed");
        assert_eq!(retrieved.id, ThreadId::new(1));
    }

    #[test]
    fn test_insert_duplicate() {
        let mut repo = ThreadRepository::new();
        let thread1 = create_test_thread(ThreadId::new(1));
        let thread2 = create_test_thread(ThreadId::new(1));

        repo.insert(thread1).expect("first insert failed");
        let result = repo.insert(thread2);

        assert!(result.is_err());
        assert_eq!(repo.len(), 1);
    }

    #[test]
    fn test_get_mut() {
        let mut repo = ThreadRepository::new();
        let thread = create_test_thread(ThreadId::new(1));
        let id = repo.insert(thread).expect("insert failed");

        {
            let thread = repo.get_mut(id).expect("get_mut failed");
            thread.make_ready();
        }

        let thread = repo.get(id).expect("get failed");
        assert!(thread.is_ready());
    }

    #[test]
    fn test_remove() {
        let mut repo = ThreadRepository::new();
        let thread = create_test_thread(ThreadId::new(1));
        let id = repo.insert(thread).expect("insert failed");

        assert_eq!(repo.len(), 1);

        let removed = repo.remove(id).expect("remove failed");
        assert_eq!(removed.id, ThreadId::new(1));
        assert_eq!(repo.len(), 0);

        // Second remove should return None
        assert!(repo.remove(id).is_none());
    }

    #[test]
    fn test_contains() {
        let mut repo = ThreadRepository::new();
        let thread = create_test_thread(ThreadId::new(1));
        let id = repo.insert(thread).expect("insert failed");

        assert!(repo.contains(id));
        assert!(!repo.contains(ThreadId::new(999)));

        repo.remove(id);
        assert!(!repo.contains(id));
    }

    #[test]
    fn test_iter() {
        let mut repo = ThreadRepository::new();

        for i in 1..=5 {
            let thread = create_test_thread(ThreadId::new(i));
            repo.insert(thread).expect("insert failed");
        }

        let ids: Vec<ThreadId> = repo.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            ids,
            vec![
                ThreadId::new(1),
                ThreadId::new(2),
                ThreadId::new(3),
                ThreadId::new(4),
                ThreadId::new(5),
            ]
        );
    }

    #[test]
    fn test_iter_mut() {
        let mut repo = ThreadRepository::new();

        for i in 1..=3 {
            let thread = create_test_thread(ThreadId::new(i));
            repo.insert(thread).expect("insert failed");
        }

        // Make all threads ready
        for (_, thread) in repo.iter_mut() {
            thread.make_ready();
        }

        // Verify all are ready
        for (_, thread) in repo.iter() {
            assert!(thread.is_ready());
        }
    }

    #[test]
    fn test_clear() {
        let mut repo = ThreadRepository::new();

        for i in 1..=5 {
            let thread = create_test_thread(ThreadId::new(i));
            repo.insert(thread).expect("insert failed");
        }

        assert_eq!(repo.len(), 5);

        repo.clear();
        assert_eq!(repo.len(), 0);
        assert!(repo.is_empty());
    }

    #[test]
    fn test_get_nonexistent() {
        let repo = ThreadRepository::new();
        assert!(repo.get(ThreadId::new(999)).is_none());
        assert!(!repo.contains(ThreadId::new(999)));
    }
}
