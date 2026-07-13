//! Style, Border, and join helpers for libtui.
//!
//! no_std + alloc. Provides:
//! - `Style`: fg/bg/attrs builder with SGR escape generation
//! - `Border` + `bordered`: box-drawing wrappers
//! - `Alignment` + `join_horizontal`/`join_vertical`: block composition

extern crate alloc;
use alloc::format;
use alloc::string::String;
use alloc::string::ToString;
use alloc::vec::Vec;

// =========================================================================
// Style
// =========================================================================

/// Styling descriptor: 256-color fg/bg plus bold/underline/reverse attrs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub fg: Option<u8>,
    pub bg: Option<u8>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
}

impl Style {
    pub fn new() -> Self {
        Style::default()
    }
    pub fn fg(mut self, fg: u8) -> Self { self.fg = Some(fg); self }
    pub fn bg(mut self, bg: u8) -> Self { self.bg = Some(bg); self }
    pub fn bold(mut self) -> Self { self.bold = true; self }
    pub fn italic(mut self) -> Self { self.italic = true; self }
    pub fn underline(mut self) -> Self { self.underline = true; self }
    pub fn reverse(mut self) -> Self { self.reverse = true; self }

    pub fn to_sgr(&self) -> String {
        let mut s = String::from("\x1b[0");
        if self.bold { s.push_str(";1"); }
        if self.italic { s.push_str(";3"); }
        if self.underline { s.push_str(";4"); }
        if self.reverse { s.push_str(";7"); }
        if let Some(fg) = self.fg { s.push_str(&format!(";38;5;{}", fg)); }
        if let Some(bg) = self.bg { s.push_str(&format!(";48;5;{}", bg)); }
        s.push('m');
        s
    }

    /// Wrap content with SGR prefix + reset suffix.
    pub fn apply(&self, content: &str) -> String {
        format!("{}{}\x1b[0m", self.to_sgr(), content)
    }
}

// =========================================================================
// Border
// =========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Border {
    None,
    Single,  // ─│┌┐└┘
    Double,  // ═║╔╗╚╝
    Rounded, // ╭╮╰╯─│
}

struct BorderChars {
    tl: char, tr: char, bl: char, br: char, h: char, v: char,
}

fn border_chars(b: Border) -> BorderChars {
    match b {
        Border::None    => BorderChars { tl:' ', tr:' ', bl:' ', br:' ', h:' ', v:' ' },
        Border::Single  => BorderChars { tl:'┌', tr:'┐', bl:'└', br:'┘', h:'─', v:'│' },
        Border::Double  => BorderChars { tl:'╔', tr:'╗', bl:'╚', br:'╝', h:'═', v:'║' },
        Border::Rounded => BorderChars { tl:'╭', tr:'╮', bl:'╰', br:'╯', h:'─', v:'│' },
    }
}

/// Wrap multi-line content with a styled border. Border chars receive `style`;
/// content is left unstyled (caller styles it separately). `Border::None`
/// returns content unchanged.
pub fn bordered(content: &str, border: Border, style: &Style) -> String {
    if border == Border::None {
        return content.to_string();
    }
    let bc = border_chars(border);
    let lines: Vec<&str> = content.split('\n').collect();
    let width = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let sgr = style.to_sgr();
    let reset = "\x1b[0m";
    let horiz: String = core::iter::repeat(bc.h).take(width).collect();

    let mut out = String::new();
    out.push_str(&sgr);
    out.push(bc.tl);
    out.push_str(&horiz);
    out.push(bc.tr);
    out.push_str(reset);
    out.push('\n');
    for line in &lines {
        let pad = width - line.chars().count();
        out.push_str(&sgr);
        out.push(bc.v);
        out.push_str(reset);
        out.push_str(line);
        for _ in 0..pad { out.push(' '); }
        out.push_str(&sgr);
        out.push(bc.v);
        out.push_str(reset);
        out.push('\n');
    }
    out.push_str(&sgr);
    out.push(bc.bl);
    out.push_str(&horiz);
    out.push(bc.br);
    out.push_str(reset);
    out
}

// =========================================================================
// Alignment + join
// =========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alignment {
    Left,
    Center,
    Right,
}

/// Pad distribution for a deficit of `d` given alignment.
/// Returns (leading, trailing) padding amounts.
fn distribute(d: usize, a: Alignment) -> (usize, usize) {
    match a {
        Alignment::Left   => (0, d),
        Alignment::Right  => (d, 0),
        Alignment::Center => (d / 2, d - d / 2),
    }
}

