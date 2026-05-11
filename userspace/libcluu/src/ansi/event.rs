#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attr {
    pub fg: u32,
    pub bg: u32,
    pub bold: bool,
}

impl Attr {
    pub const fn default_attr() -> Self {
        Self { fg: 0x00FFFFFF, bg: 0x00000000, bold: false }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Print(u8),
    Newline,
    CarriageReturn,
    Backspace,
    Tab,
    Bell,
    MoveCursorUp(u16),
    MoveCursorDown(u16),
    MoveCursorRight(u16),
    MoveCursorLeft(u16),
    MoveCursorAbs { row: u16, col: u16 },
    EraseLine,
    EraseDisplay,
    SetAttr(Attr),
    ResetAttr,
    Scroll(i16),
}
