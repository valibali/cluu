//! Thread Management
//!
//! This module defines the Thread structure (TCB - Thread Control Block)
//! and related types for thread management in the CLUU microkernel.
//!
//! # Thread States
//!
//! A thread can be in one of five states:
//! - **Init**: Created but not yet ready to run
//! - **Ready**: Ready to execute, waiting for CPU
//! - **Running**: Currently executing on CPU
//! - **Blocked**: Waiting for IPC or event
//! - **Dead**: Terminated
//!
//! # Priority System
//!
//! Threads have priorities from 0 (lowest) to 255 (highest).
//! Higher priority threads preempt lower priority threads.
//!
//! # INITMODE vs NORMALMODE
//!
//! - **INITMODE**: Early kernel initialization, may disable preemption
//! - **NORMALMODE**: Full preemptive scheduling active
//!
//! Threads with the COOPERATIVE flag won't be preempted in INITMODE.

use crate::mm::space::AddressSpace;
use crate::sched::context::Context;
use crate::sched::fpu::FpuState;
use crate::token::scope::{EndpointId, NotificationId, ReplyId};
use x86_64::{PhysAddr, VirtAddr};

/// Thread ID type
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ThreadId(pub u64);

impl ThreadId {
    /// Create a new thread ID
    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    /// Get the raw ID value
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

/// Thread state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadState {
    /// Thread created, not yet ready to run
    Init,
    /// Ready to execute, waiting for CPU
    Ready,
    /// Currently executing on CPU
    Running,
    /// Blocked waiting for IPC/event
    Blocked,
    /// Terminated
    Dead,
}

/// Thread priority (0 = lowest, 255 = highest)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Priority(pub u8);

impl Priority {
    /// Create a new priority
    pub const fn new(priority: u8) -> Self {
        Self(priority)
    }

    /// Get the raw priority value
    pub const fn as_u8(self) -> u8 {
        self.0
    }

    /// Lowest priority
    pub const LOWEST: Self = Self(0);

    /// Highest priority
    pub const HIGHEST: Self = Self(255);

    /// Default priority
    pub const DEFAULT: Self = Self(128);
}

/// Thread flags
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadFlags(u32);

/// Information stored when a thread is waiting for a reply from IPC call
#[derive(Debug, Clone, Copy)]
pub struct CallReplyInfo {
    /// The caller thread (to wake up when reply arrives)
    pub caller: ThreadId,
    /// Userspace buffer to receive reply
    pub reply_buf_ptr: usize,
    /// Size of reply buffer
    pub reply_buf_len: usize,
    /// Page table root for copying reply to userspace
    pub page_table_root: PhysAddr,
    /// Server thread authorized to reply. Set when message is delivered.
    pub server_thread_id: Option<ThreadId>,
}

/// Result metadata for a direct recv delivery while blocked/armed.
#[derive(Debug, Clone, Copy)]
pub struct RecvWaitDelivery {
    pub endpoint: EndpointId,
    pub len: usize,
    pub sender: Option<ThreadId>,
}

/// Fault type discriminant for IPC fault messages
#[repr(u64)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultType {
    PageFault = 0,
    GeneralProtection = 1,
    InvalidOpcode = 2,
    DivideByZero = 3,
}

/// Saved state of a faulted thread waiting for handler decision
#[derive(Debug, Clone, Copy)]
pub struct FaultState {
    pub fault_type: FaultType,
    pub fault_addr: u64,
    pub error_code: u64,
    pub saved_context: Context,
    pub reply_id: ReplyId,
}

impl ThreadFlags {
    /// No flags
    pub const NONE: Self = Self(0);

    /// Don't preempt in INITMODE (cooperative scheduling)
    pub const COOPERATIVE: Self = Self(1 << 0);
    /// Thread is externally suspended (e.g., SIGSTOP/job control).
    pub const SUSPENDED: Self = Self(1 << 1);
    /// Thread was already blocked before suspend.
    /// On resume, it should remain blocked instead of being woken immediately.
    pub const SUSPENDED_WAS_BLOCKED: Self = Self(1 << 2);

    /// Create empty flags
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Check if a flag is set
    pub const fn contains(self, flag: Self) -> bool {
        (self.0 & flag.0) != 0
    }

    /// Set a flag
    pub const fn with(self, flag: Self) -> Self {
        Self(self.0 | flag.0)
    }

    /// Clear a flag
    pub const fn without(self, flag: Self) -> Self {
        Self(self.0 & !flag.0)
    }
}

