//! CPU Context for Context Switching
//!
//! This module defines the Context structure that holds the CPU register
//! state for context switching between threads.
//!
//! # Layout
//!
//! The Context struct matches the layout expected by the NASM context
//! switching routines in `kernel/src/architecture/x86_64/syscall_entry.asm`
//! and `kernel/src/architecture/x86_64/interrupts.asm`:
//!
//! ```nasm
//! %define CONTEXT_RAX     0x00
//! %define CONTEXT_RBX     0x08
//! %define CONTEXT_RCX     0x10
//! %define CONTEXT_RDX     0x18
//! %define CONTEXT_RSI     0x20
//! %define CONTEXT_RDI     0x28
//! %define CONTEXT_R8      0x30
//! %define CONTEXT_R9      0x38
//! %define CONTEXT_R10     0x40
//! %define CONTEXT_R11     0x48
//! %define CONTEXT_R12     0x50
//! %define CONTEXT_R13     0x58
//! %define CONTEXT_R14     0x60
//! %define CONTEXT_R15     0x68
//! %define CONTEXT_RBP     0x70
//! %define CONTEXT_RSP     0x78
//! %define CONTEXT_RIP     0x80
//! %define CONTEXT_RFLAGS  0x88
//! %define CONTEXT_CS      0x90
//! %define CONTEXT_SS      0x98
//! %define CONTEXT_CR3     0xA0
//! ```
//!
//! # Important
//!
//! - The struct must be `#[repr(C)]` to match NASM layout
//! - Field order matters - it must match the NASM offsets exactly
//! - Total size: 96 bytes (0x60)

/// CPU context for context switching
///
/// This structure holds the complete CPU state for a thread.
/// It is saved/restored during context switches by the assembly routines.
///
/// # Fields
///
/// - **General-purpose registers**: RAX, RBX, RCX, RDX, RSI, RDI, R8-R15, RBP
/// - **Execution state**: RSP (stack pointer), RIP (instruction pointer), RFLAGS
/// - **Memory context**: CR3 (page table root for address space)
/// - **Segment selectors**: CS (code segment), SS (stack segment)
///
/// # Safety
///
/// This struct is manipulated by assembly code. The layout must never change
/// without updating the corresponding assembly offsets.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Context {
    // General-purpose registers
    pub rax: u64, // 0x00
    pub rbx: u64, // 0x08
    pub rcx: u64, // 0x10
    pub rdx: u64, // 0x18
    pub rsi: u64, // 0x20
    pub rdi: u64, // 0x28
    pub r8: u64,  // 0x30
    pub r9: u64,  // 0x38
    pub r10: u64, // 0x40
    pub r11: u64, // 0x48
    pub r12: u64, // 0x50
    pub r13: u64, // 0x58
    pub r14: u64, // 0x60
    pub r15: u64, // 0x68
    pub rbp: u64, // 0x70

    // Stack and instruction pointers
    pub rsp: u64, // 0x78 - Stack pointer
    pub rip: u64, // 0x80 - Instruction pointer

    // Processor flags
    pub rflags: u64, // 0x88 - RFLAGS register

    // Segment selectors
    pub cs: u64, // 0x90 - Code segment
    pub ss: u64, // 0x98 - Stack segment

    // Page table base
    pub cr3: u64, // 0xA0 - Page table root (physical address)
}

impl Context {
    /// Create a new zeroed context
    pub const fn new() -> Self {
        Self {
            rax: 0,
            rbx: 0,
            rcx: 0,
            rdx: 0,
            rsi: 0,
            rdi: 0,
            r8: 0,
            r9: 0,
            r10: 0,
            r11: 0,
            rbp: 0,
            r12: 0,
            r13: 0,
            r14: 0,
            r15: 0,
            rsp: 0,
            rip: 0,
            rflags: 0,
            cs: 0,
            ss: 0,
            cr3: 0,
        }
    }

