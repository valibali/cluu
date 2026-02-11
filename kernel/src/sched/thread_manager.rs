//! Thread Manager - Global Thread Scheduling
//!
//! This module provides the global scheduler instance and thread management.
//!
//! # Scheduler Modes
//!
//! - **INITMODE**: Cooperative scheduling for critical processes during boot
//! - **NORMALMODE**: Preemptive scheduling for normal operation

use crate::architecture::x86_64::gdt::set_tss_rsp0;
use crate::sched::{
    CallReplyInfo, Context, Priority, PriorityBitmapScheduler, SchedulingPolicy, Thread, ThreadId,
    ThreadRepository,
};
use crate::token::ReplyId;

/// Info stored when a thread is waiting for fault handler reply
#[derive(Debug, Clone, Copy)]
pub struct FaultReplyInfo {
    pub faulted_thread: ThreadId,
}
use alloc::collections::{BTreeMap, BinaryHeap};
use core::cmp::Reverse;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;
use x86_64::PhysAddr;

// ═══════════════════════════════════════════════════════════════════════════
// Scheduler Mode
// ═══════════════════════════════════════════════════════════════════════════

/// Scheduler execution mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerMode {
    /// Cooperative scheduling during boot (no preemption)
    Init,
    /// Preemptive scheduling during normal operation
    Normal,
}

// ═══════════════════════════════════════════════════════════════════════════
// Global State
// ═══════════════════════════════════════════════════════════════════════════

lazy_static! {
    /// Global thread repository
    static ref THREAD_REPOSITORY: Mutex<ThreadRepository> = Mutex::new(ThreadRepository::new());

    /// Global scheduler instance
    static ref SCHEDULER: Mutex<PriorityBitmapScheduler> = Mutex::new(PriorityBitmapScheduler::new());

    /// Currently running thread
    static ref CURRENT_THREAD: Mutex<Option<ThreadId>> = Mutex::new(None);

    /// Min-heap of (deadline_tick, thread_id) for O(log m) timeout checking
    /// Only threads with active timeouts are in the heap. Stale entries
    /// (threads woken by other means) are cleaned lazily when popped.
    static ref TIMEOUT_HEAP: Mutex<BinaryHeap<Reverse<(u64, u64)>>> =
        Mutex::new(BinaryHeap::new());

    /// Map from ReplyId to CallReplyInfo for IPC call/reply
    static ref CALL_REPLY_MAP: Mutex<BTreeMap<ReplyId, CallReplyInfo>> =
        Mutex::new(BTreeMap::new());

    /// Map from ReplyId to FaultReplyInfo for fault IPC
    static ref FAULT_REPLY_MAP: Mutex<BTreeMap<ReplyId, FaultReplyInfo>> =
        Mutex::new(BTreeMap::new());
}

/// Counter for generating unique ReplyIds
static NEXT_REPLY_ID: AtomicU64 = AtomicU64::new(1);

/// Current scheduler mode (starts in INITMODE)
static SCHEDULER_MODE: AtomicBool = AtomicBool::new(false); // false = INIT, true = NORMAL

/// Number of critical processes still initializing
static CRITICAL_PROCESS_COUNT: AtomicUsize = AtomicUsize::new(0);
static CURRENT_THREAD_ID: AtomicU64 = AtomicU64::new(u64::MAX);

/// Global scheduler tick counter (incremented by timer interrupt)
static SCHEDULER_TICKS: AtomicU64 = AtomicU64::new(0);

/// Multi-slot pending wake queue (lock-free)
/// Each slot holds a thread ID (0 = empty). Allows multiple concurrent wakes.
const PENDING_WAKE_SLOTS: usize = 8;
static PENDING_WAKE_QUEUE: [AtomicU64; PENDING_WAKE_SLOTS] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];

// ═══════════════════════════════════════════════════════════════════════════
// Thread Manager API
// ═══════════════════════════════════════════════════════════════════════════

/// Thread Manager - Zero-Sized Type for namespaced access
pub struct ThreadManager;

impl ThreadManager {
    /// Create a new thread and add to repository
    ///
    /// # Arguments
    ///
    /// * `thread` - Thread to add
    ///
    /// # Returns
    ///
    /// ThreadId of the added thread
    pub fn add_thread(thread: Thread) -> ThreadId {
        let thread_id = thread.id;
        let priority = thread.priority;

        // Insert into repository
        let mut repo = THREAD_REPOSITORY.lock();
        repo.insert(thread).expect("Failed to insert thread");
        drop(repo);

        // Add to scheduler
        let mut scheduler = SCHEDULER.lock();
        scheduler.add(thread_id, priority);

        thread_id
    }

