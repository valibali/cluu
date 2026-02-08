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

use lazy_static::lazy_static;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};
use x86_64::VirtAddr;

extern "C" {
    fn timer_interrupt_entry();
    fn gpf_interrupt_entry();
    fn pf_interrupt_entry();
}

/// Send End of Interrupt (EOI) signal to PIC
///
/// This function properly handles EOI for both master and slave PIC.
/// For IRQs 0-7 (master PIC), only master EOI is needed.
/// For IRQs 8-15 (slave PIC), both slave and master EOI are needed.
unsafe fn pic_eoi(irq: u8) {
    use x86_64::instructions::port::Port;

    // If the IRQ came from the slave (>=8), EOI slave first
    if irq >= 8 {
        unsafe { Port::<u8>::new(0xA0).write(0x20) };
    }
    unsafe { Port::<u8>::new(0x20).write(0x20) };
}

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();

        // Set up all exception handlers
        idt.divide_error.set_handler_fn(divide_error_handler);
        idt.debug.set_handler_fn(debug_handler);
        idt.non_maskable_interrupt.set_handler_fn(nmi_handler);
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        idt.overflow.set_handler_fn(overflow_handler);
        idt.bound_range_exceeded.set_handler_fn(bound_range_exceeded_handler);
        idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);
        idt.device_not_available.set_handler_fn(device_not_available_handler);
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
                .set_handler_addr(VirtAddr::new(gpf_interrupt_entry as *const () as u64));
        }
        unsafe {
            idt.page_fault
                .set_handler_addr(VirtAddr::new(pf_interrupt_entry as *const () as u64));
        }
        idt.x87_floating_point.set_handler_fn(x87_floating_point_handler);
        idt.alignment_check.set_handler_fn(alignment_check_handler);
        idt.machine_check.set_handler_fn(machine_check_handler);
        idt.simd_floating_point.set_handler_fn(simd_floating_point_handler);
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

