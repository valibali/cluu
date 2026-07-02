//! ANSI / CSI byte-stream parser. Emits `Event`s; the consumer applies them
//! to its own cell grid. No knowledge of the rendering target.

extern crate alloc;

mod event;
mod state;

pub use event::{Attr, EraseMode, Event};
pub use state::Parser;

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    fn collect(bytes: &[u8]) -> Vec<Event> {
        let mut p = Parser::new();
        let mut out = Vec::new();
        p.feed(bytes, |ev| out.push(ev));
        out
    }

    #[test]
    fn print_ascii() {
        let evs = collect(b"hi");
        assert_eq!(evs, vec![Event::Print(b'h'), Event::Print(b'i')]);
    }

    #[test]
    fn csi_cursor_up() {
        let evs = collect(b"\x1b[3A");
        assert_eq!(evs, vec![Event::MoveCursorUp(3)]);
    }

    #[test]
    fn csi_default_param_is_one() {
        let evs = collect(b"\x1b[A");
        assert_eq!(evs, vec![Event::MoveCursorUp(1)]);
    }

    #[test]
    fn sgr_red_fg() {
        let evs = collect(b"\x1b[31m");
        let attr = match &evs[..] {
            [Event::SetAttr(a)] => *a,
            _ => panic!("got {:?}", evs),
        };
        assert_eq!(attr.fg, 0x00AA0000);
    }

    #[test]
    fn erase_line() {
        assert_eq!(collect(b"\x1b[K"), vec![Event::EraseLine(EraseMode::ToEnd)]);
    }

    #[test]
    fn erase_display() {
        assert_eq!(collect(b"\x1b[2J"), vec![Event::EraseDisplay(EraseMode::All)]);
    }

    #[test]
    fn erase_line_full() {
        assert_eq!(collect(b"\x1b[2K"), vec![Event::EraseLine(EraseMode::All)]);
    }

    #[test]
    fn erase_line_to_start() {
        assert_eq!(collect(b"\x1b[1K"), vec![Event::EraseLine(EraseMode::ToStart)]);
    }

    #[test]
    fn dectcem_hide_cursor() {
        assert_eq!(collect(b"\x1b[?25l"), vec![Event::SetCursorVisible(false)]);
    }

    #[test]
    fn dectcem_show_cursor() {
        assert_eq!(collect(b"\x1b[?25h"), vec![Event::SetCursorVisible(true)]);
    }

    #[test]
    fn dectcem_after_other_csi() {
        // Ensure the `private` flag is reset between sequences — a plain
        // CSI m after a ?25 l must not be misread as private mode.
        let evs = collect(b"\x1b[?25l\x1b[31m");
        assert_eq!(evs, vec![Event::SetCursorVisible(false), Event::SetAttr({
            let mut a = Attr::default_attr();
            a.fg = 0x00AA0000;
            a
        })]);
    }
}
