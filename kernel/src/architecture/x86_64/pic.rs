//! Legacy 8259 PIC (Programmable Interrupt Controller) support
//!
//! The 8259 PIC is the legacy interrupt controller used in x86 systems.
//! Modern systems use APIC/xAPIC/x2APIC, but the PIC must still be
//! disabled or properly configured during boot.
//!
//! Why this is important:
//! - The PIC defaults to sending IRQs to vectors that conflict with CPU exceptions
//! - We need to either remap the PIC or disable it entirely
//! - Failure to configure the PIC can cause spurious interrupts and system hangs

use x86_64::instructions::port::Port;

/// PIC1 (Master) command port
const PIC1_COMMAND: u16 = 0x20;
/// PIC1 (Master) data port
const PIC1_DATA: u16 = 0x21;
/// PIC2 (Slave) command port
const PIC2_COMMAND: u16 = 0xA0;
/// PIC2 (Slave) data port
const PIC2_DATA: u16 = 0xA1;

/// PIC initialization command
const ICW1_INIT: u8 = 0x10;
const ICW1_ICW4: u8 = 0x01;
/// Disable both PICs by masking all interrupts
///
/// This is the safest approach during early boot to prevent
/// spurious interrupts from causing issues.
/// Disable the legacy PIC by masking all IRQ lines.
///
/// # Safety
///
/// Must be called only during early boot or when interrupts are already
/// masked, because it directly programs the PIC without synchronization.
pub unsafe fn disable() {
    klibcluu::info("Disabling legacy 8259 PIC...");

    // Mask all interrupts on both PICs
    Port::<u8>::new(PIC1_DATA).write(0xFF);
    Port::<u8>::new(PIC2_DATA).write(0xFF);

    klibcluu::info("PIC disabled (all interrupts masked)");
}

/// Remap the PIC to non-conflicting interrupt vectors
///
/// By default, the PIC sends IRQs to vectors 0x08-0x0F (master)
/// and 0x70-0x77 (slave), which conflict with CPU exceptions.
/// We remap them to 0x20-0x2F (32-47) instead.
///
/// # Arguments
/// * `offset1` - Vector offset for master PIC (typically 32)
/// * `offset2` - Vector offset for slave PIC (typically 40)
///
/// # Safety
///
/// The caller must ensure no IRQs are firing during the remap sequence and
/// that the offsets do not overlap CPU exception vectors.
pub unsafe fn remap(offset1: u8, offset2: u8) {
    klibcluu::info("Remapping 8259 PIC...");

    let mut cmd_pic1 = Port::<u8>::new(PIC1_COMMAND);
    let mut data_pic1 = Port::<u8>::new(PIC1_DATA);
    let mut cmd_pic2 = Port::<u8>::new(PIC2_COMMAND);
    let mut data_pic2 = Port::<u8>::new(PIC2_DATA);

    // Save current masks
    let mask1 = data_pic1.read();
    let mask2 = data_pic2.read();

    // Start initialization sequence
    cmd_pic1.write(ICW1_INIT | ICW1_ICW4);
    io_wait();
    cmd_pic2.write(ICW1_INIT | ICW1_ICW4);
    io_wait();

    // Set vector offsets
    data_pic1.write(offset1);
    io_wait();
    data_pic2.write(offset2);
    io_wait();

    // Configure cascade (PIC2 connected to PIC1 IRQ2)
    data_pic1.write(4); // PIC2 is at IRQ2
    io_wait();
    data_pic2.write(2); // Cascade identity
    io_wait();

    // Set 8086 mode
    data_pic1.write(0x01);
    io_wait();
    data_pic2.write(0x01);
    io_wait();

    // Restore masks (or mask all)
    data_pic1.write(mask1);
    data_pic2.write(mask2);

    klibcluu::info("PIC remapped to vectors 32-47");
}

/// Initialize the PIC for use with remapped vectors
///
/// This remaps the PIC to vectors 32-47 and masks all interrupts
/// except those we explicitly enable.
/// Initialize the PIC to a known state and mask all IRQs.
///
/// # Safety
///
/// Must only be called once during early boot before enabling interrupts.
pub unsafe fn init() {
    // Remap PIC to vectors 32-47 (IRQ 0-15 → INT 32-47)
    remap(32, 40);

    // Mask all interrupts initially
    // We'll unmask them individually as we set up handlers
    Port::<u8>::new(PIC1_DATA).write(0xFF);
    Port::<u8>::new(PIC2_DATA).write(0xFF);

    klibcluu::info("PIC initialized (all IRQs masked)");
}

/// Unmask a specific IRQ line on the PIC.
///
/// This enables the given IRQ number (0-15).
/// Unmask a specific IRQ line on the PIC.
///
/// # Safety
///
/// The caller must ensure the IRQ handler is installed before unmasking.
pub unsafe fn unmask(irq: u8) {
    if irq >= 16 {
        return;
    }
    if irq < 8 {
        let mut data_pic1 = Port::<u8>::new(PIC1_DATA);
        let mask = data_pic1.read() & !(1 << irq);
        data_pic1.write(mask);
    } else {
        // Unmask cascade on PIC1 first.
        let mut data_pic1 = Port::<u8>::new(PIC1_DATA);
        let mask1 = data_pic1.read() & !(1 << 2);
        data_pic1.write(mask1);

        let mut data_pic2 = Port::<u8>::new(PIC2_DATA);
        let mask2 = data_pic2.read() & !(1 << (irq - 8));
        data_pic2.write(mask2);
    }
}

/// I/O wait - small delay for PIC to process commands
///
/// Some systems need a small delay between PIC I/O operations.
/// We use port 0x80 (POST diagnostic port) for this.
#[inline]
fn io_wait() {
    unsafe {
        Port::<u8>::new(0x80).write(0);
    }
}