    /// Access a thread by ID (immutable)
    pub fn with_thread<F, R>(id: ThreadId, f: F) -> Option<R>
    where
        F: FnOnce(&Thread) -> R,
    {
        let repo = THREAD_REPOSITORY.lock();
        repo.get(id).map(f)
    }

    /// Modify a thread by ID
    pub fn with_thread_mut<F, R>(id: ThreadId, f: F) -> Option<R>
    where
        F: FnOnce(&mut Thread) -> R,
    {
        let mut repo = THREAD_REPOSITORY.lock();
        repo.get_mut(id).map(f)
    }

    /// Number of threads currently tracked in the repository (including dead).
    pub fn thread_count_total() -> usize {
        THREAD_REPOSITORY.lock().len()
    }

    /// Number of threads that are not in Dead state.
    pub fn thread_count_live() -> usize {
        THREAD_REPOSITORY
            .lock()
            .iter()
            .filter(|(_, thread)| !thread.is_dead())
            .count()
    }

    /// Allocate a new ThreadId
    pub fn alloc_thread_id() -> ThreadId {
        let mut repo = THREAD_REPOSITORY.lock();
        repo.alloc_id()
    }

    /// Pick the next thread to run
    ///
    /// Returns None if no threads are ready.
    pub fn pick_next() -> Option<ThreadId> {
        let mut scheduler = SCHEDULER.lock();
        scheduler.pick_next()
    }

    /// Yield current thread (expire to expired array for fair scheduling)
    pub fn yield_current() {
        let current = {
            let mut current_lock = CURRENT_THREAD.lock();
            current_lock.take()
        };

        if let Some(thread_id) = current {
            let mut scheduler = SCHEDULER.lock();
            scheduler.expire_current();
            drop(scheduler);
            let _ = thread_id; // Silence unused warning
        }
    }

    /// Set the currently running thread
    pub fn set_current(thread_id: ThreadId) {
        let mut current = CURRENT_THREAD.lock();
        *current = Some(thread_id);
        CURRENT_THREAD_ID.store(thread_id.as_u64(), Ordering::Release);
    }

    /// Get the currently running thread (lock-free via atomic)
    pub fn current() -> Option<ThreadId> {
        let raw = CURRENT_THREAD_ID.load(Ordering::Acquire);
        if raw == u64::MAX {
            None
        } else {
            Some(ThreadId::new(raw))
        }
    }

    pub fn current_id_raw() -> u64 {
        CURRENT_THREAD_ID.load(Ordering::Acquire)
    }

    pub fn mark_current_dead() {
        let current = match Self::current() {
            Some(id) => id,
            None => return,
        };
        let _ = Self::mark_thread_dead(current);
    }

    /// Mark a thread as dead and remove it from scheduler queues.
    pub fn mark_thread_dead(thread_id: ThreadId) -> bool {
        let found = Self::with_thread_mut(thread_id, |thread| {
            thread.make_dead();
            thread.clear_timeout_deadline();
            thread.woke_from_timeout = false;
            thread.disarm_recv_wait();
            thread.clear_suspended();
        })
        .is_some();
        if !found {
            return false;
        }
        let mut scheduler = SCHEDULER.lock();
        scheduler.remove(thread_id);
        true
    }

    /// Allocate a new unique ReplyId
    pub fn alloc_reply_id() -> ReplyId {
        ReplyId::new(NEXT_REPLY_ID.fetch_add(1, Ordering::SeqCst))
    }

    /// Store call reply info for a reply ID
    pub fn set_call_reply_info(reply_id: ReplyId, info: CallReplyInfo) {
        CALL_REPLY_MAP.lock().insert(reply_id, info);
    }

    /// Take and remove call reply info for a reply ID (one-time use)
    pub fn take_call_reply_info(reply_id: ReplyId) -> Option<CallReplyInfo> {
        CALL_REPLY_MAP.lock().remove(&reply_id)
    }

    /// Check if call reply info exists for a reply ID
    pub fn has_call_reply_info(reply_id: ReplyId) -> bool {
        CALL_REPLY_MAP.lock().contains_key(&reply_id)
    }

    /// Store fault reply info for a reply ID
    pub fn set_fault_reply_info(reply_id: ReplyId, info: FaultReplyInfo) {
        FAULT_REPLY_MAP.lock().insert(reply_id, info);
    }