/// Thread Control Block (TCB)
///
/// Represents a single thread of execution in the system.
/// Contains all state needed to manage and schedule the thread.
///
/// # Layout
///
/// Uses #[repr(C)] to ensure predictable field ordering.
/// The `context` field is accessed by assembly code via save_context.
/// Cache-line aligned (64 bytes) to avoid false sharing when the
/// scheduler accesses different Thread structs on the hot path.
#[repr(C, align(64))]
#[derive(Debug)]
pub struct Thread {
    /// Unique thread identifier
    pub id: ThreadId,

    /// Current execution state
    pub state: ThreadState,

    /// Scheduling priority
    pub priority: Priority,

    /// Thread flags (COOPERATIVE, etc.)
    pub flags: ThreadFlags,

    /// CPU register state for context switching
    pub context: Context,

    /// FPU/SSE register state for context switching (FXSAVE/FXRSTOR)
    pub fpu_state: FpuState,

    /// Physical address of page table root (CR3 value)
    /// This is what gets loaded into CR3 during context switch
    pub page_table_root: PhysAddr,

    /// Remaining time slice in ticks (for round-robin within priority)
    pub time_slice_remaining: u64,

    /// Deadline tick for timeout wakeup (None = no timeout, blocks forever)
    pub timeout_deadline: Option<u64>,

    /// Set to true when thread was woken due to timeout expiry
    pub woke_from_timeout: bool,

    /// Information for pending IPC call reply (set when thread is waiting for reply)
    pub call_reply_info: Option<CallReplyInfo>,

    /// Set while thread is in recv_any waiter-registration window.
    /// This closes the race where sender arrives between waiter registration
    /// and the receiver transition to Blocked.
    pub recv_wait_armed: bool,
    /// Monotonic ticket for recv wait registration generation.
    /// Incremented on every arm so endpoint wait queues can reject stale waiters.
    pub recv_wait_ticket: u64,

    /// Userspace buffer metadata used for direct send->recv transfer.
    pub recv_wait_buf_ptr: usize,
    pub recv_wait_buf_len: usize,
    /// Direct-delivery completion metadata consumed by sys_recv.
    pub recv_wait_delivery: Option<RecvWaitDelivery>,

    /// Rotating scan hint for multi-endpoint recv_any fairness.
    pub recv_scan_hint: usize,

    /// Endpoint to receive fault notifications (None = kill on fault)
    pub fault_endpoint: Option<EndpointId>,

    /// Saved fault state while waiting for handler response
    pub fault_state: Option<FaultState>,

    /// Cumulative CPU ticks consumed by this thread
    pub cpu_ticks_consumed: u64,

    /// Notification object this thread is waiting on (None if not waiting)
    pub notification_wait: Option<NotificationId>,

    /// Session ID for visibility scoping.
    /// 0 = system/root scope (sees all threads in enumerate).
    /// Non-zero = session-scoped (enumerate returns only same-session threads).
    /// Set by procmgr via InvokeOp::ThreadSetSession after thread_create,
    /// before thread_resume. Defaults to 0 (kernel/system threads).
    pub session_id: u64,

    /// System-scope visibility flag. When true, thread_enumerate returns
    /// all threads regardless of session_id (like session_id == 0, but for
    /// privileged threads living in a non-zero session, e.g. root user).
    /// Set by procmgr via InvokeOp::ThreadSetSystemScope after thread_create,
    /// before thread_resume. Defaults to false.
    pub system_scope: bool,
}

