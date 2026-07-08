/*
 * Interrupt Descriptor Table (IDT) Implementation
 *
 * The Interrupt Descriptor Table (IDT) is a critical data structure in x86_64 architecture
 * that defines how the CPU should handle interrupts and exceptions. It's essentially a table
 * of function pointers that the CPU uses to determine which code to execute when specific
 * events occur.
 *
 * Why IDT is Important:
 * 1. Exception Handling: When the CPU encounters errors like division by zero, page faults,
 *    or general protection faults, it needs to know what code to execute to handle these
 *    situations gracefully instead of crashing.
 *
 * 2. Interrupt Processing: Hardware devices (keyboard, timer, disk drives) need to notify
 *    the CPU when they need attention. The IDT defines handlers for these hardware interrupts.
 *
 * 3. System Calls: User programs need a way to request services from the kernel. System calls
 *    are implemented using software interrupts defined in the IDT.
 *
 * 4. Memory Protection: The IDT helps enforce privilege levels and memory protection by
 *    defining which code can handle which types of interrupts.
 *
 * Structure:
 * - The IDT contains up to 256 entries (0-255)
 * - Each entry is 16 bytes and describes an interrupt gate, trap gate, or task gate
 * - Entries 0-31 are reserved for CPU exceptions (divide error, page fault, etc.)
 * - Entries 32-255 are available for hardware interrupts and software interrupts
 *
 * For a microkernel, proper IDT setup is crucial because:
 * - It enables proper error handling and debugging
 * - It allows the kernel to respond to hardware events
 * - It provides the foundation for implementing system calls
 * - It ensures system stability by preventing crashes from becoming system hangs
 */

use crate::ipc::endpoint;
use crate::sched::context::Context;
use crate::sched::thread::FaultType;
use crate::sched::ThreadManager;
use core::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use lazy_static::lazy_static;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};
use x86_64::VirtAddr;

/// Diagnostic: countdown of timer ticks to log after a fault is forwarded
static POST_FAULT_TIMER_DIAG: AtomicU64 = AtomicU64::new(0);

extern "C" {
    fn timer_interrupt_entry();
    fn gpf_interrupt_entry();
    fn pf_interrupt_entry();
    fn generic_fault_entry_de();
    fn generic_fault_entry_ud();
    fn generic_fault_entry_of();
    fn generic_fault_entry_br();
    fn generic_fault_entry_nm();
    fn generic_fault_entry_mf();
    fn generic_fault_entry_xm();
}

/// Send End of Interrupt (EOI) signal to PIC.
///
/// Thin wrapper around `pic::send_eoi` for local use.
#[inline(always)]
unsafe fn pic_eoi(irq: u8) {
    crate::architecture::x86_64::pic::send_eoi(irq);
}

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();

        // Set up all exception handlers
        // #DE, #UD, #OF, #BR, #NM use custom assembly entries for userspace survival
        unsafe {
            idt.divide_error.set_handler_addr(VirtAddr::new(generic_fault_entry_de as *const () as u64));
            idt.invalid_opcode.set_handler_addr(VirtAddr::new(generic_fault_entry_ud as *const () as u64));
            idt.overflow.set_handler_addr(VirtAddr::new(generic_fault_entry_of as *const () as u64));
            idt.bound_range_exceeded.set_handler_addr(VirtAddr::new(generic_fault_entry_br as *const () as u64));
            idt.device_not_available.set_handler_addr(VirtAddr::new(generic_fault_entry_nm as *const () as u64));
        }
        idt.debug.set_handler_fn(debug_handler);
        idt.non_maskable_interrupt.set_handler_fn(nmi_handler);
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        // Enable IST for double fault now that GDT is properly set up
        unsafe {
            idt.double_fault
                .set_handler_fn(double_fault_handler)
                .set_stack_index(crate::architecture::x86_64::gdt::DOUBLE_FAULT_IST_INDEX);
        }
        idt.invalid_tss.set_handler_fn(invalid_tss_handler);
        idt.segment_not_present.set_handler_fn(segment_not_present_handler);
        idt.stack_segment_fault.set_handler_fn(stack_segment_fault_handler);
        unsafe {
            idt.general_protection_fault
                .set_handler_addr(VirtAddr::new(gpf_interrupt_entry as *const () as u64))
                .set_stack_index(crate::architecture::x86_64::gdt::GPF_IST_INDEX);
        }
        unsafe {
            idt.page_fault
                .set_handler_addr(VirtAddr::new(pf_interrupt_entry as *const () as u64))
                .set_stack_index(crate::architecture::x86_64::gdt::PF_IST_INDEX);
        }
        unsafe {
            idt.x87_floating_point.set_handler_addr(VirtAddr::new(generic_fault_entry_mf as *const () as u64));
            idt.simd_floating_point.set_handler_addr(VirtAddr::new(generic_fault_entry_xm as *const () as u64));
        }
        idt.alignment_check.set_handler_fn(alignment_check_handler);
        idt.machine_check.set_handler_fn(machine_check_handler);
        idt.virtualization.set_handler_fn(virtualization_handler);
        idt.security_exception.set_handler_fn(security_exception_handler);

        // TODO Phase 8: Set up software interrupt for voluntary yielding (INT 0x81)
        // This allows yield_now() to trigger context switches using interrupt mechanism
        // unsafe {
        //     idt[0x81].set_handler_addr(
        //         core::mem::transmute::<*const (), x86_64::VirtAddr>(
        //             crate::scheduler::yield_interrupt_handler as *const ()
        //         )
        //     );
        // }

        // Set up hardware interrupt handlers (IRQ 0-15 map to interrupts 32-47)
        // IRQ 0 - Timer: Use our simple timer handler (scheduler not yet implemented)
        unsafe {
            idt[32].set_handler_addr(VirtAddr::new(timer_interrupt_entry as *const () as u64));
        }
        idt[33].set_handler_fn(keyboard_interrupt_handler); // IRQ 1 - Keyboard
        idt[36].set_handler_fn(serial_interrupt_handler);   // IRQ 4 - Serial COM1
        idt[39].set_handler_fn(serial_interrupt_handler);   // IRQ 7 - Serial COM2
        idt[43].set_handler_fn(virtio_blk_interrupt_handler); // IRQ 11 - virtio-blk-pci
        idt[44].set_handler_fn(mouse_interrupt_handler);    // IRQ 12 - PS/2 Mouse

        // Set up a generic handler for interrupt 0x68 (104)
        idt[0x68].set_handler_fn(generic_interrupt_handler);

        idt
    };
}

