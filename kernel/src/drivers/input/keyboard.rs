/*
 * PS/2 Keyboard Driver
 *
 * This module provides a PS/2 keyboard driver using the pc_keyboard crate
 * for reliable scancode decoding. It handles keyboard interrupts and provides
 * a way to read characters from the kernel.
 *
 * Features:
 * - Uses pc_keyboard crate for robust scancode decoding
 * - Support for multiple keyboard layouts
 * - No heap allocation required
 * - Simple character buffer for kernel input
 * - Full modifier key support (Shift, Ctrl, Alt)
 */

use core::sync::atomic::{AtomicUsize, Ordering};
use pc_keyboard::{DecodedKey, HandleControl, Keyboard, ScancodeSet1, layouts};
use x86_64::instructions::port::Port;

/// PS/2 keyboard data port
const KEYBOARD_DATA_PORT: u16 = 0x60;

/// Simple circular buffer for keyboard input
const BUFFER_SIZE: usize = 64;

/// IRQ-safe keyboard buffer using atomics
static KEYBOARD_BUFFER: [core::sync::atomic::AtomicU32; BUFFER_SIZE] =
    [const { core::sync::atomic::AtomicU32::new(0) }; BUFFER_SIZE];
static BUFFER_READ_POS: AtomicUsize = AtomicUsize::new(0);
static BUFFER_WRITE_POS: AtomicUsize = AtomicUsize::new(0);
static BUFFER_COUNT: AtomicUsize = AtomicUsize::new(0);

/// IRQ-safe keyboard decoder state
static mut KEYBOARD_DECODER: Option<Keyboard<layouts::Us104Key, ScancodeSet1>> = None;
static KEYBOARD_INIT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Initialize keyboard decoder (called once during boot)
pub fn init_keyboard() {
    unsafe {
        KEYBOARD_DECODER = Some(Keyboard::new(
            ScancodeSet1::new(),
            layouts::Us104Key,
            HandleControl::Ignore,
        ));
    }
    KEYBOARD_INIT.store(true, Ordering::SeqCst);
}

/// IRQ-safe buffer operations
fn buffer_push(ch: char) {
    let current_count = BUFFER_COUNT.load(Ordering::Acquire);
    if current_count < BUFFER_SIZE {
        let write_pos = BUFFER_WRITE_POS.load(Ordering::Acquire);
        KEYBOARD_BUFFER[write_pos].store(ch as u32, Ordering::Release);

        let new_write_pos = (write_pos + 1) % BUFFER_SIZE;
        BUFFER_WRITE_POS.store(new_write_pos, Ordering::Release);
        BUFFER_COUNT.store(current_count + 1, Ordering::Release);
    }
}

fn buffer_pop() -> Option<char> {
    let current_count = BUFFER_COUNT.load(Ordering::Acquire);
    if current_count > 0 {
        let read_pos = BUFFER_READ_POS.load(Ordering::Acquire);
        let ch = KEYBOARD_BUFFER[read_pos].load(Ordering::Acquire) as u8 as char;

        let new_read_pos = (read_pos + 1) % BUFFER_SIZE;
        BUFFER_READ_POS.store(new_read_pos, Ordering::Release);
        BUFFER_COUNT.store(current_count - 1, Ordering::Release);
        Some(ch)
    } else {
        None
    }
}

fn buffer_is_empty() -> bool {
    BUFFER_COUNT.load(Ordering::Acquire) == 0
}

/// Handle keyboard interrupt (IRQ-safe, no mutex usage)
///
/// This is the ISR top-half: minimal work in interrupt context.
/// - Read scancode from port
/// - Decode to character
/// - Push to ring buffer
/// - Wake any waiting thread
pub fn handle_keyboard_interrupt() {
    if !KEYBOARD_INIT.load(Ordering::Acquire) {
        crate::utils::debug::irq_log::irq_log("KEYBOARD", "not_initialized");
        return; // Keyboard not initialized yet
    }

    let mut keyboard_port = Port::new(KEYBOARD_DATA_PORT);
    let scancode = unsafe { keyboard_port.read() };

    // Access keyboard decoder without mutex (IRQ context)
    unsafe {
        if let Some(ref mut keyboard) = KEYBOARD_DECODER {
            if let Ok(Some(key_event)) = keyboard.add_byte(scancode) {
                if let Some(key) = keyboard.process_keyevent(key_event) {
                    match key {
                        DecodedKey::Unicode(character) => {
                            buffer_push(character);

                            // Wake any threads waiting for keyboard input
                            crate::scheduler::wake_io_waiters(crate::scheduler::IoChannel::Keyboard);
                        }
                        DecodedKey::RawKey(_key) => {}
                    }
                }
            }
        }
    }
}

/// Read a character from the keyboard buffer (non-blocking)
pub fn read_char() -> Option<char> {
    buffer_pop()
}

/// Check if there are characters available in the keyboard buffer
pub fn has_char() -> bool {
    !buffer_is_empty()
}

/// Read a character from the keyboard buffer (blocking)
///
/// This function implements true blocking I/O using the generic I/O wait queue system.
/// If the buffer is empty, the calling thread will be blocked (removed from ready queue)
/// until a keyboard interrupt arrives with input.
///
/// **How it works:**
/// 1. Check if buffer has data - if yes, return immediately
/// 2. If buffer empty:
///    - Call wait_for_io(IoChannel::Keyboard) to block on keyboard channel
///    - Thread is removed from scheduler
/// 3. Keyboard ISR calls wake_io_waiters(IoChannel::Keyboard) when key pressed
/// 4. Thread wakes up and reads character
///
/// This results in **0% CPU usage** while waiting for input.
pub fn read_char_blocking() -> char {
    loop {
        // Try to read from buffer first
        if let Some(ch) = buffer_pop() {
            return ch;
        }

        // Buffer is empty - block until keyboard interrupt arrives
        // Double-check buffer isn't empty (race condition check)
        if !buffer_is_empty() {
            continue; // Loop will read the character
        }

        // Block on keyboard I/O channel
        crate::scheduler::wait_for_io(crate::scheduler::IoChannel::Keyboard);

        // When we wake up here, input has arrived (or we were spuriously woken)
        // Loop again to read it
    }
}

/// Read a line from the keyboard (blocking until Enter is pressed)
pub fn read_line(buf: &mut [u8]) -> usize {
    let mut pos = 0;

    loop {
        if let Some(ch) = read_char() {
            match ch {
                '\n' | '\r' => {
                    // Echo newline
                    log::info!("");
                    break;
                }
                '\x08' => {
                    // Backspace
                    if pos > 0 {
                        pos -= 1;
                        // Echo backspace
                        crate::print!("\x08 \x08");
                    }
                }
                ch if ch.is_ascii() && pos < buf.len() - 1 => {
                    buf[pos] = ch as u8;
                    pos += 1;
                    // Echo character
                    crate::print!("{}", ch);
                }
                _ => {}
            }
        } else {
            // No character available, yield CPU
            x86_64::instructions::nop();
        }
    }

    buf[pos] = 0; // Null terminate
    pos
}