impl Thread {
    /// Create a new thread
    ///
    /// # Arguments
    ///
    /// * `id` - Unique thread identifier
    /// * `page_table_root` - Physical address of page table (from AddressSpace)
    /// * `entry` - Entry point (instruction pointer)
    /// * `stack` - Stack pointer
    /// * `priority` - Scheduling priority
    /// * `flags` - Thread flags
    ///
    /// # Returns
    ///
    /// A new Thread in Init state with the given configuration.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// let thread = Thread::new(
    ///     ThreadId::new(1),
    ///     PhysAddr::new(0x1000),
    ///     VirtAddr::new(0x400000),  // Entry point
    ///     VirtAddr::new(0x7ff00000), // Stack top
    ///     Priority::DEFAULT,
    ///     ThreadFlags::empty(),
    /// );
    /// ```
    pub fn new(
        id: ThreadId,
        page_table_root: PhysAddr,
        entry: VirtAddr,
        stack: VirtAddr,
        priority: Priority,
        flags: ThreadFlags,
    ) -> Self {
        let mut context = Context::new();
        context.rip = entry.as_u64();
        // SysV x86-64 ABI: at function entry, (RSP + 8) % 16 == 0
        // This is because `call` pushes an 8-byte return address.
        // Since we enter via iretq (no call), we must set RSP to 16n+8.
        context.rsp = stack.as_u64() - 8;
        context.cr3 = page_table_root.as_u64();

        // Set up initial RFLAGS (interrupts enabled)
        // Bit 1 (reserved, must be 1) | Bit 9 (IF - Interrupt Enable)
        context.rflags = 0x202;

        // Set up segment selectors for userspace (Ring 3)
        // Note: TSS takes 2 GDT entries, so user segments start at index 5
        context.cs = 0x33; // User code segment (GDT index 6, RPL 3)
        context.ss = 0x2b; // User data segment (GDT index 5, RPL 3)

        Self {
            id,
            state: ThreadState::Init,
            priority,
            flags,
            context,
            fpu_state: FpuState::new_init(),
            page_table_root,
            time_slice_remaining: 10, // Default 10 ticks
            timeout_deadline: None,
            woke_from_timeout: false,
            call_reply_info: None,            recv_wait_armed: false,
            recv_wait_ticket: 0,
            recv_wait_buf_ptr: 0,
            recv_wait_buf_len: 0,
            recv_wait_delivery: None,
            recv_scan_hint: 0,
            fault_endpoint: None,
            fault_state: None,
            cpu_ticks_consumed: 0,
            notification_wait: None,
            session_id: 0,
            system_scope: false,
        }
    }

    /// Create a new kernel thread (Ring 0)
    ///
    /// Uses kernel segments and the provided stack pointer.
    pub fn new_kernel(
        id: ThreadId,
        page_table_root: PhysAddr,
        entry: VirtAddr,
        stack: VirtAddr,
        priority: Priority,
        flags: ThreadFlags,
    ) -> Self {
        let mut context =
            Context::for_new_thread(entry.as_u64(), stack.as_u64(), page_table_root.as_u64());

        // Ensure interrupts are enabled for the idle thread.
        context.rflags = 0x202;

        Self {
            id,
            state: ThreadState::Init,
            priority,
            flags,
            context,
            fpu_state: FpuState::new_init(),
            page_table_root,
            time_slice_remaining: 10,
            timeout_deadline: None,
            woke_from_timeout: false,
            call_reply_info: None,
            recv_wait_armed: false,
            recv_wait_ticket: 0,
            recv_wait_buf_ptr: 0,
            recv_wait_buf_len: 0,
            recv_wait_delivery: None,
            recv_scan_hint: 0,
            fault_endpoint: None,
            fault_state: None,
            cpu_ticks_consumed: 0,
            notification_wait: None,
            session_id: 0,
            system_scope: false,
        }
    }

    /// Create a new thread from an address space
    ///
    /// Convenience method that extracts page_table_root from AddressSpace.
    pub fn from_address_space(
        id: ThreadId,
        space: &AddressSpace,
        entry: VirtAddr,
        stack: VirtAddr,
        priority: Priority,
        flags: ThreadFlags,
    ) -> Self {
        Self::new(id, space.page_table_root, entry, stack, priority, flags)
    }

    /// Transition to Ready state
    pub fn make_ready(&mut self) {
        self.state = ThreadState::Ready;
    }

    /// Transition to Running state
    pub fn make_running(&mut self) {
        self.state = ThreadState::Running;
    }

    /// Transition to Blocked state
    pub fn make_blocked(&mut self) {
        self.state = ThreadState::Blocked;
    }

    /// Transition to Dead state
    pub fn make_dead(&mut self) {
        self.state = ThreadState::Dead;
    }

    /// Check if thread is ready to run
    pub fn is_ready(&self) -> bool {
        self.state == ThreadState::Ready
    }

    /// Check if thread is running
    pub fn is_running(&self) -> bool {
        self.state == ThreadState::Running
    }

    /// Check if thread is blocked
    pub fn is_blocked(&self) -> bool {
        self.state == ThreadState::Blocked
    }

    /// Check if thread is dead
    pub fn is_dead(&self) -> bool {
        self.state == ThreadState::Dead
    }

    /// Check if thread can be preempted in INITMODE
    pub fn is_cooperative(&self) -> bool {
        self.flags.contains(ThreadFlags::COOPERATIVE)
    }

