//! `/bin/awk` — minimal text processor (pattern-action pairs).
//!
//! Supports: BEGIN{}, END{}, /pattern/{action}, {action},
//! $0 $1..$9 $NF, print, printf, assignment, -F sep.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use libcluu::debug_print;
use libcluu::fs::client::VfsClient;
use libcluu::posix::{_read, _write};
use libcluu::registry;

fn write_fd(fd: i32, data: &[u8]) {
    let _ = _write(fd, data.as_ptr() as *const _, data.len());
}

struct Rule { pattern: Option<String>, action: String }
struct Program { begin: Option<String>, end: Option<String>, rules: Vec<Rule> }

fn extract_block(s: &[u8], start: usize) -> (String, usize) {
    let (mut depth, mut i) = (0, start);
    while i < s.len() {
        if s[i] == b'{' { depth += 1; }
        else if s[i] == b'}' { depth -= 1; if depth == 0 { break; } }
        i += 1;
    }
    (String::from_utf8_lossy(&s[start + 1..i]).into_owned(), i + 1)
}

fn skip_ws(s: &[u8], mut i: usize) -> usize {
    while i < s.len() && (s[i] as char).is_whitespace() { i += 1; }
    i
}

fn parse_program(prog: &str) -> Program {
    let b = prog.as_bytes();
    let (mut begin, mut end) = (None, None);
    let mut rules: Vec<Rule> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        i = skip_ws(b, i);
        if i >= b.len() { break; }
        if b[i..].starts_with(b"BEGIN") {
            i = skip_ws(b, i + 5);
            if i < b.len() && b[i] == b'{' { let (blk, n) = extract_block(b, i); begin = Some(blk); i = n; }
        } else if b[i..].starts_with(b"END") {
            i = skip_ws(b, i + 3);
            if i < b.len() && b[i] == b'{' { let (blk, n) = extract_block(b, i); end = Some(blk); i = n; }
        } else if b[i] == b'/' {
            let mut j = i + 1;
            while j < b.len() && b[j] != b'/' { j += 1; }
            let pat = String::from_utf8_lossy(&b[i + 1..j]).into_owned();
            i = skip_ws(b, j + 1);
            if i < b.len() && b[i] == b'{' { let (blk, n) = extract_block(b, i); rules.push(Rule { pattern: Some(pat), action: blk }); i = n; }
        } else if b[i] == b'{' {
            let (blk, n) = extract_block(b, i); rules.push(Rule { pattern: None, action: blk }); i = n;
        } else { i += 1; }
    }
    Program { begin, end, rules }
}

fn split_fields(line: &str, fs: char) -> Vec<String> {
    if fs == ' ' { line.split_whitespace().map(String::from).collect() }
    else { line.split(fs).map(String::from).collect() }
}

fn eval_expr(e: &str, fields: &[String], nf: usize, line: &str,
             vars: &[(String, String)]) -> String {
    let e = e.trim();
    if e.is_empty() { return String::new(); }
    if e.starts_with('$') {
        let r = &e[1..];
        if r == "NF" { return fields.get(nf - 1).cloned().unwrap_or_default(); }
        if let Ok(n) = r.parse::<usize>() {
            if n == 0 { return String::from(line); }
            return fields.get(n - 1).cloned().unwrap_or_default();
        }
        return String::new();
    }
    if e.starts_with('"') && e.ends_with('"') && e.len() >= 2 { return String::from(&e[1..e.len() - 1]); }
    if e == "NF" { return format!("{}", nf); }
    for (k, v) in vars { if k == e { return v.clone(); } }
    String::from(e)
}

fn set_var(vars: &mut Vec<(String, String)>, name: &str, val: &str) {
    for entry in vars.iter_mut() { if entry.0 == name { entry.1 = String::from(val); return; } }
    vars.push((String::from(name), String::from(val)));
}

fn split_args(s: &str) -> Vec<String> {
    let (mut parts, mut cur, mut in_str) = (Vec::new(), String::new(), false);
    for ch in s.chars() {
        if ch == '"' { in_str = !in_str; cur.push(ch); }
        else if ch == ',' && !in_str { parts.push(cur.trim().to_string()); cur.clear(); }
        else { cur.push(ch); }
    }
    let t = cur.trim().to_string();
    if !t.is_empty() || !parts.is_empty() { parts.push(t); }
    parts
}

