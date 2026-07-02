//! Thread Manager - Global Thread Scheduling
//!
//! This module provides the global scheduler instance and thread management.
//!
//! # Scheduler Modes
//!
//! - **INITMODE**: Cooperative scheduling for critical processes during boot
//! - **NORMALMODE**: Preemptive scheduling for normal operation
//!
//! # Lock ordering
//!
//! THREAD_REPOSITORY → SCHEDULER → ENDPOINT_SHARDS / TOKEN_TABLE_SHARDS
//! (ENDPOINT_SHARDS and TOKEN_TABLE_SHARDS are independent — never nested with each other)

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
    pub server_thread_id: Option<ThreadId>,
}

// ═══════════════════════════════════════════════════════════════════════════
// O(1) Reply Map — Open-Addressing Hash Table
// ═══════════════════════════════════════════════════════════════════════════

const REPLY_MAP_SLOTS: usize = 256;
const REPLY_MAP_MASK: usize = REPLY_MAP_SLOTS - 1;

/// Fixed-size open-addressing hash map for O(1) reply lookups.
/// Uses linear probing with ReplyId as hash key.
/// 50% load cap (max 128 entries) ensures O(1) amortized performance.
struct ReplyMap<T: Copy> {
    slots: [Option<(ReplyId, T)>; REPLY_MAP_SLOTS],
    count: usize,
}

impl<T: Copy> ReplyMap<T> {
    const fn new() -> Self {
        Self {
            slots: [None; REPLY_MAP_SLOTS],
            count: 0,
        }
    }

    /// Insert entry. Returns false if table >50% full or duplicate key.
    fn insert(&mut self, reply_id: ReplyId, data: T) -> bool {
        if self.count >= REPLY_MAP_SLOTS / 2 {
            return false;
        }
        let mut idx = reply_id.as_u64() as usize & REPLY_MAP_MASK;
        for _ in 0..REPLY_MAP_SLOTS {
            match &self.slots[idx] {
                None => {
                    self.slots[idx] = Some((reply_id, data));
                    self.count += 1;
                    return true;
                }
                Some((rid, _)) if *rid == reply_id => return false,
                _ => idx = (idx + 1) & REPLY_MAP_MASK,
            }
        }
        false
    }

    /// Lookup by ReplyId. O(1) amortized.
    fn get(&self, reply_id: ReplyId) -> Option<&T> {
        let mut idx = reply_id.as_u64() as usize & REPLY_MAP_MASK;
        for _ in 0..REPLY_MAP_SLOTS {
            match &self.slots[idx] {
                Some((rid, data)) if *rid == reply_id => return Some(data),
                None => return None,
                _ => idx = (idx + 1) & REPLY_MAP_MASK,
            }
        }
        None
    }

    /// Mutable lookup by ReplyId. O(1) amortized.
    fn get_mut(&mut self, reply_id: ReplyId) -> Option<&mut T> {
        let mut idx = reply_id.as_u64() as usize & REPLY_MAP_MASK;
        let found = loop {
            match &self.slots[idx] {
                Some((rid, _)) if *rid == reply_id => break Some(idx),
                None => break None,
                _ => idx = (idx + 1) & REPLY_MAP_MASK,
            }
        };
        found.and_then(move |i| self.slots[i].as_mut().map(|(_, data)| data))
    }

    /// Remove entry. O(1) amortized. Uses backward-shift deletion.
    fn remove(&mut self, reply_id: ReplyId) -> Option<T> {
        // Find the entry
        let mut idx = reply_id.as_u64() as usize & REPLY_MAP_MASK;
        loop {
            match &self.slots[idx] {
                Some((rid, _)) if *rid == reply_id => break,
                None => return None,
                _ => idx = (idx + 1) & REPLY_MAP_MASK,
            }
        }
        let data = self.slots[idx].take().unwrap().1;
        self.count -= 1;

        // Backward-shift: independent scan variable j always advances
        let mut empty = idx;
        let mut j = (idx + 1) & REPLY_MAP_MASK;
        loop {
            if self.slots[j].is_none() {
                break;
            }
            let natural = self.slots[j].as_ref().unwrap().0.as_u64() as usize & REPLY_MAP_MASK;
            let should_move = if j >= empty {
                natural <= empty || natural > j
            } else {
                natural <= empty && natural > j
            };
            if should_move {
                self.slots[empty] = self.slots[j].take();
                empty = j;
            }
            j = (j + 1) & REPLY_MAP_MASK;
        }
        Some(data)
    }
}