    /// Mark thread as suspended and remember whether it was already blocked.
    pub fn mark_suspended(&mut self, was_blocked: bool) {
        self.flags = self.flags.with(ThreadFlags::SUSPENDED);
        self.flags = if was_blocked {
            self.flags.with(ThreadFlags::SUSPENDED_WAS_BLOCKED)
        } else {
            self.flags.without(ThreadFlags::SUSPENDED_WAS_BLOCKED)
        };
    }

    /// Clear suspended state bookkeeping.
    pub fn clear_suspended(&mut self) {
        self.flags = self
            .flags
            .without(ThreadFlags::SUSPENDED)
            .without(ThreadFlags::SUSPENDED_WAS_BLOCKED);
    }

    /// Returns true if thread is externally suspended.
    pub fn is_suspended(&self) -> bool {
        self.flags.contains(ThreadFlags::SUSPENDED)
    }

    /// Returns true if thread was blocked when suspend was applied.
    pub fn suspended_was_blocked(&self) -> bool {
        self.flags.contains(ThreadFlags::SUSPENDED_WAS_BLOCKED)
    }

    /// Reset time slice
    pub fn reset_time_slice(&mut self, ticks: u64) {
        self.time_slice_remaining = ticks;
    }

    /// Decrement time slice (returns true if exhausted)
    pub fn tick(&mut self) -> bool {
        if self.time_slice_remaining > 0 {
            self.time_slice_remaining -= 1;
            self.time_slice_remaining == 0
        } else {
            true
        }
    }

    /// Set timeout deadline for blocking operations
    pub fn set_timeout_deadline(&mut self, deadline: u64) {
        self.timeout_deadline = Some(deadline);
    }

    /// Clear timeout deadline
    pub fn clear_timeout_deadline(&mut self) {
        self.timeout_deadline = None;
    }

    /// Check if timeout has expired given the current tick
    pub fn is_timeout_expired(&self, current_tick: u64) -> bool {
        match self.timeout_deadline {
            Some(deadline) => current_tick >= deadline,
            None => false,
        }
    }

    /// Return the starting index for the next recv_any scan and advance the hint.
    pub fn next_recv_scan_start(&mut self, endpoints: usize) -> usize {
        if endpoints == 0 {
            return 0;
        }
        let start = self.recv_scan_hint % endpoints;
        self.recv_scan_hint = (start + 1) % endpoints;
        start
    }

    pub fn arm_recv_wait(&mut self) {
        self.recv_wait_ticket = self.recv_wait_ticket.wrapping_add(1);
        if self.recv_wait_ticket == 0 {
            self.recv_wait_ticket = 1;
        }
        self.recv_wait_armed = true;
    }

    pub fn arm_recv_wait_with_buffer(&mut self, buf_ptr: usize, buf_len: usize) {
        self.recv_wait_ticket = self.recv_wait_ticket.wrapping_add(1);
        if self.recv_wait_ticket == 0 {
            self.recv_wait_ticket = 1;
        }
        self.recv_wait_armed = true;
        self.recv_wait_buf_ptr = buf_ptr;
        self.recv_wait_buf_len = buf_len;
        self.recv_wait_delivery = None;
    }

    pub fn disarm_recv_wait(&mut self) {
        self.recv_wait_armed = false;
        self.recv_wait_buf_ptr = 0;
        self.recv_wait_buf_len = 0;
    }

    /// Whether the thread should actually transition to Blocked for a recv-wait
    /// armed with `ticket`. Returns false when the wait is no longer current
    /// (ticket mismatch, disarmed) OR when a direct delivery is already pending
    /// — a sender raced between the re-check scan and the block call, and the
    /// delivery must be consumed by the caller instead of blocking.
    pub fn should_block_for_recv_wait(&self, ticket: u64) -> bool {
        self.recv_wait_ticket == ticket
            && self.recv_wait_armed
            && self.recv_wait_delivery.is_none()
    }

    pub fn is_recv_wait_armed(&self) -> bool {
        self.recv_wait_armed
    }

    pub fn recv_wait_ticket(&self) -> u64 {
        self.recv_wait_ticket
    }

    pub fn recv_wait_buffer(&self) -> Option<(usize, usize)> {
        if self.recv_wait_buf_len == 0 {
            return None;
        }
        Some((self.recv_wait_buf_ptr, self.recv_wait_buf_len))
    }

    pub fn set_recv_wait_delivery(&mut self, delivery: RecvWaitDelivery) {
        self.recv_wait_delivery = Some(delivery);
    }

