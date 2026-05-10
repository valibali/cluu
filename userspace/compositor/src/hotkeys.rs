//! Compositor hotkey table.
//!
//! Modifier model — CLUU kbd has no Super bit, so we map "Super" hotkeys
//! onto Ctrl+Alt. Ctrl+Alt+F1..F4 (the VT switch) is consumed by kbd
//! BEFORE the compositor sees the event, so there is no collision with
//! our Ctrl+Alt+Q/N/Arrow hotkeys (different scancodes).

const MOD_SHIFT: u8 = 1 << 0;
const MOD_CTRL: u8 = 1 << 1;
const MOD_ALT: u8 = 1 << 2;

/// PS/2 set-1 scancodes (press/release bit already stripped by kbd).
pub const SCAN_TAB: u8 = 0x0F;
pub const SCAN_Q: u8 = 0x10;
pub const SCAN_N: u8 = 0x31;

/// Extended-key codes (kbd's own KEY_*).
pub const EXT_NONE: u8 = 0;
pub const EXT_UP: u8 = 1;
pub const EXT_DOWN: u8 = 2;
pub const EXT_LEFT: u8 = 3;
pub const EXT_RIGHT: u8 = 4;

#[derive(Debug)]
pub enum Hotkey {
    FocusNext,
    FocusPrev,
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    ResizeLeft,
    ResizeRight,
    ResizeUp,
    ResizeDown,
    CloseRequest,
    SpawnDemo,
}

/// Decode a (modifiers, scancode, extended) tuple into a Hotkey, if any.
///
/// Caller should only invoke this on key-down events; kbd already filters
/// release events out before forwarding.
pub fn match_hotkey(mods: u8, scancode: u8, extended: u8) -> Option<Hotkey> {
    let alt   = (mods & MOD_ALT)   != 0;
    let shift = (mods & MOD_SHIFT) != 0;
    let ctrl  = (mods & MOD_CTRL)  != 0;
    // Super-equivalent: Ctrl+Alt (CLUU kbd has no MOD_SUPER bit).
    let supr  = ctrl && alt;

    // Alt+Tab / Alt+Shift+Tab: focus cycle.
    if alt && !ctrl && scancode == SCAN_TAB {
        return Some(if shift { Hotkey::FocusPrev } else { Hotkey::FocusNext });
    }

    // Ctrl+Alt+<arrow>       — move focused window.
    // Ctrl+Alt+Shift+<arrow> — resize focused window.
    if supr && extended != EXT_NONE {
        return Some(match (shift, extended) {
            (false, EXT_LEFT)  => Hotkey::MoveLeft,
            (false, EXT_RIGHT) => Hotkey::MoveRight,
            (false, EXT_UP)    => Hotkey::MoveUp,
            (false, EXT_DOWN)  => Hotkey::MoveDown,
            (true,  EXT_LEFT)  => Hotkey::ResizeLeft,
            (true,  EXT_RIGHT) => Hotkey::ResizeRight,
            (true,  EXT_UP)    => Hotkey::ResizeUp,
            (true,  EXT_DOWN)  => Hotkey::ResizeDown,
            _ => return None,
        });
    }

    // Ctrl+Alt+Q: request close of focused window.
    if supr && scancode == SCAN_Q {
        return Some(Hotkey::CloseRequest);
    }

    // Ctrl+Alt+N: spawn demo window.
    if supr && scancode == SCAN_N {
        return Some(Hotkey::SpawnDemo);
    }

    None
}