/// Place blocks side-by-side. Each block is split into lines; shorter blocks
/// are padded with blank lines per `alignment` (Left=top, Center=middle,
/// Right=bottom). Each block column is padded to its own widest line.
/// Lines are concatenated horizontally with no separator.
pub fn join_horizontal(blocks: &[&str], alignment: Alignment) -> String {
    if blocks.is_empty() { return String::new(); }
    let split: Vec<Vec<&str>> = blocks.iter().map(|b| b.split('\n').collect()).collect();
    let max_height = split.iter().map(|l| l.len()).max().unwrap_or(0);
    let col_widths: Vec<usize> = split.iter()
        .map(|lines| lines.iter().map(|l| l.chars().count()).max().unwrap_or(0))
        .collect();
    let mut out = String::new();
    for row in 0..max_height {
        for (i, lines) in split.iter().enumerate() {
            let h = lines.len();
            let (top, _bot) = distribute(max_height - h, alignment);
            let line = if row < top || row >= top + h { "" } else { lines[row - top] };
            out.push_str(line);
            let pad = col_widths[i] - line.chars().count();
            for _ in 0..pad { out.push(' '); }
        }
        out.push('\n');
    }
    if out.ends_with('\n') { out.pop(); }
    out
}

/// Stack blocks top-to-bottom. Each block padded to the widest block's width
/// per `alignment` (Left/Center/Right horizontal positioning). Blocks joined
/// with `\n`.
pub fn join_vertical(blocks: &[&str], alignment: Alignment) -> String {
    if blocks.is_empty() { return String::new(); }
    let split: Vec<Vec<&str>> = blocks.iter().map(|b| b.split('\n').collect()).collect();
    let max_width = split.iter()
        .flat_map(|l| l.iter().map(|s| s.chars().count()))
        .max().unwrap_or(0);
    let mut all_lines: Vec<String> = Vec::new();
    for lines in &split {
        for line in lines {
            let (lead, trail) = distribute(max_width - line.chars().count(), alignment);
            let mut s = String::new();
            for _ in 0..lead { s.push(' '); }
            s.push_str(line);
            for _ in 0..trail { s.push(' '); }
            all_lines.push(s);
        }
    }
    all_lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_default_is_reset() {
        assert_eq!(Style::new().to_sgr(), "\x1b[0m");
    }

    #[test]
    fn style_bold_only() {
        assert_eq!(Style::new().bold().to_sgr(), "\x1b[0;1m");
    }

    #[test]
    fn style_fg_color() {
        assert_eq!(Style::new().fg(196).to_sgr(), "\x1b[0;38;5;196m");
    }

    #[test]
    fn style_fg_bg_bold() {
        let s = Style::new().fg(21).bg(196).bold().to_sgr();
        assert!(s.starts_with("\x1b[0;1;38;5;21;48;5;196m"), "got: {}", s);
    }

    #[test]
    fn style_apply_wraps_content() {
        let s = Style::new().fg(196).bold();
        let out = s.apply("hi");
        assert!(out.starts_with("\x1b[0;1;38;5;196m"));
        assert!(out.ends_with("\x1b[0m"));
        assert!(out.contains("hi"));
    }

    #[test]
    fn border_single_around_hello() {
        let out = bordered("hello", Border::Single, &Style::new());
        let lines: Vec<&str> = out.split('\n').collect();
        assert!(lines[0].contains('┌') && lines[0].contains('┐'));
        assert!(lines[0].contains('─'));
        assert!(lines[1].contains('│') && lines[1].contains("hello"));
        assert!(lines[2].contains('└') && lines[2].contains('┘'));
    }

    #[test]
    fn border_double_around_multi_line() {
        let out = bordered("a\nb", Border::Double, &Style::new());
        let lines: Vec<&str> = out.split('\n').collect();
        assert_eq!(lines.len(), 4);
        assert!(lines[0].contains('╔') && lines[0].contains('╗'));
        assert!(lines[0].contains('═'));
        assert!(lines[1].contains('║') && lines[1].contains('a'));
        assert!(lines[2].contains('║') && lines[2].contains('b'));
        assert!(lines[3].contains('╚') && lines[3].contains('╝'));
    }

    #[test]
    fn border_rounded_corners() {
        let out = bordered("x", Border::Rounded, &Style::new());
        let lines: Vec<&str> = out.split('\n').collect();
        assert!(lines[0].contains('╭') && lines[0].contains('╮'));
        assert!(lines[2].contains('╰') && lines[2].contains('╯'));
    }

    #[test]
    fn border_none_returns_content() {
        let out = bordered("hello\nworld", Border::None, &Style::new());
        assert_eq!(out, "hello\nworld");
    }

    #[test]
    fn join_horizontal_two_blocks() {
        let out = join_horizontal(&["hi", "yo"], Alignment::Left);
        assert_eq!(out, "hiyo");
    }

    #[test]
    fn join_horizontal_different_heights() {
        // block 0: 2 lines, block 1: 1 line. Left align -> block 1 at top.
        let out = join_horizontal(&["ab\ncd", "e"], Alignment::Left);
        let lines: Vec<&str> = out.split('\n').collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "abe");
        assert_eq!(lines[1], "cd ");
    }

    #[test]
    fn join_vertical_two_blocks() {
        let out = join_vertical(&["a", "b"], Alignment::Left);
        assert_eq!(out, "a\nb");
    }

    #[test]
    fn join_vertical_different_widths() {
        let out = join_vertical(&["ab", "c"], Alignment::Left);
        assert_eq!(out, "ab\nc ");
    }
}
