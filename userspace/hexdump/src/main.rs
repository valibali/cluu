//! Hexdump — hex viewer using libtui viewport.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
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

enum HexMsg {
    Up,
    Down,
    Quit,
}

struct HexModel {
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

fn hex_line(offset: usize, chunk: &[u8]) -> String {
    let mut hex_part = String::new();
    for i in 0..16 {
        if i > 0 && i % 8 == 0 {
            hex_part.push(' ');
        }
        if i < chunk.len() {
            hex_part.push_str(&format!("{:02x} ", chunk[i]));
        } else {
            hex_part.push_str("   ");
        }
    }
    let mut ascii_part = String::new();
    for &b in chunk {
        if (0x20..=0x7e).contains(&b) {
            ascii_part.push(b as char);
        } else {
            ascii_part.push('.');
        }
    }
    format!("{:08x}  {} {}", offset, hex_part, ascii_part)
}

fn build_hex_lines(data: &[u8]) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut offset = 0;
    while offset < data.len() {
        let end = (offset + 16).min(data.len());
        lines.push(hex_line(offset, &data[offset..end]));
        offset += 16;
    }
    if lines.is_empty() {
        lines.push(String::from("00000000  (empty file)"));
    }
    lines
}

impl Model for HexModel {
    type Msg = HexMsg;

    fn init() -> (Self, Cmd) {
        if INIT_DONE.swap(true, Ordering::SeqCst) {
            return (HexModel { vp: Viewport::new(22), error: None }, Cmd::none());
        }
        let args = libcluu::args::args();
        if args.len() < 2 {
            return (
                HexModel {
                    vp: Viewport::new(22),
                    error: Some(String::from("usage: hexdump <file>")),
                },
                Cmd::none(),
            );
        }
        match read_file(&args[1]) {
            Ok(data) => {
                let lines = build_hex_lines(&data);
                let mut vp = Viewport::new(22);
                vp.set_lines(lines);
                (HexModel { vp, error: None }, Cmd::none())
            }
            Err(e) => (
                HexModel {
                    vp: Viewport::new(22),
                    error: Some(String::from(e)),
                },
                Cmd::none(),
            ),
        }
    }

    fn update(&mut self, msg: HexMsg) -> Cmd {
        match msg {
            HexMsg::Up => self.vp.scroll_up(1),
            HexMsg::Down => self.vp.scroll_down(1),
            HexMsg::Quit => return Cmd::quit(),
        }
        Cmd::none()
    }

    fn view(&self) -> View {
        let mut v = View::new(80, 24);
        v.write_str(0, 0, "hexdump - q:quit  arrows:scroll");
        if let Some(ref err) = self.error {
            v.write_str(1, 0, err);
            return v;
        }
        self.vp.render(1, 0, &mut v);
        v
    }

    fn from_key(key: KeyEvent) -> Option<HexMsg> {
        match key {
            KeyEvent::Char('q') | KeyEvent::Ctrl('c') => Some(HexMsg::Quit),
            KeyEvent::Arrow(Direction::Up) => Some(HexMsg::Up),
            KeyEvent::Arrow(Direction::Down) => Some(HexMsg::Down),
            _ => None,
        }
    }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = debug_print("HEXDUMP_OK\n");
    let mut prog = Program::<HexModel>::new();
    match prog.run() {
        Ok(()) => 0,
        Err(_) => 1,
    }
}