    pub fn take_recv_wait_delivery(&mut self) -> Option<RecvWaitDelivery> {
        self.recv_wait_delivery.take()
    }

    pub fn peek_recv_wait_delivery_endpoint(&self) -> Option<crate::token::scope::EndpointId> {
        self.recv_wait_delivery.as_ref().map(|d| d.endpoint)
    }
}

/// Thread Builder
///
/// Provides a fluent API for constructing threads with optional parameters.
///
/// # Example
///
/// ```rust,no_run
/// let thread = ThreadBuilder::new(ThreadId::new(1))
///     .page_table_root(PhysAddr::new(0x1000))
///     .entry(VirtAddr::new(0x400000))
///     .stack(VirtAddr::new(0x7ff00000))
///     .priority(Priority::new(200))
///     .cooperative()
///     .build();
/// ```
pub struct ThreadBuilder {
    id: ThreadId,
    page_table_root: Option<PhysAddr>,
    entry: Option<VirtAddr>,
    stack: Option<VirtAddr>,
    priority: Priority,
    flags: ThreadFlags,
}

impl ThreadBuilder {
    /// Create a new thread builder
    pub fn new(id: ThreadId) -> Self {
        Self {
            id,
            page_table_root: None,
            entry: None,
            stack: None,
            priority: Priority::DEFAULT,
            flags: ThreadFlags::empty(),
        }
    }

    /// Set page table root
    pub fn page_table_root(mut self, page_table_root: PhysAddr) -> Self {
        self.page_table_root = Some(page_table_root);
        self
    }

    /// Set entry point
    pub fn entry(mut self, entry: VirtAddr) -> Self {
        self.entry = Some(entry);
        self
    }

    /// Set stack pointer
    pub fn stack(mut self, stack: VirtAddr) -> Self {
        self.stack = Some(stack);
        self
    }

