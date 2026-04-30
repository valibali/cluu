//! Ex command parser + dispatch. See spec §9.
//!
//! Commands handled in this module:
//!   :w [path], :q, :q!, :wq [path], :e path, :e! path,
//!   :N (line number), :%s/old/new/[g], :N1,N2 s/old/new/[g], :s/old/new/[g],
//!   :set ..., :help

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;
use crate::mode::Editor;

pub enum ExCmd<'a> {
    Write(Option<&'a str>),
    Quit,
    QuitForce,
    WriteQuit(Option<&'a str>),
    Edit(&'a str),
    EditForce(&'a str),
    GotoLine(usize),
    Substitute { range: Range, pattern: &'a str, replacement: &'a str, global: bool },
    Set(&'a str),
    Help,
    Unknown(&'a str),
}

#[derive(Clone, Copy)]
pub enum Range {
    Whole,
    Line(usize),
    Lines(usize, usize),
    Current,
}

pub fn parse(line: &str) -> ExCmd<'_> {
    let line = line.trim_start();
    // Range prefix: %, $, ., digits, possibly N1,N2.
    let (range, rest) = parse_range(line);
    let rest = rest.trim_start();
    if rest.is_empty() {
        // bare range = goto line
        return match range {
            Range::Line(n) => ExCmd::GotoLine(n),
            _              => ExCmd::Unknown(line),
        };
    }
    // Command word.
    let (cmd, args) = split_cmd(rest);
    match cmd {
        "w"        => ExCmd::Write(if args.is_empty() { None } else { Some(args) }),
        "q"        => ExCmd::Quit,
        "q!"       => ExCmd::QuitForce,
        "wq" | "x" => ExCmd::WriteQuit(if args.is_empty() { None } else { Some(args) }),
        "e"        => ExCmd::Edit(args),
        "e!"       => ExCmd::EditForce(args),
        "set"      => ExCmd::Set(args),
        "help"     => ExCmd::Help,
        "s"        => parse_subst(range, args),
        _          => ExCmd::Unknown(line),
    }
}

fn parse_range(line: &str) -> (Range, &str) {
    let bytes = line.as_bytes();
    if bytes.first() == Some(&b'%') { return (Range::Whole, &line[1..]); }
    // numeric or digit-comma-digit.
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() { i += 1; }
    if i == 0 { return (Range::Current, line); }
    let n: usize = line[..i].parse().unwrap_or(1);
    if bytes.get(i) == Some(&b',') {
        let mut j = i + 1;
        while j < bytes.len() && bytes[j].is_ascii_digit() { j += 1; }
        if j > i + 1 {
            let m: usize = line[i+1..j].parse().unwrap_or(n);
            return (Range::Lines(n, m), &line[j..]);
        }
    }
    (Range::Line(n), &line[i..])
}

fn split_cmd(rest: &str) -> (&str, &str) {
    // Treat the first run of [a-zA-Z!] as the command word.
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() && (bytes[i].is_ascii_alphabetic() || bytes[i] == b'!') { i += 1; }
    if i == 0 { return ("", rest); }
    let cmd = &rest[..i];
    let args = rest[i..].trim_start();
    (cmd, args)
}

fn parse_subst(range: Range, args: &str) -> ExCmd<'_> {
    let bytes = args.as_bytes();
    if bytes.first() != Some(&b'/') { return ExCmd::Unknown(args); }
    // Find next unescaped '/' for end of pattern.
    let mut i = 1;
    while i < bytes.len() && bytes[i] != b'/' { if bytes[i] == b'\\' { i += 1; } i += 1; }
    if i >= bytes.len() { return ExCmd::Unknown(args); }
    let pat_end = i;
    i += 1;
    let repl_start = i;
    while i < bytes.len() && bytes[i] != b'/' { if bytes[i] == b'\\' { i += 1; } i += 1; }
    let repl_end = i;
    let global = if i < bytes.len() && i + 1 < bytes.len() && &args[i+1..].trim() == &"g" { true } else { false };
    ExCmd::Substitute {
        range,
        pattern: &args[1..pat_end],
        replacement: &args[repl_start..repl_end],
        global,
    }
}

pub fn dispatch(state: &mut Editor, line: &str) {
    match parse(line) {
        ExCmd::Quit => {
            if state.buf.dirty { state.message = "E37: No write since last change".into(); }
            else { state.running = false; }
        }
        ExCmd::QuitForce => state.running = false,
        ExCmd::Write(p) => crate::vfs_io::save(state, p),
        ExCmd::WriteQuit(p) => {
            crate::vfs_io::save(state, p);
            if !state.message.starts_with('E') { state.running = false; }
        }
        ExCmd::Edit(p) => {
            if state.buf.dirty { state.message = "E37: No write since last change".into(); }
            else { crate::vfs_io::load(state, p); }
        }
        ExCmd::EditForce(p) => crate::vfs_io::load(state, p),
        ExCmd::GotoLine(n) => {
            let idx = state.buf.pieces.line_index().to_vec();
            let target = if n == 0 { 0 } else { (n - 1).min(idx.len().saturating_sub(1)) };
            state.buf.cursor = idx[target];
        }
        ExCmd::Substitute { range, pattern, replacement, global } => {
            crate::search::substitute(state, range, pattern, replacement, global);
        }
        ExCmd::Set(args) => crate::settings::dispatch(state, args),
        ExCmd::Help => crate::help::open(state),
        ExCmd::Unknown(s) => state.message = alloc::format!("E492: Not an editor command: {}", s),
    }
}
