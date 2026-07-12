//! File manager — dual-pane file browser using libtui.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use libcluu::debug_print;
use libcluu::fs::client::{VfsClient, VfsDirEntry};
use libcluu::posix::{current_dir_string, resolve_path};
use libcluu::registry;
use libtui::components::list::List;
use libtui::input::{Direction, KeyEvent};
use libtui::{Cmd, Model, View};
use libtui::program::Program;

enum FmMsg {
    Up,
    Down,
    Enter,
    UpDir,
    Quit,
}

struct FmModel {
    cwd: String,
    entries: Vec<VfsDirEntry>,
    list: List<String>,
    info: String,
}

fn list_dir(path: &str) -> Vec<VfsDirEntry> {
    let ep = match registry::lookup_service("vfs:main") {
        Some(e) => e,
        None => return Vec::new(),
    };
    let cid = registry::control_endpoint();
    let vfs = VfsClient::new(ep, cid);
    match vfs.readdir(path) {
        Ok(mut e) => {
            e.sort_by(|a, b| {
                let a_dir = a.is_dir;
                let b_dir = b.is_dir;
                b_dir.cmp(&a_dir).then(a.name.cmp(&b.name))
            });
            e
        }
        Err(_) => Vec::new(),
    }
}

fn entry_label(e: &VfsDirEntry) -> String {
    if e.is_dir {
        format!("[{}] ", e.name)
    } else {
        e.name.clone()
    }
}

fn file_info(cwd: &str, entry: &VfsDirEntry) -> String {
    let full = if cwd.ends_with('/') {
        format!("{}{}", cwd, entry.name)
    } else {
        format!("{}/{}", cwd, entry.name)
    };
    let kind = if entry.is_dir { "directory" } else { "file" };
    format!(
        "Path: {}\nName: {}\nType: {}\nSize: {}\nMode: {:o}",
        full, entry.name, kind, entry.stat.size, entry.stat.mode
    )
}

impl Model for FmModel {
    type Msg = FmMsg;

    fn init() -> (Self, Cmd) {
        let cwd = current_dir_string();
        let entries = list_dir(&cwd);
        let labels: Vec<String> = entries.iter().map(entry_label).collect();
        let list = List::new(labels, 20);
        let info = entries.first().map(|e| file_info(&cwd, e)).unwrap_or_default();
        (FmModel { cwd, entries, list, info }, Cmd::none())
    }

    fn update(&mut self, msg: FmMsg) -> Cmd {
        match msg {
            FmMsg::Up => self.list.prev(),
            FmMsg::Down => self.list.next(),
            FmMsg::Enter => {
                if let Some(idx) = self.list.selected_index() {
                    if idx < self.entries.len() && self.entries[idx].is_dir {
                        let name = self.entries[idx].name.clone();
                        let new_path = if self.cwd.ends_with('/') {
                            format!("{}{}", self.cwd, name)
                        } else {
                            format!("{}/{}", self.cwd, name)
                        };
                        let resolved = resolve_path(&new_path);
                        let entries = list_dir(&resolved);
                        let labels: Vec<String> = entries.iter().map(entry_label).collect();
                        self.cwd = resolved;
                        self.entries = entries;
                        self.list = List::new(labels, 20);
                        self.info = self.entries.first()
                            .map(|e| file_info(&self.cwd, e))
                            .unwrap_or_default();
                    }
                }
            }
            FmMsg::UpDir => {
                let parent = resolve_path(&format!("{}/..", self.cwd.trim_end_matches('/')));
                let entries = list_dir(&parent);
                let labels: Vec<String> = entries.iter().map(entry_label).collect();
                self.cwd = parent;
                self.entries = entries;
                self.list = List::new(labels, 20);
                self.info = self.entries.first()
                    .map(|e| file_info(&self.cwd, e))
                    .unwrap_or_default();
            }
            FmMsg::Quit => return Cmd::quit(),
        }
        if let Some(idx) = self.list.selected_index() {
            if idx < self.entries.len() {
                self.info = file_info(&self.cwd, &self.entries[idx]);
            }
        }
        Cmd::none()
    }

    fn view(&self) -> View {
        let mut v = View::new(80, 24);
        v.write_str(0, 0, "fm - q:quit  enter:open  backspace:up");
        v.write_str(1, 0, &self.cwd);
        self.list.render(2, 0, 38, &mut v);
        for (i, line) in self.info.lines().enumerate() {
            v.write_str(2 + i, 40, line);
        }
        v
    }

    fn from_key(key: KeyEvent) -> Option<FmMsg> {
        match key {
            KeyEvent::Char('q') | KeyEvent::Ctrl('c') => Some(FmMsg::Quit),
            KeyEvent::Arrow(Direction::Up) => Some(FmMsg::Up),
            KeyEvent::Arrow(Direction::Down) => Some(FmMsg::Down),
            KeyEvent::Enter => Some(FmMsg::Enter),
            KeyEvent::Backspace => Some(FmMsg::UpDir),
            _ => None,
        }
    }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = debug_print("FM_OK\n");
    let mut prog = Program::<FmModel>::new();
    match prog.run() {
        Ok(()) => 0,
        Err(_) => 1,
    }
}