extern "x86-interrupt" fn divide_error_handler(_stack_frame: InterruptStackFrame) {
    klibcluu::warn("DIVIDE_ERROR");
    // Simple error message without panic for debugging
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn debug_handler(_stack_frame: InterruptStackFrame) {
    klibcluu::warn("DEBUG_EXCEPTION");
}

extern "x86-interrupt" fn nmi_handler(_stack_frame: InterruptStackFrame) {
    klibcluu::warn("NMI");
    // Simple error handling without panic for debugging
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn breakpoint_handler(_stack_frame: InterruptStackFrame) {
    klibcluu::warn("BREAKPOINT");
}

extern "x86-interrupt" fn overflow_handler(_stack_frame: InterruptStackFrame) {
    klibcluu::warn("OVERFLOW");
    // Simple error handling without panic for debugging
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn bound_range_exceeded_handler(_stack_frame: InterruptStackFrame) {
    klibcluu::warn("BOUND_RANGE_EXCEEDED");
    // Simple error handling without panic for debugging
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn invalid_opcode_handler(stack_frame: InterruptStackFrame) {
    klibcluu::warn("INVALID_OPCODE");

    // Extract values from interrupt stack frame
    let rip = stack_frame.instruction_pointer.as_u64();
    let cs = stack_frame.code_segment.0 as u64;
    let rsp = stack_frame.stack_pointer.as_u64();
    let ss = stack_frame.stack_segment.0 as u64;
    let rflags = stack_frame.cpu_flags.bits();
    let is_userspace = (cs & 3) == 3;

    // Use IRQ-safe logging
    klibcluu::log_hex(klibcluu::LogLevel::Warn, "  RIP=", rip);
    klibcluu::log_hex(klibcluu::LogLevel::Warn, "  CS=", cs);
    klibcluu::log_hex(klibcluu::LogLevel::Warn, "  RSP=", rsp);
    klibcluu::log_hex(klibcluu::LogLevel::Warn, "  SS=", ss);
    klibcluu::log_hex(klibcluu::LogLevel::Warn, "  RFLAGS=", rflags);

    if is_userspace {
        klibcluu::warn("  Fault in USERSPACE (Ring 3)\n");

        // TODO Phase 8: Get current thread ID using scheduler
        // let thread_id = crate::scheduler::ThreadManager::current_id();
        // klibcluu::log_hex(klibcluu::LogLevel::Warn, "  ThreadID=", thread_id.0 as u64);

        // TODO Phase 8: Try to get process info using ThreadManager API
        // if let Some(process_id) = crate::scheduler::ThreadManager::with_current(|t| t.process_id.0) {
        //     klibcluu::log_hex(klibcluu::LogLevel::Warn, "  ProcessID=", process_id as u64);
        // }
    } else {
        klibcluu::warn("  Fault in KERNEL (Ring 0)\n");
    }

    // Try to disassemble instruction at RIP (if accessible via physmap)
    klibcluu::warn("  Instruction bytes at RIP: ");
    unsafe {
        // Attempt to read first 8 bytes at RIP (may fail)
        // If kernel space, try direct read; if userspace, would need page table walk
        if !is_userspace {
            let bytes = core::slice::from_raw_parts(rip as *const u8, 8);
            for byte in bytes.iter().take(8) {
                klibcluu::log_hex(klibcluu::LogLevel::Warn, "", (*byte) as u64);
                klibcluu::warn(" ");
            }
        } else {
            klibcluu::warn("(userspace - skipping)");
        }
    }
    klibcluu::warn("");

    // Halt system
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn device_not_available_handler(_stack_frame: InterruptStackFrame) {
    klibcluu::warn("DEVICE_NOT_AVAILABLE");
    // Simple error handling without panic for debugging
    loop {
        x86_64::instructions::hlt();
    }
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
        klibcluu::log_hex(klibcluu::LogLevel::Warn, "GPF: Selector index=", selector_index);
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

#[no_mangle]
extern "C" fn gpf_with_regs(frame: *const GpfDebugFrame) -> ! {
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

    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: x86_64::structures::idt::PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;
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

#[no_mangle]
extern "C" fn pf_with_regs(frame: *const PfDebugFrame) -> ! {
    use klibcluu::uart::COM2;
    use x86_64::registers::control::Cr2;
    use core::arch::asm;

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

    #[inline(always)]
    fn read_gs_u64(offset: u64) -> u64 {
        let value: u64;
        unsafe {
            asm!(
                "mov {0}, qword ptr gs:[{1}]",
                out(reg) value,
                in(reg) offset,
                options(nostack, preserves_flags)
            );
        }
        value
    }

    COM2.write_str("[WARN]  PAGE_FAULT (regs)\n");
    uart_hex("PF: Fault address (CR2)=", cr2);
    uart_hex("PF: Error code=", f.error_code);
    uart_hex("PF: RIP=", f.rip);
    uart_hex("PF: CS=", f.cs);
    uart_hex("PF: RFLAGS=", f.rflags);
    uart_hex("PF: RSP=", f.rsp);
    uart_hex("PF: SS=", f.ss);
    uart_hex("PF: RBX=", f.rbx);
    uart_hex("PF: RBP=", f.rbp);
    uart_hex("PF: RDI=", f.rdi);
    uart_hex("PF: RSI=", f.rsi);
    uart_hex("PF: R12=", f.r12);
    uart_hex("PF: R13=", f.r13);
    uart_hex("PF: R14=", f.r14);
    uart_hex("PF: R15=", f.r15);

    // If fault came from userspace, dump last syscall info from PerCpuData
    if (f.cs & 0x3) == 0x3 {
        const PERCPU_LAST_SYSNO: u64 = 0x28;
        const PERCPU_LAST_RIP: u64 = 0x30;
        const PERCPU_LAST_RSP: u64 = 0x38;
        const PERCPU_LAST_RBX: u64 = 0x40;
        const PERCPU_LAST_ARG1: u64 = 0x48;
        const PERCPU_LAST_ARG2: u64 = 0x50;
        const PERCPU_LAST_ARG3: u64 = 0x58;
        const PERCPU_LAST_ARG4: u64 = 0x60;
        const PERCPU_LAST_ARG5: u64 = 0x68;
        const PERCPU_LAST_ARG6: u64 = 0x70;
        const PERCPU_LAST_RBX_RET: u64 = 0x78;

        let last_sysno = read_gs_u64(PERCPU_LAST_SYSNO);
        let last_rip = read_gs_u64(PERCPU_LAST_RIP);
        let last_rsp = read_gs_u64(PERCPU_LAST_RSP);
        let last_rbx = read_gs_u64(PERCPU_LAST_RBX);
        let last_arg1 = read_gs_u64(PERCPU_LAST_ARG1);
        let last_arg2 = read_gs_u64(PERCPU_LAST_ARG2);
        let last_arg3 = read_gs_u64(PERCPU_LAST_ARG3);
        let last_arg4 = read_gs_u64(PERCPU_LAST_ARG4);
        let last_arg5 = read_gs_u64(PERCPU_LAST_ARG5);
        let last_arg6 = read_gs_u64(PERCPU_LAST_ARG6);
        let last_rbx_ret = read_gs_u64(PERCPU_LAST_RBX_RET);

        COM2.write_str("[DBG] Last syscall (from PerCpuData)\n");
        uart_hex("PF: last_sysno=", last_sysno);
        uart_hex("PF: last_rip=", last_rip);
        uart_hex("PF: last_rsp=", last_rsp);
        uart_hex("PF: last_rbx=", last_rbx);
        uart_hex("PF: last_arg1=", last_arg1);
        uart_hex("PF: last_arg2=", last_arg2);
        uart_hex("PF: last_arg3=", last_arg3);
        uart_hex("PF: last_arg4=", last_arg4);
        uart_hex("PF: last_arg5=", last_arg5);
        uart_hex("PF: last_arg6=", last_arg6);
        uart_hex("PF: last_rbx_ret=", last_rbx_ret);
    }

    loop {
        x86_64::instructions::hlt();
    }
}

/// Handle page fault via demand paging (lazy allocation)
///
/// Returns Some(true) if page was successfully allocated,
/// Some(false) if fault is not in a demand-pageable region,
/// None if allocation failed.
fn handle_heap_fault(fault_addr: x86_64::VirtAddr) -> Option<bool> {
    use crate::mm::space::layout;
    use x86_64::registers::control::Cr3;

    let addr = fault_addr.as_u64();

    // Check if fault is in a demand-pageable region
    let is_stack_region = (layout::USER_STACK_BOTTOM..layout::USER_STACK_TOP).contains(&addr);
    let is_heap_region = (layout::USER_HEAP_START..layout::USER_HEAP_MAX).contains(&addr);

    if !is_stack_region && !is_heap_region {
        // Fault is not in a demand-pageable region
        return Some(false);
    }

    klibcluu::trace("Demand paging: allocating page for fault at 0x");
    klibcluu::log_hex(klibcluu::LogLevel::Trace, "", addr);

    // Get current page table root from CR3
    let (pml4_frame, _) = Cr3::read();
    let page_table_root = pml4_frame.start_address();

    // Allocate a physical frame
    let frame_phys = match crate::mm::pmm::alloc_frame() {
        Some(f) => f,
        None => {
            klibcluu::warn("Demand paging: out of memory");
            return None;
        }
    };

    // Zero the frame via physmap before mapping (security: prevent info leakage)
    let frame_virt = unsafe { crate::mm::physmap::phys_to_virt_u64(frame_phys) };
    unsafe {
        core::ptr::write_bytes(frame_virt as *mut u8, 0, 4096);
    }

    // Map the page with user read+write permissions (no execute for stack/heap)
    let virt_page = addr & !0xFFF; // Align to page boundary
    let result = unsafe {
        crate::elf::map_user_page(
            virt_page,
            frame_phys,
            true,  // writable
            false, // not executable
            page_table_root,
        )
    };

    match result {
        Ok(()) => {
            klibcluu::trace("Demand paging: successfully mapped page");
            Some(true)
        }
        Err(_) => {
            klibcluu::warn("Demand paging: failed to map page");
            // Free the frame since mapping failed
            crate::mm::pmm::free_frame(frame_phys);
            None
        }
    }
}

extern "x86-interrupt" fn x87_floating_point_handler(_stack_frame: InterruptStackFrame) {
    klibcluu::warn("X87_FP_EXCEPTION");
    panic!("x87 floating point exception");
}

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

extern "x86-interrupt" fn simd_floating_point_handler(_stack_frame: InterruptStackFrame) {
    klibcluu::warn("SIMD_FP_EXCEPTION");
    panic!("SIMD floating point exception");
}

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

    // Only preempt in NORMALMODE
    let next_ctx = if crate::sched::ThreadManager::is_normal_mode() {
        unsafe { crate::sched::ThreadManager::schedule_and_switch(current_ctx_ptr) }
    } else {
        core::ptr::null()
    };

    // Send EOI to interrupt controller
    if crate::architecture::x86_64::apic::is_enabled() {
        crate::architecture::x86_64::apic::eoi();
    } else {
        unsafe {
            pic_eoi(0);
        }
    }

    next_ctx
}

#[no_mangle]
extern "C" fn timer_interrupt_ack() {
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
    crate::devices::irq::dispatch_scancode(1, scancode);

    if crate::architecture::x86_64::apic::is_enabled() {
        crate::architecture::x86_64::apic::eoi();
    }
    // Always ACK the PIC for IRQ1 while we're still using 8259 routing.
    unsafe {
        pic_eoi(1); // IRQ 1 - Keyboard
    }
}

extern "x86-interrupt" fn serial_interrupt_handler(_stack_frame: InterruptStackFrame) {
    klibcluu::warn("SERIAL_IRQ");

    // Serial interrupt - just acknowledge
    unsafe {
        pic_eoi(4); // IRQ 4 - Serial COM1/COM2
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
