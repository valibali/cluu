//! /bin/irc -- minimal IRC client using libtui.
//!
//! Connects to 10.0.2.2:6667, sends NICK/USER, then enters a TUI
//! where the user can type /join, /nick, /quit, or plain text (PRIVMSG).

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicI32, Ordering};
use libcluu::debug_print;
use libcluu::posix::socket;
use libtui::components::list::List;
use libtui::components::textinput::TextInput;
use libtui::input::KeyEvent;
use libtui::program::Program;
use libtui::{Cmd, Model, View};

const IRC_HOST: u32 = (10 << 24) | (0 << 16) | (2 << 8) | 2;
const IRC_PORT: u16 = 6667;

static IRC_FD: AtomicI32 = AtomicI32::new(-1);

fn irc_send(fd: i32, line: &str) -> bool {
    let data = format!("{}\r\n", line);
    let bytes = data.as_bytes();
    socket::send(fd, bytes.as_ptr(), bytes.len(), 0) == bytes.len() as isize
}

fn irc_recv_lines(fd: i32) -> Vec<String> {
    let mut buf = [0u8; 1024];
    let n = socket::recv(fd, buf.as_mut_ptr(), buf.len(), 0);
    let mut out = Vec::new();
    if n <= 0 {
        return out;
    }
    let text = core::str::from_utf8(&buf[..n as usize]).unwrap_or("");
    for line in text.split('\n') {
        let trimmed = line.trim_end_matches('\r');
        if !trimmed.is_empty() {
            out.push(String::from(trimmed));
        }
    }
    out
}

fn absorb_server_lines(fd: i32, lines: &[String], messages: &mut Vec<String>) {
    for line in lines {
        if let Some(rest) = line.strip_prefix("PING ") {
            let _ = irc_send(fd, &format!("PONG {}", rest));
            messages.push(String::from("< ping/pong"));
        } else {
            messages.push(line.clone());
        }
    }
}

enum IrcMsg {
    Input(char),
    Backspace,
    Send,
    Quit,
}

struct IrcModel {
    fd: i32,
    input: TextInput,
    messages: Vec<String>,
    list: List<String>,
}

impl Model for IrcModel {
    type Msg = IrcMsg;

    fn init() -> (Self, Cmd) {
        let fd = IRC_FD.load(Ordering::SeqCst);
        let messages = vec![String::from("Connected. /join #ch, /nick name, /quit")];
        let list = List::new(messages.clone(), 20);
        (IrcModel {
            fd,
            input: TextInput::with_placeholder("type /join #ch or message"),
            messages,
            list,
        }, Cmd::none())
    }

    fn update(&mut self, msg: IrcMsg) -> Cmd {
        match msg {
            IrcMsg::Input(c) => self.input.insert(c),
            IrcMsg::Backspace => self.input.backspace(),
            IrcMsg::Quit => return Cmd::quit(),
            IrcMsg::Send => {
                let text = self.input.value();
                self.input.clear();
                if self.fd < 0 || text.is_empty() {
                    return Cmd::none();
                }
                if let Some(rest) = text.strip_prefix('/') {
                    let (cmd, arg) = rest.split_once(' ').unwrap_or((rest, ""));
                    match cmd {
                        "join" if !arg.is_empty() => {
                            let _ = irc_send(self.fd, &format!("JOIN {}", arg));
                            self.messages.push(format!("* joined {}", arg));
                        }
                        "nick" if !arg.is_empty() => {
                            let _ = irc_send(self.fd, &format!("NICK {}", arg));
                            self.messages.push(format!("* nick -> {}", arg));
                        }
                        "quit" => {
                            let _ = irc_send(self.fd, "QUIT :bye");
                            return Cmd::quit();
                        }
                        _ => {}
                    }
                } else {
                    let _ = irc_send(self.fd, &format!("PRIVMSG :{}", text));
                    self.messages.push(format!("> {}", text));
                }
                let lines = irc_recv_lines(self.fd);
                absorb_server_lines(self.fd, &lines, &mut self.messages);
                self.list = List::new(self.messages.clone(), 20);
            }
        }
        Cmd::none()
    }

    fn view(&self) -> View {
        let mut v = View::new(80, 24);
        v.write_str(0, 0, "irc - esc:quit  enter:send  /join /nick /quit");
        self.list.render(1, 0, 78, &mut v);
        v.write_str(23, 0, "> ");
        self.input.render(23, 2, &mut v);
        v
    }

    fn from_key(key: KeyEvent) -> Option<IrcMsg> {
        match key {
            KeyEvent::Esc | KeyEvent::Ctrl('c') => Some(IrcMsg::Quit),
            KeyEvent::Enter => Some(IrcMsg::Send),
            KeyEvent::Backspace => Some(IrcMsg::Backspace),
            KeyEvent::Char(c) => Some(IrcMsg::Input(c)),
            _ => None,
        }
    }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let fd = socket::socket(socket::AF_INET, socket::SOCK_STREAM, 0);
    if fd < 0 {
        let _ = debug_print("IRC_FAIL: socket\n");
        return 1;
    }
    if socket::connect(fd, IRC_HOST, IRC_PORT) != 0 {
        let _ = debug_print("IRC_FAIL: connect\n");
        let _ = socket::close_socket(fd);
        return 1;
    }
    let _ = debug_print("IRC_CONNECT_OK\n");
    let _ = irc_send(fd, "NICK cluu-user");
    let _ = irc_send(fd, "USER cluu-user 0 * :CLUU IRC");
    IRC_FD.store(fd, Ordering::SeqCst);

    let mut prog = Program::<IrcModel>::new();
    let result = prog.run();
    let _ = socket::close_socket(fd);
    match result {
        Ok(()) => 0,
        Err(_) => 1,
    }
}