    /// Create a context for a new thread
    ///
    /// # Arguments
    ///
    /// * `entry` - Entry point (RIP)
    /// * `stack` - Stack pointer (RSP)
    /// * `cr3` - Page table root physical address
    ///
    /// # Returns
    ///
    /// A new Context configured for starting execution at `entry` with
    /// the given stack and page tables.
    pub fn for_new_thread(entry: u64, stack: u64, cr3: u64) -> Self {
        Self {
            rax: 0,
            rbx: 0,
            rcx: 0,
            rdx: 0,
            rsi: 0,
            rdi: 0,
            r8: 0,
            r9: 0,
            r10: 0,
            r11: 0,
            rbp: 0,
            r12: 0,
            r13: 0,
            r14: 0,
            r15: 0,
            rsp: stack,
            rip: entry,
            rflags: 0x202, // Bit 1 (reserved, always 1) | Bit 9 (IF)
            cs: 0x08,      // Kernel code segment (placeholder until GDT setup)
            ss: 0x10,      // Kernel data segment (placeholder until GDT setup)
            cr3,
        }
    }

    /// Zero out the context (for testing/debugging)
    pub fn zero(&mut self) {
        *self = Self::new();
    }
}

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}

// Verify size and alignment
#[cfg(test)]
const _: () = {
    // Context should be 168 bytes (0xA8)
    assert!(core::mem::size_of::<Context>() == 168);

    // Context should be 8-byte aligned
    assert!(core::mem::align_of::<Context>() == 8);
};

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem;

    #[test]
    fn test_context_size() {
        assert_eq!(mem::size_of::<Context>(), 168);
        assert_eq!(mem::align_of::<Context>(), 8);
    }

    #[test]
    fn test_context_offsets() {
        // Verify field offsets match NASM definitions
        use core::mem::offset_of;

        assert_eq!(offset_of!(Context, rax), 0x00);
        assert_eq!(offset_of!(Context, rbx), 0x08);
        assert_eq!(offset_of!(Context, rcx), 0x10);
        assert_eq!(offset_of!(Context, rdx), 0x18);
        assert_eq!(offset_of!(Context, rsi), 0x20);
        assert_eq!(offset_of!(Context, rdi), 0x28);
        assert_eq!(offset_of!(Context, r8), 0x30);
        assert_eq!(offset_of!(Context, r9), 0x38);
        assert_eq!(offset_of!(Context, r10), 0x40);
        assert_eq!(offset_of!(Context, r11), 0x48);
        assert_eq!(offset_of!(Context, r12), 0x50);
        assert_eq!(offset_of!(Context, r13), 0x58);
        assert_eq!(offset_of!(Context, r14), 0x60);
        assert_eq!(offset_of!(Context, r15), 0x68);
        assert_eq!(offset_of!(Context, rbp), 0x70);
        assert_eq!(offset_of!(Context, rsp), 0x78);
        assert_eq!(offset_of!(Context, rip), 0x80);
        assert_eq!(offset_of!(Context, rflags), 0x88);
        assert_eq!(offset_of!(Context, cs), 0x90);
        assert_eq!(offset_of!(Context, ss), 0x98);
        assert_eq!(offset_of!(Context, cr3), 0xA0);
    }

    #[test]
    fn test_context_new() {
        let ctx = Context::new();
        assert_eq!(ctx.rip, 0);
        assert_eq!(ctx.rsp, 0);
        assert_eq!(ctx.cr3, 0);
    }

    #[test]
    fn test_context_for_new_thread() {
        let ctx = Context::for_new_thread(0x400000, 0x7ff00000, 0x1000);
        assert_eq!(ctx.rip, 0x400000);
        assert_eq!(ctx.rsp, 0x7ff00000);
        assert_eq!(ctx.cr3, 0x1000);
        assert_eq!(ctx.rflags, 0x200); // IF flag set
    }

    #[test]
    fn test_context_zero() {
        let mut ctx = Context::for_new_thread(0x400000, 0x7ff00000, 0x1000);
        ctx.zero();
        assert_eq!(ctx.rip, 0);
        assert_eq!(ctx.rsp, 0);
        assert_eq!(ctx.cr3, 0);
    }
}