    /// Take and remove fault reply info for a reply ID (one-time use)
    pub fn take_fault_reply_info(reply_id: ReplyId) -> Option<FaultReplyInfo> {
        FAULT_REPLY_MAP.lock().remove(&reply_id)
    }

    /// Schedule next thread after a fault (safe for IST context)
    ///
    /// Unlike `schedule_and_switch`, this does NOT idle if no threads are ready
    /// (idling on IST stack is unsafe — re-entrant exceptions would clobber it).
    /// Instead, halts if no threads are available (shouldn't happen if a fault
    /// message was just sent to wake a handler).
    pub fn schedule_next_from_fault() -> *const Context {
        Self::drain_pending_wake();
        let next_id = match Self::pick_next() {
            Some(id) => id,
            None => {
                klibcluu::error("schedule_next_from_fault: no runnable threads!");
                loop {
                    x86_64::instructions::hlt();
                }
            }
        };
        Self::set_current(next_id);
        Self::get_context_ptr(next_id)
    }

    pub fn block_current() {
        let current = match Self::current() {
            Some(id) => id,
            None => return,
        };
        Self::with_thread_mut(current, |thread| {
            thread.make_blocked();
        });
    }

    /// Block current thread with a timeout deadline
    ///
    /// The thread will be automatically woken when the deadline expires.
    /// Adds the thread to the timeout heap for O(log m) expiry checking.
    pub fn block_current_with_timeout(deadline: u64) {
        let current = match Self::current() {
            Some(id) => id,
            None => return,
        };
        klibcluu::trace("block_current_with_timeout: thread=");
        klibcluu::log_dec(klibcluu::LogLevel::Trace, "", current.as_u64());
        klibcluu::trace(" deadline=");
        klibcluu::log_dec(klibcluu::LogLevel::Trace, "", deadline);
        klibcluu::trace(" current_tick=");
        klibcluu::log_dec(klibcluu::LogLevel::Trace, "", Self::current_tick());
        Self::with_thread_mut(current, |thread| {
            thread.set_timeout_deadline(deadline);
            thread.make_blocked();
        });

        // Add to timeout heap for efficient expiry checking.
        // This runs in syscall/thread context, so we must not drop timeout registration.
        TIMEOUT_HEAP
            .lock()
            .push(Reverse((deadline, current.as_u64())));
    }

    /// Convert milliseconds to tick deadline from now
    ///
    /// Timer runs at 250Hz = 4ms per tick.
    /// Returns absolute deadline tick.
    pub fn ms_to_deadline(timeout_ms: u64) -> u64 {
        const MS_PER_TICK: u64 = 4;
        let current = Self::current_tick();
        let ticks = timeout_ms.div_ceil(MS_PER_TICK); // Round up
        current + ticks
    }

    /// Check if current thread woke from timeout and clear the flag
    ///
    /// Returns true if the thread was woken due to timeout expiry.
    /// The flag is cleared after checking.
    pub fn check_and_clear_timeout_wake() -> bool {
        let current = match Self::current() {
            Some(id) => id,
            None => return false,
        };
        Self::with_thread_mut(current, |thread| {
            let was_timeout = thread.woke_from_timeout;
            thread.woke_from_timeout = false;
            was_timeout
        })
        .unwrap_or(false)
    }

    pub fn arm_current_recv_wait() {
        let current = match Self::current() {
            Some(id) => id,
            None => return,
        };
        Self::with_thread_mut(current, |thread| {
            thread.arm_recv_wait();
        });
    }

    pub fn arm_current_recv_wait_with_buffer(buf_ptr: usize, buf_len: usize) {
        let current = match Self::current() {
            Some(id) => id,
            None => return,
        };
        Self::with_thread_mut(current, |thread| {
            thread.arm_recv_wait_with_buffer(buf_ptr, buf_len);
        });
    }

    pub fn disarm_current_recv_wait() {
        let current = match Self::current() {
            Some(id) => id,
            None => return,
        };
        Self::with_thread_mut(current, |thread| {
            thread.disarm_recv_wait();
        });
    }

    pub fn is_thread_recv_waiting(thread_id: ThreadId) -> bool {
        Self::with_thread(thread_id, |thread| {
            thread.is_blocked() || thread.is_recv_wait_armed()
        })
        .unwrap_or(false)
    }

    pub fn is_thread_recv_waiting_ticket(thread_id: ThreadId, ticket: u64) -> bool {
        Self::with_thread(thread_id, |thread| {
            thread.recv_wait_ticket() == ticket
                && (thread.is_blocked() || thread.is_recv_wait_armed())
        })
        .unwrap_or(false)
    }

