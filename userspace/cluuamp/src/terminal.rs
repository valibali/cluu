//! Terminal size negotiation. cluuamp requires a minimum terminal size
//! (76x29) for its layout. On init, `ensure_terminal_size()` checks the
//! current terminal and requests a resize via ioctl(TIOCSWINSZ) if the
//! terminal is too small. The resize propagates: cluuterm resizes its
//! grid and asks the compositor to grow the window.

use crate::layout;

const TIOCGWINSZ: u32 = 0x5413;
const TIOCSWINSZ: u32 = 0x5414;

#[repr(C)]
struct WinSize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

extern "C" {
    fn _ioctl(fd: i32, req: u32, arg: *mut core::ffi::c_void) -> i32;
}

/// Returns the current terminal size, or (80, 25) as fallback.
pub fn current_size() -> (usize, usize) {
    let mut ws = WinSize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    for _ in 0..10 {
        let ret = unsafe { _ioctl(1, TIOCGWINSZ, &mut ws as *mut _ as *mut core::ffi::c_void) };
        if ret == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
            return (ws.ws_col as usize, ws.ws_row as usize);
        }
        let _ = libcluu::syscall::yield_cpu();
    }
    (80, 25)
}

/// Requests a terminal resize if the current size is smaller than cluuamp's
/// minimum. Returns the terminal size after negotiation (with a brief sleep
/// to let the resize propagate).
pub fn ensure_terminal_size() -> (usize, usize) {
    let min_w = layout::Layout::min_width();
    let min_h = layout::Layout::min_height();

    let (cur_w, cur_h) = current_size();

    if cur_w >= min_w && cur_h >= min_h {
        return (cur_w, cur_h);
    }

    let req_w = cur_w.max(min_w) as u16;
    let req_h = cur_h.max(min_h) as u16;
    let ws = WinSize {
        ws_row: req_h,
        ws_col: req_w,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe { _ioctl(1, TIOCSWINSZ, &ws as *const _ as *mut core::ffi::c_void) };

    libcluu::thread::sleep_ms(100);
    current_size()
}
