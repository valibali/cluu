//! feed — minimal RSS/Atom reader using libtui.
//!
//! HTTP GET a feed from 10.0.2.2:9876, parse <item> elements,
//! extract title/link/description. TUI: feed list + article viewport.

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

fn ip_to_u32(a: u8, b: u8, c: u8, d: u8) -> u32 {
    ((a as u32) << 24) | ((b as u32) << 16) | ((c as u32) << 8) | (d as u32)
}

struct FeedItem {
    title: String,
    link: String,
    description: String,
}

impl core::fmt::Display for FeedItem {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.title)
    }
}

fn extract_tag(content: &str, tag: &str) -> Option<String> {
    let open = format!("<{}", tag);
    let close = format!("</{}>", tag);
    let start = content.find(&open)?;
    let after_open = &content[start..];
    let gt = after_open.find('>')?;
    let rest = &after_open[gt + 1..];
    let end = rest.find(&close)?;
    Some(rest[..end].trim().to_string())
}

fn parse_rss(xml: &str) -> Vec<FeedItem> {
    let mut items: Vec<FeedItem> = Vec::new();
    let mut remaining = xml;
    while let Some(start) = remaining.find("<item") {
        let after = &remaining[start..];
        let gt = match after.find('>') {
            Some(p) => p,
            None => break,
        };
        let rest = &after[gt + 1..];
        let end = match rest.find("</item>") {
            Some(p) => p,
            None => break,
        };
        let block = &rest[..end];
        let title = extract_tag(block, "title").unwrap_or_default();
        let link = extract_tag(block, "link").unwrap_or_default();
        let description = extract_tag(block, "description").unwrap_or_default();
        items.push(FeedItem { title, link, description });
        remaining = &rest[end + 7..];
    }
    items
}

fn fetch_feed() -> Result<String, String> {
    let ip = ip_to_u32(10, 0, 2, 2);
    let fd = socket::socket(socket::AF_INET, socket::SOCK_STREAM, 0);
    if fd < 0 {
        return Err(String::from("socket failed"));
    }
    if socket::connect(fd, ip, 9876) != 0 {
        let _ = socket::close_socket(fd);
        return Err(String::from("connect failed"));
    }
    let request = b"GET /feed.xml HTTP/1.1\r\nHost: 10.0.2.2:9876\r\nConnection: close\r\n\r\n";
    let _ = socket::send(fd, request.as_ptr(), request.len(), 0);

    let mut data: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = socket::recv(fd, buf.as_mut_ptr(), buf.len(), 0);
        if n <= 0 {
            break;
        }
        data.extend_from_slice(&buf[..n as usize]);
    }
    let _ = socket::close_socket(fd);

    let raw = core::str::from_utf8(&data).map_err(|_| String::from("utf8 error"))?;
    let body_start = raw.find("\r\n\r\n").unwrap_or(0);
    Ok(raw[body_start..].to_string())
}

enum FeedMsg {
    Up,
    Down,
    Enter,
    Quit,
}

struct FeedModel {
    list: List<String>,
    items: Vec<FeedItem>,
    viewport: Viewport,
    status: String,
}

impl Model for FeedModel {
    type Msg = FeedMsg;

    fn init() -> (Self, Cmd) {
        let (status, items) = match fetch_feed() {
            Ok(xml) => {
                let parsed = parse_rss(&xml);
                let _ = debug_print("FEED_OK\n");
                (String::from("ok"), parsed)
            }
            Err(e) => (e, Vec::new()),
        };
        let labels: Vec<String> = items.iter().map(|i| i.title.clone()).collect();
        let list = List::new(labels, 20);
        (FeedModel {
            list,
            items,
            viewport: Viewport::new(22),
            status,
        }, Cmd::none())
    }

    fn update(&mut self, msg: FeedMsg) -> Cmd {
        match msg {
            FeedMsg::Up => self.list.prev(),
            FeedMsg::Down => self.list.next(),
            FeedMsg::Enter => {
                if let Some(idx) = self.list.selected_index() {
                    if idx < self.items.len() {
                        let item = &self.items[idx];
                        let lines: Vec<String> = format!(
                            "Title: {}\nLink: {}\n\n{}",
                            item.title, item.link, item.description
                        )
                        .lines()
                        .map(String::from)
                        .collect();
                        self.viewport.set_lines(lines);
                    }
                }
            }
            FeedMsg::Quit => return Cmd::quit(),
        }
        Cmd::none()
    }

    fn view(&self) -> View {
        let mut v = View::new(80, 24);
        v.write_str(0, 0, "feed - q:quit  enter:open  arrows:navigate");
        v.write_str(1, 0, &self.status);
        self.list.render(2, 0, 38, &mut v);
        self.viewport.render(2, 40, &mut v);
        v
    }

    fn from_key(key: KeyEvent) -> Option<FeedMsg> {
        match key {
            KeyEvent::Char('q') | KeyEvent::Ctrl('c') => Some(FeedMsg::Quit),
            KeyEvent::Arrow(Direction::Up) => Some(FeedMsg::Up),
            KeyEvent::Arrow(Direction::Down) => Some(FeedMsg::Down),
            KeyEvent::Enter => Some(FeedMsg::Enter),
            _ => None,
        }
    }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = debug_print("FEED_START\n");
    let mut prog = Program::<FeedModel>::new();
    match prog.run() {
        Ok(()) => 0,
        Err(_) => 1,
    }
}
