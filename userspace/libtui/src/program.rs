extern crate alloc;

use crate::diff::ScreenBuffer;
use crate::input::{decode, StdinReader};
use crate::render::{cursor_move, Renderer, CLEAR_SCREEN, CURSOR_HOME, RESET_SGR};
use crate::{Cmd, Model};
use libcluu::posix::tty::{enter_raw, restore, SavedTty};
use libcluu::{stdout, Error, Result};

extern "C" {
    fn _ioctl(fd: i32, request: usize, argp: *mut core::ffi::c_void) -> i32;
}

#[repr(C)]
struct WinSize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

const TIOCGWINSZ: usize = 0x5413;

fn terminal_size() -> (usize, usize) {
    let mut ws = WinSize { ws_row: 0, ws_col: 0, ws_xpixel: 0, ws_ypixel: 0 };
    let rc = unsafe { _ioctl(1, TIOCGWINSZ, &mut ws as *mut _ as *mut core::ffi::c_void) };
    if rc == 0 && ws.ws_col > 0 {
        (ws.ws_col as usize, ws.ws_row.max(1) as usize)
    } else {
        (80, 24)
    }
}

pub struct Program<M: Model> {
    model: M,
    init_cmd: Cmd,
    reader: StdinReader,
    renderer: Renderer,
    prev_buffer: ScreenBuffer,
}

impl<M: Model> Program<M> {
    pub fn new() -> Self {
        let (model, init_cmd) = M::init();
        Program {
            model,
            init_cmd,
            reader: StdinReader::new(),
            renderer: Renderer::new(),
            prev_buffer: ScreenBuffer::new(0, 0),
        }
    }

    pub fn run(&mut self) -> Result<()> {
        let tty_ep = stdout();
        if tty_ep == 0 {
            return Err(Error::InvalidState);
        }
        let saved_tty = enter_raw(tty_ep)?;

        self.renderer.enter_alt_screen();
        self.renderer.clear_screen();

        if self.init_cmd.should_quit() {
            self.cleanup(saved_tty)?;
            return Ok(());
        }

        loop {
            // Detect terminal resize by querying console dims each iteration.
            // On change: clear screen, reset prev_buffer, notify model.
            let (cur_w, cur_h) = terminal_size();
            if cur_w != self.prev_buffer.width() || cur_h != self.prev_buffer.height() {
                self.renderer.clear_screen();
                self.prev_buffer = ScreenBuffer::new(0, 0);
                self.model.on_resize();
            }

            let view = self.model.view();
            let mut new_buffer = ScreenBuffer::new(view.width, view.height);
            for (i, cell) in view.cells.iter().enumerate() {
                let row = if view.width > 0 { i / view.width } else { 0 };
                let col = if view.width > 0 { i % view.width } else { 0 };
                new_buffer.set(row, col, *cell);
            }
            let diff = new_buffer.diff_render(&self.prev_buffer);
            if !diff.is_empty() {
                self.renderer.write(diff.as_bytes());
            }
            self.prev_buffer = new_buffer;

            if let Some((row, col)) = self.model.cursor_position() {
                self.renderer.write(&cursor_move(row + 1, col + 1));
            }

            if !self.reader.wait_for_data(200) {
                continue;
            }
            let Some(key) = decode(&mut self.reader) else {
                continue;
            };
            let Some(msg) = M::from_key(key) else {
                continue;
            };
            let cmd = self.model.update(msg);
            if cmd.should_quit() {
                break;
            }
        }

        self.cleanup(saved_tty)
    }

    fn cleanup(&self, saved_tty: SavedTty) -> Result<()> {
        self.renderer.write(RESET_SGR);
        self.renderer.write(CLEAR_SCREEN);
        self.renderer.write(CURSOR_HOME);
        self.renderer.exit_alt_screen();
        restore(saved_tty)
    }
}