/// Initialize the Interrupt Descriptor Table
///
/// This function sets up the IDT with handlers for common CPU exceptions.
/// It must be called early in the boot process before interrupts are enabled.
pub fn init() {
    klibcluu::info("Setting up IDT handlers...");

    // Ensure interrupts are disabled before loading IDT
    x86_64::instructions::interrupts::disable();
    klibcluu::info("Interrupts disabled");

    IDT.load();

    klibcluu::info("IDT loaded successfully");
    klibcluu::info("IDT initialized successfully");
}

// Exception handlers - these functions are called when CPU exceptions occur

extern "x86-interrupt" fn debug_handler(_stack_frame: InterruptStackFrame) {
    klibcluu::warn("DEBUG_EXCEPTION");
}

extern "x86-interrupt" fn nmi_handler(_stack_frame: InterruptStackFrame) {
    klibcluu::warn("NMI");
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn breakpoint_handler(_stack_frame: InterruptStackFrame) {
    klibcluu::warn("BREAKPOINT");
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) -> ! {
    klibcluu::warn("DOUBLE_FAULT");
    klibcluu::warn("  RIP: ");
    klibcluu::log_hex(
        klibcluu::LogLevel::Warn,
        "",
        stack_frame.instruction_pointer.as_u64(),
    );
    klibcluu::warn("  RSP: ");
    klibcluu::log_hex(
        klibcluu::LogLevel::Warn,
        "",
        stack_frame.stack_pointer.as_u64(),
    );
    klibcluu::warn("  Error code: ");
    klibcluu::log_hex(klibcluu::LogLevel::Warn, "", error_code);
    // Critical error - halt immediately without panic
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn invalid_tss_handler(
    _stack_frame: InterruptStackFrame,
    _error_codee: u64,
) {
    klibcluu::warn("INVALID_TSS");
    // Simple error handling without panic for debugging
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn segment_not_present_handler(
    _stack_frame: InterruptStackFrame,
    _error_code: u64,
) {
    klibcluu::warn("SEGMENT_NOT_PRESENT");
    // Simple error handling without panic for debugging
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn stack_segment_fault_handler(
    _stack_frame: InterruptStackFrame,
    _error_code: u64,
) {
    klibcluu::warn("STACK_SEGMENT_FAULT");
    // Simple error handling without panic for debugging
    loop {
        x86_64::instructions::hlt();
    }
}

#[allow(dead_code)]
extern "x86-interrupt" fn general_protection_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    klibcluu::warn("GENERAL_PROTECTION_FAULT");

    // Extract values (SegmentSelector.0 to get u16, then convert to u64)
    let rip = stack_frame.instruction_pointer.as_u64();
    let cs = stack_frame.code_segment.0 as u64;
    let rsp = stack_frame.stack_pointer.as_u64();
    let ss = stack_frame.stack_segment.0 as u64;
    let is_userspace = (cs & 3) == 3;
    let rflags = stack_frame.cpu_flags.bits();
    let cr2 = match x86_64::registers::control::Cr2::read() {
        Ok(addr) => addr.as_u64(),
        Err(_) => 0,
    };

    // Use IRQ-safe logging
    klibcluu::log_hex(klibcluu::LogLevel::Warn, "GPF: error_code=", error_code);
    klibcluu::log_hex(klibcluu::LogLevel::Warn, "GPF: RIP=", rip);
    klibcluu::log_hex(klibcluu::LogLevel::Warn, "GPF: CS=", cs);
    klibcluu::log_hex(klibcluu::LogLevel::Warn, "GPF: RSP=", rsp);
    klibcluu::log_hex(klibcluu::LogLevel::Warn, "GPF: SS=", ss);
    klibcluu::log_hex(klibcluu::LogLevel::Warn, "GPF: RFLAGS=", rflags);
    klibcluu::log_hex(klibcluu::LogLevel::Warn, "GPF: CR2=", cr2);

    if is_userspace {
        klibcluu::warn("GPF: Fault in USERSPACE (Ring 3)\n");
    } else {
        klibcluu::warn("GPF: Fault in KERNEL (Ring 0)\n");
    }

    // Decode error code
    let selector_index = (error_code >> 3) & 0x1FFF;
    if selector_index != 0 {
        klibcluu::log_hex(
            klibcluu::LogLevel::Warn,
            "GPF: Selector index=",
            selector_index,
        );
    }

    let table = (error_code >> 1) & 0x3;
    match table {
        0 => klibcluu::warn("GPF: Descriptor in GDT\n"),
        1 | 3 => klibcluu::warn("GPF: Descriptor in IDT\n"),
        2 => klibcluu::warn("GPF: Descriptor in LDT\n"),
        _ => {}
    }

    // Halt system
    loop {
        x86_64::instructions::hlt();
    }
}

#[repr(C)]
struct GpfDebugFrame {
    rax: u64,
    rbx: u64,
    rcx: u64,
    rdx: u64,
    rsi: u64,
    rdi: u64,
    rbp: u64,
    r8: u64,
    r9: u64,
    r10: u64,
    r11: u64,
    r12: u64,
    r13: u64,
    r14: u64,
    r15: u64,
    error_code: u64,
    rip: u64,
    cs: u64,
    rflags: u64,
    rsp: u64,
    ss: u64,
}

// ═══════════════════════════════════════════════════════════════════════════
// Fault Forwarding Helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Convert GpfDebugFrame to a Context (for saving faulted thread state)
fn gpf_frame_to_context(f: &GpfDebugFrame) -> Context {
    let cr3 = x86_64::registers::control::Cr3::read()
        .0
        .start_address()
        .as_u64();
    // Read the faulting thread's FS base (TLS) from MSR before it's clobbered
    let fs_base = unsafe { x86_64::registers::model_specific::Msr::new(0xC000_0100).read() };
    Context {
        rax: f.rax,
        rbx: f.rbx,
        rcx: f.rcx,
        rdx: f.rdx,
        rsi: f.rsi,
        rdi: f.rdi,
        r8: f.r8,
        r9: f.r9,
        r10: f.r10,
        r11: f.r11,
        r12: f.r12,
        r13: f.r13,
        r14: f.r14,
        r15: f.r15,
        rbp: f.rbp,
        rsp: f.rsp,
        rip: f.rip,
        rflags: f.rflags,
        cs: f.cs,
        ss: f.ss,
        cr3,
        fs_base,
        _pad: 0,
    }
}

/// Convert PfDebugFrame to a Context (for saving faulted thread state)
fn pf_frame_to_context(f: &PfDebugFrame) -> Context {
    let cr3 = x86_64::registers::control::Cr3::read()
        .0
        .start_address()
        .as_u64();
    // Read the faulting thread's FS base (TLS) from MSR before it's clobbered
    let fs_base = unsafe { x86_64::registers::model_specific::Msr::new(0xC000_0100).read() };
    Context {
        rax: f.rax,
        rbx: f.rbx,
        rcx: f.rcx,
        rdx: f.rdx,
        rsi: f.rsi,
        rdi: f.rdi,
        r8: f.r8,
        r9: f.r9,
        r10: f.r10,
        r11: f.r11,
        r12: f.r12,
        r13: f.r13,
        r14: f.r14,
        r15: f.r15,
        rbp: f.rbp,
        rsp: f.rsp,
        rip: f.rip,
        rflags: f.rflags,
        cs: f.cs,
        ss: f.ss,
        cr3,
        fs_base,
        _pad: 0,
    }
}

/// Try to forward a user fault to the thread's fault endpoint.
///
/// Returns true if fault was successfully forwarded (thread is now blocked).
/// Returns false if no handler, recursion guard, or send failed.
fn try_forward_fault(
    fault_type: FaultType,
    fault_addr: u64,
    error_code: u64,
    saved_context: &Context,
) -> bool {
    use crate::sched::thread::FaultState;
    use crate::sched::FaultReplyInfo;

    let current_id = match ThreadManager::current() {
        Some(id) => id,
        None => return false,
    };

    // Get fault endpoint, guard against recursive faults
    let fault_ep = match ThreadManager::with_thread(current_id, |t| {
        if t.fault_state.is_some() {
            None // Already handling a fault — recursion guard
        } else {
            t.fault_endpoint
        }
    }) {
        Some(Some(ep)) => ep,
        _ => return false,
    };

    // Allocate reply ID (no token minting — implicit reply cap)
    let reply_id = ThreadManager::alloc_reply_id();

    // Build fault message: label=0xFA017, words=[type, addr, err, rip, tid, reply_id]
    let mut msg_bytes = [0u8; core::mem::size_of::<endpoint::UserMessage>()];
    let msg = unsafe { &mut *(msg_bytes.as_mut_ptr() as *mut endpoint::UserMessage) };
    msg.tag.label = 0xFA017;
    msg.tag.words = 6;
    msg.tag.extra = endpoint::REPLY_ID_TAG;
    msg.tag._pad = 0;
    msg.words[0] = fault_type as usize;
    msg.words[1] = fault_addr as usize;
    msg.words[2] = error_code as usize;
    msg.words[3] = saved_context.rip as usize;
    msg.words[4] = current_id.as_u64() as usize;
    msg.words[5] = reply_id.as_u64() as usize;

    // Non-blocking send with reply_id (safe from IST context — uses try_lock)
    match endpoint::try_send_with_reply_id(fault_ep, &msg_bytes, reply_id) {
        Ok(receiver_to_wake) => {
            if let Some(thread_id) = receiver_to_wake {
                klibcluu::warn("fault: waking receiver tid=");
                klibcluu::log_dec(klibcluu::LogLevel::Warn, "", thread_id.as_u64());
                ThreadManager::wake_thread(thread_id);
            } else {
                klibcluu::warn("fault: no receiver registered (msg queued only)");
            }
        }
        Err(_) => {
            klibcluu::warn("fault: try_send_with_reply_id FAILED (lock contention)");
            return false;
        }
    }

    // Arm post-fault timer diagnostic (log next 5 timer ticks)
    POST_FAULT_TIMER_DIAG.store(5, AtomicOrdering::Release);

    // Store fault reply info and save fault state on thread
    if !ThreadManager::set_fault_reply_info(
        reply_id,
        FaultReplyInfo {
            faulted_thread: current_id,
            server_thread_id: None,
        },
    ) {
        // Reply map full — can't track the fault reply; kill the thread instead.
        klibcluu::error("set_fault_reply_info failed (reply map full), killing thread");
        ThreadManager::mark_thread_dead(current_id);
        return false;
    }

    ThreadManager::with_thread_mut(current_id, |t| {
        // Copy FPU state from per-CPU scratch buffer (filled by assembly FXSAVE on entry)
        unsafe {
            let scratch = crate::architecture::x86_64::syscall::percpu_fpu_scratch_ptr();
            core::ptr::copy_nonoverlapping(scratch, t.fpu_state.data.as_mut_ptr(), 512);
        }
        t.fault_state = Some(FaultState {
            fault_type,
            fault_addr,
            error_code,
            saved_context: *saved_context,
            reply_id,
        });
        t.make_blocked();
    });

    klibcluu::warn("Fault forwarded to handler endpoint");
    true
}

/// Frame layout for generic faults without error code (#DE, #UD, #OF, #BR, #NM)
#[repr(C)]
struct GenericFaultFrame {
    rax: u64, rbx: u64, rcx: u64, rdx: u64,
    rsi: u64, rdi: u64, rbp: u64,
    r8: u64, r9: u64, r10: u64, r11: u64,
    r12: u64, r13: u64, r14: u64, r15: u64,
    vector: u64,
    rip: u64, cs: u64, rflags: u64, rsp: u64, ss: u64,
}

/// Generic fault handler for exceptions without error code.
/// Returns null for kernel faults (assembly halts), or context pointer for next thread.
#[no_mangle]
extern "C" fn generic_fault_with_regs(frame: *const GenericFaultFrame) -> *const Context {
    let f = unsafe { &*frame };
    let is_userspace = (f.cs & 0x3) == 0x3;

    let name = match f.vector {
        0 => "DIVIDE_ERROR",
        4 => "OVERFLOW",
        5 => "BOUND_RANGE",
        6 => "INVALID_OPCODE",
        7 => "DEVICE_NOT_AVAILABLE",
        16 => "X87_FP_EXCEPTION",
        19 => "SIMD_FP_EXCEPTION",
        _ => "UNKNOWN_EXCEPTION",
    };

    klibcluu::warn(name);
    klibcluu::log_hex(klibcluu::LogLevel::Warn, " RIP=", f.rip);
    klibcluu::log_hex(klibcluu::LogLevel::Warn, " CS=", f.cs);

    if !is_userspace {
        klibcluu::warn("KERNEL FAULT — halting");
        return core::ptr::null();
    }

    // Userspace fault — try to forward to fault handler, or kill thread
    let saved_ctx = Context {
        rax: f.rax, rbx: f.rbx, rcx: f.rcx, rdx: f.rdx,
        rsi: f.rsi, rdi: f.rdi,
        r8: f.r8, r9: f.r9, r10: f.r10, r11: f.r11,
        r12: f.r12, r13: f.r13, r14: f.r14, r15: f.r15,
        rbp: f.rbp, rsp: f.rsp, rip: f.rip,
        cs: f.cs, ss: f.ss, rflags: f.rflags,
        cr3: x86_64::registers::control::Cr3::read_raw().0.start_address().as_u64(),
        fs_base: {
            let (lo, hi): (u32, u32);
            unsafe { core::arch::asm!("rdmsr", in("ecx") 0xC000_0100u32, out("eax") lo, out("edx") hi); }
            ((hi as u64) << 32) | (lo as u64)
        },
        _pad: 0,
    };

    let fault_type = match f.vector {
        0 => FaultType::DivideByZero,
        6 => FaultType::InvalidOpcode,
        _ => FaultType::GeneralProtection, // best-effort for uncommon faults
    };

    if try_forward_fault(fault_type, 0, 0, &saved_ctx) {
        return ThreadManager::schedule_next_from_fault();
    }

    // No handler — queue deferred notification and kill thread
    if let Some(current_id) = ThreadManager::current() {
        let fault_ep = ThreadManager::with_thread(current_id, |t| t.fault_endpoint);
        if let Some(Some(ep)) = fault_ep {
            ThreadManager::queue_deferred_fault(current_id, ep, f.vector, 0, 0, f.rip);
        }
    }
    klibcluu::warn("Killing thread (no fault handler)");
    ThreadManager::mark_current_dead();
    ThreadManager::schedule_next_from_fault()
}

#[no_mangle]
extern "C" fn gpf_with_regs(frame: *const GpfDebugFrame) -> *const Context {
    use klibcluu::uart::COM2;

    #[inline(always)]
    fn uart_hex(prefix: &str, value: u64) {
        COM2.write_str(prefix);
        COM2.write_str("0x");
        let mut started = false;
        for shift in (0..16).rev() {
            let nibble = (value >> (shift * 4)) & 0xF;
            if nibble != 0 || started || shift == 0 {
                started = true;
                let c = if nibble < 10 {
                    b'0' + (nibble as u8)
                } else {
                    b'a' + ((nibble - 10) as u8)
                };
                COM2.write_byte(c);
            }
        }
        COM2.write_str("\n");
    }

    let f = unsafe { &*frame };
    COM2.write_str("[WARN]  GENERAL_PROTECTION_FAULT (regs)\n");
    uart_hex("GPF: error_code=", f.error_code);
    uart_hex("GPF: RIP=", f.rip);
    uart_hex("GPF: CS=", f.cs);
    uart_hex("GPF: RFLAGS=", f.rflags);
    uart_hex("GPF: RSP=", f.rsp);
    uart_hex("GPF: SS=", f.ss);
    uart_hex("GPF: RBX=", f.rbx);
    uart_hex("GPF: RBP=", f.rbp);
    uart_hex("GPF: RDI=", f.rdi);
    uart_hex("GPF: RSI=", f.rsi);
    uart_hex("GPF: R12=", f.r12);
    uart_hex("GPF: R13=", f.r13);
    uart_hex("GPF: R14=", f.r14);
    uart_hex("GPF: R15=", f.r15);

    // Userspace fault: try to forward or kill
    if (f.cs & 0x3) == 0x3 {
        let saved_ctx = gpf_frame_to_context(f);
        if try_forward_fault(FaultType::GeneralProtection, 0, f.error_code, &saved_ctx) {
            return ThreadManager::schedule_next_from_fault();
        }
        // try_forward_fault failed (lock contention or no handler).
        // Read fault_endpoint BEFORE mark_current_dead so we can queue
        // a deferred notification for procmgr.
        if let Some(current_id) = ThreadManager::current() {
            let fault_ep = ThreadManager::with_thread(current_id, |t| t.fault_endpoint);
            if let Some(Some(ep)) = fault_ep {
                ThreadManager::queue_deferred_fault(
                    current_id,
                    ep,
                    FaultType::GeneralProtection as u64,
                    0,
                    f.error_code,
                    f.rip,
                );
            }
        }
        klibcluu::warn("GPF: killing thread (deferred notification queued if handler set)");
        ThreadManager::mark_current_dead();
        return ThreadManager::schedule_next_from_fault();
    }

    // Kernel fault — halt
    klibcluu::warn("GPF: KERNEL FAULT — halting");
    loop {
        x86_64::instructions::hlt();
    }
}

#[allow(dead_code)]
extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: x86_64::structures::idt::PageFaultErrorCode,
) {
    use klibcluu::uart::COM2;
    use x86_64::registers::control::Cr2;

    #[inline(always)]
    fn uart_hex(prefix: &str, value: u64) {
        COM2.write_str(prefix);
        COM2.write_str("0x");
        let mut started = false;
        for shift in (0..16).rev() {
            let nibble = (value >> (shift * 4)) & 0xF;
            if nibble != 0 || started || shift == 0 {
                started = true;
                let c = if nibble < 10 {
                    b'0' + (nibble as u8)
                } else {
                    b'a' + ((nibble - 10) as u8)
                };
                COM2.write_byte(c);
            }
        }
        COM2.write_str("\n");
    }

    // Read the faulting address from CR2
    // CR2 always contains the faulting address; if invalid, system is in bad state
    let fault_addr = match Cr2::read() {
        Ok(addr) => addr,
        Err(_) => {
            COM2.write_str("[WARN]  Failed to read CR2 register (invalid fault address)\n");
            loop {
                x86_64::instructions::hlt();
            }
        }
    };

    // Extract stack frame values
    let rip = stack_frame.instruction_pointer.as_u64();
    let cs = stack_frame.code_segment.0 as u64;
    let rsp = stack_frame.stack_pointer.as_u64();
    let ss = stack_frame.stack_segment.0 as u64;
    let rflags = stack_frame.cpu_flags.bits();

    // Parse error code flags
    let is_present =
        error_code.contains(x86_64::structures::idt::PageFaultErrorCode::PROTECTION_VIOLATION);
    let is_write =
        error_code.contains(x86_64::structures::idt::PageFaultErrorCode::CAUSED_BY_WRITE);
    let is_user = error_code.contains(x86_64::structures::idt::PageFaultErrorCode::USER_MODE);
    let is_instruction_fetch =
        error_code.contains(x86_64::structures::idt::PageFaultErrorCode::INSTRUCTION_FETCH);

    // Log page fault with detailed information
    COM2.write_str("[WARN]  PAGE_FAULT\n");
    uart_hex("PF: Fault address (CR2)=", fault_addr.as_u64());
    uart_hex("PF: RIP=", rip);
    uart_hex("PF: CS=", cs);
    uart_hex("PF: RSP=", rsp);
    uart_hex("PF: SS=", ss);
    uart_hex("PF: RFLAGS=", rflags);

    // Log error code details
    if is_user {
        COM2.write_str("[WARN]  PF: Fault in USERSPACE (Ring 3)\n");
    } else {
        COM2.write_str("[WARN]  PF: Fault in KERNEL (Ring 0)\n");
    }

    if is_present {
        COM2.write_str("[WARN]  PF: Protection violation (page is present)\n");
    } else {
        COM2.write_str("[WARN]  PF: Page not present\n");
    }

    if is_write {
        COM2.write_str("[WARN]  PF: Caused by WRITE\n");
    } else {
        COM2.write_str("[WARN]  PF: Caused by READ\n");
    }

    if is_instruction_fetch {
        COM2.write_str("[WARN]  PF: Caused by INSTRUCTION FETCH\n");
    }

    // If page is not present and fault is from user mode, try lazy allocation
    if !is_present && is_user {
        if let Some(success) = handle_heap_fault(fault_addr) {
            if success {
                // Page allocated successfully, resume execution
                klibcluu::warn("[PF] Handled (lazy heap alloc)");
                return;
            }
        }
    }

    // Unrecoverable page fault
    klibcluu::warn("[PF] UNRECOVERABLE - halting");
    loop {
        x86_64::instructions::hlt();
    }
}

#[repr(C)]
struct PfDebugFrame {
    rax: u64,
    rbx: u64,
    rcx: u64,
    rdx: u64,
    rsi: u64,
    rdi: u64,
    rbp: u64,
    r8: u64,
    r9: u64,
    r10: u64,
    r11: u64,
    r12: u64,
    r13: u64,
    r14: u64,
    r15: u64,
    error_code: u64,
    rip: u64,
    cs: u64,
    rflags: u64,
    rsp: u64,
    ss: u64,
}

/// Page fault handler with full register context.
///
/// Returns:
/// - null: lazy allocation succeeded, resume faulting instruction via iretq
/// - non-null: context pointer for next thread (fault forwarded or thread killed)
/// - never returns for kernel faults (halts)
#[no_mangle]
extern "C" fn pf_with_regs(frame: *const PfDebugFrame) -> *const Context {
    use klibcluu::uart::COM2;
    use x86_64::registers::control::Cr2;

    #[inline(always)]
    fn uart_hex(prefix: &str, value: u64) {
        COM2.write_str(prefix);
        COM2.write_str("0x");
        let mut started = false;
        for shift in (0..16).rev() {
            let nibble = (value >> (shift * 4)) & 0xF;
            if nibble != 0 || started || shift == 0 {
                started = true;
                let c = if nibble < 10 {
                    b'0' + (nibble as u8)
                } else {
                    b'a' + ((nibble - 10) as u8)
                };
                COM2.write_byte(c);
            }
        }
        COM2.write_str("\n");
    }

    let f = unsafe { &*frame };
    let cr2 = match Cr2::read() {
        Ok(addr) => addr.as_u64(),
        Err(_) => 0,
    };

    let is_userspace = (f.cs & 0x3) == 0x3;
    let is_present = (f.error_code & 1) != 0;

    // Try lazy allocation for not-present userspace page faults
    if !is_present && is_userspace {
        if let Some(true) = handle_heap_fault(x86_64::VirtAddr::new(cr2)) {
            return core::ptr::null(); // Resume — lazy alloc succeeded
        }
        if let Some(true) = handle_text_fault(cr2) {
            return core::ptr::null(); // Resume — text demand-page succeeded
        }
    }

    // Log the fault details (only for unrecoverable faults)
    COM2.write_str("[WARN]  PAGE_FAULT (regs)\n");
    uart_hex("PF: Fault address (CR2)=", cr2);
    uart_hex("PF: Error code=", f.error_code);
    uart_hex("PF: RIP=", f.rip);
    uart_hex("PF: CS=", f.cs);
    uart_hex("PF: RFLAGS=", f.rflags);
    uart_hex("PF: RSP=", f.rsp);
    uart_hex("PF: SS=", f.ss);

    // Diagnostic for the wild-instruction-fetch case (RIP==CR2 in user space):
    // dump 8 quadwords starting at RSP. If [rsp+0] equals RIP, the fault came
    // from a `ret` that popped a corrupted return address. If not, the fault
    // came from an indirect `call`/`jmp` through a clobbered register/memory
    // operand; in that case the bad target is from a register, not the stack.
    //
    // CR3 is still the faulting process's at this point, so user pointer
    // reads work directly. Wrap each read so a bad RSP doesn't recursively
    // fault — but we trust f.rsp here because we got here on the user IRQ
    // path, which means RSP was valid enough to push the iretq frame.
    if is_userspace && f.rip == cr2 && f.rip < 0x4_4000_0000 {
        // Whichever register held the bad target tells us which call site
        // is misbehaving. Most indirect-call patterns use RAX (Rust's
        // default for `call rax`-style trait/closure dispatch) but the
        // compiler is free to use any GPR. Dump them all.
        COM2.write_str("PF: GPRs at fault:\n");
        uart_hex("  rax=", f.rax);
        uart_hex("  rbx=", f.rbx);
        uart_hex("  rcx=", f.rcx);
        uart_hex("  rdx=", f.rdx);
        uart_hex("  rsi=", f.rsi);
        uart_hex("  rdi=", f.rdi);
        uart_hex("  rbp=", f.rbp);
        uart_hex("  r8 =", f.r8);
        uart_hex("  r9 =", f.r9);
        uart_hex("  r10=", f.r10);
        uart_hex("  r11=", f.r11);
        uart_hex("  r12=", f.r12);
        uart_hex("  r13=", f.r13);
        uart_hex("  r14=", f.r14);
        uart_hex("  r15=", f.r15);

        // Aliasing diagnostic: walk page tables to find any other space that
        // maps the same physical frame as the faulting RIP page. `translate_vaddr`
        // returns Option, so this is memory-safe even when the address isn't
        // mapped in the faulting process.
        use x86_64::registers::control::Cr3;
        let faulting_root = Cr3::read_raw().0.start_address();
        let fault_page = f.rip & !0xFFF;
        let fault_phys = crate::elf::translate_vaddr(
            faulting_root,
            x86_64::VirtAddr::new(fault_page),
        );
        if let Some(p) = fault_phys {
            let phys_aligned = p.as_u64() & !0xFFF;
            uart_hex("PF: faulting page -> phys ", phys_aligned);
            crate::mm::space_repository::for_each(|space_id, root| {
                if root == faulting_root {
                    return;
                }
                if let Some(va) = crate::elf::find_first_va_for_phys(root, phys_aligned) {
                    COM2.write_str("PF: ALIAS in space_id=");
                    uart_hex("", space_id.as_u64());
                    uart_hex("PF:  -> va ", va);
                }
            });
        } else {
            COM2.write_str("PF: faulting page not mapped in this CR3\n");
        }

        // NOTE: A previous diagnostic block read 6 qwords from a hardcoded
        // table at 0x43c968 (console-specific) and 72 qwords around f.rsp.
        // Both could fault on unmapped pages while CR3 is the faulting
        // process's, producing a nested kernel PF that clobbered the IST
        // stack and halted the kernel. Removed 2026-05-15. If you need a
        // stack walk, gate every read on a translate_vaddr Some() check.
    }

    // Userspace fault that can't be handled by lazy alloc
    if is_userspace {
        let saved_ctx = pf_frame_to_context(f);
        if try_forward_fault(FaultType::PageFault, cr2, f.error_code, &saved_ctx) {
            // Fault forwarded to handler. Schedule next runnable thread
            // and return its context pointer — assembly will context-switch
            // to it via BSP_STACK (same pattern as GPF handler).
            return ThreadManager::schedule_next_from_fault();
        }
        // try_forward_fault failed (lock contention or no handler).
        // Read fault_endpoint BEFORE mark_current_dead so we can queue
        // a deferred notification for procmgr.
        if let Some(current_id) = ThreadManager::current() {
            let fault_ep = ThreadManager::with_thread(current_id, |t| t.fault_endpoint);
            if let Some(Some(ep)) = fault_ep {
                ThreadManager::queue_deferred_fault(
                    current_id,
                    ep,
                    FaultType::PageFault as u64,
                    cr2,
                    f.error_code,
                    f.rip,
                );
            }
        }
        klibcluu::warn("PF: killing thread (deferred notification queued if handler set)");
        ThreadManager::mark_current_dead();
        return ThreadManager::schedule_next_from_fault();
    }

    // Kernel fault — halt
    klibcluu::warn("PF: KERNEL FAULT — halting");
    loop {
        x86_64::instructions::hlt();
    }
}

/// Demand-page a heap or stack fault. Returns Some(true) if a page was
/// mapped, Some(false) if the fault is outside any demand-pageable region,
/// None on allocation failure. Stack faults route through `handle_stack_fault`
/// (read+write+no-exec + growth-threshold warnings); heap faults map directly.
///
/// M6 ASLR: the stack guard boundary is looked up per-process via CR3 →
/// space_repository. Falls back to the global `USER_STACK_BOTTOM + 0x1000`
/// for kernel threads and early boot (before the space repository is
/// populated). Heap region bounds remain global constants — ASLR
/// randomizes the heap start upward, so the global lower bound is still
/// correct for demand paging.
fn handle_heap_fault(fault_addr: x86_64::VirtAddr) -> Option<bool> {
    use crate::mm::space::layout;
    use x86_64::registers::control::Cr3;

    let addr = fault_addr.as_u64();

    let (stack_guard_end, stack_top) = current_aslr_stack_bounds().unwrap_or_else(|| {
        (layout::USER_STACK_BOTTOM + 0x1000, layout::USER_STACK_TOP)
    });
    let is_stack_region = (stack_guard_end..stack_top).contains(&addr);
    let is_heap_region = (layout::USER_HEAP_START..layout::USER_HEAP_MAX).contains(&addr);

    if !is_stack_region && !is_heap_region {
        return Some(false);
    }

    if is_stack_region {
        return handle_stack_fault(addr, stack_top);
    }

    // Heap fault: only demand-map addresses below the process's current brk.
    // Addresses in the heap range but above brk must NOT be demand-paged —
    // doing so would bypass sbrk/brk and let a process without SPACE_MAP
    // allocate writable memory by simply touching it (security: heap brk
    // limit enforcement). Fall back to the global range check for kernel
    // threads / early boot, where no AddressSpace is registered for the
    // current CR3.
    let (pml4_frame, _) = Cr3::read();
    let page_table_root = pml4_frame.start_address();
    let heap_brk = crate::mm::space_repository::with_space_by_pml4(
        page_table_root,
        |space| space.heap.current_brk().as_u64(),
    );

    match heap_brk {
        Some(brk) => {
            let is_heap_allocated = addr >= layout::USER_HEAP_START && addr < brk;
            if !is_heap_allocated {
                return Some(false);
            }
            demand_map_page(addr)
        }
        None => demand_map_page(addr),
    }
}

/// Look up the current address space's ASLR stack bounds by CR3.
/// Returns (guard_end, stack_top) where guard_end is the first address
/// above the guard page and stack_top is the upper bound of the demand-
/// pageable stack region. Returns None for kernel threads / early boot.
fn current_aslr_stack_bounds() -> Option<(u64, u64)> {
    use x86_64::registers::control::Cr3;
    let (pml4_frame, _) = Cr3::read();
    let pml4_phys = pml4_frame.start_address();
    crate::mm::space_repository::with_space_by_pml4(pml4_phys, |space| {
        (space.aslr_stack_guard_end, crate::mm::space::layout::USER_STACK_TOP)
    })
}

/// Stack demand-fault path (M10): warn at 1/4/8 MB growth thresholds, then
/// map a read+write+no-exec page. The 16 MB hard limit (`USER_STACK_SIZE`)
/// is enforced structurally — the guard page at `USER_STACK_BOTTOM` is never
/// demand-paged, so overflow kills the thread rather than silently corrupting.
fn handle_stack_fault(addr: u64, stack_top: u64) -> Option<bool> {
    warn_stack_growth_threshold(addr & !0xFFF, stack_top);
    demand_map_page(addr)
}

/// Fire a one-shot warning when stack growth crosses 1/4/8 MB. Each threshold
/// boundary lives in exactly one 4 KB page; firing when that page is faulted
/// deduplicates naturally (no per-process state, IRQ-safe, no allocation) and
/// tolerates large stack frames.
fn warn_stack_growth_threshold(virt_page: u64, stack_top: u64) {
    const MB: u64 = 1024 * 1024;
    let thresholds: &[(u64, &str)] = &[
        (8 * MB, "8 MB"),
        (4 * MB, "4 MB"),
        (1 * MB, "1 MB"),
    ];
    for &(bytes, label) in thresholds {
        let boundary = stack_top - bytes;
        if virt_page <= boundary && boundary < virt_page + 0x1000 {
            klibcluu::log_str_pair(
                klibcluu::LogLevel::Warn,
                "Stack growth: crossed ",
                label,
            );
            klibcluu::warn("Stack growth: approaching 16 MB USER_STACK_SIZE limit");
            return;
        }
    }
}

/// Allocate a zeroed frame and map it read+write+no-exec at `addr` in the
/// current CR3. Owner lookup tags intermediate PT frames correctly; falls
/// back to `KERNEL_OWNER` early in boot (better than sentinel 0).
fn demand_map_page(addr: u64) -> Option<bool> {
    use x86_64::registers::control::Cr3;

    klibcluu::trace("Demand paging: allocating page for fault at 0x");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "", addr);

    let (pml4_frame, _) = Cr3::read();
    let page_table_root = pml4_frame.start_address();

    // try_alloc_frame is mandatory: pf_with_regs runs with interrupts
    // disabled. A blocking PMM.lock() while another thread holds the PMM
    // would spin forever in IRQ context (halt pattern of 393cd6b). None
    // here triggers the fault path; the access retries on the next attempt.
    let frame_phys = match crate::mm::pmm::try_alloc_frame() {
        Some(f) => f,
        None => {
            klibcluu::warn("Demand paging: PMM busy or OOM");
            return None;
        }
    };

    // Zero via physmap before mapping (security: prevent info leakage).
    let frame_virt = unsafe { crate::mm::physmap::phys_to_virt_u64(frame_phys) };
    unsafe {
        core::ptr::write_bytes(frame_virt as *mut u8, 0, 4096);
    }

    let page_table_root_phys = page_table_root.as_u64();
    let demand_owner = {
        let mut found = crate::token::scope::KERNEL_OWNER;
        crate::mm::space_repository::for_each(|sid, pml4_pa| {
            if pml4_pa.as_u64() == page_table_root_phys {
                found = sid;
            }
        });
        found
    };
    let virt_page = addr & !0xFFF;
    let _ = crate::mm::frame_table::retype_to_user(frame_phys, demand_owner);
    let result = unsafe {
        crate::elf::map_user_page(
            virt_page,
            frame_phys,
            true,  // writable
            false, // not executable — stack AND heap are no-exec
            page_table_root,
            demand_owner,
        )
    };

    match result {
        Ok(()) => {
            klibcluu::trace("Demand paging: successfully mapped page");
            Some(true)
        }
        Err(_) => {
            klibcluu::warn("Demand paging: failed to map page");
            crate::mm::pmm::free_frame(frame_phys);
            None
        }
    }
}

/// Demand-page a text segment fault (M9). Returns Some(true) if a page was
/// mapped with the original text content, Some(false) if the fault is outside
/// any demand-paged text region, None on allocation or source-translation
/// failure. Parallel to `handle_heap_fault` but maps read+exec (not r+w)
/// and copies from the recorded source instead of zero-filling.
fn handle_text_fault(fault_addr: u64) -> Option<bool> {
    use x86_64::registers::control::Cr3;
    use klibcluu::util::PAGE_SIZE_USIZE as PAGE_SIZE;

    let (pml4_frame, _) = Cr3::read();
    let page_table_root = pml4_frame.start_address();

    // The source bytes live in a kernel heap buffer inside TextSource, so
    // the copy must happen while borrowing the space. PMM allocation is
    // independent of space_repository, so it is safe to do inside this
    // closure. The mapping + demand-owner lookup happen outside because
    // for_each cannot be called with the repo lock held.
    let frame_result = crate::mm::space_repository::with_space_by_pml4(
        page_table_root,
        |space| {
            let text = &space.text;
            if text.size == 0 {
                return None;
            }
            if fault_addr < text.start.as_u64()
                || fault_addr >= text.start.as_u64() + text.size as u64
            {
                return None;
            }
            let source = match space.text_source.as_ref() {
                Some(s) => s,
                None => return None,
            };

            klibcluu::trace("Text demand-fault at 0x");
            klibcluu::log_hex(klibcluu::LogLevel::Trace, "", fault_addr);

            let frame_phys = match crate::mm::pmm::try_alloc_frame() {
                Some(f) => f,
                None => {
                    klibcluu::warn("Text demand-fault: PMM busy or OOM");
                    return Some(None);
                }
            };

            let frame_virt = unsafe { crate::mm::physmap::phys_to_virt_u64(frame_phys) };
            unsafe {
                core::ptr::write_bytes(frame_virt as *mut u8, 0, PAGE_SIZE);
            }

            let page_start = fault_addr & !0xFFFu64;
            let page_offset = (page_start - text.start.as_u64()) as usize;
            let file_size = source.source_data.len();
            let bytes_to_copy = if page_offset < file_size {
                let remaining = file_size - page_offset;
                if remaining > PAGE_SIZE { PAGE_SIZE } else { remaining }
            } else {
                0
            };

            if bytes_to_copy > 0 {
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        source.source_data.as_ptr().add(page_offset),
                        frame_virt as *mut u8,
                        bytes_to_copy,
                    );
                }
            }

            Some(Some(frame_phys))
        },
    );

    let frame_phys = match frame_result {
        Some(Some(Some(phys))) => phys,
        Some(Some(None)) => return None, // PMM OOM
        Some(None) | None => return Some(false), // not a text fault / no source
    };

    let demand_owner = {
        let mut found = crate::token::scope::KERNEL_OWNER;
        crate::mm::space_repository::for_each(|sid, pml4_pa| {
            if pml4_pa.as_u64() == page_table_root.as_u64() {
                found = sid;
            }
        });
        found
    };

    let virt_page = fault_addr & !0xFFFu64;
    let _ = crate::mm::frame_table::retype_to_user(frame_phys, demand_owner);
    let result = unsafe {
        crate::elf::map_user_page(
            virt_page,
            frame_phys,
            false, // not writable
            true,  // executable
            page_table_root,
            demand_owner,
        )
    };

    match result {
        Ok(()) => {
            klibcluu::trace("Text demand-fault: mapped page at 0x");
            klibcluu::log_hex(klibcluu::LogLevel::Trace, "", virt_page);
            Some(true)
        }
        Err(_) => {
            klibcluu::warn("Text demand-fault: failed to map page");
            crate::mm::pmm::free_frame(frame_phys);
            None
        }
    }
}

