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

pub fn init_pit(frequency_hz: u32) {
    let pit_frequency: u32 = 1_193_182; // Hz
    let divisor: u16 = (pit_frequency / frequency_hz) as u16;

    unsafe {
        let mut command = Port::<u8>::new(0x43);
        let mut channel0 = Port::<u8>::new(0x40);

        // Channel 0, access mode lo/hi, mode 3 (square wave), binary
        command.write(0x36);

        channel0.write((divisor & 0xFF) as u8); // low byte
        channel0.write((divisor >> 8) as u8); // high byte
    }
}
