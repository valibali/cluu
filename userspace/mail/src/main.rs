//! mail — minimal IMAP client using libtui.
//!
//! Connects to 10.0.2.2:143, sends LOGIN + LIST + FETCH.
//! TUI: message list (left) + message view (right).

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
use libcluu::posix::socket;
use libtui::components::list::List;
use libtui::components::viewport::Viewport;
use libtui::input::{Direction, KeyEvent};
use libtui::program::Program;
use libtui::{Cmd, Model, View};

const IMAP_PORT: u16 = 143;

enum MailMsg {
    Up,
    Down,
    Enter,
    Quit,
}

struct MailModel {
    list: List<String>,
    viewport: Viewport,
    messages: Vec<String>,
    status: String,
}

fn ip_to_u32(a: u8, b: u8, c: u8, d: u8) -> u32 {
    ((a as u32) << 24) | ((b as u32) << 16) | ((c as u32) << 8) | (d as u32)
}

fn imap_connect_and_login() -> (String, Vec<String>) {
    let ip = ip_to_u32(10, 0, 2, 2);
    let fd = socket::socket(socket::AF_INET, socket::SOCK_STREAM, 0);
    if fd < 0 {
        return (String::from("socket failed"), Vec::new());
    }
    if socket::connect(fd, ip, IMAP_PORT) != 0 {
        let _ = socket::close_socket(fd);
        return (String::from("connect failed"), Vec::new());
    }
    let _ = debug_print("MAIL_CONNECT_OK\n");

    let mut greeting = [0u8; 1024];
    let _ = socket::recv(fd, greeting.as_mut_ptr(), greeting.len(), 0);

    let login = b"A1 LOGIN root root\r\n";
    let _ = socket::send(fd, login.as_ptr(), login.len(), 0);
    let mut resp = [0u8; 1024];
    let _ = socket::recv(fd, resp.as_mut_ptr(), resp.len(), 0);

    let list_cmd = b"A2 LIST \"\" \"*\"\r\n";
    let _ = socket::send(fd, list_cmd.as_ptr(), list_cmd.len(), 0);
    let mut list_resp = [0u8; 2048];
    let n = socket::recv(fd, list_resp.as_mut_ptr(), list_resp.len(), 0);

    let mut mailboxes: Vec<String> = Vec::new();
    if n > 0 {
        let text = core::str::from_utf8(&list_resp[..n as usize]).unwrap_or("");
        for line in text.lines() {
            if line.contains("LIST") {
                if let Some(name) = line.split('"').last().filter(|s| !s.is_empty()) {
                    mailboxes.push(name.to_string());
                }
            }
        }
    }
    if mailboxes.is_empty() {
        mailboxes.push(String::from("INBOX"));
    }

    let fetch = b"A3 FETCH 1 BODY[]\r\n";
    let _ = socket::send(fd, fetch.as_ptr(), fetch.len(), 0);
    let mut fetch_resp = [0u8; 2048];
    let _ = socket::recv(fd, fetch_resp.as_mut_ptr(), fetch_resp.len(), 0);

    let _ = socket::close_socket(fd);
    (String::from("connected"), mailboxes)
}

impl Model for MailModel {
    type Msg = MailMsg;

    fn init() -> (Self, Cmd) {
        let (status, mailboxes) = imap_connect_and_login();
        let list = List::new(mailboxes.clone(), 20);
        let messages = mailboxes;
        (MailModel {
            list,
            viewport: Viewport::new(22),
            messages,
            status,
        }, Cmd::none())
    }

    fn update(&mut self, msg: MailMsg) -> Cmd {
        match msg {
            MailMsg::Up => self.list.prev(),
            MailMsg::Down => self.list.next(),
            MailMsg::Enter => {
                if let Some(idx) = self.list.selected_index() {
                    if idx < self.messages.len() {
                        let lines: Vec<String> = format!(
                            "Mailbox: {}\nNo new messages.\n(Fetch not fully implemented)",
                            self.messages[idx]
                        )
                        .lines()
                        .map(String::from)
                        .collect();
                        self.viewport.set_lines(lines);
                    }
                }
            }
            MailMsg::Quit => return Cmd::quit(),
        }
        Cmd::none()
    }

    fn view(&self) -> View {
        let mut v = View::new(80, 24);
        v.write_str(0, 0, "mail - q:quit  enter:open  arrows:navigate");
        v.write_str(1, 0, &self.status);
        self.list.render(2, 0, 38, &mut v);
        self.viewport.render(2, 40, &mut v);
        v
    }

    fn from_key(key: KeyEvent) -> Option<MailMsg> {
        match key {
            KeyEvent::Char('q') | KeyEvent::Ctrl('c') => Some(MailMsg::Quit),
            KeyEvent::Arrow(Direction::Up) => Some(MailMsg::Up),
            KeyEvent::Arrow(Direction::Down) => Some(MailMsg::Down),
            KeyEvent::Enter => Some(MailMsg::Enter),
            _ => None,
        }
    }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = debug_print("MAIL_START\n");
    let mut prog = Program::<MailModel>::new();
    match prog.run() {
        Ok(()) => 0,
        Err(_) => 1,
    }
}
