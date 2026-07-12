//! glow — markdown viewer using libtui.
//!
//! Parses markdown: headers (#), bold (**), italic (*), code (`),
//! lists (-). Renders with libtui styling (colors, bold).
//! Reads a file argument.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use alloc::string::ToString;
use alloc::vec::Vec;
use libcluu::debug_print;
use libcluu::fs::client::VfsClient;
use libcluu::registry;
use libtui::components::viewport::Viewport;
use libtui::input::{Direction, KeyEvent};
use libtui::program::Program;
use libtui::{
    Cell, Cmd, Model, View, ATTR_BOLD, COLOR_CYAN, COLOR_DEFAULT, COLOR_GREEN,
    COLOR_YELLOW,
};

fn read_file(path: &str) -> Result<String, String> {
    let ep = registry::subscribe_output("vfs", "main")
        .map_err(|_| String::from("vfs unavailable"))?;
    let cid = registry::control_endpoint();
    let vfs = VfsClient::new(ep, cid);
    let file = vfs.open(path).map_err(|e| format!("{:?}", e))?;
    if file.size == 0 {
        let _ = vfs.close(file);
        return Ok(String::new());
    }
    let info = libcluu::boot::process_info();
    let token = info.tokens[libcluu::boot::TOKEN_SPACE];
    let chunk = file.size.min(64 * 1024);
    let alloc_size = (chunk + 4095) & !4095;
    let base = libcluu::vspace::VSPACE
        .lock()
        .alloc(alloc_size)
        .map_err(|_| String::from("alloc failed"))?;
    let mut data: Vec<u8> = Vec::new();
    let mut offset = 0usize;
    while offset < file.size {
        let want = (file.size - offset).min(64 * 1024);
        match vfs.read_grant(file, offset, want, token, base) {
            Ok(grant) => {
                if grant.len == 0 {
                    break;
                }
                let slice = unsafe { core::slice::from_raw_parts(base as *const u8, grant.len) };
                data.extend_from_slice(slice);
                offset += grant.len;
            }
            Err(e) => {
                let _ = libcluu::vspace::VSPACE.lock().free(base, alloc_size);
                let _ = vfs.close(file);
                return Err(format!("{:?}", e));
            }
        }
    }
    let _ = libcluu::vspace::VSPACE.lock().free(base, alloc_size);
    let _ = vfs.close(file);
    String::from_utf8(data).map_err(|_| String::from("invalid UTF-8"))
}

struct MdLine {
    text: String,
    fg: u8,
    attrs: u8,
}

fn parse_markdown(content: &str) -> Vec<MdLine> {
    let mut lines: Vec<MdLine> = Vec::new();
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("# ") {
            lines.push(MdLine { text: rest.to_string(), fg: COLOR_CYAN, attrs: ATTR_BOLD });
        } else if let Some(rest) = line.strip_prefix("## ") {
            lines.push(MdLine { text: rest.to_string(), fg: COLOR_CYAN, attrs: ATTR_BOLD });
        } else if let Some(rest) = line.strip_prefix("### ") {
            lines.push(MdLine { text: rest.to_string(), fg: COLOR_YELLOW, attrs: ATTR_BOLD });
        } else if line.starts_with("- ") {
            let rest = &line[2..];
            let rendered = render_inline(rest);
            lines.push(MdLine { text: format!("  - {}", rendered), fg: COLOR_DEFAULT, attrs: 0 });
        } else if line.starts_with("```") {
            lines.push(MdLine { text: String::from("--- code ---"), fg: COLOR_GREEN, attrs: 0 });
        } else {
            let rendered = render_inline(line);
            lines.push(MdLine { text: rendered, fg: COLOR_DEFAULT, attrs: 0 });
        }
    }
    lines
}

fn render_inline(s: &str) -> String {
    // Strip markdown markers for plain-text rendering.
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'*' {
            i += 2;
            continue;
        }
        if bytes[i] == b'`' || bytes[i] == b'*' {
            i += 1;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

enum GlowMsg {
    Up,
    Down,
    Quit,
}

struct GlowModel {
    vp: Viewport,
    lines: Vec<MdLine>,
    error: Option<String>,
}

impl Model for GlowModel {
    type Msg = GlowMsg;

    fn init() -> (Self, Cmd) {
        let _ = debug_print("GLOW_OK\n");
        let args = libcluu::args::args();
        if args.len() < 2 {
            return (GlowModel {
                vp: Viewport::new(22),
                lines: Vec::new(),
                error: Some(String::from("usage: glow <file>")),
            }, Cmd::none());
        }
        match read_file(&args[1]) {
            Ok(content) => {
                let lines = parse_markdown(&content);
                let plain: Vec<String> = lines.iter().map(|l| l.text.clone()).collect();
                let mut vp = Viewport::new(22);
                vp.set_lines(plain);
                (GlowModel { vp, lines, error: None }, Cmd::none())
            }
            Err(e) => (GlowModel {
                vp: Viewport::new(22),
                lines: Vec::new(),
                error: Some(e),
            }, Cmd::none()),
        }
    }

    fn update(&mut self, msg: GlowMsg) -> Cmd {
        match msg {
            GlowMsg::Up => self.vp.scroll_up(1),
            GlowMsg::Down => self.vp.scroll_down(1),
            GlowMsg::Quit => return Cmd::quit(),
        }
        Cmd::none()
    }

    fn view(&self) -> View {
        let mut v = View::new(80, 24);
        v.write_str(0, 0, "glow - q:quit  arrows:scroll");
        if let Some(ref err) = self.error {
            v.write_str(1, 0, err);
            return v;
        }
        let visible = self.vp.visible_lines();
        let offset = self.vp.offset();
        for (i, _line) in visible.iter().enumerate() {
            let idx = offset + i;
            if idx >= self.lines.len() {
                break;
            }
            let md = &self.lines[idx];
            let row = 1 + i;
            for (col, ch) in md.text.chars().enumerate() {
                if col >= 78 {
                    break;
                }
                let cell = Cell::new(ch).fg(md.fg).attrs(md.attrs);
                v.set(row, col, cell);
            }
        }
        v
    }

    fn from_key(key: KeyEvent) -> Option<GlowMsg> {
        match key {
            KeyEvent::Char('q') | KeyEvent::Ctrl('c') => Some(GlowMsg::Quit),
            KeyEvent::Arrow(Direction::Up) => Some(GlowMsg::Up),
            KeyEvent::Arrow(Direction::Down) => Some(GlowMsg::Down),
            _ => None,
        }
    }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = debug_print("GLOW_START\n");
    let mut prog = Program::<GlowModel>::new();
    match prog.run() {
        Ok(()) => 0,
        Err(_) => 1,
    }
}