    pub fn is_thread_recv_wait_active_ticket(thread_id: ThreadId, ticket: u64) -> bool {
        Self::with_thread(thread_id, |thread| {
            thread.recv_wait_ticket() == ticket
                && thread.is_recv_wait_armed()
                && thread.is_blocked()
        })
        .unwrap_or(false)
    }

    pub fn is_thread_blocked(thread_id: ThreadId) -> bool {
        Self::with_thread(thread_id, |thread| thread.is_blocked()).unwrap_or(false)
    }

    pub fn current_recv_wait_ticket() -> Option<u64> {
        let current = Self::current()?;
        Self::with_thread(current, |thread| thread.recv_wait_ticket())
    }

    pub fn recv_wait_buffer(thread_id: ThreadId) -> Option<(usize, usize, PhysAddr)> {
        Self::with_thread(thread_id, |thread| {
            let (buf_ptr, buf_len) = thread.recv_wait_buffer()?;
            Some((buf_ptr, buf_len, thread.page_table_root))
        })
        .flatten()
    }

    pub fn set_recv_wait_delivery(
        thread_id: ThreadId,
        endpoint: crate::token::scope::EndpointId,
        len: usize,
        sender: Option<ThreadId>,
    ) -> bool {
        Self::with_thread_mut(thread_id, |thread| {
            thread.set_recv_wait_delivery(crate::sched::thread::RecvWaitDelivery {
                endpoint,
                len,
                sender,
            });
        })
        .is_some()
    }

    pub fn take_current_recv_wait_delivery(
    ) -> Option<(crate::token::scope::EndpointId, usize, Option<ThreadId>)> {
        let current = Self::current()?;
        Self::with_thread_mut(current, |thread| {
            let delivery = thread.take_recv_wait_delivery()?;
            Some((delivery.endpoint, delivery.len, delivery.sender))
        })
        .flatten()
    }

    pub fn wake_thread(thread_id: ThreadId) {
        // Try to wake immediately if locks are available
        let priority = {
            let mut repo = match THREAD_REPOSITORY.try_lock() {
                Some(repo) => repo,
                None => {
                    // Can't get lock, queue for later
                    Self::queue_pending_wake(thread_id);
                    return;
                }
            };
            match repo.get_mut(thread_id) {
                Some(thread) if !thread.is_dead() && !thread.is_suspended() => {
                    thread.make_ready();
                    thread.clear_timeout_deadline(); // Clear any pending timeout
                    thread.woke_from_timeout = false; // Not a timeout wake
                    thread.disarm_recv_wait();
                    Some(thread.priority)
                }
                _ => None,
            }
        };

        let priority = match priority {
            Some(p) => p,
            None => return, // Thread not found or dead
        };

        if let Some(mut scheduler) = SCHEDULER.try_lock() {
            scheduler.add(thread_id, priority);
        } else {
            // Can't get scheduler lock, queue for later
            Self::queue_pending_wake(thread_id);
        }
    }

    /// Suspend a thread and deschedule it without destroying resources.
    /// Returns true if the thread exists and is now suspended.
    pub fn suspend_thread(thread_id: ThreadId) -> bool {
        let should_deschedule = {
            let mut repo = THREAD_REPOSITORY.lock();
            let Some(thread) = repo.get_mut(thread_id) else {
                return false;
            };
            if thread.is_dead() {
                return false;
            }
            if thread.is_suspended() {
                return true;
            }
            let was_blocked = thread.is_blocked();
            thread.mark_suspended(was_blocked);
            if !was_blocked {
                thread.make_blocked();
                thread.clear_timeout_deadline();
                thread.woke_from_timeout = false;
                thread.disarm_recv_wait();
            }
            true
        };

        if should_deschedule {
            let mut scheduler = SCHEDULER.lock();
            scheduler.remove(thread_id);
        }
        true
    }

    /// Resume a previously suspended thread.
    ///
    /// Returns:
    /// - `Some(true)` when resumed and made runnable
    /// - `Some(false)` when resumed but stays blocked (was blocked before suspend)
    /// - `None` when thread does not exist or is not suspended
    pub fn resume_thread(thread_id: ThreadId) -> Option<bool> {
        let wake_data = {
            let mut repo = THREAD_REPOSITORY.lock();
            let thread = repo.get_mut(thread_id)?;
            if thread.is_dead() || !thread.is_suspended() {
                return None;
            }
            let was_blocked_before_suspend = thread.suspended_was_blocked();
            thread.clear_suspended();
            if was_blocked_before_suspend {
                None
            } else {
                thread.make_ready();
                thread.clear_timeout_deadline();
                thread.woke_from_timeout = false;
                thread.disarm_recv_wait();
                Some(thread.priority)
            }
        };

        if let Some(priority) = wake_data {
            let mut scheduler = SCHEDULER.lock();
            scheduler.add(thread_id, priority);
            Some(true)
        } else {
            Some(false)
        }
    }

