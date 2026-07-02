//! PS/2 mouse 3-byte packet parser.
//!
//! State machine reassembles the 3-byte packet from individual IRQ bytes.
//! Byte 0 sync bit (bit 3, 0x08) must be set; if not, the byte is
//! discarded and the state machine resets to resync.
//!
//! Packet layout (OSDev Wiki / EDK2 CommPs2.c cross-confirmed):
//! - byte 0: [Yovf Xovf Ysign Xsign 1 Mid Right Left]
//! - byte 1: X movement (signed 8-bit)
//! - byte 2: Y movement (signed 8-bit, inverted from screen coords)
//!
//! Overflow (bits 6/7) → packet discarded. Y is negated for screen coords.

const SYNC_MASK: u8 = 0x08;
const X_SIGN: u8 = 0x10;
const Y_SIGN: u8 = 0x20;
const X_OVERFLOW: u8 = 0x40;
const Y_OVERFLOW: u8 = 0x80;

#[derive(Clone, Copy)]
pub struct MouseEvent {
    pub dx: i32,
    pub dy: i32,
    pub buttons: u8,
}

pub struct PacketParser {
    state: u8,
    buf: [u8; 3],
}

impl PacketParser {
    pub const fn new() -> Self {
        Self { state: 0, buf: [0; 3] }
    }

    pub fn feed(&mut self, byte: u8) -> Option<MouseEvent> {
        if self.state == 0 {
            if byte & SYNC_MASK == 0 || byte & X_OVERFLOW != 0 || byte & Y_OVERFLOW != 0 {
                return None;
            }
        }
        self.buf[self.state as usize] = byte;
        self.state += 1;
        if self.state < 3 {
            return None;
        }
        self.state = 0;
        Some(self.decode())
    }

    fn decode(&self) -> MouseEvent {
        let flags = self.buf[0];
        if flags & X_OVERFLOW != 0 || flags & Y_OVERFLOW != 0 {
            return MouseEvent { dx: 0, dy: 0, buttons: flags & 0x07 };
        }
        let dx = sign_extend(self.buf[1], flags & X_SIGN != 0);
        let dy = -sign_extend(self.buf[2], flags & Y_SIGN != 0);
        MouseEvent {
            dx,
            dy,
            buttons: flags & 0x07,
        }
    }
}

fn sign_extend(byte: u8, negative: bool) -> i32 {
    if negative {
        ((byte as u16 | 0xFF00) as i16) as i32
    } else {
        byte as i32
    }
}
