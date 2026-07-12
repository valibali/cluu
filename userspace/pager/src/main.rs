//! Pager — less-like pager using libtui viewport.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};
use libcluu::debug_print;
use libcluu::posix::{_close, _open, _read, O_RDONLY};
use libtui::components::viewport::Viewport;
use libtui::input::{Direction, KeyEvent};
use libtui::{Cmd, Model, View};
use libtui::program::Program;

static INIT_DONE: AtomicBool = AtomicBool::new(false);

enum PagerMsg {
    Up,
    Down,
    PageUp,
    PageDown,
    Quit,
}

struct PagerModel {
    vp: Viewport,
    error: Option<String>,
}

fn read_file(path: &str) -> Result<Vec<u8>, &'static str> {
    let mut p = String::from(path);
    p.push('\0');
    let fd = _open(p.as_ptr() as *const i8, O_RDONLY, 0);
    if fd < 0 {
        return Err("open failed");
    }
    let mut data: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = _read(fd, buf.as_mut_ptr() as *mut _, buf.len());
        if n <= 0 {
            break;
        }
        data.extend_from_slice(&buf[..n as usize]);
    }
    _close(fd);
    Ok(data)
}

fn read_stdin() -> Vec<u8> {
    let mut data: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = _read(0, buf.as_mut_ptr() as *mut _, buf.len());
        if n <= 0 {
            break;
        }
        data.extend_from_slice(&buf[..n as usize]);
    }
    data
}

impl Model for PagerModel {
    type Msg = PagerMsg;

    fn init() -> (Self, Cmd) {
        if INIT_DONE.swap(true, Ordering::SeqCst) {
            return (PagerModel { vp: Viewport::new(22), error: None }, Cmd::none());
        }
        let args = libcluu::args::args();
        let data = if args.len() > 1 {
            match read_file(&args[1]) {
                Ok(d) => d,
                Err(e) => {
                    return (
                        PagerModel {
                            vp: Viewport::new(22),
                            error: Some(String::from(e)),
                        },
                        Cmd::none(),
                    )
                }
            }
        } else {
            read_stdin()
        };
        let text = core::str::from_utf8(&data).unwrap_or("");
        let lines: Vec<String> = text.lines().map(String::from).collect();
        let mut vp = Viewport::new(22);
        vp.set_lines(lines);
        (PagerModel { vp, error: None }, Cmd::none())
    }

    fn update(&mut self, msg: PagerMsg) -> Cmd {
        match msg {
            PagerMsg::Up => self.vp.scroll_up(1),
            PagerMsg::Down => self.vp.scroll_down(1),
            PagerMsg::PageUp => self.vp.scroll_up(22),
            PagerMsg::PageDown => self.vp.scroll_down(22),
            PagerMsg::Quit => return Cmd::quit(),
        }
        Cmd::none()
    }

    fn view(&self) -> View {
        let mut v = View::new(80, 24);
        v.write_str(0, 0, "pager - q:quit  arrows:scroll  pgup/pgdn:page");
        if let Some(ref err) = self.error {
            v.write_str(1, 0, err);
            return v;
        }
        self.vp.render(1, 0, &mut v);
        v
    }

    fn from_key(key: KeyEvent) -> Option<PagerMsg> {
        match key {
            KeyEvent::Char('q') | KeyEvent::Ctrl('c') => Some(PagerMsg::Quit),
            KeyEvent::Arrow(Direction::Up) => Some(PagerMsg::Up),
            KeyEvent::Arrow(Direction::Down) => Some(PagerMsg::Down),
            KeyEvent::PageUp => Some(PagerMsg::PageUp),
            KeyEvent::PageDown => Some(PagerMsg::PageDown),
            _ => None,
        }
    }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = debug_print("PAGER_OK\n");
    let mut prog = Program::<PagerModel>::new();
    match prog.run() {
        Ok(()) => 0,
        Err(_) => 1,
    }
}
