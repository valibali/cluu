//! Synchronization primitives for the kernel

pub use spin::{Mutex as SpinLock, MutexGuard as SpinLockGuard};

// Re-export commonly used sync types
pub use spin::{RwLock, RwLockReadGuard, RwLockWriteGuard};
