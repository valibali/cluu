/*
 * Thread Management
 *
 * This module defines the Thread structure and related types
 * for the preemptive scheduler.
 */

use alloc::{boxed::Box, string::String};
use core::fmt;

use super::{InterruptContext, process::ProcessId};

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
///
/// Threads belong to a Process and share that process's address space and
/// file descriptor table.
pub struct Thread {
    pub id: ThreadId,
    pub name: String,
    pub state: ThreadState,
    pub stack: Box<[u8]>,

    // Interrupt-based context for preemptive scheduling
    pub interrupt_context: InterruptContext,

    // CPU time tracking (in milliseconds)
    pub cpu_time_ms: u64,
    pub last_scheduled_time: u64,

    // Sleep tracking - if non-zero, thread is sleeping until this time
    pub sleep_until_ms: u64,

    // Process this thread belongs to
    pub process_id: ProcessId,
}

impl Thread {
    pub fn new(
        id: ThreadId,
        name: String,
        stack: Box<[u8]>,
        interrupt_context: InterruptContext,
        process_id: ProcessId,
    ) -> Self {
        Self {
            id,
            name,
            state: ThreadState::Ready,
            stack,
            interrupt_context,
            cpu_time_ms: 0,
            last_scheduled_time: 0,
            sleep_until_ms: 0,
            process_id,
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
