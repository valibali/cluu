//! PS/2 controller (i8042) aux port initialization.
//!
//! Enables the second PS/2 port (mouse/aux) so it generates IRQ12
//! interrupts. The kernel does the controller-level init + mouse
//! device setup at boot; the userspace `mouse` driver is a pure
//! IRQ consumer that reassembles 3-byte packets and emits
//! `MOUSE_EVENT_LABEL` events.
//!
//! Why kernel-side: keeps the mouse driver's capability profile
//! identical to kbd (`DEVICE irq` only, no `PCI_ACCESS` needed for
//! port IO). The kbd pattern is "kernel reads the byte, userspace
//! decodes" — we extend it to "kernel sets up the port + device,
//! userspace decodes."
//!
//! Userspace test case this unblocks: mouse-driven window
//! focus/move/resize in the compositor.
//!
//! References (cross-confirmed):
//! - OSDev Wiki, "8042" PS/2 Controller + Mouse Input
//! - Linux kernel include/linux/i8042.h (I8042_CTR_* constants)
//! - EDK2 MdeModulePkg/Bus/Isa/Ps2MouseDxe/CommPs2.{h,c}

use x86_64::instructions::port::Port;

const CMD_PORT: u16 = 0x64;
const DATA_PORT: u16 = 0x60;

// Controller command bytes.
const CMD_READ_CONFIG: u8 = 0x20;
const CMD_WRITE_CONFIG: u8 = 0x60;
const CMD_DISABLE_AUX: u8 = 0xA7;
const CMD_ENABLE_AUX: u8 = 0xA8;
const CMD_DISABLE_KBD: u8 = 0xAD;
const CMD_ENABLE_KBD: u8 = 0xAE;
const CMD_WRITE_AUX: u8 = 0xD4; // next byte to 0x60 goes to mouse

// Controller config byte bits.
#[allow(dead_code)]
const CFG_KBD_IRQ: u8 = 0x01; // bit 0: first port IRQ
const CFG_AUX_IRQ: u8 = 0x02; // bit 1: second port IRQ (IRQ12)
const CFG_AUX_CLOCK_DIS: u8 = 0x20; // bit 5: second port clock (1=disabled)

// Mouse device commands (sent via 0xD4 prefix).
const MOUSE_RESET: u8 = 0xFF;
const MOUSE_SET_DEFAULTS: u8 = 0xF6;
const MOUSE_ENABLE_STREAM: u8 = 0xF4;

#[allow(dead_code)]
// rationale: PS/2 mouse protocol ACK/BAT response codes for future
// mouse-init handshake; not yet needed by the current polled driver.
const MOUSE_ACK: u8 = 0xFA;
const MOUSE_BAT_OK: u8 = 0xAA;

// Bounded poll iterations — avoids hanging boot if no mouse responds.
const POLL_LIMIT: u32 = 100_000;

/// Wait until the input buffer is empty (controller ready to accept a command).
fn wait_input_empty() -> bool {
    let mut status = Port::<u8>::new(CMD_PORT);
    for _ in 0..POLL_LIMIT {
        if unsafe { status.read() } & 0x02 == 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

/// Wait until the output buffer is full (data available to read from 0x60).
fn wait_output_full() -> bool {
    let mut status = Port::<u8>::new(CMD_PORT);
    for _ in 0..POLL_LIMIT {
        if unsafe { status.read() } & 0x01 != 0 {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

/// Send a command byte to the controller (port 0x64).
unsafe fn controller_cmd(cmd: u8) {
    if !wait_input_empty() {
        klibcluu::warn("ps2: controller_cmd timeout");
        return;
    }
    Port::<u8>::new(CMD_PORT).write(cmd);
}

/// Read the output buffer (port 0x60). Returns 0 on timeout.
unsafe fn read_data() -> u8 {
    if !wait_output_full() {
        return 0;
    }
    Port::<u8>::new(DATA_PORT).read()
}

/// Send a byte to the mouse device via the 0xD4 prefix.
/// Reads and discards the ACK. Best-effort — does not fail boot if mouse
/// is absent.
unsafe fn send_mouse_byte(byte: u8) {
    controller_cmd(CMD_WRITE_AUX);
    if !wait_input_empty() {
        return;
    }
    Port::<u8>::new(DATA_PORT).write(byte);
    // Drain ACK (and any BAT/result bytes) best-effort.
    let _ = read_data();
}

/// Initialize the PS/2 aux (mouse) port and device.
///
/// Called from `kstart()` after `idt::init()`. Idempotent-ish: if no
/// second port exists, logs a warning and returns without failing boot.
///
/// # Safety
///
/// Programs hardware I/O ports directly. Must be called before
/// interrupts are enabled and before the userspace mouse driver
/// attaches to IRQ12.
pub unsafe fn init_aux() {
    klibcluu::info("ps2: initializing aux (mouse) port");

    // 1. Disable both ports during config.
    controller_cmd(CMD_DISABLE_KBD);
    controller_cmd(CMD_DISABLE_AUX);

    // 2. Read current controller config byte.
    controller_cmd(CMD_READ_CONFIG);
    let config = read_data();

    // 3. Enable IRQ12 (set bit 1), enable aux clock (clear bit 5).
    //    All other bits (kbd IRQ, translation, system flag) preserved.
    let new_config = (config | CFG_AUX_IRQ) & !CFG_AUX_CLOCK_DIS;

    controller_cmd(CMD_WRITE_CONFIG);
    if wait_input_empty() {
        Port::<u8>::new(DATA_PORT).write(new_config);
    }

    // 4. Enable the aux port device interface.
    controller_cmd(CMD_ENABLE_AUX);

    // 5. Detect whether the controller actually has a second port.
    //    On a single-channel controller, 0xA8 is a no-op and config
    //    bit 5 stays set. Read config back to check.
    controller_cmd(CMD_READ_CONFIG);
    let check = read_data();
    if check & CFG_AUX_CLOCK_DIS != 0 {
        klibcluu::warn("ps2: no second port detected (single-channel controller)");
        // Re-enable kbd and bail — no mouse hardware present.
        controller_cmd(CMD_ENABLE_KBD);
        return;
    }

    // 6. Initialize the mouse device: reset → defaults → enable streaming.
    //    Each command is sent via 0xD4 prefix; ACKs are drained best-effort.
    send_mouse_byte(MOUSE_RESET);
    let _bat = read_data();
    let _id = read_data();

    send_mouse_byte(MOUSE_SET_DEFAULTS);
    send_mouse_byte(MOUSE_ENABLE_STREAM);

    // Flush any residual bytes from the output buffer so they don't
    // fire a spurious IRQ12 on the first enable.
    for _ in 0..16 {
        if !wait_output_full() {
            break;
        }
        let _ = unsafe { Port::<u8>::new(DATA_PORT).read() };
    }

    // 7. Re-enable the keyboard port.
    controller_cmd(CMD_ENABLE_KBD);

    klibcluu::info("ps2: aux port initialized, mouse streaming enabled");
}