fn exec_action(action: &str, fields: &[String], nf: usize, line: &str,
               vars: &mut Vec<(String, String)>) {
    let act = action.trim();
    if act.is_empty() { return; }
    if act.starts_with("print") {
        let rest = act[5..].trim();
        if rest.is_empty() { write_fd(1, line.as_bytes()); write_fd(1, b"\n"); return; }
        let parts = split_args(rest);
        let mut out = String::new();
        for (i, p) in parts.iter().enumerate() { if i > 0 { out.push(' '); } out.push_str(&eval_expr(p, fields, nf, line, vars)); }
        out.push('\n'); write_fd(1, out.as_bytes());
    } else if act.starts_with("printf") {
        let parts = split_args(act[6..].trim());
        if parts.is_empty() { return; }
        let fmt = eval_expr(&parts[0], fields, nf, line, vars);
        let args: Vec<String> = parts[1..].iter().map(|p| eval_expr(p, fields, nf, line, vars)).collect();
        let (mut out, mut ai) = (String::new(), 0);
        let mut chars = fmt.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '%' {
                match chars.next() {
                    Some('s') => { out.push_str(args.get(ai).map(|s| s.as_str()).unwrap_or("")); ai += 1; }
                    Some('d') => { out.push_str(args.get(ai).map(|s| s.as_str()).unwrap_or("0")); ai += 1; }
                    Some('%') => out.push('%'),
                    Some(c) => { out.push('%'); out.push(c); }
                    None => break,
                }
            } else { out.push(ch); }
        }
        write_fd(1, out.as_bytes());
    } else if let Some(eq) = act.find('=') {
        if eq > 0 && act.as_bytes()[eq - 1] != b'!' && act.as_bytes()[eq - 1] != b'<' && act.as_bytes()[eq - 1] != b'>' {
            set_var(vars, act[..eq].trim(), &eval_expr(act[eq + 1..].trim(), fields, nf, line, vars));
        }
    }
}

fn read_whole_file(vfs: &VfsClient, path: &str, dst: &mut Vec<u8>) -> Result<(), ()> {
    use libcluu::boot::{process_info, TOKEN_SPACE};
    const FCHUNK: usize = 64 * 1024;
    let file = vfs.open(path).map_err(|_| ())?;
    let total = file.size;
    if total == 0 { let _ = vfs.close(file); return Ok(()); }
    let info = process_info();
    let tok = info.tokens[TOKEN_SPACE];
    let sz = ((FCHUNK.min(total)) + 4095) & !4095;
    let base = libcluu::vspace::VSPACE.lock().alloc(sz).map_err(|_| { let _ = vfs.close(file); })?;
    let (mut off, mut res) = (0, Ok(()));
    while off < total {
        let want = (total - off).min(FCHUNK);
        match vfs.read_grant(file, off, want, tok, base) {
            Ok(g) => { if g.len == 0 { break; } let s = unsafe { core::slice::from_raw_parts(base as *const u8, g.len) }; dst.extend_from_slice(s); off += g.len; }
            Err(_) => { res = Err(()); break; }
        }
    }
    let _ = libcluu::vspace::VSPACE.lock().free(base, sz);
    let _ = vfs.close(file);
    res
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = debug_print("AWK_OK\n");
    let argv: Vec<String> = libcluu::args::args();
    let (mut fs, mut prog, mut file) = (' ', None, None);
    let mut i = 1;
    while i < argv.len() {
        if argv[i].starts_with("-F") && argv[i].len() > 2 { fs = argv[i].as_bytes()[2] as char; }
        else if prog.is_none() { prog = Some(argv[i].clone()); }
        else { file = Some(argv[i].clone()); }
        i += 1;
    }
    let prog_str = match prog { Some(p) => p, None => { write_fd(2, b"awk: no program\n"); return 1; } };
    let text = if let Some(fp) = file {
        let Ok(vfs_ep) = registry::subscribe_output("vfs", "main") else { write_fd(2, b"awk: vfs unavailable\n"); return 1; };
        let vfs = VfsClient::new(vfs_ep, registry::control_endpoint());
        let resolved = libcluu::posix::resolve_path(&fp);
        let mut buf: Vec<u8> = Vec::new();
        if read_whole_file(&vfs, &resolved, &mut buf).is_err() { let m = format!("awk: {}: cannot read\n", fp); write_fd(2, m.as_bytes()); return 1; }
        match String::from_utf8(buf) { Ok(s) => s, Err(_) => { write_fd(2, b"awk: not UTF-8\n"); return 1; } }
    } else {
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 4096];
        loop { let r = _read(0, chunk.as_mut_ptr() as *mut _, chunk.len()); if r <= 0 { break; } buf.extend_from_slice(&chunk[..r as usize]); }
        match String::from_utf8(buf) { Ok(s) => s, Err(_) => { write_fd(2, b"awk: not UTF-8\n"); return 1; } }
    };
    let program = parse_program(&prog_str);
    let mut vars: Vec<(String, String)> = Vec::new();
    if let Some(ref begin) = program.begin { exec_action(begin, &[], 0, "", &mut vars); }
    for line in text.lines() {
        let fields = split_fields(line, fs);
        let nf = fields.len();
        for rule in &program.rules {
            if rule.pattern.as_ref().map_or(true, |p| line.contains(p.as_str())) {
                exec_action(&rule.action, &fields, nf, line, &mut vars);
            }
        }
    }
    if let Some(ref end) = program.end { exec_action(end, &[], 0, "", &mut vars); }
    let _ = debug_print("awk: ok (exit 0)");
    0
}