// x87 FP exceptions now handled by generic_fault_entry_mf (assembly)

extern "x86-interrupt" fn alignment_check_handler(
    _stack_frame: InterruptStackFrame,
    _error_code: u64,
) {
    klibcluu::warn("ALIGNMENT_CHECK");
    panic!("Alignment check exception");
}

extern "x86-interrupt" fn machine_check_handler(_stack_frame: InterruptStackFrame) -> ! {
    klibcluu::warn("MACHINE_CHECK");
    // Critical hardware error - halt immediately
    loop {
        x86_64::instructions::hlt();
    }
}

// SIMD FP exceptions now handled by generic_fault_entry_xm (assembly)

extern "x86-interrupt" fn virtualization_handler(_stack_frame: InterruptStackFrame) {
    klibcluu::warn("VIRTUALIZATION_EXCEPTION");
    panic!("Virtualization exception");
}

extern "x86-interrupt" fn security_exception_handler(
    _stack_frame: InterruptStackFrame,
    _error_code: u64,
) {
    klibcluu::warn("SECURITY_EXCEPTION");
    panic!("Security exception");
}

// Hardware interrupt handlers

#[no_mangle]
extern "C" fn timer_interrupt_dispatch(
    current_ctx_ptr: *const crate::sched::Context,
) -> *const crate::sched::Context {
    // Tick the scheduler (handles timeslice expiration in NORMALMODE)
    if crate::sched::ThreadManager::is_normal_mode() {
        crate::sched::ThreadManager::tick();
    }

    // Send EOI BEFORE scheduling — schedule_and_switch may call
    // idle_until_runnable() which does sti;hlt. If EOI hasn't been sent,
    // the timer IRQ stays masked and the system deadlocks.
    if crate::architecture::x86_64::apic::is_enabled() {
        crate::architecture::x86_64::apic::eoi();
    } else {
        unsafe {
            pic_eoi(0);
        }
    }

    // Post-fault timer diagnostic
    let diag_remaining = POST_FAULT_TIMER_DIAG.load(AtomicOrdering::Acquire);
    if diag_remaining > 0 {
        POST_FAULT_TIMER_DIAG.store(diag_remaining - 1, AtomicOrdering::Release);
        let cur = crate::sched::ThreadManager::current_id_raw();
        klibcluu::warn("post_fault_tick: cur_tid=");
        klibcluu::log_dec(klibcluu::LogLevel::Warn, "", cur);
        if !current_ctx_ptr.is_null() {
            let cs = unsafe { (*current_ctx_ptr).cs };
            klibcluu::warn("post_fault_tick: CS=");
            klibcluu::log_hex(klibcluu::LogLevel::Warn, "", cs);
        }
    }

    // Only preempt in NORMALMODE
    if crate::sched::ThreadManager::is_normal_mode() {
        let result = unsafe { crate::sched::ThreadManager::schedule_and_switch(current_ctx_ptr) };
        if diag_remaining > 0 && !result.is_null() {
            let next_cs = unsafe { (*result).cs };
            let next_tid = crate::sched::ThreadManager::current_id_raw();
            klibcluu::warn("post_fault_tick: switched_to tid=");
            klibcluu::log_dec(klibcluu::LogLevel::Warn, "", next_tid);
            klibcluu::warn("post_fault_tick: next_CS=");
            klibcluu::log_hex(klibcluu::LogLevel::Warn, "", next_cs);
        }
        result
    } else {
        core::ptr::null()
    }
}

