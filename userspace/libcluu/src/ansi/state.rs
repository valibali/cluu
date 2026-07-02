use super::event::{Attr, EraseMode, Event};

#[derive(Clone, Copy, PartialEq)]
enum EscState {
    Normal,
    Escape,
    Csi,
}

pub struct Parser {
    state: EscState,
    params: [u16; 4],
    param_count: usize,
    current: u16,
    attr: Attr,
    /// True when the CSI sequence opened with `?` (DEC private mode).
    private: bool,
}

// Color tables copied from userspace/console/src/renderer.rs (lines 28-47).
const ANSI_COLORS: [u32; 8] = [
    0x00000000, // black
    0x00AA0000, // red
    0x0000AA00, // green
    0x00AA5500, // yellow/brown
    0x000000AA, // blue
    0x00AA00AA, // magenta
    0x0000AAAA, // cyan
    0x00AAAAAA, // white/gray
];
const ANSI_BRIGHT_COLORS: [u32; 8] = [
    0x00555555, // bright black
    0x00FF5555, // bright red
    0x0055FF55, // bright green
    0x00FFFF55, // bright yellow
    0x005555FF, // bright blue
    0x00FF55FF, // bright magenta
    0x0055FFFF, // bright cyan
    0x00FFFFFF, // bright white
];

fn ansi_color(i: u16) -> u32 {
    ANSI_COLORS[i as usize % 8]
}

fn ansi_bright_color(i: u16) -> u32 {
    ANSI_BRIGHT_COLORS[i as usize % 8]
}

impl Parser {
    pub fn new() -> Self {
        Self {
            state: EscState::Normal,
            params: [0; 4],
            param_count: 0,
            current: 0,
            attr: Attr::default_attr(),
            private: false,
        }
    }

    pub fn feed<F: FnMut(Event)>(&mut self, bytes: &[u8], mut emit: F) {
        for &b in bytes {
            match self.state {
                EscState::Normal => self.feed_normal(b, &mut emit),
                EscState::Escape => self.feed_escape(b, &mut emit),
                EscState::Csi => self.feed_csi(b, &mut emit),
            }
        }
    }

    fn feed_normal<F: FnMut(Event)>(&mut self, b: u8, emit: &mut F) {
        match b {
            0x1B => self.state = EscState::Escape,
            b'\n' => emit(Event::Newline),
            b'\r' => emit(Event::CarriageReturn),
            0x08 => emit(Event::Backspace),
            b'\t' => emit(Event::Tab),
            0x07 => emit(Event::Bell),
            _ => emit(Event::Print(b)),
        }
    }

    fn feed_escape<F: FnMut(Event)>(&mut self, b: u8, _emit: &mut F) {
        match b {
            b'[' => {
                self.state = EscState::Csi;
                self.param_count = 0;
                self.current = 0;
                self.private = false;
            }
            _ => self.state = EscState::Normal,
        }
    }

    fn feed_csi<F: FnMut(Event)>(&mut self, b: u8, emit: &mut F) {
        match b {
            b'?' if self.param_count == 0 && self.current == 0 => {
                self.private = true;
            }
            b'0'..=b'9' => {
                self.current = self.current.saturating_mul(10).saturating_add((b - b'0') as u16);
            }
            b';' => {
                self.push_param();
            }
            b'A' => {
                self.push_param();
                emit(Event::MoveCursorUp(self.param(0, 1)));
                self.reset();
            }
            b'B' => {
                self.push_param();
                emit(Event::MoveCursorDown(self.param(0, 1)));
                self.reset();
            }
            b'C' => {
                self.push_param();
                emit(Event::MoveCursorRight(self.param(0, 1)));
                self.reset();
            }
            b'D' => {
                self.push_param();
                emit(Event::MoveCursorLeft(self.param(0, 1)));
                self.reset();
            }
            b'H' | b'f' => {
                self.push_param();
                let row = self.param(0, 1);
                let col = self.param(1, 1);
                emit(Event::MoveCursorAbs { row, col });
                self.reset();
            }
            b'K' => {
                self.push_param();
                let mode = match self.param(0, 0) {
                    1 => EraseMode::ToStart,
                    2 => EraseMode::All,
                    _ => EraseMode::ToEnd,
                };
                emit(Event::EraseLine(mode));
                self.reset();
            }
            b'J' => {
                self.push_param();
                let mode = match self.param(0, 0) {
                    1 => EraseMode::ToStart,
                    2 => EraseMode::All,
                    _ => EraseMode::ToEnd,
                };
                emit(Event::EraseDisplay(mode));
                self.reset();
            }
            b'm' => {
                self.push_param();
                self.apply_sgr(emit);
                self.reset();
            }
            b'h' if self.private => {
                self.push_param();
                if self.param(0, 0) == 25 {
                    emit(Event::SetCursorVisible(true));
                }
                self.reset();
            }
            b'l' if self.private => {
                self.push_param();
                if self.param(0, 0) == 25 {
                    emit(Event::SetCursorVisible(false));
                }
                self.reset();
            }
            _ => self.reset(),
        }
    }

    fn push_param(&mut self) {
        if self.param_count < self.params.len() {
            self.params[self.param_count] = self.current;
            self.param_count += 1;
        }
        self.current = 0;
    }

    fn param(&self, idx: usize, default: u16) -> u16 {
        if idx < self.param_count && self.params[idx] != 0 {
            self.params[idx]
        } else {
            default
        }
    }

    fn reset(&mut self) {
        self.state = EscState::Normal;
        self.param_count = 0;
        self.current = 0;
        self.private = false;
    }

    fn apply_sgr<F: FnMut(Event)>(&mut self, emit: &mut F) {
        // If no params were accumulated (bare ESC[m), treat as reset.
        if self.param_count == 0 {
            emit(Event::ResetAttr);
            return;
        }
        for i in 0..self.param_count {
            let p = self.params[i];
            match p {
                0 => emit(Event::ResetAttr),
                1 => {
                    self.attr.bold = true;
                    emit(Event::SetAttr(self.attr));
                }
                30..=37 => {
                    self.attr.fg = ansi_color(p - 30);
                    emit(Event::SetAttr(self.attr));
                }
                40..=47 => {
                    self.attr.bg = ansi_color(p - 40);
                    emit(Event::SetAttr(self.attr));
                }
                90..=97 => {
                    self.attr.fg = ansi_bright_color(p - 90);
                    emit(Event::SetAttr(self.attr));
                }
                100..=107 => {
                    self.attr.bg = ansi_bright_color(p - 100);
                    emit(Event::SetAttr(self.attr));
                }
                _ => {}
            }
        }
    }
}