use alloc::collections::BinaryHeap;
use core::cmp::Reverse;
use core::cell::UnsafeCell;
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
}

/// Lock-free wrapper for ReplyMap. Single-CPU kernel, syscall handlers are
/// non-reentrant, interrupt handlers never touch reply maps.
struct PerCpuReplyMap<T: Copy> {
    inner: UnsafeCell<ReplyMap<T>>,
}
unsafe impl<T: Copy> Sync for PerCpuReplyMap<T> {}

impl<T: Copy> PerCpuReplyMap<T> {
    const fn new() -> Self {
        Self { inner: UnsafeCell::new(ReplyMap::new()) }
    }
    /// # Safety: single-CPU kernel, non-reentrant syscall handlers
    unsafe fn get(&self) -> &mut ReplyMap<T> {
        &mut *self.inner.get()
    }
}

/// Map from ReplyId to CallReplyInfo for IPC call/reply (O(1) hash map).
static CALL_REPLY_MAP: PerCpuReplyMap<CallReplyInfo> = PerCpuReplyMap::new();

/// Map from ReplyId to FaultReplyInfo for fault IPC (O(1) hash map).
static FAULT_REPLY_MAP: PerCpuReplyMap<FaultReplyInfo> = PerCpuReplyMap::new();

/// Counter for generating unique ReplyIds
static NEXT_REPLY_ID: AtomicU64 = AtomicU64::new(1);

/// Current scheduler mode (starts in INITMODE)
static SCHEDULER_MODE: AtomicBool = AtomicBool::new(false); // false = INIT, true = NORMAL

/// Number of critical processes still initializing
static CRITICAL_PROCESS_COUNT: AtomicUsize = AtomicUsize::new(0);
static CURRENT_THREAD_ID: AtomicU64 = AtomicU64::new(u64::MAX);

/// Global live-thread counter (incremented on create, decremented on death).
static TOTAL_THREAD_COUNT: AtomicU64 = AtomicU64::new(0);

/// Hard cap on total live threads system-wide.
const MAX_TOTAL_THREADS: u64 = 4096;

/// Global scheduler tick counter (incremented by timer interrupt)
static SCHEDULER_TICKS: AtomicU64 = AtomicU64::new(0);

/// Multi-slot pending wake queue (lock-free)
/// Each slot holds a thread ID (0 = empty). Allows multiple concurrent wakes.
const PENDING_WAKE_SLOTS: usize = 32;
const WAKE_ZERO: AtomicU64 = AtomicU64::new(0);
static PENDING_WAKE_QUEUE: [AtomicU64; PENDING_WAKE_SLOTS] = [WAKE_ZERO; PENDING_WAKE_SLOTS];
static PENDING_WAKE_OVERFLOW: AtomicU64 = AtomicU64::new(0);

/// Read the cumulative count of pending-wake-queue slot exhaustion events (H10).
/// Each increment means a wake had no free slot — the wakee may sleep longer
/// than expected and rely on a later kick to make progress.
pub fn pending_wake_overflow_count() -> u64 {
    PENDING_WAKE_OVERFLOW.load(Ordering::Relaxed)
}

