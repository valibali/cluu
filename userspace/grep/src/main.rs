//! `/bin/grep` — search for patterns in files.
//!
//! Flags: -i, -v, -n, -c, -l, -L, -r/-R, -w, -x, -E, -F, -q, -H, -h, --color=auto

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use libcluu::boot::{process_info, TOKEN_SPACE};
use libcluu::cli::{parse, render_help, CliError, Spec};
use libcluu::debug_print;
use libcluu::fs::client::VfsClient;
use libcluu::posix::{_read, _write};
use libcluu::registry;

const CHUNK_SIZE: usize = 64 * 1024;

fn spec() -> Spec {
    Spec::new()
        .program("grep")
        .version("0.1.0")
        .usage("[-ivncwxEFqrRHh] [--color=auto] PATTERN [FILE]...")
        .flag('i', "ignore-case", "ignore case distinctions")
        .flag('v', "invert-match", "select non-matching lines")
        .flag('n', "line-number", "print line number with output lines")
        .flag('c', "count", "print only a count of matching lines per file")
        .flag('l', "files-with-matches", "print only names of files with matches")
        .flag('L', "files-without-match", "print only names of files with no matches")
        .flag('r', "recursive", "read all files under each directory, recursively")
        .flag('R', "recursive-cap", "alias for -r")
        .flag('w', "word-regexp", "force PATTERN to match only whole words")
        .flag('x', "line-regexp", "force PATTERN to match only whole lines")
        .flag('E', "extended-regexp", "PATTERN is an extended regular expression (literal match only in CLUU)")
        .flag('F', "fixed-strings", "PATTERN is a set of newline-separated fixed strings")
        .flag('q', "quiet", "suppress all normal output; exit 0 if match found")
        .flag('H', "with-filename", "print filename with output lines")
        .flag('h', "no-filename", "suppress the filename prefix on output")
        .optional('C', "color", "use markers to highlight matching text (auto|always|never)")
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = libcluu::args::args();
    let sp = spec();
    let parsed = match parse(&sp, &args) {
        Ok(p) => p,
        Err(CliError::HelpRequested) => {
            let h = render_help(&sp);
            write_fd(1, h.as_bytes());
            return 0;
        }
        Err(CliError::VersionRequested) => {
            write_fd(1, b"grep 0.1.0\n");
            return 0;
        }
        Err(e) => {
            let msg = format!("grep: {}\n", e);
            write_fd(2, msg.as_bytes());
            return 2;
        }
    };

    if parsed.positional.is_empty() {
        write_fd(2, b"grep: usage: grep [-invncwxEFqrRHh] PATTERN [FILE]...\n");
        return 2;
    }

    let pattern = parsed.positional[0].clone();
    let files: Vec<String> = parsed.positional[1..].to_vec();

    let case_insensitive = parsed.is_set("ignore-case");
    let invert = parsed.is_set("invert-match");
    let show_line_no = parsed.is_set("line-number");
    let count_only = parsed.is_set("count");
    let files_with = parsed.is_set("files-with-matches");
    let files_without = parsed.is_set("files-without-match");
    let recursive = parsed.is_set("recursive") || parsed.is_set("recursive-cap");
    let word_regexp = parsed.is_set("word-regexp");
    let line_regexp = parsed.is_set("line-regexp");
    let quiet = parsed.is_set("quiet");
    let force_filename = parsed.is_set("with-filename");
    let no_filename = parsed.is_set("no-filename");

    // Color: --color=auto enables color on tty.
    let color_raw = parsed.value("color");
    let color = match color_raw {
        Some("always") => true,
        Some("never") | None if parsed.is_set("color") => {
            // -C without value = auto
            stdout_is_tty() && getenv_str("NO_COLOR").is_none()
        }
        Some(_) => stdout_is_tty() && getenv_str("NO_COLOR").is_none(),
        None => false,
    };

    let needle: String = if case_insensitive {
        pattern.to_lowercase()
    } else {
        pattern.clone()
    };

    let Ok(vfs_endpoint) = registry::subscribe_output("vfs", "main") else {
        write_fd(2, b"grep: vfs unavailable\n");
        return 2;
    };
    let vfs = VfsClient::new(vfs_endpoint, registry::control_endpoint());

    let opts = GrepOpts {
        needle: needle.clone(),
        pattern: pattern.clone(),
        case_insensitive,
        invert,
        show_line_no,
        count_only,
        files_with,
        files_without,
        word_regexp,
        line_regexp,
        quiet,
        force_filename,
        no_filename,
        color,
    };

    if files.is_empty() {
        // Read from stdin.
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; CHUNK_SIZE];
        loop {
            let n = _read(0, chunk.as_mut_ptr() as *mut _, chunk.len());
            if n <= 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n as usize]);
        }
        let text = match core::str::from_utf8(&buf) {
            Ok(s) => s,
            Err(_) => {
                write_fd(2, b"grep: input is not valid UTF-8\n");
                return 2;
            }
        };
        let matched = search_text(text, "(standard input)", false, &opts);
        let ec = if matched { 0 } else { 1 };
        let _ = debug_print(&format!("grep: ok (exit {})", ec));
        return ec;
    }

    // Expand directories if -r.
    let all_files = if recursive {
        expand_dirs(&vfs, &files)
    } else {
        files.clone()
    };

    let multi = all_files.len() > 1;
    let mut any_match = false;

    for path in &all_files {
        let resolved = libcluu::posix::resolve_path(path);
        let mut buf: Vec<u8> = Vec::new();
        if read_whole_file_into(&vfs, &resolved, &mut buf).is_err() {
            let msg = format!("grep: {}: cannot read\n", path);
            write_fd(2, msg.as_bytes());
            continue;
        }
        let text = match core::str::from_utf8(&buf) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let show_fname = (multi || force_filename) && !no_filename;
        if search_text(text, path, show_fname, &opts) {
            any_match = true;
        }
    }

    let exit_code = if any_match { 0 } else { 1 };
    let _ = debug_print(&format!("grep: ok (exit {})", exit_code));
    exit_code
}

