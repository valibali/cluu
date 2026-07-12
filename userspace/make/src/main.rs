//! `/bin/make` — minimal build tool (Makefile parser + recipe executor).
//!
//! Supports: target: prereqs, TAB-recipe, VAR = value, $(VAR), .PHONY.
//! No pattern/implicit rules. Recipes run via system().

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
use libcluu::posix::{_write, system};
use libcluu::registry;

fn write_fd(fd: i32, data: &[u8]) {
    let _ = _write(fd, data.as_ptr() as *const _, data.len());
}

struct Rule {
    target: String,
    prereqs: Vec<String>,
    recipe: Vec<String>,
}

struct Makefile {
    vars: Vec<(String, String)>,
    rules: Vec<Rule>,
    phony: Vec<String>,
}

fn expand_vars(s: &str, vars: &[(String, String)]) -> String {
    let mut result = String::from(s);
    for (k, v) in vars {
        let pat = format!("$({})", k);
        result = result.replace(&pat, v);
    }
    result
}

fn parse_makefile(text: &str) -> Makefile {
    let mut vars: Vec<(String, String)> = Vec::new();
    let mut rules: Vec<Rule> = Vec::new();
    let mut phony: Vec<String> = Vec::new();
    let mut current: Option<usize> = None;

    for line in text.lines() {
        if line.is_empty() {
            current = None;
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with('\t') {
            if let Some(idx) = current {
                let recipe_line = &line[1..];
                rules[idx].recipe.push(String::from(recipe_line));
            }
            continue;
        }
        if line.starts_with(".PHONY:") {
            for t in line[7..].split_whitespace() {
                phony.push(String::from(t));
            }
            current = None;
            continue;
        }
        if let Some(eq) = line.find('=') {
            if eq > 0 && !line[..eq].contains(':') {
                let name = line[..eq].trim().to_string();
                let val = line[eq + 1..].trim().to_string();
                vars.push((name, val));
                current = None;
                continue;
            }
        }
        if let Some(colon) = line.find(':') {
            let target = line[..colon].trim().to_string();
            let prereqs: Vec<String> = line[colon + 1..]
                .split_whitespace()
                .map(String::from)
                .collect();
            rules.push(Rule {
                target: target.clone(),
                prereqs,
                recipe: Vec::new(),
            });
            current = Some(rules.len() - 1);
        }
    }
    Makefile { vars, rules, phony }
}

fn run_recipe(cmd: &str) -> i32 {
    let mut c = String::from(cmd);
    c.push('\0');
    system(c.as_ptr() as *const i8)
}

fn build_target(mf: &Makefile, target: &str, built: &mut Vec<String>) -> i32 {
    if built.iter().any(|t| t == target) {
        return 0;
    }
    let rule_idx = mf.rules.iter().position(|r| r.target == target);
    let idx = match rule_idx {
        Some(i) => i,
        None => {
            let m = format!("make: no rule for target '{}'\n", target);
            write_fd(2, m.as_bytes());
            return 1;
        }
    };
    for prereq in &mf.rules[idx].prereqs {
        let ec = build_target(mf, prereq, built);
        if ec != 0 {
            return ec;
        }
    }
    let is_phony = mf.phony.iter().any(|p| p == target);
    if !is_phony && mf.rules[idx].recipe.is_empty() {
        built.push(String::from(target));
        return 0;
    }
    for cmd in &mf.rules[idx].recipe {
        let expanded = expand_vars(cmd, &mf.vars);
        let label = format!("{}\n", expanded);
        write_fd(1, label.as_bytes());
        let ec = run_recipe(&expanded);
        if ec != 0 {
            let m = format!("make: recipe for '{}' failed (exit {})\n", target, ec);
            write_fd(2, m.as_bytes());
            return ec;
        }
    }
    built.push(String::from(target));
    0
}

fn read_whole_file(vfs: &VfsClient, path: &str, dst: &mut Vec<u8>) -> Result<(), ()> {
    use libcluu::boot::{process_info, TOKEN_SPACE};
    const FCHUNK: usize = 64 * 1024;
    let file = vfs.open(path).map_err(|_| ())?;
    let total = file.size;
    if total == 0 {
        let _ = vfs.close(file);
        return Ok(());
    }
    let info = process_info();
    let tok = info.tokens[TOKEN_SPACE];
    let sz = ((FCHUNK.min(total)) + 4095) & !4095;
    let base = libcluu::vspace::VSPACE
        .lock()
        .alloc(sz)
        .map_err(|_| { let _ = vfs.close(file); })?;
    let (mut off, mut res) = (0, Ok(()));
    while off < total {
        let want = (total - off).min(FCHUNK);
        match vfs.read_grant(file, off, want, tok, base) {
            Ok(g) => {
                if g.len == 0 { break; }
                let s = unsafe { core::slice::from_raw_parts(base as *const u8, g.len) };
                dst.extend_from_slice(s);
                off += g.len;
            }
            Err(_) => { res = Err(()); break; }
        }
    }
    let _ = libcluu::vspace::VSPACE.lock().free(base, sz);
    let _ = vfs.close(file);
    res
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = debug_print("MAKE_OK\n");
    let argv: Vec<String> = libcluu::args::args();
    let mut makefile_path = String::from("Makefile");
    let mut targets: Vec<String> = Vec::new();
    let mut i = 1;
    while i < argv.len() {
        if argv[i] == "-f" && i + 1 < argv.len() {
            makefile_path = argv[i + 1].clone();
            i += 2;
        } else {
            targets.push(argv[i].clone());
            i += 1;
        }
    }

    let Ok(vfs_ep) = registry::subscribe_output("vfs", "main") else {
        write_fd(2, b"make: vfs unavailable\n");
        return 1;
    };
    let vfs = VfsClient::new(vfs_ep, registry::control_endpoint());
    let resolved = libcluu::posix::resolve_path(&makefile_path);
    let mut buf: Vec<u8> = Vec::new();
    if read_whole_file(&vfs, &resolved, &mut buf).is_err() {
        let m = format!("make: {}: cannot read\n", makefile_path);
        write_fd(2, m.as_bytes());
        return 1;
    }
    let text = match String::from_utf8(buf) {
        Ok(s) => s,
        Err(_) => { write_fd(2, b"make: not UTF-8\n"); return 1; }
    };

    let mf = parse_makefile(&text);

    if targets.is_empty() {
        if let Some(first) = mf.rules.first() {
            targets.push(first.target.clone());
        } else {
            write_fd(2, b"make: no targets\n");
            return 1;
        }
    }

    let mut built: Vec<String> = Vec::new();
    let mut ec = 0;
    for target in &targets {
        ec = build_target(&mf, target, &mut built);
        if ec != 0 {
            break;
        }
    }
    let _ = debug_print(&format!("make: exit {}", ec));
    ec
}