    /// Set priority
    pub fn priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }

    /// Mark as cooperative (don't preempt in INITMODE)
    pub fn cooperative(mut self) -> Self {
        self.flags = self.flags.with(ThreadFlags::COOPERATIVE);
        self
    }

    /// Build the thread
    ///
    /// # Panics
    ///
    /// Panics if required fields (page_table_root, entry, stack) are not set.
    pub fn build(self) -> Thread {
        Thread::new(
            self.id,
            self.page_table_root.expect("page_table_root required"),
            self.entry.expect("entry required"),
            self.stack.expect("stack required"),
            self.priority,
            self.flags,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_thread_id() {
        let id = ThreadId::new(42);
        assert_eq!(id.as_u64(), 42);
    }

    #[test]
    fn test_priority_ordering() {
        assert!(Priority::new(200) > Priority::new(100));
        assert!(Priority::new(0) < Priority::new(255));
        assert_eq!(Priority::LOWEST, Priority::new(0));
        assert_eq!(Priority::HIGHEST, Priority::new(255));
    }

    #[test]
    fn test_thread_flags() {
        let flags = ThreadFlags::empty();
        assert!(!flags.contains(ThreadFlags::COOPERATIVE));

        let flags = flags.with(ThreadFlags::COOPERATIVE);
        assert!(flags.contains(ThreadFlags::COOPERATIVE));

        let flags = flags.without(ThreadFlags::COOPERATIVE);
        assert!(!flags.contains(ThreadFlags::COOPERATIVE));
    }

    #[test]
    fn test_thread_creation() {
        let thread = Thread::new(
            ThreadId::new(1),
            PhysAddr::new(0x1000),
            VirtAddr::new(0x400000),
            VirtAddr::new(0x7ff00000),
            Priority::DEFAULT,
            ThreadFlags::empty(),
        );

        assert_eq!(thread.id, ThreadId::new(1));
        assert_eq!(thread.state, ThreadState::Init);
        assert_eq!(thread.priority, Priority::DEFAULT);
        assert_eq!(thread.context.rip, 0x400000);
        assert_eq!(thread.context.rsp, 0x7ff00000);
        assert_eq!(thread.context.cr3, 0x1000);
    }

    #[test]
    fn test_thread_state_transitions() {
        let mut thread = Thread::new(
            ThreadId::new(1),
            PhysAddr::new(0x1000),
            VirtAddr::new(0x400000),
            VirtAddr::new(0x7ff00000),
            Priority::DEFAULT,
            ThreadFlags::empty(),
        );

        assert_eq!(thread.state, ThreadState::Init);

        thread.make_ready();
        assert!(thread.is_ready());

        thread.make_running();
        assert!(thread.is_running());

        thread.make_blocked();
        assert!(thread.is_blocked());

        thread.make_dead();
        assert!(thread.is_dead());
    }

    #[test]
    fn test_thread_time_slice() {
        let mut thread = Thread::new(
            ThreadId::new(1),
            PhysAddr::new(0x1000),
            VirtAddr::new(0x400000),
            VirtAddr::new(0x7ff00000),
            Priority::DEFAULT,
            ThreadFlags::empty(),
        );

        thread.reset_time_slice(3);
        assert_eq!(thread.time_slice_remaining, 3);

        assert!(!thread.tick()); // 2 remaining
        assert!(!thread.tick()); // 1 remaining
        assert!(thread.tick()); // 0 remaining, exhausted
        assert!(thread.tick()); // Still exhausted
    }

    #[test]
    fn test_thread_builder() {
        let thread = ThreadBuilder::new(ThreadId::new(1))
            .page_table_root(PhysAddr::new(0x1000))
            .entry(VirtAddr::new(0x400000))
            .stack(VirtAddr::new(0x7ff00000))
            .priority(Priority::new(200))
            .cooperative()
            .build();

        assert_eq!(thread.id, ThreadId::new(1));
        assert_eq!(thread.priority, Priority::new(200));
        assert!(thread.is_cooperative());
    }

    #[test]
    #[should_panic(expected = "page_table_root required")]
    fn test_thread_builder_missing_page_table() {
        ThreadBuilder::new(ThreadId::new(1))
            .entry(VirtAddr::new(0x400000))
            .stack(VirtAddr::new(0x7ff00000))
            .build();
    }

    #[test]
    fn test_recv_scan_hint_rotates() {
        let mut thread = Thread::new(
            ThreadId::new(1),
            PhysAddr::new(0x1000),
            VirtAddr::new(0x400000),
            VirtAddr::new(0x7ff00000),
            Priority::DEFAULT,
            ThreadFlags::empty(),
        );

        assert_eq!(thread.next_recv_scan_start(3), 0);
        assert_eq!(thread.next_recv_scan_start(3), 1);
        assert_eq!(thread.next_recv_scan_start(3), 2);
        assert_eq!(thread.next_recv_scan_start(3), 0);
    }

    #[test]
    fn test_recv_wait_arm_disarm() {
        let mut thread = Thread::new(
            ThreadId::new(1),
            PhysAddr::new(0x1000),
            VirtAddr::new(0x400000),
            VirtAddr::new(0x7ff00000),
            Priority::DEFAULT,
            ThreadFlags::empty(),
        );

        assert!(!thread.is_recv_wait_armed());
        thread.arm_recv_wait();
        assert!(thread.is_recv_wait_armed());
        thread.disarm_recv_wait();
        assert!(!thread.is_recv_wait_armed());
    }

    #[test]
    fn test_should_block_for_recv_wait_rejects_pending_delivery() {
        let mut thread = Thread::new(
            ThreadId::new(1),
            PhysAddr::new(0x1000),
            VirtAddr::new(0x400000),
            VirtAddr::new(0x7ff00000),
            Priority::DEFAULT,
            ThreadFlags::empty(),
        );

        thread.arm_recv_wait_with_buffer(0x10000, 4096);
        let ticket = thread.recv_wait_ticket();

        assert!(
            thread.should_block_for_recv_wait(ticket),
            "armed + ticket match + no delivery should block"
        );

        thread.set_recv_wait_delivery(RecvWaitDelivery {
            endpoint: crate::token::scope::EndpointId(1),
            len: 64,
            sender: None,
        });

        assert!(
            !thread.should_block_for_recv_wait(ticket),
            "pending delivery must prevent blocking — sender raced between re-check and block"
        );
    }

    #[test]
    fn test_should_block_for_recv_wait_rejects_stale_ticket() {
        let mut thread = Thread::new(
            ThreadId::new(1),
            PhysAddr::new(0x1000),
            VirtAddr::new(0x400000),
            VirtAddr::new(0x7ff00000),
            Priority::DEFAULT,
            ThreadFlags::empty(),
        );

        thread.arm_recv_wait_with_buffer(0x10000, 4096);
        let stale_ticket = thread.recv_wait_ticket().wrapping_sub(1);

        assert!(
            !thread.should_block_for_recv_wait(stale_ticket),
            "stale ticket must not block"
        );
    }
}
