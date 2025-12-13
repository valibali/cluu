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

    unsafe {
        let master_mask: u8 = master_data.read();
        let slave_mask: u8 = slave_data.read();
        log::info!(
            "PIC masks after init: master=0x{:02x} slave=0x{:02x}",
            master_mask,
            slave_mask
        );
    }
}

pub fn init_pit(frequency_hz: u32) {
    let pit_frequency: u32 = 1_193_182; // Hz - PIT base frequency
    let divisor: u16 = (pit_frequency / frequency_hz) as u16;

    log::info!(
        "Initializing PIT with {}Hz (divisor: {})",
        frequency_hz,
        divisor
    );

    unsafe {
        let mut command = Port::<u8>::new(0x43);
        let mut channel0 = Port::<u8>::new(0x40);

        // Channel 0, access mode lo/hi, mode 3 (square wave), binary
        command.write(0x36);

        // Write divisor in two parts: low byte first, then high byte
        channel0.write((divisor & 0xFF) as u8); // low byte
        channel0.write((divisor >> 8) as u8); // high byte
    }

    log::info!("PIT configured for {}Hz timer interrupts", frequency_hz);
}