    /// Queue a thread ID for deferred wake (lock-free)
    fn queue_pending_wake(thread_id: ThreadId) {
        let tid = thread_id.as_u64();
        // Try each slot until we find an empty one
        for slot in &PENDING_WAKE_QUEUE {
            if slot
                .compare_exchange(0, tid, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return; // Successfully queued
            }
        }
        // All slots full - this is a bug if it happens frequently
        // The wake will be lost, but thread will eventually be woken by retry logic
    }

    /// Get page table root (CR3) of currently running thread
    pub fn current_page_table_root() -> Option<PhysAddr> {
        let thread_id = Self::current()?;
        let repo = THREAD_REPOSITORY.lock();
        repo.get(thread_id).map(|t| t.page_table_root)
    }

    /// Get current scheduler mode
    pub fn mode() -> SchedulerMode {
        if SCHEDULER_MODE.load(Ordering::Acquire) {
            SchedulerMode::Normal
        } else {
            SchedulerMode::Init
        }
    }

    /// Register a critical process (call during bootstrap)
    pub fn register_critical_thread(_thread_id: ThreadId) {
        CRITICAL_PROCESS_COUNT.fetch_add(1, Ordering::SeqCst);
    }

    /// Signal that a critical process has completed initialization
    ///
    /// When the last critical process signals, switches to NORMALMODE.
    ///
    /// Returns true if this was the last critical process and mode switched.
    pub fn signal_critical_process_ready() -> bool {
        let prev =
            CRITICAL_PROCESS_COUNT.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
                if count == 0 {
                    None
                } else {
                    Some(count - 1)
                }
            });

        match prev {
            Ok(remaining) => {
                if remaining == 1 {
                    // This was the last critical process
                    klibcluu::info("========================================");
                    klibcluu::info("All critical processes initialized");
                    klibcluu::info("Switching to NORMALMODE (preemptive)");
                    klibcluu::info("========================================");

                    SCHEDULER_MODE.store(true, Ordering::Release);
                    Self::demote_current_thread();

                    // Mark init (thread 1) as dead - its bootstrap job is done
                    Self::with_thread_mut(ThreadId::new(1), |thread| {
                        thread.make_dead();
                    });

                    true
                } else {
                    false
                }
            }
            Err(_) => false,
        }
    }

    /// Get number of critical processes still initializing
    pub fn critical_processes_remaining() -> usize {
        CRITICAL_PROCESS_COUNT.load(Ordering::SeqCst)
    }

    /// Check if running in INITMODE (cooperative)
    pub fn is_init_mode() -> bool {
        Self::mode() == SchedulerMode::Init
    }

    /// Check if running in NORMALMODE (preemptive)
    pub fn is_normal_mode() -> bool {
        Self::mode() == SchedulerMode::Normal
    }

    /// Handle timer tick (only in NORMALMODE)
    pub fn tick() {
        // Increment global tick counter
        let tick = SCHEDULER_TICKS.fetch_add(1, Ordering::SeqCst) + 1;

        // Log first 30 ticks and then every 50 ticks for debugging
        if tick <= 30 || tick.is_multiple_of(50) {
            klibcluu::trace("tick: ");
            klibcluu::log_dec(klibcluu::LogLevel::Trace, "", tick);
        }

        if Self::is_normal_mode() {
            if let Some(mut scheduler) = SCHEDULER.try_lock() {
                scheduler.tick();
            }
        }

        // Check for expired timeouts and wake blocked threads
        Self::check_timeouts();
    }

    /// Get current scheduler tick count
    pub fn current_tick() -> u64 {
        SCHEDULER_TICKS.load(Ordering::Acquire)
    }

    /// Check timeout heap for expired timeouts and wake threads
    ///
    /// O(k log m) where k = expired threads, m = threads with timeouts.
    /// Uses try_lock to avoid deadlock when called from interrupt context.
    fn check_timeouts() {
        let current_tick = Self::current_tick();

        // Pop all expired entries from the heap
        let mut candidates = alloc::vec::Vec::new();
        if let Some(mut heap) = TIMEOUT_HEAP.try_lock() {
            while let Some(&Reverse((deadline, thread_raw))) = heap.peek() {
                if deadline > current_tick {
                    break; // No more expired
                }
                heap.pop();
                candidates.push(ThreadId::new(thread_raw));
            }
        } else {
            // Can't get heap lock, skip this tick
            return;
        }

        if candidates.is_empty() {
            return;
        }

        // Try to get repo lock to verify and wake threads
        let mut repo = match THREAD_REPOSITORY.try_lock() {
            Some(r) => r,
            None => {
                // Can't get repo lock - re-queue candidates for next tick
                if let Some(mut heap) = TIMEOUT_HEAP.try_lock() {
                    for thread_id in candidates {
                        // Re-add with deadline 0 so they're checked next tick
                        heap.push(Reverse((current_tick, thread_id.as_u64())));
                    }
                }
                return;
            }
        };

        // Verify each candidate is still blocked with an EXPIRED timeout
        // Thread may have been woken by IPC and re-blocked with a new (later) deadline,
        // making the heap entry stale. We must check the actual deadline, not just presence.
        let mut to_wake = alloc::vec::Vec::new();
        for thread_id in candidates {
            if let Some(thread) = repo.get_mut(thread_id) {
                // Only wake if still blocked AND actual deadline has expired
                if thread.is_blocked() && thread.is_timeout_expired(current_tick) {
                    if thread.is_suspended() {
                        continue;
                    }
                    klibcluu::trace("check_timeouts: waking thread ");
                    klibcluu::log_dec(klibcluu::LogLevel::Trace, "", thread_id.as_u64());
                    klibcluu::trace(" at tick ");
                    klibcluu::log_dec(klibcluu::LogLevel::Trace, "", current_tick);
                    thread.make_ready();
                    thread.clear_timeout_deadline();
                    thread.woke_from_timeout = true;
                    thread.disarm_recv_wait();
                    to_wake.push((thread_id, thread.priority));
                }
            }
        }
        drop(repo);

        // Add woken threads to scheduler
        if !to_wake.is_empty() {
            if let Some(mut scheduler) = SCHEDULER.try_lock() {
                for (thread_id, priority) in to_wake {
                    scheduler.add(thread_id, priority);
                }
            } else {
                for (thread_id, _priority) in to_wake {
                    Self::queue_pending_wake(thread_id);
                }
            }
        }
    }

    /// Idle in kernel context until an interrupt wakes a thread
    ///
    /// Called when no threads are runnable. Enables interrupts and halts
    /// until an interrupt (timer, IRQ) potentially wakes a thread.
    fn idle_until_runnable() {
        // Enable interrupts so we can receive timer/IRQ
        x86_64::instructions::interrupts::enable();
        // Halt until next interrupt
        x86_64::instructions::hlt();
        // Interrupts are disabled again after hlt returns
    }

    /// Start the scheduler and jump to the first thread
    ///
    /// This function never returns - it jumps to userspace.
    ///
    /// # Safety
    ///
    /// Must be called with interrupts disabled.
    /// Must have at least one thread in the scheduler.
    pub unsafe fn start() -> ! {
        klibcluu::info("Starting scheduler...");

        // Pick the first thread to run
        let thread_id = Self::pick_next().expect("No threads to schedule");

        klibcluu::info("Jumping to first thread: ");
        klibcluu::log_dec(klibcluu::LogLevel::Info, "", thread_id.as_u64());

        // Set as current
        Self::set_current(thread_id);

        // Get thread context
        let context = Self::with_thread(thread_id, |t| t.context).expect("Thread disappeared");

        // Jump to thread context
        jump_to_thread(&context);
    }

    /// Called from syscall_entry.asm to perform context switch
    ///
    /// Saves current thread's context and returns pointer to next thread's context.
    ///
    /// # Returns
    ///
    /// Pointer to next thread's Context, or null if no switch needed
    ///
    /// # Safety
    ///
    /// `current_ctx_ptr` must point to a valid saved context for the current thread.
    /// The caller must ensure interrupts are in a safe state for switching.
    #[no_mangle]
    pub unsafe extern "C" fn schedule_and_switch(
        current_ctx_ptr: *const Context,
    ) -> *const Context {
        // Get current thread ID
        let current_id = match Self::current() {
            Some(id) => id,
            None => {
                klibcluu::error("schedule_and_switch: No current thread!");
                return core::ptr::null();
            }
        };

        // Save current thread's context
        if !current_ctx_ptr.is_null() {
            Self::save_context(current_id, &*current_ctx_ptr);
        }

        // Expire current thread (moves to expired array for fair scheduling)
        Self::expire_current_thread(current_id);

        Self::drain_pending_wake();

        // Pick next thread, idling if none ready
        let next_id = loop {
            if let Some(id) = Self::pick_next() {
                break id;
            }
            // No threads ready - idle until interrupt wakes one
            Self::idle_until_runnable();
            // After interrupt, drain any pending wakes and retry
            Self::drain_pending_wake();
        };

        // No switch needed if same thread
        if next_id == current_id {
            return core::ptr::null();
        }

        // Set next thread as current
        Self::set_current(next_id);

        // Return pointer to next thread's context
        Self::get_context_ptr(next_id)
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Private Helper Functions
    // ═══════════════════════════════════════════════════════════════════════

    /// Save context to thread structure
    fn save_context(thread_id: ThreadId, context: &Context) {
        // Validate RIP before saving (sanity check for memory corruption)
        // RIP should be in userspace range (0x400000 - 0x7FFFFFFFFFFF)
        // Very low addresses (< 0x1000) are almost certainly invalid
        if context.rip < 0x1000 {
            klibcluu::error("FATAL: Attempted to save context with invalid RIP=0x");
            klibcluu::log_hex(klibcluu::LogLevel::Error, "", context.rip);
            klibcluu::error("This indicates memory corruption - halting");
            loop {
                x86_64::instructions::hlt();
            }
        }
        // Check if canonical (bits 63:47 must be 0 for userspace)
        if (context.rip >> 47) != 0 {
            klibcluu::error("FATAL: Attempted to save context with non-canonical RIP=0x");
            klibcluu::log_hex(klibcluu::LogLevel::Error, "", context.rip);
            klibcluu::error("This indicates memory corruption - halting");
            loop {
                x86_64::instructions::hlt();
            }
        }
        Self::with_thread_mut(thread_id, |thread| {
            thread.context = *context;
        });
    }

    fn demote_current_thread() {
        let current = match Self::current() {
            Some(id) => id,
            None => return,
        };

        Self::with_thread_mut(current, |thread| {
            thread.priority = Priority::LOWEST;
        });
    }

    /// Expire current thread (move to expired array for fair scheduling)
    ///
    /// After a thread yields or uses its timeslice, it moves to the expired
    /// array. Once all threads in active have run, arrays swap and the cycle
    /// repeats. This guarantees every thread gets a timeslice per epoch.
    fn expire_current_thread(thread_id: ThreadId) {
        let state = Self::with_thread(thread_id, |t| (t.is_dead(), t.is_blocked()))
            .unwrap_or((true, false));
        if state.0 || state.1 {
            // Dead or blocked threads don't go back to scheduler
            return;
        }

        let mut scheduler = SCHEDULER.lock();
        scheduler.expire_current();
    }

    fn drain_pending_wake() {
        // Collect all pending thread IDs first (lock-free)
        let mut pending_threads = alloc::vec::Vec::new();
        for slot in &PENDING_WAKE_QUEUE {
            let raw = slot.swap(0, Ordering::AcqRel);
            if raw != 0 {
                pending_threads.push(ThreadId::new(raw));
            }
        }

        if pending_threads.is_empty() {
            return;
        }

        // Batch update threads (minimize lock hold time)
        let mut to_schedule = alloc::vec::Vec::new();
        {
            let mut repo = THREAD_REPOSITORY.lock();
            for thread_id in pending_threads {
                if let Some(thread) = repo.get_mut(thread_id) {
                    if !thread.is_dead() {
                        thread.make_ready();
                        thread.clear_timeout_deadline(); // Clear any pending timeout
                        thread.woke_from_timeout = false; // Not a timeout wake
                        thread.disarm_recv_wait();
                        to_schedule.push((thread_id, thread.priority));
                    }
                }
            }
        } // Drop repo lock before acquiring scheduler lock

        // Batch add to scheduler (reduces lock contention)
        if !to_schedule.is_empty() {
            let mut scheduler = SCHEDULER.lock();
            for (thread_id, priority) in to_schedule {
                scheduler.add(thread_id, priority);
            }
        }
    }

    /// Get pointer to thread's context
    fn get_context_ptr(thread_id: ThreadId) -> *const Context {
        Self::with_thread(thread_id, |thread| &thread.context as *const Context)
            .unwrap_or(core::ptr::null())
    }
}

