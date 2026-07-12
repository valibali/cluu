//! Diff — line-by-line diff viewer using libtui viewport.

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

enum DiffMsg {
    Up,
    Down,
    Quit,
}

struct DiffModel {
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

fn split_lines(data: &[u8]) -> Vec<String> {
    let text = core::str::from_utf8(data).unwrap_or("");
    text.lines().map(String::from).collect()
}

fn build_diff(a: &[String], b: &[String]) -> Vec<String> {
    let max = a.len().max(b.len());
    let mut out: Vec<String> = Vec::new();
    for i in 0..max {
        let ae = i < a.len();
        let be = i < b.len();
        if ae && be {
            if a[i] == b[i] {
                out.push(format!("  {}", a[i]));
            } else {
                out.push(format!("- {}", a[i]));
                out.push(format!("+ {}", b[i]));
            }
        } else if ae {
            out.push(format!("- {}", a[i]));
        } else {
            out.push(format!("+ {}", b[i]));
        }
    }
    if out.is_empty() {
        out.push(String::from("(files are identical)"));
    }
    out
}

impl Model for DiffModel {
    type Msg = DiffMsg;

    fn init() -> (Self, Cmd) {
        if INIT_DONE.swap(true, Ordering::SeqCst) {
            return (DiffModel { vp: Viewport::new(22), error: None }, Cmd::none());
        }
        let args = libcluu::args::args();
        if args.len() < 3 {
            return (
                DiffModel {
                    vp: Viewport::new(22),
                    error: Some(String::from("usage: diff <file1> <file2>")),
                },
                Cmd::none(),
            );
        }
        let a = match read_file(&args[1]) {
            Ok(d) => d,
            Err(e) => {
                return (
                    DiffModel {
                        vp: Viewport::new(22),
                        error: Some(format!("file1: {}", e)),
                    },
                    Cmd::none(),
                )
            }
        };
        let b = match read_file(&args[2]) {
            Ok(d) => d,
            Err(e) => {
                return (
                    DiffModel {
                        vp: Viewport::new(22),
                        error: Some(format!("file2: {}", e)),
                    },
                    Cmd::none(),
                )
            }
        };
        let lines = build_diff(&split_lines(&a), &split_lines(&b));
        let mut vp = Viewport::new(22);
        vp.set_lines(lines);
        (DiffModel { vp, error: None }, Cmd::none())
    }

    fn update(&mut self, msg: DiffMsg) -> Cmd {
        match msg {
            DiffMsg::Up => self.vp.scroll_up(1),
            DiffMsg::Down => self.vp.scroll_down(1),
            DiffMsg::Quit => return Cmd::quit(),
        }
        Cmd::none()
    }

    fn view(&self) -> View {
        let mut v = View::new(80, 24);
        v.write_str(0, 0, "diff - q:quit  arrows:scroll  (- =first + =second)");
        if let Some(ref err) = self.error {
            v.write_str(1, 0, err);
            return v;
        }
        self.vp.render(1, 0, &mut v);
        v
    }

    fn from_key(key: KeyEvent) -> Option<DiffMsg> {
        match key {
            KeyEvent::Char('q') | KeyEvent::Ctrl('c') => Some(DiffMsg::Quit),
            KeyEvent::Arrow(Direction::Up) => Some(DiffMsg::Up),
            KeyEvent::Arrow(Direction::Down) => Some(DiffMsg::Down),
            _ => None,
        }
    }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = debug_print("DIFF_OK\n");
    let mut prog = Program::<DiffModel>::new();
    match prog.run() {
        Ok(()) => 0,
        Err(_) => 1,
    }
}
