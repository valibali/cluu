//! Compositor hotkey table.
//!
//! Modifier model — CLUU kbd has no Super bit, so we map "Super" hotkeys
//! onto Ctrl+Alt. Ctrl+Alt+F1..F4 (the VT switch) is consumed by kbd
//! BEFORE the compositor sees the event, so there is no collision with
//! our Ctrl+Alt+X/N/Arrow hotkeys (different scancodes).

const MOD_SHIFT: u8 = 1 << 0;
const MOD_CTRL: u8 = 1 << 1;
const MOD_ALT: u8 = 1 << 2;

/// PS/2 set-1 scancodes (press/release bit already stripped by kbd).
pub const SCAN_TAB: u8 = 0x0F;
pub const SCAN_X: u8 = 0x2D;
pub const SCAN_N: u8 = 0x31;
pub const SCAN_ESC: u8 = 0x01;

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
    SpawnCluuterm,
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

    // Ctrl+Alt+X: request close of focused window. Ctrl+Alt+Q is reserved
    // by QEMU as a host-level quit shortcut.
    if supr && scancode == SCAN_X {
        return Some(Hotkey::CloseRequest);
    }

    // Ctrl+Alt+N: spawn demo window.
    if supr && scancode == SCAN_N {
        return Some(Hotkey::SpawnCluuterm);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_alt_x_requests_close() {
        assert!(matches!(
            match_hotkey(MOD_CTRL | MOD_ALT, SCAN_X, EXT_NONE),
            Some(Hotkey::CloseRequest)
        ));
    }

    #[test]
    fn ctrl_alt_q_is_not_a_compositor_hotkey() {
        assert!(match_hotkey(MOD_CTRL | MOD_ALT, 0x10, EXT_NONE).is_none());
    }
}
