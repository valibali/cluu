const BUTTON_A: u8 = 0x01;
const BUTTON_B: u8 = 0x02;
const BUTTON_SELECT: u8 = 0x04;
const BUTTON_START: u8 = 0x08;
const BUTTON_UP: u8 = 0x10;
const BUTTON_DOWN: u8 = 0x20;
const BUTTON_LEFT: u8 = 0x40;
const BUTTON_RIGHT: u8 = 0x80;

/// Represents an NES controller / gamepad state.
///
/// Button state is stored as a compact bitmask (one bit per button).
/// The serial read logic matches real NES controller behavior:
/// strobing latches the current state, then each read shifts out one bit.
pub struct Gamepad {
    buttons: u8,
    pub strobe: bool,
    index: u8,
    buttons_serial: [u8; 2],
}

impl Default for Gamepad {
    fn default() -> Self {
        Self::new()
    }
}

impl Gamepad {
    pub fn new() -> Self {
        Self {
            buttons: 0,
            strobe: false,
            index: 0,
            buttons_serial: [0; 2],
        }
    }

    pub fn set_a(&mut self, pressed: bool) {
        self.buttons.set_bit(BUTTON_A, pressed);
    }

    pub fn set_b(&mut self, pressed: bool) {
        self.buttons.set_bit(BUTTON_B, pressed);
    }

    pub fn set_select(&mut self, pressed: bool) {
        self.buttons.set_bit(BUTTON_SELECT, pressed);
    }

    pub fn set_start(&mut self, pressed: bool) {
        self.buttons.set_bit(BUTTON_START, pressed);
    }

    pub fn set_up(&mut self, pressed: bool) {
        self.buttons.set_bit(BUTTON_UP, pressed);
    }

    pub fn set_down(&mut self, pressed: bool) {
        self.buttons.set_bit(BUTTON_DOWN, pressed);
    }

    pub fn set_left(&mut self, pressed: bool) {
        self.buttons.set_bit(BUTTON_LEFT, pressed);
    }

    pub fn set_right(&mut self, pressed: bool) {
        self.buttons.set_bit(BUTTON_RIGHT, pressed);
    }

    fn latch(&mut self) {
        self.buttons_serial[0] = self.buttons;
        self.buttons_serial[1] = 0;
    }

    pub fn write(&mut self, val: u8) {
        self.strobe = val & 0x01 != 0;
        if self.strobe {
            self.index = 0;
            self.latch();
        }
    }

    pub fn read(&mut self) -> u8 {
        if self.strobe {
            self.latch();
            return self.buttons_serial[0] & 0x01;
        }
        let bit = if self.index < 8 {
            (self.buttons_serial[0] >> self.index) & 0x01
        } else {
            0x01
        };
        self.index = self.index.wrapping_add(1);
        bit
    }
}

trait BitFlags {
    fn set_bit(&mut self, mask: u8, set: bool);
}

impl BitFlags for u8 {
    #[inline(always)]
    fn set_bit(&mut self, mask: u8, set: bool) {
        *self = if set { *self | mask } else { *self & !mask };
    }
}