/// Jump to a thread's context (initial userspace entry via iretq)
///
/// # Safety
///
/// Must be called with interrupts disabled and valid context
unsafe fn jump_to_thread(context: &Context) -> ! {
    klibcluu::trace("jump_to_thread: entering userspace at RIP 0x");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "", context.rip);

    setup_kernel_stack();
    load_address_space(context.cr3);
    klibcluu::info("Entering userspace...");
    enter_userspace(context);
}

/// Setup kernel stack for SYSCALL and interrupts
///
/// Updates PerCpuData.kernel_rsp (for SYSCALL) and TSS.RSP0 (for interrupts)
/// to point to the current thread's kernel stack.
///
/// # SMP Design
///
/// - Each CPU has its own PerCpuData (GS points to it)
/// - Each thread will have its own kernel stack (TODO: Phase 8)
/// - Before running a thread: Update PerCpuData.kernel_rsp to thread's stack
///
/// # Current Implementation
///
/// All threads share BSP_STACK (single-CPU, single kernel stack)
unsafe fn setup_kernel_stack() {
    extern "C" {
        static BSP_STACK: u8;
    }
    let kernel_stack_top = ((&raw const BSP_STACK as u64) + (64 * 1024)) & !0xF;

    // Update PerCpuData.kernel_rsp (for SYSCALL path)
    crate::architecture::x86_64::syscall::set_current_thread_kernel_stack(kernel_stack_top);

    // Update TSS.RSP0 (for interrupt/exception path)
    set_tss_rsp0(kernel_stack_top);

    // Verify kernel stack is set correctly
    let verified = crate::architecture::x86_64::syscall::get_current_kernel_stack();
    if verified != kernel_stack_top {
        klibcluu::error("FATAL: Kernel stack setup failed!");
        loop {
            x86_64::instructions::hlt();
        }
    }
}

