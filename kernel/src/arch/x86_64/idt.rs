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
                .set_stack_index(crate::arch::x86_64::gdt::DOUBLE_FAULT_IST_INDEX);
        }
        idt.invalid_tss.set_handler_fn(invalid_tss_handler);
        idt.segment_not_present.set_handler_fn(segment_not_present_handler);
        idt.stack_segment_fault.set_handler_fn(stack_segment_fault_handler);
        idt.general_protection_fault.set_handler_fn(general_protection_fault_handler);
        idt.page_fault.set_handler_fn(page_fault_handler);
        idt.x87_floating_point.set_handler_fn(x87_floating_point_handler);
        idt.alignment_check.set_handler_fn(alignment_check_handler);
        idt.machine_check.set_handler_fn(machine_check_handler);
        idt.simd_floating_point.set_handler_fn(simd_floating_point_handler);
        idt.virtualization.set_handler_fn(virtualization_handler);
        idt.security_exception.set_handler_fn(security_exception_handler);

        // Set up hardware interrupt handlers (IRQ 0-15 map to interrupts 32-47)
        idt[32].set_handler_fn(timer_interrupt_handler);  // IRQ 0 - Timer
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
    log::info!("Setting up IDT handlers...");
    log::info!("Loading IDT...");
    IDT.load();
    log::info!("IDT loaded successfully");
    log::info!("IDT initialized successfully");
}

// Exception handlers - these functions are called when CPU exceptions occur

extern "x86-interrupt" fn divide_error_handler(_stack_frame: InterruptStackFrame) {
    // Simple error message without panic for debugging
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn debug_handler(stack_frame: InterruptStackFrame) {
    log::debug!("EXCEPTION: DEBUG\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn nmi_handler(_stack_frame: InterruptStackFrame) {
    // Simple error handling without panic for debugging
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    log::info!("EXCEPTION: BREAKPOINT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn overflow_handler(_stack_frame: InterruptStackFrame) {
    // Simple error handling without panic for debugging
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn bound_range_exceeded_handler(_stack_frame: InterruptStackFrame) {
    // Simple error handling without panic for debugging
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn invalid_opcode_handler(_stack_frame: InterruptStackFrame) {
    // Simple error handling without panic for debugging
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn device_not_available_handler(_stack_frame: InterruptStackFrame) {
    // Simple error handling without panic for debugging
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn double_fault_handler(
    _stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    // Critical error - halt immediately without panic
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn invalid_tss_handler(
    _stack_frame: InterruptStackFrame,
    _error_codee: u64,
) {
    // Simple error handling without panic for debugging
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn segment_not_present_handler(
    _stack_frame: InterruptStackFrame,
    _error_code: u64,
) {
    // Simple error handling without panic for debugging
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn stack_segment_fault_handler(
    _stack_frame: InterruptStackFrame,
    _error_code: u64,
) {
    // Simple error handling without panic for debugging
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn general_protection_fault_handler(
    _stack_frame: InterruptStackFrame,
    _error_code: u64,
) {
    // Simple error handling without panic for debugging
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn page_fault_handler(
    _stack_frame: InterruptStackFrame,
    _error_code: x86_64::structures::idt::PageFaultErrorCode,
) {
    // Simple error handling without panic for debugging
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn x87_floating_point_handler(stack_frame: InterruptStackFrame) {
    log::error!("EXCEPTION: x87 FLOATING POINT\n{:#?}", stack_frame);
    panic!("x87 floating point exception");
}

extern "x86-interrupt" fn alignment_check_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    log::error!(
        "EXCEPTION: ALIGNMENT CHECK (Error Code: {})\n{:#?}",
        error_code,
        stack_frame
    );
    panic!("Alignment check exception");
}

extern "x86-interrupt" fn machine_check_handler(_stack_frame: InterruptStackFrame) -> ! {
    // Critical hardware error - halt immediately
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn simd_floating_point_handler(stack_frame: InterruptStackFrame) {
    log::error!("EXCEPTION: SIMD FLOATING POINT\n{:#?}", stack_frame);
    panic!("SIMD floating point exception");
}

extern "x86-interrupt" fn virtualization_handler(stack_frame: InterruptStackFrame) {
    log::error!("EXCEPTION: VIRTUALIZATION\n{:#?}", stack_frame);
    panic!("Virtualization exception");
}

extern "x86-interrupt" fn security_exception_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    log::error!(
        "EXCEPTION: SECURITY EXCEPTION (Error Code: {})\n{:#?}",
        error_code,
        stack_frame
    );
    panic!("Security exception");
}

// Hardware interrupt handlers

extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    // Timer interrupt - just acknowledge and return
    // Send End of Interrupt (EOI) to PIC
    unsafe {
        use x86_64::instructions::port::Port;
        let mut port = Port::new(0x20);
        port.write(0x20u8);
    }
}

extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    // Keyboard interrupt - read the scancode to clear the interrupt
    unsafe {
        use x86_64::instructions::port::Port;
        let mut port = Port::new(0x60);
        let _scancode: u8 = port.read();

        // Send EOI to PIC
        let mut pic_port = Port::new(0x20);
        pic_port.write(0x20u8);
    }
}

extern "x86-interrupt" fn serial_interrupt_handler(_stack_frame: InterruptStackFrame) {
    // Serial interrupt - just acknowledge
    unsafe {
        use x86_64::instructions::port::Port;
        let mut port = Port::new(0x20);
        port.write(0x20u8);
    }
}

extern "x86-interrupt" fn generic_interrupt_handler(stack_frame: InterruptStackFrame) {
    // Generic handler for unhandled interrupts
    log::debug!(
        "INTERRUPT: Generic interrupt handler called\n{:#?}",
        stack_frame
    );

    // Send EOI to both PICs (in case it's from slave PIC)
    unsafe {
        use x86_64::instructions::port::Port;
        let mut pic1_port = Port::new(0x20);
        let mut pic2_port = Port::new(0xA0);
        pic2_port.write(0x20u8); // EOI to slave PIC
        pic1_port.write(0x20u8); // EOI to master PIC
    }
}