#[no_mangle]
extern "C" fn timer_interrupt_ack() {
    // Post-fault timer diagnostic (kernel-mode fast path)
    let diag_remaining = POST_FAULT_TIMER_DIAG.load(AtomicOrdering::Acquire);
    if diag_remaining > 0 {
        POST_FAULT_TIMER_DIAG.store(diag_remaining - 1, AtomicOrdering::Release);
        let cur = crate::sched::ThreadManager::current_id_raw();
        klibcluu::warn("post_fault_ack: cur_tid=");
        klibcluu::log_dec(klibcluu::LogLevel::Warn, "", cur);
    }

    // Always tick the scheduler counter, even on fast path (kernel mode without scheduling)
    // This ensures timeouts are checked and the tick counter advances
    if crate::sched::ThreadManager::is_normal_mode() {
        crate::sched::ThreadManager::tick();
    }

    if crate::architecture::x86_64::apic::is_enabled() {
        crate::architecture::x86_64::apic::eoi();
    } else {
        unsafe {
            pic_eoi(0);
        }
    }
}

#[no_mangle]
extern "C" fn timer_interrupt_should_schedule() -> u8 {
    if !crate::sched::ThreadManager::is_normal_mode() {
        return 0;
    }
    if crate::sched::ThreadManager::current_id_raw() == 0 {
        return 1;
    }
    0
}

extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    let mut port = x86_64::instructions::port::Port::<u8>::new(0x60);
    let scancode = unsafe { port.read() };
    crate::devices::irq::dispatch_irq(1, crate::devices::irq::KBD_RAW_LABEL, scancode);

    if crate::architecture::x86_64::apic::is_enabled() {
        crate::architecture::x86_64::apic::eoi();
    }
    // Always ACK the PIC for IRQ1 while we're still using 8259 routing.
    unsafe {
        pic_eoi(1); // IRQ 1 - Keyboard
    }
}

extern "x86-interrupt" fn mouse_interrupt_handler(_stack_frame: InterruptStackFrame) {
    let mut port = x86_64::instructions::port::Port::<u8>::new(0x60);
    let byte = unsafe { port.read() };
    crate::devices::irq::dispatch_irq(12, crate::devices::irq::KBD_RAW_LABEL, byte);

    if crate::architecture::x86_64::apic::is_enabled() {
        crate::architecture::x86_64::apic::eoi();
    }
    unsafe {
        pic_eoi(12);
    }
}

extern "x86-interrupt" fn serial_interrupt_handler(_stack_frame: InterruptStackFrame) {
    klibcluu::warn("SERIAL_IRQ");

    // Serial interrupt - just acknowledge
    unsafe {
        pic_eoi(4); // IRQ 4 - Serial COM1/COM2
    }
}

