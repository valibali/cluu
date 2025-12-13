/*
 * Thread Management
 *
 * This module defines the Thread structure and related types
 * for the preemptive scheduler.
 */

use alloc::{boxed::Box, string::String};
use core::fmt;

use super::InterruptContext;

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
///
/// Each thread has its own stack and interrupt context for preemptive scheduling.
/// The interrupt context stores all CPU registers + interrupt frame, allowing
/// threads to be switched at any time via timer interrupts or voluntary yields.
pub struct Thread {
    pub id: ThreadId,
    pub name: String,
    pub state: ThreadState,
    pub stack: Box<[u8]>,

    // Interrupt-based context for preemptive scheduling
    pub interrupt_context: InterruptContext,
}

impl Thread {
    pub fn new(id: ThreadId, name: String, stack: Box<[u8]>, interrupt_context: InterruptContext) -> Self {
        Self {
            id,
            name,
            state: ThreadState::Ready,
            stack,
            interrupt_context,
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
