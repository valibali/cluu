/*
 * Thread Management
 *
 * This module defines the Thread structure and related types
 * for the cooperative scheduler.
 */

use alloc::{boxed::Box, string::String};
use core::fmt;

use super::CpuContext;

/// Thread identifier
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ThreadId(pub usize);

impl fmt::Display for ThreadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Thread({})", self.0)
    }
}

/// Thread state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadState {
    Ready,
    Running,
    Blocked,
    Terminated,
}

/// Thread structure
pub struct Thread {
    pub id: ThreadId,
    pub name: String,
    pub state: ThreadState,
    pub stack: Box<[u8]>,
    pub context: CpuContext,
}

impl Thread {
    pub fn new(id: ThreadId, name: String, stack: Box<[u8]>, context: CpuContext) -> Self {
        Self {
            id,
            name,
            state: ThreadState::Ready,
            stack,
            context,
        }
    }
}

impl fmt::Debug for Thread {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Thread")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("state", &self.state)
            .field("stack_size", &self.stack.len())
            .finish()
    }
}
