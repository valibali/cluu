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

/// Sub-mode for CSI K (EraseLine) and CSI J (EraseDisplay).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EraseMode {
    /// CSI 0 — from cursor to end (default if no param)
    ToEnd,
    /// CSI 1 — from start to cursor
    ToStart,
    /// CSI 2 — full line / full display
    All,
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
    EraseLine(EraseMode),
    EraseDisplay(EraseMode),
    SetAttr(Attr),
    ResetAttr,
    Scroll(i16),
    /// DECTCEM (CSI ?25 h / CSI ?25 l). `true` = show, `false` = hide.
    /// TUI apps (top, kilo) emit this on enter/exit to suppress the
    /// blinking text cursor while they own the screen.
    SetCursorVisible(bool),
}