// ═══════════════════════════════════════════════════════════════════════════
// Deferred Fault Notification Queue (lock-free, IST-safe)
// ═══════════════════════════════════════════════════════════════════════════
//
// When try_forward_fault fails (IST try_lock contention), the fault handler
// queues a deferred notification here. Timer tick drains it using try_send.
//
// Atomics protocol:
// - Writer (IST): stores EP, TYPE, ADDR, ERR, RIP first, then stores TID
//   with Release ordering. TID != 0 signals "slot is occupied".
// - Reader (tick): swaps TID with 0 using Acquire ordering, then reads
//   the other fields. Acquire on TID ensures visibility of all prior stores.
const DEFERRED_FAULT_SLOTS: usize = 16;
const FAULT_ZERO: AtomicU64 = AtomicU64::new(0);
static DEFERRED_FAULT_TID: [AtomicU64; DEFERRED_FAULT_SLOTS] = [FAULT_ZERO; DEFERRED_FAULT_SLOTS];
static DEFERRED_FAULT_EP: [AtomicU64; DEFERRED_FAULT_SLOTS] = [FAULT_ZERO; DEFERRED_FAULT_SLOTS];
static DEFERRED_FAULT_TYPE: [AtomicU64; DEFERRED_FAULT_SLOTS] = [FAULT_ZERO; DEFERRED_FAULT_SLOTS];
static DEFERRED_FAULT_ADDR: [AtomicU64; DEFERRED_FAULT_SLOTS] = [FAULT_ZERO; DEFERRED_FAULT_SLOTS];
static DEFERRED_FAULT_ERR: [AtomicU64; DEFERRED_FAULT_SLOTS] = [FAULT_ZERO; DEFERRED_FAULT_SLOTS];
static DEFERRED_FAULT_RIP: [AtomicU64; DEFERRED_FAULT_SLOTS] = [FAULT_ZERO; DEFERRED_FAULT_SLOTS];
static DEFERRED_FAULT_OVERFLOW: AtomicU64 = AtomicU64::new(0);

