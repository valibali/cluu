#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::string::ToString;
use libcluu::debug_print;
use libtui::input::KeyEvent;
use libtui::style::{Border, Style};
use libtui::{
    Cell, Cmd, Model, View,
    ATTR_BOLD, ATTR_UNDERLINE,
    COLOR_BLACK, COLOR_BLUE, COLOR_CYAN, COLOR_DEFAULT, COLOR_GREEN,
    COLOR_MAGENTA, COLOR_RED, COLOR_WHITE, COLOR_YELLOW,
};
use libtui::program::Program;

const WIDTH: usize = 80;
const HEIGHT: usize = 24;

enum DemoMsg {
    Key(KeyEvent),
    Quit,
    Tick,
}

struct DemoModel {
    progress: u32,
    key_count: u32,
}

impl Model for DemoModel {
    type Msg = DemoMsg;

    fn init() -> (Self, Cmd) {
        (DemoModel { progress: 0, key_count: 0 }, Cmd::none())
    }

    fn update(&mut self, msg: Self::Msg) -> Cmd {
        match msg {
            DemoMsg::Key(_) => {
                self.key_count += 1;
                self.progress = (self.progress + 4).min(100);
                Cmd::none()
            }
            DemoMsg::Tick => {
                if self.progress < 100 {
                    self.progress += 2;
                }
                Cmd::none()
            }
            DemoMsg::Quit => Cmd::quit(),
        }
    }

    fn view(&self) -> View {
        let mut v = View::new(WIDTH, HEIGHT);

        draw_title(&mut v);
        draw_text_styles(&mut v);
        draw_colors(&mut v);
        draw_borders(&mut v);
        draw_blocks(&mut v);
        draw_progress(&mut v, self.progress);
        draw_footer(&mut v, self.key_count);

        v
    }

    fn from_key(key: KeyEvent) -> Option<Self::Msg> {
        match key {
            KeyEvent::Ctrl('c') => Some(DemoMsg::Quit),
            KeyEvent::Char('q') => Some(DemoMsg::Quit),
            KeyEvent::Char('Q') => Some(DemoMsg::Quit),
            other => Some(DemoMsg::Key(other)),
        }
    }
}

fn draw_title(v: &mut View) {
    let title = "libtui Feature Showcase";
    let centered = (WIDTH.saturating_sub(title.len())) / 2;
    for (i, ch) in title.chars().enumerate() {
        v.set(0, centered + i, Cell::new(ch).fg(COLOR_CYAN).attrs(ATTR_BOLD));
    }
    let line = "─".repeat(WIDTH);
    v.write_str(1, 0, &line);
}

fn draw_text_styles(v: &mut View) {
    v.write_str(2, 0, "Text Styles:");
    let row = 3;
    v.write_styled(row, 2, "Normal", COLOR_DEFAULT, COLOR_DEFAULT, 0);
    v.write_styled(row, 12, "Bold", COLOR_DEFAULT, COLOR_DEFAULT, ATTR_BOLD);
    v.write_styled(row, 20, "Underlined", COLOR_DEFAULT, COLOR_DEFAULT, ATTR_UNDERLINE);
    v.write_styled(row, 36, "Bold+Underline", COLOR_DEFAULT, COLOR_DEFAULT, ATTR_BOLD | ATTR_UNDERLINE);
    v.write_styled(row, 56, "Reverse", COLOR_DEFAULT, COLOR_DEFAULT, 0);
}

fn draw_colors(v: &mut View) {
    v.write_str(5, 0, "Colors (fg):");
    let colors: &[(u8, &str)] = &[
        (COLOR_RED, "Red"),
        (COLOR_GREEN, "Green"),
        (COLOR_YELLOW, "Yellow"),
        (COLOR_BLUE, "Blue"),
        (COLOR_MAGENTA, "Magenta"),
        (COLOR_CYAN, "Cyan"),
        (COLOR_WHITE, "White"),
    ];
    let row = 6;
    for (i, (color, name)) in colors.iter().enumerate() {
        v.write_styled(row, 2 + i * 10, name, *color, COLOR_DEFAULT, ATTR_BOLD);
    }

    v.write_str(7, 0, "Colors (bg):");
    let bg_row = 8;
    for (i, (color, name)) in colors.iter().enumerate() {
        v.write_styled(bg_row, 2 + i * 10, name, COLOR_BLACK, *color, 0);
    }
}