/// Load address space by switching CR3
///
/// Switches to the thread's page table. Kernel mappings must be present
/// in the new address space for continued execution.
unsafe fn load_address_space(cr3: u64) {
    use x86_64::registers::control::{Cr3, Cr3Flags};
    use x86_64::structures::paging::PhysFrame;
    use x86_64::PhysAddr;

    let frame = PhysFrame::containing_address(PhysAddr::new(cr3));
    Cr3::write(frame, Cr3Flags::empty());
}

/// Enter userspace via iretq
///
/// Builds interrupt frame on stack and executes iretq to switch to Ring 3.
unsafe fn enter_userspace(context: &Context) -> ! {
    klibcluu::trace("Executing iretq to userspace");
    klibcluu::log_hex(klibcluu::LogLevel::Info, "Entry RIP=", context.rip);
    klibcluu::log_hex(klibcluu::LogLevel::Info, "Entry RSP=", context.rsp);
    klibcluu::log_hex(klibcluu::LogLevel::Info, "Entry CS=", context.cs);
    klibcluu::log_hex(klibcluu::LogLevel::Info, "Entry SS=", context.ss);

    core::arch::asm!(
        // Initialize userspace segment registers (DS, ES, FS)
        "mov ax, 0x2b",
        "mov ds, ax",
        "mov es, ax",
        "mov fs, ax",
        // Build iretq frame
        "push {0}",      // SS
        "push {1}",      // RSP
        "push {2}",      // RFLAGS
        "push {3}",      // CS
        "push {4}",      // RIP
        // Swap GS to user mode — kernel GS base moves to KernelGsBase MSR
        // so the next syscall's swapgs will load it correctly.
        "swapgs",
        "iretq",
        in(reg) context.ss,
        in(reg) context.rsp,
        in(reg) context.rflags,
        in(reg) context.cs,
        in(reg) context.rip,
        options(noreturn)
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mode_tracking() {
        assert_eq!(ThreadManager::mode(), SchedulerMode::Init);
        assert!(ThreadManager::is_init_mode());
        assert!(!ThreadManager::is_normal_mode());
    }

    #[test]
    fn test_critical_process_counting() {
        let tid1 = ThreadId::new(1);
        let tid2 = ThreadId::new(2);
        let tid3 = ThreadId::new(3);

        ThreadManager::register_critical_thread(tid1);
        ThreadManager::register_critical_thread(tid2);
        ThreadManager::register_critical_thread(tid3);

        assert!(!ThreadManager::signal_critical_process_ready());
        assert!(!ThreadManager::signal_critical_process_ready());
        assert!(ThreadManager::signal_critical_process_ready()); // Last one

        assert!(ThreadManager::is_normal_mode());
    }
}
