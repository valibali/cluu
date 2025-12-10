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

use pc_keyboard::{DecodedKey, HandleControl, Keyboard, ScancodeSet1, layouts};
use spin::Mutex;
use x86_64::instructions::port::Port;

/// PS/2 keyboard data port
const KEYBOARD_DATA_PORT: u16 = 0x60;

/// Simple circular buffer for keyboard input
const BUFFER_SIZE: usize = 64;

pub struct KeyboardBuffer {
    buffer: [char; BUFFER_SIZE],
    read_pos: usize,
    write_pos: usize,
    count: usize,
}

impl KeyboardBuffer {
    const fn new() -> Self {
        Self {
            buffer: ['\0'; BUFFER_SIZE],
            read_pos: 0,
            write_pos: 0,
            count: 0,
        }
    }

    fn push(&mut self, ch: char) {
        if self.count < BUFFER_SIZE {
            self.buffer[self.write_pos] = ch;
            self.write_pos = (self.write_pos + 1) % BUFFER_SIZE;
            self.count += 1;
        }
    }

    fn pop(&mut self) -> Option<char> {
        if self.count > 0 {
            let ch = self.buffer[self.read_pos];
            self.read_pos = (self.read_pos + 1) % BUFFER_SIZE;
            self.count -= 1;
            Some(ch)
        } else {
            None
        }
    }

    fn is_empty(&self) -> bool {
        self.count == 0
    }
}

/// Global keyboard buffer and decoder
static KEYBOARD_BUFFER: Mutex<KeyboardBuffer> = Mutex::new(KeyboardBuffer::new());
static KEYBOARD: Mutex<Keyboard<layouts::Us104Key, ScancodeSet1>> = Mutex::new(Keyboard::new(
    ScancodeSet1::new(),
    layouts::Us104Key,
    HandleControl::Ignore,
));

/// Handle keyboard interrupt
pub fn handle_keyboard_interrupt() {
    let mut keyboard_port = Port::new(KEYBOARD_DATA_PORT);
    let scancode = unsafe { keyboard_port.read() };

    let mut keyboard = KEYBOARD.lock();

    if let Ok(Some(key_event)) = keyboard.add_byte(scancode) {
        if let Some(key) = keyboard.process_keyevent(key_event) {
            match key {
                DecodedKey::Unicode(character) => {
                    let mut buffer = KEYBOARD_BUFFER.lock();
                    buffer.push(character);

                    // Echo character to serial for debugging
                    log::info!("Key pressed: '{}'", character);
                }
                DecodedKey::RawKey(key) => {
                    // Handle special keys that don't have Unicode representation
                    log::info!("Special key pressed: {:?}", key);
                }
            }
        }
    }
}

/// Read a character from the keyboard buffer
pub fn read_char() -> Option<char> {
    let mut buffer = KEYBOARD_BUFFER.lock();
    buffer.pop()
}

/// Check if there are characters available in the keyboard buffer
pub fn has_char() -> bool {
    let buffer = KEYBOARD_BUFFER.lock();
    !buffer.is_empty()
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
            x86_64::instructions::hlt();
        }
    }

    buf[pos] = 0; // Null terminate
    pos
}
