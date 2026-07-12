#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::string::ToString;
use libcluu::debug_print;
use libtui::input::KeyEvent;
use libtui::{Cell, Cmd, Model, View, COLOR_GREEN, COLOR_YELLOW, ATTR_BOLD};
use libtui::program::Program;

enum DemoMsg {
    Key(KeyEvent),
    Quit,
}

struct DemoModel {
    key_count: u32,
}

impl Model for DemoModel {
    type Msg = DemoMsg;

    fn init() -> (Self, Cmd) {
        (DemoModel { key_count: 0 }, Cmd::none())
    }

    fn update(&mut self, msg: Self::Msg) -> Cmd {
        match msg {
            DemoMsg::Key(_) => {
                self.key_count += 1;
                Cmd::none()
            }
            DemoMsg::Quit => Cmd::quit(),
        }
    }

    fn view(&self) -> View {
        let mut v = View::new(80, 24);
        v.write_str(0, 0, "libtui demo -- press q to quit");
        let count_str = alloc::format!("Keys pressed: {}", self.key_count);
        v.write_str(1, 0, &count_str);
        v
    }

    fn from_key(key: KeyEvent) -> Option<Self::Msg> {
        match key {
            KeyEvent::Char('q') => Some(DemoMsg::Quit),
            KeyEvent::Char('Q') => Some(DemoMsg::Quit),
            KeyEvent::Ctrl('c') => Some(DemoMsg::Quit),
            other => Some(DemoMsg::Key(other)),
        }
    }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = debug_print("LIBTUI_DEMO_OK\n");
    let mut prog = Program::<DemoModel>::new();
    match prog.run() {
        Ok(()) => 0,
        Err(_) => 1,
    }
}
