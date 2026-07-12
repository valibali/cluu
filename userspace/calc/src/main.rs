//! Calculator — arithmetic evaluator using libtui textinput.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use libcluu::debug_print;
use libtui::components::textinput::TextInput;
use libtui::input::KeyEvent;
use libtui::{Cmd, Model, View, COLOR_GREEN};
use libtui::program::Program;

enum CalcMsg {
    Char(char),
    Backspace,
    Evaluate,
    Clear,
    Quit,
}

struct CalcModel {
    input: TextInput,
    result: String,
}

// --- Recursive descent parser: expr = term (('+'|'-') term)* ---

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn new(s: &str) -> Self {
        Parser { chars: s.chars().collect(), pos: 0 }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while self.pos < self.chars.len() && self.chars[self.pos].is_whitespace() {
            self.pos += 1;
        }
    }

    fn parse_expr(&mut self) -> Result<i64, String> {
        let mut val = self.parse_term()?;
        loop {
            self.skip_ws();
            match self.peek() {
                Some('+') => { self.pos += 1; val = val.wrapping_add(self.parse_term()?); }
                Some('-') => { self.pos += 1; val = val.wrapping_sub(self.parse_term()?); }
                _ => break,
            }
        }
        Ok(val)
    }

    fn parse_term(&mut self) -> Result<i64, String> {
        let mut val = self.parse_factor()?;
        loop {
            self.skip_ws();
            match self.peek() {
                Some('*') => { self.pos += 1; val = val.wrapping_mul(self.parse_factor()?); }
                Some('/') => {
                    self.pos += 1;
                    let d = self.parse_factor()?;
                    if d == 0 { return Err(String::from("divide by zero")); }
                    val = val / d;
                }
                _ => break,
            }
        }
        Ok(val)
    }

    fn parse_factor(&mut self) -> Result<i64, String> {
        self.skip_ws();
        match self.peek() {
            Some('(') => {
                self.pos += 1;
                let val = self.parse_expr()?;
                self.skip_ws();
                if self.peek() != Some(')') {
                    return Err(String::from("expected )"));
                }
                self.pos += 1;
                Ok(val)
            }
            Some('-') => { self.pos += 1; Ok(self.parse_factor()?.wrapping_neg()) }
            Some(c) if c.is_ascii_digit() => self.parse_number(),
            _ => Err(String::from("unexpected token")),
        }
    }

    fn parse_number(&mut self) -> Result<i64, String> {
        let start = self.pos;
        while self.pos < self.chars.len() && self.chars[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        let s: String = self.chars[start..self.pos].iter().collect();
        s.parse::<i64>().map_err(|_| String::from("number overflow"))
    }
}

impl Model for CalcModel {
    type Msg = CalcMsg;

    fn init() -> (Self, Cmd) {
        (
            CalcModel {
                input: TextInput::with_placeholder("enter expression..."),
                result: String::new(),
            },
            Cmd::none(),
        )
    }

    fn update(&mut self, msg: CalcMsg) -> Cmd {
        match msg {
            CalcMsg::Char(c) => self.input.insert(c),
            CalcMsg::Backspace => self.input.backspace(),
            CalcMsg::Evaluate => {
                let expr = self.input.value();
                if expr.is_empty() {
                    self.result = String::new();
                } else {
                    let mut p = Parser::new(&expr);
                    match p.parse_expr() {
                        Ok(v) => self.result = format!("= {}", v),
                        Err(e) => self.result = format!("error: {}", e),
                    }
                }
            }
            CalcMsg::Clear => {
                self.input.clear();
                self.result.clear();
            }
            CalcMsg::Quit => return Cmd::quit(),
        }
        Cmd::none()
    }

    fn view(&self) -> View {
        let mut v = View::new(80, 24);
        v.write_str(0, 0, "calc - q:quit  c:clear  enter:evaluate");
        v.write_str(1, 0, "> ");
        self.input.render(1, 2, &mut v);
        if !self.result.is_empty() {
            v.write_str(3, 0, &self.result);
        }
        v
    }

    fn from_key(key: KeyEvent) -> Option<CalcMsg> {
        match key {
            KeyEvent::Char('q') | KeyEvent::Ctrl('c') => Some(CalcMsg::Quit),
            KeyEvent::Char('c') => Some(CalcMsg::Clear),
            KeyEvent::Char(ch) if ch.is_ascii_digit()
                || matches!(ch, '+' | '-' | '*' | '/' | '(' | ')' | ' ') =>
            {
                Some(CalcMsg::Char(ch))
            }
            KeyEvent::Enter => Some(CalcMsg::Evaluate),
            KeyEvent::Backspace => Some(CalcMsg::Backspace),
            _ => None,
        }
    }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = debug_print("CALC_OK\n");
    let mut prog = Program::<CalcModel>::new();
    match prog.run() {
        Ok(()) => 0,
        Err(_) => 1,
    }
}