/// Read the cumulative count of deferred-fault-queue slot exhaustion events (H9).
/// Each increment means a fault could not be queued and was dropped on the floor —
/// the faulting thread may have been killed without notifying its supervisor.
pub fn deferred_fault_overflow_count() -> u64 {
    DEFERRED_FAULT_OVERFLOW.load(Ordering::Relaxed)
}

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
        let suspended = thread.is_suspended();

        // Insert into repository
        let mut repo = THREAD_REPOSITORY.lock();
        repo.insert(thread).expect("Failed to insert thread");
        drop(repo);

        // Threads created SUSPENDED stay out of the scheduler runqueue;
        // userspace must call thread_resume to make them runnable. Used by
        // procmgr to install per-thread VFS views before the thread runs
        // (closes a SET_VIEW vs first-call race).
        if !suspended {
            let mut scheduler = SCHEDULER.lock();
            scheduler.add(thread_id, priority);
        }

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

    /// Global thread count from atomic counter.
    pub fn thread_count_global() -> u64 {
        TOTAL_THREAD_COUNT.load(Ordering::Relaxed)
    }

    /// Number of threads that are not in Dead state.
    pub fn thread_count_live() -> usize {
        THREAD_REPOSITORY
            .lock()
            .iter()
            .filter(|(_, thread)| !thread.is_dead())
            .count()
    }

    /// Collect all live (non-Dead) thread IDs into a Vec.
    /// Used by InvokeOp::ThreadEnumerate to serve /proc readdir
    /// without going through procmgr IPC.
    pub fn enumerate_live_tids() -> alloc::vec::Vec<ThreadId> {
        THREAD_REPOSITORY
            .lock()
            .iter()
            .filter(|(_, thread)| !thread.is_dead())
            .map(|(id, _)| *id)
            .collect()
    }

    /// Collect live thread IDs visible to `caller_session_id`.
    /// session_id == 0 is root/system scope: sees all threads.
    /// Non-zero: sees only threads with the same session_id.
    pub fn enumerate_live_tids_in_session(caller_session_id: u64) -> alloc::vec::Vec<ThreadId> {
        THREAD_REPOSITORY
            .lock()
            .iter()
            .filter(|(_, t)| !t.is_dead())
            .filter(|(_, t)| caller_session_id == 0 || t.session_id == caller_session_id)
            .map(|(id, _)| *id)
            .collect()
    }

    /// Read the session_id of a thread. Returns 0 if thread not found
    /// (treat unknown as root scope — fail-safe for visibility).
    pub fn thread_session_id(id: ThreadId) -> u64 {
        THREAD_REPOSITORY
            .lock()
            .get(id)
            .map(|t| t.session_id)
            .unwrap_or(0)
    }

    /// Set the session_id on a thread. Called by procmgr via
    /// InvokeOp::ThreadSetSession after thread_create, before thread_resume.
    pub fn set_thread_session(id: ThreadId, session_id: u64) -> bool {
        if let Some(mut repo) = THREAD_REPOSITORY.try_lock() {
            if let Some(thread) = repo.get_mut(id) {
                thread.session_id = session_id;
                return true;
            }
        }
        false
    }

    /// Fallible thread ID allocation with global limit enforcement.
    pub fn try_alloc_thread_id() -> Result<ThreadId, &'static str> {
        loop {
            let current = TOTAL_THREAD_COUNT.load(Ordering::Relaxed);
            if current >= MAX_TOTAL_THREADS {
                return Err("Global thread limit reached");
            }
            if TOTAL_THREAD_COUNT
                .compare_exchange_weak(current, current + 1, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
        }
        let mut repo = THREAD_REPOSITORY.lock();
        Ok(repo.alloc_id())
    }

    /// Allocate a new ThreadId
    pub fn alloc_thread_id() -> ThreadId {
        Self::try_alloc_thread_id().expect("Thread limit reached during boot")
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
    ///
    /// Also revokes all tokens that reference this thread (cleanup hook).
    pub fn mark_thread_dead(thread_id: ThreadId) -> bool {
        let notification_wait = Self::with_thread_mut(thread_id, |thread| {
            thread.make_dead();
            thread.clear_timeout_deadline();
            thread.woke_from_timeout = false;
            thread.disarm_recv_wait();
            thread.clear_suspended();
            thread.notification_wait.take()
        });
        let found = notification_wait.is_some();
        // Clean up notification waiter registration if thread was waiting
        if let Some(Some(notif_id)) = notification_wait {
            crate::ipc::notification::clear_waiter(notif_id, thread_id);
        }
        if !found {
            return false;
        }
        // Decrement global thread count
        TOTAL_THREAD_COUNT.fetch_sub(1, Ordering::Relaxed);
        {
            let mut scheduler = SCHEDULER.lock();
            scheduler.remove(thread_id);
        }
        // Outside scheduler lock: revoke tokens referencing the dead thread.
        // This prevents dangling token references to destroyed objects.
        crate::token::table::revoke_tokens_for_object(crate::token::scope::ObjectRef::Thread(
            thread_id,
        ));

        // Clean up CALL_REPLY_MAP entries involving this thread.
        // - Dead caller: remove the entry (no one to receive the reply).
        // - Dead server: wake the blocked caller with an error.
        // NOTE: This is an O(REPLY_MAP_SLOTS) scan (currently 256). Acceptable at
        // current scale. If slot count grows significantly, consider a per-thread
        // reply set for O(1) cleanup.
        {
            let map = unsafe { CALL_REPLY_MAP.get() };
            let mut callers_to_wake = alloc::vec::Vec::new();
            let mut to_remove = alloc::vec::Vec::new();

            for i in 0..REPLY_MAP_SLOTS {
                if let Some((_rid, info)) = &map.slots[i] {
                    if info.caller == thread_id {
                        to_remove.push(map.slots[i].as_ref().unwrap().0);
                    } else if info.server_thread_id == Some(thread_id) {
                        callers_to_wake.push(info.caller);
                        to_remove.push(map.slots[i].as_ref().unwrap().0);
                    }
                }
            }
            for rid in to_remove {
                map.remove(rid);
            }
            for caller in callers_to_wake {
                // Encode as negative errno in rax (same convention as syscall return path).
                // deliver_reply would normally overwrite rax with byte count.
                Self::with_thread_mut(caller, |t| {
                    t.context.rax = crate::Error::NotFound.to_errno() as u64;
                });
                Self::wake_thread(caller);
            }
        }

        // Clean up FAULT_REPLY_MAP entries involving this thread.
        // Dead faulted_thread: remove the entry (thread is gone).
        // Dead server: the faulted thread stays blocked (no good recovery).
        {
            let map = unsafe { FAULT_REPLY_MAP.get() };
            let mut to_remove = alloc::vec::Vec::new();
            for i in 0..REPLY_MAP_SLOTS {
                if let Some((_rid, info)) = &map.slots[i] {
                    if info.faulted_thread == thread_id {
                        to_remove.push(map.slots[i].as_ref().unwrap().0);
                    }
                }
            }
            for rid in to_remove {
                map.remove(rid);
            }
        }

        // LAST: remove the Thread struct from the repository.
        // All cleanup above is done; no references remain.
        {
            let mut repo = THREAD_REPOSITORY.lock();
            repo.remove(thread_id);
        }

        true
    }

    /// Allocate a new unique ReplyId
    pub fn alloc_reply_id() -> ReplyId {
        ReplyId::new(NEXT_REPLY_ID.fetch_add(1, Ordering::SeqCst))
    }

    /// Store call reply info for a reply ID. Returns false if map is full.
    pub fn set_call_reply_info(reply_id: ReplyId, info: CallReplyInfo) -> bool {
        unsafe { CALL_REPLY_MAP.get() }.insert(reply_id, info)
    }

    /// Take and remove call reply info for a reply ID (one-time use)
    pub fn take_call_reply_info(reply_id: ReplyId) -> Option<CallReplyInfo> {
        unsafe { CALL_REPLY_MAP.get() }.remove(reply_id)
    }

    /// Check if call reply info exists for a reply ID
    pub fn has_call_reply_info(reply_id: ReplyId) -> bool {
        unsafe { CALL_REPLY_MAP.get() }.get(reply_id).is_some()
    }

    /// Store fault reply info for a reply ID. Returns false if map is full.
    pub fn set_fault_reply_info(reply_id: ReplyId, info: FaultReplyInfo) -> bool {
        unsafe { FAULT_REPLY_MAP.get() }.insert(reply_id, info)
    }

    /// Take and remove fault reply info for a reply ID (one-time use)
    pub fn take_fault_reply_info(reply_id: ReplyId) -> Option<FaultReplyInfo> {
        unsafe { FAULT_REPLY_MAP.get() }.remove(reply_id)
    }

    /// Bind a reply_id to the server thread that received the call message.
    pub fn bind_call_reply_to_server(reply_id: ReplyId, server: ThreadId) -> bool {
        if let Some(info) = unsafe { CALL_REPLY_MAP.get() }.get_mut(reply_id) {
            info.server_thread_id = Some(server);
            true
        } else {
            false
        }
    }

    /// Bind a fault reply_id to the server thread that received the fault message.
    pub fn bind_fault_reply_to_server(reply_id: ReplyId, server: ThreadId) -> bool {
        if let Some(info) = unsafe { FAULT_REPLY_MAP.get() }.get_mut(reply_id) {
            info.server_thread_id = Some(server);
            true
        } else {
            false
        }
    }

    /// Take call reply info, verifying the server thread matches.
    /// Returns None if reply_id not found OR server_thread_id doesn't match.
    pub fn take_call_reply_info_verified(
        reply_id: ReplyId,
        server: ThreadId,
    ) -> Option<CallReplyInfo> {
        let map = unsafe { CALL_REPLY_MAP.get() };
        let info = map.get(reply_id)?;
        match info.server_thread_id {
            Some(bound) if bound == server => map.remove(reply_id),
            _ => None,
        }
    }

    /// Take fault reply info, verifying the server thread matches.
    pub fn take_fault_reply_info_verified(
        reply_id: ReplyId,
        server: ThreadId,
    ) -> Option<FaultReplyInfo> {
        let map = unsafe { FAULT_REPLY_MAP.get() };
        let info = map.get(reply_id)?;
        match info.server_thread_id {
            Some(bound) if bound == server => map.remove(reply_id),
            _ => None,
        }
    }

    /// Prepare scheduler after fault forwarding without context switch.
    ///
    /// Drains pending wakes (so the fault receiver is in the run queue)
    /// and sets current thread to idle (tid=0) so that the next timer
    /// interrupt will call schedule_and_switch.
    pub fn prepare_idle_after_fault() {
        Self::drain_pending_wake();
        // Set current to idle pseudo-thread so timer_interrupt_should_schedule
        // returns 1 for the kernel-mode idle loop we're about to enter.
        CURRENT_THREAD_ID.store(0, Ordering::Release);
        let mut current = CURRENT_THREAD.lock();
        *current = None;
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
            Some(id) => {
                klibcluu::warn("fault_sched: next tid=");
                klibcluu::log_dec(klibcluu::LogLevel::Warn, "", id.as_u64());
                id
            }
            None => {
                klibcluu::error("schedule_next_from_fault: no runnable threads!");
                loop {
                    x86_64::instructions::interrupts::enable();
                    x86_64::instructions::hlt();
                }
            }
        };
        Self::set_current(next_id);
        let ctx = Self::get_context_ptr(next_id);
        if !ctx.is_null() {
            let cs = unsafe { (*ctx).cs };
            klibcluu::warn("fault_sched: target CS=");
            klibcluu::log_hex(klibcluu::LogLevel::Warn, "", cs);
            let rip = unsafe { (*ctx).rip };
            klibcluu::warn("fault_sched: target RIP=");
            klibcluu::log_hex(klibcluu::LogLevel::Warn, "", rip);
        }
        ctx
    }

    pub fn block_current() {
        let current = match Self::current() {
            Some(id) => id,
            None => return,
        };
        Self::with_thread_mut(current, |thread| {
            // Non-recv block: thread is entering sys_call / futex / notification
            // wait, not a recv-wait. Any leftover recv_wait_ticket from a prior
            // sys_recv is now stale — clear it so that pop_next_receiver_to_wake
            // on a prior endpoint scrubs the leftover waiter instead of
            // direct-delivering to this thread spuriously (rax=0 from sys_call,
            // VfsFile{fd:0} downstream).
            thread.recv_wait_ticket = 0;
            thread.make_blocked();
        });
    }

    /// Block current thread only if its recv-wait ticket is still armed.
    ///
    /// Returns `true` when the thread transitioned to blocked state, `false`
    /// when a wake raced before the block (ticket changed or wait disarmed).
    pub fn block_current_recv_wait(ticket: u64) -> bool {
        let current = match Self::current() {
            Some(id) => id,
            None => return false,
        };
        Self::with_thread_mut(current, |thread| {
            if !thread.should_block_for_recv_wait(ticket) {
                return false;
            }
            thread.make_blocked();
            true
        })
        .unwrap_or(false)
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
            // See block_current: clear stale recv-wait ticket on non-recv block.
            thread.recv_wait_ticket = 0;
            thread.set_timeout_deadline(deadline);
            thread.make_blocked();
        });

        // Add to timeout heap for efficient expiry checking.
        // This runs in syscall/thread context, so we must not drop timeout registration.
        TIMEOUT_HEAP
            .lock()
            .push(Reverse((deadline, current.as_u64())));
    }

    /// Block current thread with timeout only if recv-wait ticket is still armed.
    ///
    /// Returns `true` when the thread transitioned to blocked state, `false`
    /// when a wake raced before the block (ticket changed or wait disarmed).
    pub fn block_current_recv_wait_with_timeout(ticket: u64, deadline: u64) -> bool {
        let current = match Self::current() {
            Some(id) => id,
            None => return false,
        };
        let should_block = Self::with_thread_mut(current, |thread| {
            if !thread.should_block_for_recv_wait(ticket) {
                return false;
            }
            thread.set_timeout_deadline(deadline);
            thread.make_blocked();
            true
        })
        .unwrap_or(false);

        if should_block {
            TIMEOUT_HEAP
                .lock()
                .push(Reverse((deadline, current.as_u64())));
        }

        should_block
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
        for slot in &PENDING_WAKE_QUEUE {
            if slot
                .compare_exchange(0, tid, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return;
            }
        }
        PENDING_WAKE_OVERFLOW.fetch_add(1, Ordering::Relaxed);
    }

    /// Queue a deferred fault notification (IST-safe, lock-free).
    ///
    /// Called from PF/GPF handlers when try_forward_fault fails due to
    /// lock contention. The notification is drained by drain_deferred_faults()
    /// on the next timer tick.
    pub fn queue_deferred_fault(
        tid: ThreadId,
        endpoint: crate::token::EndpointId,
        fault_type: u64,
        fault_addr: u64,
        error_code: u64,
        rip: u64,
    ) {
        let tid_raw = tid.as_u64();
        for i in 0..DEFERRED_FAULT_SLOTS {
            // Check if slot is empty (TID == 0)
            if DEFERRED_FAULT_TID[i].load(Ordering::Relaxed) != 0 {
                continue;
            }
            // Write data fields first (before the TID flag)
            DEFERRED_FAULT_EP[i].store(endpoint.0, Ordering::Relaxed);
            DEFERRED_FAULT_TYPE[i].store(fault_type, Ordering::Relaxed);
            DEFERRED_FAULT_ADDR[i].store(fault_addr, Ordering::Relaxed);
            DEFERRED_FAULT_ERR[i].store(error_code, Ordering::Relaxed);
            DEFERRED_FAULT_RIP[i].store(rip, Ordering::Relaxed);
            // Release store on TID makes all prior stores visible to the reader
            if DEFERRED_FAULT_TID[i]
                .compare_exchange(0, tid_raw, Ordering::Release, Ordering::Relaxed)
                .is_ok()
            {
                return;
            }
            // CAS failed — another IST writer took this slot. Try next.
        }
        // All slots full — fault notification lost. Thread is already dead.
        DEFERRED_FAULT_OVERFLOW.fetch_add(1, Ordering::Relaxed);
        klibcluu::warn("deferred fault queue full — notification dropped");
    }

    /// Drain deferred fault notifications. Called from tick() after check_timeouts().
    ///
    /// For each queued fault, sends the notification to the fault endpoint via
    /// try_send. If the endpoint is contended, the notification is re-queued
    /// for the next tick.
    fn drain_deferred_faults() {
        use crate::ipc::endpoint::{self, UserMessage};

        for i in 0..DEFERRED_FAULT_SLOTS {
            // Acquire swap on TID: if non-zero, we own the slot's data
            let tid_raw = DEFERRED_FAULT_TID[i].swap(0, Ordering::Acquire);
            if tid_raw == 0 {
                continue;
            }
            let ep_raw = DEFERRED_FAULT_EP[i].load(Ordering::Relaxed);
            let fault_type = DEFERRED_FAULT_TYPE[i].load(Ordering::Relaxed);
            let fault_addr = DEFERRED_FAULT_ADDR[i].load(Ordering::Relaxed);
            let error_code = DEFERRED_FAULT_ERR[i].load(Ordering::Relaxed);
            let rip = DEFERRED_FAULT_RIP[i].load(Ordering::Relaxed);

            let fault_ep = crate::token::EndpointId(ep_raw);

            // Build fault message (same format as try_forward_fault but no reply_id —
            // the thread is already dead, so no resume is possible)
            let mut msg_bytes = [0u8; core::mem::size_of::<UserMessage>()];
            let msg = unsafe { &mut *(msg_bytes.as_mut_ptr() as *mut UserMessage) };
            msg.tag.label = 0xFA017;
            msg.tag.words = 6;
            msg.tag.extra = 0; // No reply cap — thread is dead
            msg.tag._pad = 0;
            msg.words[0] = fault_type as usize;
            msg.words[1] = fault_addr as usize;
            msg.words[2] = error_code as usize;
            msg.words[3] = rip as usize;
            msg.words[4] = tid_raw as usize;
            msg.words[5] = 0; // No reply_id

            match endpoint::try_send(fault_ep, &msg_bytes) {
                Ok(receiver_to_wake) => {
                    if let Some(thread_id) = receiver_to_wake {
                        Self::queue_pending_wake(thread_id);
                    }
                    klibcluu::warn("Deferred fault notification sent");
                }
                Err(_) => {
                    // Still contended — re-queue for next tick
                    DEFERRED_FAULT_EP[i].store(ep_raw, Ordering::Relaxed);
                    DEFERRED_FAULT_TYPE[i].store(fault_type, Ordering::Relaxed);
                    DEFERRED_FAULT_ADDR[i].store(fault_addr, Ordering::Relaxed);
                    DEFERRED_FAULT_ERR[i].store(error_code, Ordering::Relaxed);
                    DEFERRED_FAULT_RIP[i].store(rip, Ordering::Relaxed);
                    DEFERRED_FAULT_TID[i].store(tid_raw, Ordering::Release);
                }
            }
        }
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

        // Account CPU tick to the currently running thread.
        // CRITICAL: tick() runs from the timer IRQ handler with interrupts
        // disabled. Using a *blocking* THREAD_REPOSITORY.lock() here would
        // spin-wait inside the ISR if any user-context path is currently
        // holding the repo (e.g. set_recv_wait_delivery from try_send).
        // That spin never resolves because the holder can't be re-scheduled
        // while the timer ISR runs — total kernel halt.  Use try_lock and
        // accept that very occasional ticks are missed in the cpu accounting
        // counter; this is a stat, not a correctness invariant.
        if let Some(current_id) = Self::current() {
            if let Some(mut repo) = THREAD_REPOSITORY.try_lock() {
                if let Some(t) = repo.get_mut(current_id) {
                    t.cpu_ticks_consumed += 1;
                }
            }
        }

        if Self::is_normal_mode() {
            if let Some(mut scheduler) = SCHEDULER.try_lock() {
                scheduler.tick();
            }
        }

        // Check for expired timeouts and wake blocked threads
        Self::check_timeouts();

        // Drain deferred fault notifications (IST couldn't send due to lock contention)
        Self::drain_deferred_faults();
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

        // Get thread context and stage initial FPU/SSE state in per-CPU scratch.
        // enter_userspace will do FXRSTOR from scratch before iretq.
        let context = Self::with_thread(thread_id, |t| {
            unsafe {
                let scratch = crate::architecture::x86_64::syscall::percpu_fpu_scratch_ptr();
                core::ptr::copy_nonoverlapping(t.fpu_state.data.as_ptr(), scratch, 512);
            }
            t.context
        }).expect("Thread disappeared");

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

        // Clear current thread so idle ticks are not mis-attributed while
        // we search for the next runnable thread (or HLT in the idle loop).
        CURRENT_THREAD_ID.store(u64::MAX, Ordering::Release);

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
            Self::set_current(current_id);
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
            // Copy FPU state from per-CPU scratch buffer (filled by assembly FXSAVE on entry)
            unsafe {
                let scratch = crate::architecture::x86_64::syscall::percpu_fpu_scratch_ptr();
                core::ptr::copy_nonoverlapping(scratch, thread.fpu_state.data.as_mut_ptr(), 512);
            }
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

    /// Get pointer to thread's context (also stages FPU/SSE state for restore)
    fn get_context_ptr(thread_id: ThreadId) -> *const Context {
        Self::with_thread(thread_id, |thread| {
            // Copy next thread's FPU state to per-CPU scratch buffer.
            // Assembly will do FXRSTOR from scratch on the exit path.
            unsafe {
                let scratch = crate::architecture::x86_64::syscall::percpu_fpu_scratch_ptr();
                core::ptr::copy_nonoverlapping(thread.fpu_state.data.as_ptr(), scratch, 512);
            }

            // IBPB: flush branch predictor when switching to a different address space
            if crate::architecture::x86_64::spectre::has_ibpb() {
                let current_cr3: u64;
                unsafe {
                    core::arch::asm!("mov {}, cr3", out(reg) current_cr3, options(nomem, nostack));
                }
                if thread.context.cr3 != current_cr3 {
                    unsafe { crate::architecture::x86_64::spectre::ibpb(); }
                }
            }

            &thread.context as *const Context
        })
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
/// Sets FS base for TLS, then calls into NASM `enter_userspace_asm` which
/// restores FPU/SSE from per-CPU scratch, builds the iretq frame from the
/// Context struct, and jumps to Ring 3.
unsafe fn enter_userspace(context: &Context) -> ! {
    extern "C" {
        fn enter_userspace_asm(context: *const Context) -> !;
    }

    klibcluu::trace("Executing iretq to userspace");
    klibcluu::log_hex(klibcluu::LogLevel::Info, "Entry RIP=", context.rip);
    klibcluu::log_hex(klibcluu::LogLevel::Info, "Entry RSP=", context.rsp);
    klibcluu::log_hex(klibcluu::LogLevel::Info, "Entry CS=", context.cs);
    klibcluu::log_hex(klibcluu::LogLevel::Info, "Entry SS=", context.ss);

    // Set FS base (TLS) via MSR before entering userspace.
    x86_64::registers::model_specific::Msr::new(0xC000_0100).write(context.fs_base);

    enter_userspace_asm(context as *const Context);
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
