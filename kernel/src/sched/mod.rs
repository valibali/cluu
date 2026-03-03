//! Scheduler Module
//!
//! This module implements the CLUU microkernel scheduler with:
//! - Preemptive scheduling with priorities
//! - O(1) priority bitmap scheduler
//! - INITMODE/NORMALMODE execution modes
//! - Context switching for thread management
//!
//! # Architecture
//!
//! The scheduler follows SOLID principles:
//! - **Thread**: Basic execution unit (TCB)
//! - **Context**: CPU register state for context switching
//! - **ThreadRepository**: Storage and management of threads
//! - **PriorityBitmapScheduler**: O(1) scheduling policy
//! - **ContextSwitcher**: Assembly routines for context switching
//!
//! # Design Principles
//!
//! ## Single Responsibility
//! - Thread: Represents execution state
//! - Scheduler: Decides what to run next
//! - Repository: Stores and retrieves threads
//! - Context: Holds CPU state
//!
//! ## Dependency Inversion
//! High-level scheduler depends on traits, not concrete implementations:
//! - `SchedulingPolicy` trait for different scheduling algorithms
//! - `Repository` trait for different storage strategies
//! - `ContextSwitcher` trait for platform-specific switching
//!
//! # Example Usage
//!
//! ```rust,no_run
//! use crate::sched::{Thread, PriorityBitmapScheduler, ThreadState, Priority};
//!
//! // Create a scheduler
//! let mut scheduler = PriorityBitmapScheduler::new();
//!
//! // Add threads
//! scheduler.add(thread_id_1, Priority::new(100));
//! scheduler.add(thread_id_2, Priority::new(200));
//!
//! // Pick next thread to run
//! if let Some(next_thread) = scheduler.pick_next() {
//!     // Context switch to next_thread
//! }
//! ```

// Core types
pub mod context;
pub mod fpu;
pub mod process;
pub mod process_manager;
pub mod repository;
pub mod scheduler;
pub mod spawn;
pub mod thread;
pub mod thread_manager;

// Re-export key types
pub use context::Context;
pub use process::{Process, ProcessId, ProcessInitState, ProcessState, ProcessType};
pub use process_manager::ProcessManager;
pub use repository::ThreadRepository;
pub use scheduler::{PriorityBitmapScheduler, SchedulingPolicy};
pub use spawn::{spawn_elf_process, spawn_kernel_process};
pub use thread::{
    CallReplyInfo, FaultState, FaultType, Priority, Thread, ThreadFlags, ThreadId, ThreadState,
};
pub use thread_manager::{FaultReplyInfo, SchedulerMode, ThreadManager};