fn draw_borders(v: &mut View) {
    v.write_str(10, 0, "Borders:");
    draw_border_box(v, 11, 2, 20, 4, Border::Single, "Single", COLOR_GREEN);
    draw_border_box(v, 11, 24, 20, 4, Border::Double, "Double", COLOR_YELLOW);
    draw_border_box(v, 11, 46, 20, 4, Border::Rounded, "Rounded", COLOR_CYAN);
}

fn draw_border_box(v: &mut View, top: usize, left: usize, width: usize, height: usize, border: Border, label: &str, color: u8) {
    let (tl, tr, bl, br, h, vert) = match border {
        Border::Single => ('┌', '┐', '└', '┘', '─', '│'),
        Border::Double => ('╔', '╗', '╚', '╝', '═', '║'),
        Border::Rounded => ('╭', '╮', '╰', '╯', '─', '│'),
        Border::None => return,
    };

    v.set(top, left, Cell::new(tl).fg(color));
    v.set(top, left + width - 1, Cell::new(tr).fg(color));
    v.set(top + height - 1, left, Cell::new(bl).fg(color));
    v.set(top + height - 1, left + width - 1, Cell::new(br).fg(color));
    for i in 1..width - 1 {
        v.set(top, left + i, Cell::new(h).fg(color));
        v.set(top + height - 1, left + i, Cell::new(h).fg(color));
    }
    for i in 1..height - 1 {
        v.set(top + i, left, Cell::new(vert).fg(color));
        v.set(top + i, left + width - 1, Cell::new(vert).fg(color));
    }
    let label_y = top + height / 2;
    let label_x = left + (width.saturating_sub(label.len())) / 2;
    v.write_styled(label_y, label_x, label, color, COLOR_DEFAULT, 0);
}

fn draw_blocks(v: &mut View) {
    v.write_str(16, 0, "Block Elements:");
    let row = 17;
    v.write_styled(row, 2, "Full █", COLOR_GREEN, COLOR_DEFAULT, 0);
    v.write_styled(row, 12, "Dark ▓", COLOR_GREEN, COLOR_DEFAULT, 0);
    v.write_styled(row, 22, "Med ▒", COLOR_YELLOW, COLOR_DEFAULT, 0);
    v.write_styled(row, 32, "Light ░", COLOR_WHITE, COLOR_DEFAULT, 0);
    v.write_styled(row, 44, "Upper ▀", COLOR_CYAN, COLOR_DEFAULT, 0);
    v.write_styled(row, 54, "Lower ▄", COLOR_CYAN, COLOR_DEFAULT, 0);
}

fn draw_progress(v: &mut View, percent: u32) {
    let label = "Progress:";
    v.write_styled(19, 0, label, COLOR_WHITE, COLOR_DEFAULT, ATTR_BOLD);

    let bar_left = 12;
    let bar_width = 50;
    let filled = (bar_width as u32 * percent) / 100;

    v.set(19, bar_left - 1, Cell::new('[').fg(COLOR_WHITE));
    v.set(19, bar_left + bar_width, Cell::new(']').fg(COLOR_WHITE));

    for i in 0..bar_width {
        let ch = if (i as u32) < filled { '█' } else { '░' };
        let color = if (i as u32) < filled { COLOR_GREEN } else { COLOR_WHITE };
        v.set(19, bar_left + i, Cell::new(ch).fg(color));
    }

    let pct_str = alloc::format!("{:3}%", percent);
    v.write_styled(19, bar_left + bar_width + 2, &pct_str, COLOR_GREEN, COLOR_DEFAULT, ATTR_BOLD);
}

fn draw_footer(v: &mut View, key_count: u32) {
    let line = "─".repeat(WIDTH);
    v.write_str(21, 0, &line);

    let info = alloc::format!("Keys: {}  |  Ctrl-C or q to quit", key_count);
    let centered = (WIDTH.saturating_sub(info.len())) / 2;
    v.write_styled(22, centered, &info, COLOR_YELLOW, COLOR_DEFAULT, 0);
}

trait WriteStyled {
    fn write_styled(&mut self, row: usize, col: usize, text: &str, fg: u8, bg: u8, attrs: u8);
}

impl WriteStyled for View {
    fn write_styled(&mut self, row: usize, col: usize, text: &str, fg: u8, bg: u8, attrs: u8) {
        let mut c = col;
        for ch in text.chars() {
            if c >= self.width || row >= self.height {
                break;
            }
            self.set(row, c, Cell::new(ch).fg(fg).bg(bg).attrs(attrs));
            c += 1;
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