struct GrepOpts {
    needle: String,
    #[allow(dead_code)]
    pattern: String,
    case_insensitive: bool,
    invert: bool,
    show_line_no: bool,
    count_only: bool,
    files_with: bool,
    files_without: bool,
    word_regexp: bool,
    line_regexp: bool,
    quiet: bool,
    #[allow(dead_code)]
    force_filename: bool,
    #[allow(dead_code)]
    no_filename: bool,
    color: bool,
}

fn matches_pattern(line: &str, opts: &GrepOpts) -> bool {
    let hay: &str;
    let hay_owned;
    if opts.case_insensitive {
        hay_owned = line.to_lowercase();
        hay = &hay_owned;
    } else {
        hay = line;
    }

    let needle = opts.needle.as_str();

    if opts.line_regexp {
        return hay == needle;
    }

    if opts.word_regexp {
        // Check if needle appears as a whole word.
        let mut start = 0;
        while let Some(pos) = hay[start..].find(needle) {
            let abs = start + pos;
            let before_ok = abs == 0
                || !hay.as_bytes()[abs - 1].is_ascii_alphanumeric()
                    && hay.as_bytes()[abs - 1] != b'_';
            let end = abs + needle.len();
            let after_ok = end >= hay.len()
                || !hay.as_bytes()[end].is_ascii_alphanumeric()
                    && hay.as_bytes()[end] != b'_';
            if before_ok && after_ok {
                return true;
            }
            start = abs + 1;
            if start >= hay.len() {
                break;
            }
        }
        return false;
    }

    hay.contains(needle)
}

fn search_text(text: &str, fname: &str, show_fname: bool, opts: &GrepOpts) -> bool {
    let mut match_count = 0usize;

    for (lineno, line) in text.lines().enumerate() {
        let matched = matches_pattern(line, opts);
        let keep = matched ^ opts.invert;
        if keep {
            match_count += 1;
            if !opts.quiet && !opts.count_only && !opts.files_with && !opts.files_without {
                let mut out = String::new();
                if show_fname {
                    out.push_str(fname);
                    out.push(':');
                }
                if opts.show_line_no {
                    out.push_str(&format!("{}:", lineno + 1));
                }
                if opts.color {
                    // Highlight: simple ANSI bold-red for match.
                    // Find needle in line and wrap it.
                    let highlighted = highlight_match(line, &opts.needle, opts.case_insensitive);
                    out.push_str(&highlighted);
                } else {
                    out.push_str(line);
                }
                out.push('\n');
                write_fd(1, out.as_bytes());
            }
        }
    }

    let has_match = match_count > 0;

    if opts.count_only && !opts.quiet {
        let mut out = String::new();
        if show_fname {
            out.push_str(fname);
            out.push(':');
        }
        out.push_str(&format!("{}\n", match_count));
        write_fd(1, out.as_bytes());
    }

    if opts.files_with && has_match && !opts.quiet {
        let out = format!("{}\n", fname);
        write_fd(1, out.as_bytes());
    }

    if opts.files_without && !has_match && !opts.quiet {
        let out = format!("{}\n", fname);
        write_fd(1, out.as_bytes());
    }

    // For -l and -L, "match" means a match was found (or not found).
    if opts.files_with {
        has_match
    } else if opts.files_without {
        !has_match
    } else {
        has_match
    }
}

