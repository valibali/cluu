//! notes — local markdown notes manager using libtui.
//!
//! List + view + create notes stored in /home/root/notes/.
//! Commands: n=new, d=delete, Enter=open, q=quit.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use libcluu::debug_print;
use libcluu::fs::client::VfsClient;
use libcluu::registry;
use libtui::components::list::List;
use libtui::components::viewport::Viewport;
use libtui::input::{Direction, KeyEvent};
use libtui::program::Program;
use libtui::{Cmd, Model, View};

const NOTES_DIR: &str = "/home/root/notes";

fn vfs() -> Option<VfsClient> {
    let ep = registry::subscribe_output("vfs", "main").ok()?;
    let cid = registry::control_endpoint();
    Some(VfsClient::new(ep, cid))
}

fn list_notes() -> Vec<String> {
    let v = match vfs() {
        Some(v) => v,
        None => return Vec::new(),
    };
    match v.readdir(NOTES_DIR) {
        Ok(entries) => {
            let mut names: Vec<String> = entries
                .into_iter()
                .filter(|e| !e.is_dir && e.name.ends_with(".md"))
                .map(|e| e.name)
                .collect();
            names.sort();
            names
        }
        Err(_) => Vec::new(),
    }
}

fn read_note(name: &str) -> String {
    let v = match vfs() {
        Some(v) => v,
        None => return String::from("(vfs unavailable)"),
    };
    let path = format!("{}/{}", NOTES_DIR, name);
    match v.open(&path) {
        Ok(file) => {
            let info = libcluu::boot::process_info();
            let token = info.tokens[libcluu::boot::TOKEN_SPACE];
            let chunk = file.size.min(64 * 1024);
            let alloc_size = (chunk + 4095) & !4095;
            let base = match libcluu::vspace::VSPACE.lock().alloc(alloc_size) {
                Ok(b) => b,
                Err(_) => return String::from("(alloc failed)"),
            };
            let mut data: Vec<u8> = Vec::new();
            let mut offset = 0usize;
            while offset < file.size {
                let want = (file.size - offset).min(64 * 1024);
                match v.read_grant(file, offset, want, token, base) {
                    Ok(grant) => {
                        if grant.len == 0 {
                            break;
                        }
                        let slice = unsafe {
                            core::slice::from_raw_parts(base as *const u8, grant.len)
                        };
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
        Err(_) => String::from("(open failed)"),
    }
}

fn create_note() -> String {
    let v = match vfs() {
        Some(v) => v,
        None => return String::from("(vfs unavailable)"),
    };
    let mut idx = 1u32;
    loop {
        let name = format!("note{}.md", idx);
        let path = format!("{}/{}", NOTES_DIR, name);
        match v.open(&path) {
            Ok(f) => {
                let _ = v.close(f);
                idx += 1;
            }
            Err(_) => {
                let header = b"# New Note\n\n";
                if let Ok(file) = v.open_with(&path, 1, 0o644) {
                    let _ = v.write(file, 0, header);
                    let _ = v.close(file);
                }
                return name;
            }
        }
    }
}

fn delete_note(name: &str) -> bool {
    let v = match vfs() {
        Some(v) => v,
        None => return false,
    };
    let path = format!("{}/{}", NOTES_DIR, name);
    v.unlink(&path).is_ok()
}

enum NotesMsg {
    Up,
    Down,
    Enter,
    New,
    Delete,
    Quit,
}

struct NotesModel {
    list: List<String>,
    viewport: Viewport,
    status: String,
}

impl Model for NotesModel {
    type Msg = NotesMsg;

    fn init() -> (Self, Cmd) {
        let _ = debug_print("NOTES_OK\n");
        let names = list_notes();
        let count = names.len();
        let list = List::new(names, 20);
        (NotesModel {
            list,
            viewport: Viewport::new(22),
            status: format!("{} notes in {}", count, NOTES_DIR),
        }, Cmd::none())
    }

    fn update(&mut self, msg: NotesMsg) -> Cmd {
        match msg {
            NotesMsg::Up => self.list.prev(),
            NotesMsg::Down => self.list.next(),
            NotesMsg::Enter => {
                if let Some(name) = self.list.selected() {
                    let content = read_note(name);
                    let lines: Vec<String> = content.lines().map(String::from).collect();
                    self.viewport.set_lines(lines);
                    self.status = format!("viewing: {}", name);
                }
            }
            NotesMsg::New => {
                let name = create_note();
                let names = list_notes();
                self.list = List::new(names, 20);
                self.status = format!("created: {}", name);
            }
            NotesMsg::Delete => {
                if let Some(name) = self.list.selected().cloned() {
                    if delete_note(&name) {
                        let names = list_notes();
                        self.list = List::new(names, 20);
                        self.status = format!("deleted: {}", name);
                    }
                }
            }
            NotesMsg::Quit => return Cmd::quit(),
        }
        Cmd::none()
    }

    fn view(&self) -> View {
        let mut v = View::new(80, 24);
        v.write_str(0, 0, "notes - q:quit  n:new  d:delete  enter:open");
        v.write_str(1, 0, &self.status);
        self.list.render(2, 0, 38, &mut v);
        self.viewport.render(2, 40, &mut v);
        v
    }

    fn from_key(key: KeyEvent) -> Option<NotesMsg> {
        match key {
            KeyEvent::Char('q') | KeyEvent::Ctrl('c') => Some(NotesMsg::Quit),
            KeyEvent::Char('n') => Some(NotesMsg::New),
            KeyEvent::Char('d') => Some(NotesMsg::Delete),
            KeyEvent::Arrow(Direction::Up) => Some(NotesMsg::Up),
            KeyEvent::Arrow(Direction::Down) => Some(NotesMsg::Down),
            KeyEvent::Enter => Some(NotesMsg::Enter),
            _ => None,
        }
    }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = debug_print("NOTES_START\n");
    let mut prog = Program::<NotesModel>::new();
    match prog.run() {
        Ok(()) => 0,
        Err(_) => 1,
    }
}
