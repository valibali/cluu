#![no_std]
#![no_main]
#![allow(dead_code, unused_imports, unused_variables, unused_assignments)]

extern crate alloc;

mod buffer;
mod ex;
mod help;
mod input;
mod insert;
mod mode;
mod motion;
mod normal;
mod op_pending;
mod ops;
mod piece;
mod plugin;
mod prompt;
mod render;
mod search;
mod settings;
mod tty;
mod undo;
mod vfs_io;
mod visual;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use libcluu::{debug_print, Result};
use libtui::input::KeyEvent;
use libtui::{Cmd, Model, View};
use libtui::program::Program;

use crate::mode::{Editor, StepResult, Viewport};

struct EditModel {
    editor: Editor,
    render_data: render::RenderData,
    plugins: plugin::PluginRegistry,
}

enum EditMsg {
    Key(KeyEvent),
}

impl Model for EditModel {
    type Msg = EditMsg;

    fn init() -> (Self, Cmd) {
        let _ = debug_print("edit: starting up (libtui)\n");
        let _ = debug_print("EDIT_STARTING\n");
        let _ = debug_print("EDIT_LIBTUI_OK\n");

        let argv = libcluu::args::args();
        let initial_buf = if let Some(path) = argv.iter().nth(1) {
            let _ = debug_print(&format!("edit: loading file {}\n", path));
            let mut tmp_state = Editor::new(buffer::EditBuffer::empty());
            vfs_io::load(&mut tmp_state, path);
            tmp_state.buf
        } else {
            let _ = debug_print("edit: no file arg, empty buffer\n");
            buffer::EditBuffer::empty()
        };

        let mut editor = Editor::new(initial_buf);
        render::ensure_cursor_visible(&mut editor);
        crate::search::refresh_matches(&mut editor);
        let render_data = render::compute_render_data(&mut editor);

        let vp = Viewport::from_console();
        let _ = debug_print(&format!(
            "edit: viewport {}x{}\n", vp.width, vp.height
        ));
        let _ = debug_print("EDIT_RESIZE_OK\n");

        let plugins = plugin::PluginRegistry::load_all();

        (EditModel { editor, render_data, plugins }, Cmd::none())
    }

    fn update(&mut self, msg: EditMsg) -> Cmd {
        match msg {
            EditMsg::Key(key) => {
                if let Some(spec) = key_to_spec(&key) {
                    if let Some(callback_id) = self.plugins.has_key(&spec) {
                        if self.plugins.dispatch_key(&mut self.editor, &spec, &callback_id) {
                            self.render_data = render::compute_render_data(&mut self.editor);
                            return Cmd::none();
                        }
                    }
                }

                let result = mode::handle(&mut self.editor, key);
                match result {
                    StepResult::Quit(_) => return Cmd::quit(),
                    _ => {}
                }

                if let Some(cmd) = self.editor.plugin_ex_command.take() {
                    if let Some(cb) = self.plugins.has_command(&cmd) {
                        if self.plugins.dispatch_command(&mut self.editor, &cmd, &cb) {
                            self.editor.message = String::new();
                        }
                    }
                }

                let current = Viewport::from_console();
                if current.width != self.editor.viewport.width
                    || current.height != self.editor.viewport.height
                {
                    self.editor.viewport = current;
                    let _ = debug_print("EDIT_RESIZE_OK\n");
                }
                render::ensure_cursor_visible(&mut self.editor);
                crate::search::refresh_matches(&mut self.editor);
                self.render_data = render::compute_render_data(&mut self.editor);
                Cmd::none()
            }
        }
    }

    fn view(&self) -> View {
        render::build_view(&self.editor, &self.render_data)
    }

    fn from_key(key: KeyEvent) -> Option<EditMsg> {
        Some(EditMsg::Key(key))
    }

    fn cursor_position(&self) -> Option<(usize, usize)> {
        render::cursor_pos(&self.editor, &self.render_data)
    }

    fn on_resize(&mut self) {
        let current = Viewport::from_console();
        if current.width != self.editor.viewport.width
            || current.height != self.editor.viewport.height
        {
            self.editor.viewport = current;
            let _ = debug_print("EDIT_RESIZE_OK\n");
        }
        render::ensure_cursor_visible(&mut self.editor);
        crate::search::refresh_matches(&mut self.editor);
        self.render_data = render::compute_render_data(&mut self.editor);
    }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let mut prog = Program::<EditModel>::new();
    match prog.run() {
        Ok(()) => 0,
        Err(err) => {
            let _ = debug_print(&format!("edit: fatal {:?}\n", err));
            1
        }
    }
}

fn key_to_spec(key: &KeyEvent) -> Option<String> {
    match key {
        KeyEvent::Char(c) => Some(format!("{}", c)),
        KeyEvent::Ctrl(c) => Some(format!("Ctrl-{}", c.to_ascii_uppercase())),
        KeyEvent::Enter => Some(String::from("Enter")),
        KeyEvent::Esc => Some(String::from("Esc")),
        KeyEvent::Tab => Some(String::from("Tab")),
        KeyEvent::Backspace => Some(String::from("Backspace")),
        KeyEvent::Delete => Some(String::from("Delete")),
        KeyEvent::Home => Some(String::from("Home")),
        KeyEvent::End => Some(String::from("End")),
        KeyEvent::PageUp => Some(String::from("PageUp")),
        KeyEvent::PageDown => Some(String::from("PageDown")),
        KeyEvent::Arrow(d) => Some(format!("{:?}", d)),
        KeyEvent::ShiftTab => Some(String::from("ShiftTab")),
    }
}