fn highlight_match(line: &str, needle: &str, case_insensitive: bool) -> String {
    let hay = if case_insensitive {
        line.to_lowercase()
    } else {
        String::from(line)
    };
    let needle_lower = if case_insensitive {
        needle.to_lowercase()
    } else {
        String::from(needle)
    };

    let mut result = String::new();
    let mut remaining = line;
    let mut hay_remaining = hay.as_str();

    loop {
        match hay_remaining.find(needle_lower.as_str()) {
            None => {
                result.push_str(remaining);
                break;
            }
            Some(pos) => {
                result.push_str(&remaining[..pos]);
                result.push_str("\x1b[1;31m");
                result.push_str(&remaining[pos..pos + needle.len()]);
                result.push_str("\x1b[0m");
                remaining = &remaining[pos + needle.len()..];
                hay_remaining = &hay_remaining[pos + needle_lower.len()..];
            }
        }
    }
    result
}

fn expand_dirs(vfs: &VfsClient, paths: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    for path in paths {
        let resolved = libcluu::posix::resolve_path(path);
        match vfs.stat(&resolved) {
            Ok(st) if st.mode & 0o170000 == 0o040000 => {
                // Directory — recurse.
                collect_files_recursive(vfs, &resolved, &mut result);
            }
            _ => {
                result.push(path.clone());
            }
        }
    }
    result
}

fn collect_files_recursive(vfs: &VfsClient, dir: &str, out: &mut Vec<String>) {
    let entries = match vfs.readdir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries {
        if entry.name == "." || entry.name == ".." {
            continue;
        }
        let child = format!("{}/{}", dir.trim_end_matches('/'), entry.name);
        if entry.is_dir {
            collect_files_recursive(vfs, &child, out);
        } else {
            out.push(child);
        }
    }
}

fn read_whole_file_into(vfs: &VfsClient, path: &str, dst: &mut Vec<u8>) -> Result<(), ()> {
    let file = vfs.open(path).map_err(|_| ())?;
    let total = file.size;
    if total == 0 {
        let _ = vfs.close(file);
        return Ok(());
    }

    let info = process_info();
    let space_token = info.tokens[TOKEN_SPACE];
    let chunk_alloc = ((CHUNK_SIZE.min(total)) + 4095) & !4095;
    let scratch_base = libcluu::vspace::VSPACE
        .lock()
        .alloc(chunk_alloc)
        .map_err(|_| {
            let _ = vfs.close(file);
        })?;

    let mut offset = 0usize;
    let mut result: Result<(), ()> = Ok(());
    while offset < total {
        let want = (total - offset).min(CHUNK_SIZE);
        match vfs.read_grant(file, offset, want, space_token, scratch_base) {
            Ok(grant) => {
                if grant.len == 0 {
                    break;
                }
                let slice = unsafe {
                    core::slice::from_raw_parts(scratch_base as *const u8, grant.len)
                };
                dst.extend_from_slice(slice);
                offset += grant.len;
            }
            Err(_) => {
                result = Err(());
                break;
            }
        }
    }

    let _ = libcluu::vspace::VSPACE.lock().free(scratch_base, chunk_alloc);
    let _ = vfs.close(file);
    result
}

fn getenv_str(name: &str) -> Option<String> {
    extern "C" {
        fn getenv(name: *const u8) -> *const u8;
    }
    let mut key = String::from(name);
    key.push('\0');
    unsafe {
        let ptr = getenv(key.as_ptr());
        if ptr.is_null() {
            return None;
        }
        let mut len = 0;
        while *ptr.add(len) != 0 {
            len += 1;
        }
        let bytes = core::slice::from_raw_parts(ptr, len);
        Some(String::from_utf8_lossy(bytes).into_owned())
    }
}

fn stdout_is_tty() -> bool {
    extern "C" {
        fn isatty(fd: i32) -> i32;
    }
    unsafe { isatty(1) != 0 }
}

fn write_fd(fd: i32, data: &[u8]) {
    let _ = _write(fd, data.as_ptr() as *const _, data.len());
}
