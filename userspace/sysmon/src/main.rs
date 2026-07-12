//! sysmon — system monitor using libtui.
//!
//! Reads /proc/meminfo, /proc stat files. Shows memory,
//! process count, IPC stats in multiple viewports.

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
use libtui::{Cmd, Model, View};

fn vfs() -> Option<VfsClient> {
    let ep = registry::subscribe_output("vfs", "main").ok()?;
    let cid = registry::control_endpoint();
    Some(VfsClient::new(ep, cid))
}

fn read_proc_file(path: &str) -> String {
    let v = match vfs() {
        Some(v) => v,
        None => return String::new(),
    };
    let file = match v.open(path) {
        Ok(f) => f,
        Err(_) => return String::new(),
    };
    if file.size == 0 {
        let _ = v.close(file);
        return String::new();
    }
    let info = libcluu::boot::process_info();
    let token = info.tokens[libcluu::boot::TOKEN_SPACE];
    let chunk = file.size.min(4096);
    let alloc_size = (chunk + 4095) & !4095;
    let base = match libcluu::vspace::VSPACE.lock().alloc(alloc_size) {
        Ok(b) => b,
        Err(_) => {
            let _ = v.close(file);
            return String::new();
        }
    };
    let mut data: Vec<u8> = Vec::new();
    let mut offset = 0usize;
    while offset < file.size {
        let want = (file.size - offset).min(4096);
        match v.read_grant(file, offset, want, token, base) {
            Ok(grant) => {
                if grant.len == 0 {
                    break;
                }
                let slice = unsafe { core::slice::from_raw_parts(base as *const u8, grant.len) };
                data.extend_from_slice(slice);
                offset += grant.len;
            }
            Err(_) => break,
        }
    }
    let _ = libcluu::vspace::VSPACE.lock().free(base, alloc_size);
    let _ = v.close(file);
    String::from_utf8(data).unwrap_or_default()
}

fn count_processes() -> usize {
    let v = match vfs() {
        Some(v) => v,
        None => return 0,
    };
    match v.readdir("/proc") {
        Ok(entries) => entries
            .into_iter()
            .filter(|e| !e.is_dir && e.name.parse::<usize>().is_ok())
            .count(),
        Err(_) => 0,
    }
}

fn parse_meminfo(content: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    lines.push(String::from("=== Memory Info ==="));
    for line in content.lines() {
        if line.contains("MemTotal")
            || line.contains("MemFree")
            || line.contains("MemAvailable")
            || line.contains("Buffers")
            || line.contains("Cached")
            || line.contains("SwapTotal")
            || line.contains("SwapFree")
        {
            lines.push(line.to_string());
        }
    }
    if lines.len() == 1 {
        lines.push(String::from("(no memory info available)"));
    }
    lines
}

fn gather_stats() -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let meminfo = read_proc_file("/proc/meminfo");
    lines.extend(parse_meminfo(&meminfo));
    lines.push(String::new());
    lines.push(String::from("=== Processes ==="));
    let proc_count = count_processes();
    lines.push(format!("Process count: {}", proc_count));
    lines.push(String::new());
    lines.push(String::from("=== IPC Stats ==="));
    let stat = read_proc_file("/proc/stats");
    if stat.is_empty() {
        lines.push(String::from("(no IPC stats available)"));
    } else {
        for line in stat.lines().take(10) {
            lines.push(line.to_string());
        }
    }
    lines
}

enum SysmonMsg {
    Refresh,
    Up,
    Down,
    Quit,
}

struct SysmonModel {
    vp: Viewport,
}

impl Model for SysmonModel {
    type Msg = SysmonMsg;

    fn init() -> (Self, Cmd) {
        let _ = debug_print("SYSMON_OK\n");
        let stats = gather_stats();
        let mut vp = Viewport::new(22);
        vp.set_lines(stats);
        (SysmonModel { vp }, Cmd::none())
    }

    fn update(&mut self, msg: SysmonMsg) -> Cmd {
        match msg {
            SysmonMsg::Refresh => {
                let stats = gather_stats();
                self.vp.set_lines(stats);
            }
            SysmonMsg::Up => self.vp.scroll_up(1),
            SysmonMsg::Down => self.vp.scroll_down(1),
            SysmonMsg::Quit => return Cmd::quit(),
        }
        Cmd::none()
    }

    fn view(&self) -> View {
        let mut v = View::new(80, 24);
        v.write_str(0, 0, "sysmon - q:quit  r:refresh  arrows:scroll");
        self.vp.render(1, 0, &mut v);
        v
    }

    fn from_key(key: KeyEvent) -> Option<SysmonMsg> {
        match key {
            KeyEvent::Char('q') | KeyEvent::Ctrl('c') => Some(SysmonMsg::Quit),
            KeyEvent::Char('r') => Some(SysmonMsg::Refresh),
            KeyEvent::Arrow(Direction::Up) => Some(SysmonMsg::Up),
            KeyEvent::Arrow(Direction::Down) => Some(SysmonMsg::Down),
            _ => None,
        }
    }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = debug_print("SYSMON_START\n");
    let mut prog = Program::<SysmonModel>::new();
    match prog.run() {
        Ok(()) => 0,
        Err(_) => 1,
    }
}