/// IRQ 11 — virtio-blk-pci on QEMU's PCI INTA routing.
///
/// virtio-blk userspace `irq_attach`s its private endpoint to IRQ 11; this
/// handler delivers the wakeup. The "data byte" field of `dispatch_scancode`
/// is unused for non-keyboard IRQs (the userspace driver reads ISR via MMIO
/// to learn what fired), but the kernel-side `dispatch_scancode` is the
/// existing IRQ→IPC bridge — reusing it keeps the lock-free fast path.
///
/// Userspace failure this fixes: with no IDT[43] handler, an IRQ 11 fired
/// by virtio-blk-modern after the device started processing requests landed
/// on a null gate descriptor and the CPU raised #GP, halting the kernel
/// (boot got as far as `ext2 filesystem mounted` before the next IRQ
/// brought the system down).
extern "x86-interrupt" fn virtio_blk_interrupt_handler(_stack_frame: InterruptStackFrame) {
    crate::devices::irq::dispatch_irq(11, crate::devices::irq::KBD_RAW_LABEL, 0);
    if crate::architecture::x86_64::apic::is_enabled() {
        crate::architecture::x86_64::apic::eoi();
    }
    unsafe {
        pic_eoi(11);
    }
}

extern "x86-interrupt" fn generic_interrupt_handler(_stack_frame: InterruptStackFrame) {
    // Generic handler for unhandled interrupts
    klibcluu::warn("GENERIC_IRQ");

    // Send EOI to both PICs (conservative approach for unknown IRQ)
    // Use IRQ 15 to ensure both master and slave PIC get EOI
    unsafe {
        pic_eoi(15); // IRQ 15 - highest IRQ, ensures both PICs get EOI
    }
}
