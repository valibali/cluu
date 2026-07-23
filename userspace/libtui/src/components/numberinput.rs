//! NumberInput — stepper with +/- bounds and current value.

extern crate alloc;

use crate::buffer::{Cell, COLOR_DEFAULT, ATTR_BOLD};
use crate::layout::{Drawable, Rect};
use crate::View;

pub struct NumberInput {
    value: i64,
    min: i64,
    max: i64,
    step: i64,
    fg: u8,
    suffix: Option<alloc::string::String>,
}

impl NumberInput {
    pub fn new(value: i64, min: i64, max: i64) -> Self {
        NumberInput {
            value: value.clamp(min, max),
            min,
            max,
            step: 1,
            fg: COLOR_DEFAULT,
            suffix: None,
        }
    }

    pub fn step(mut self, step: i64) -> Self { self.step = step; self }
    pub fn fg(mut self, fg: u8) -> Self { self.fg = fg; self }
    pub fn suffix(mut self, s: &str) -> Self { self.suffix = Some(alloc::string::String::from(s)); self }

    pub fn increment(&mut self) {
        self.value = (self.value + self.step).min(self.max);
    }

    pub fn decrement(&mut self) {
        self.value = (self.value - self.step).max(self.min);
    }

    pub fn set_value(&mut self, v: i64) {
        self.value = v.clamp(self.min, self.max);
    }

    pub fn value(&self) -> i64 {
        self.value
    }
}

impl Drawable for NumberInput {
    fn draw(&self, area: Rect, buf: &mut View) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let text = if let Some(ref suffix) = self.suffix {
            alloc::format!("[−] {} {} [+]", self.value, suffix)
        } else {
            alloc::format!("[−] {} [+]", self.value)
        };
        for (i, ch) in text.chars().enumerate() {
            if i >= area.width { break; }
            buf.set(area.y, area.x + i, Cell::new(ch).fg(self.fg));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numberinput_clamps_initial() {
        let n = NumberInput::new(100, 0, 50);
        assert_eq!(n.value(), 50);
    }

    #[test]
    fn numberinput_increment() {
        let mut n = NumberInput::new(5, 0, 10);
        n.increment();
        assert_eq!(n.value(), 6);
    }

    #[test]
    fn numberinput_increment_clamps() {
        let mut n = NumberInput::new(10, 0, 10);
        n.increment();
        assert_eq!(n.value(), 10);
    }

    #[test]
    fn numberinput_decrement() {
        let mut n = NumberInput::new(5, 0, 10);
        n.decrement();
        assert_eq!(n.value(), 4);
    }

    #[test]
    fn numberinput_decrement_clamps() {
        let mut n = NumberInput::new(0, 0, 10);
        n.decrement();
        assert_eq!(n.value(), 0);
    }

    #[test]
    fn numberinput_step() {
        let mut n = NumberInput::new(0, 0, 100).step(5);
        n.increment();
        assert_eq!(n.value(), 5);
        n.decrement();
        assert_eq!(n.value(), 0);
    }

    #[test]
    fn numberinput_set_value_clamps() {
        let mut n = NumberInput::new(5, 0, 10);
        n.set_value(100);
        assert_eq!(n.value(), 10);
        n.set_value(-5);
        assert_eq!(n.value(), 0);
    }

    #[test]
    fn numberinput_draw() {
        let n = NumberInput::new(42, 0, 100);
        let mut buf = View::new(20, 1);
        n.draw(Rect::new(0, 0, 20, 1), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('['));
        assert_eq!(buf.get(0, 4).map(|c| c.ch), Some('4'));
        assert_eq!(buf.get(0, 5).map(|c| c.ch), Some('2'));
    }

    #[test]
    fn numberinput_suffix() {
        let n = NumberInput::new(50, 0, 100).suffix("%");
        let mut buf = View::new(20, 1);
        n.draw(Rect::new(0, 0, 20, 1), &mut buf);
        // Format: [−] 50 % [+]
        assert_eq!(buf.get(0, 4).map(|c| c.ch), Some('5'));
        assert_eq!(buf.get(0, 5).map(|c| c.ch), Some('0'));
        assert_eq!(buf.get(0, 7).map(|c| c.ch), Some('%'));
    }
}
