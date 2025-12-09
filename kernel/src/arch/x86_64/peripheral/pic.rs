use x86_64::instructions::port::Port;

/// Initialize the Programmable Interrupt Controller (PIC)
pub fn init_pic() {
    // Initialize master PIC
    let mut master_command = Port::new(0x20);
    let mut master_data = Port::new(0x21);

    // Initialize slave PIC
    let mut slave_command = Port::new(0xA0);
    let mut slave_data = Port::new(0xA1);

    // Start initialization sequence
    unsafe { master_command.write(0x11u8) };
    unsafe { slave_command.write(0x11u8) };

    // Set interrupt vector offsets
    unsafe { master_data.write(0x20u8) }; // Master PIC starts at interrupt 32
    unsafe { slave_data.write(0x28u8) }; // Slave PIC starts at interrupt 40

    // Configure cascade
    unsafe { master_data.write(0x04u8) }; // Slave PIC at IRQ2
    unsafe { slave_data.write(0x02u8) }; // Cascade identity

    // Set mode
    unsafe { master_data.write(0x01u8) }; // 8086 mode
    unsafe { slave_data.write(0x01u8) }; // 8086 mode

    // Mask all interrupts initially except timer and keyboard
    unsafe { master_data.write(0xFCu8) }; // Enable IRQ0 (timer) and IRQ1 (keyboard)
    unsafe { slave_data.write(0xFFu8) }; // Mask all slave interrupts
}
