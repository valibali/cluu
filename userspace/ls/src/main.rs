#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use libcluu::fs::client::{VfsClient, VfsDirEntry, VfsStat};
use libcluu::posix::_write;
use libcluu::registry;
use libcluu::debug_print;

// ──────────────────────────────────────────────────────────────────────
// Mode bit constants (matching POSIX / libcluu::posix::stat)
// ──────────────────────────────────────────────────────────────────────
const S_IFMT:  u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
const S_IFLNK: u32 = 0o120000;
const S_IFCHR: u32 = 0o020000;
const S_IFBLK: u32 = 0o060000;
const S_IFIFO: u32 = 0o010000;
const S_IFSOCK:u32 = 0o140000;

// ──────────────────────────────────────────────────────────────────────
// Option structs
// ──────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortKey { Name, Size, Mtime }

#[derive(Clone, Copy, PartialEq, Eq)]
enum ColorMode { Auto, Always, Never }

struct LsOpts {
    long:       bool,
    all:        bool,
    human:      bool,
    recursive:  bool,
    one_col:    bool,
    sort:       SortKey,
    reverse:    bool,
    color_mode: ColorMode,
}

impl Default for LsOpts {
    fn default() -> Self {
        Self {
            long:       false,
            all:        false,
            human:      false,
            recursive:  false,
            one_col:    false,
            sort:       SortKey::Name,
            reverse:    false,
            color_mode: ColorMode::Auto,
        }
    }
}

// ──────────────────────────────────────────────────────────────────────
// Formatting helpers
// ──────────────────────────────────────────────────────────────────────

fn render_mode(mode: u32) -> String {
    let mut s = String::with_capacity(10);
    s.push(match mode & S_IFMT {
        S_IFDIR  => 'd',
        S_IFREG  => '-',
        S_IFLNK  => 'l',
        S_IFCHR  => 'c',
        S_IFBLK  => 'b',
        S_IFIFO  => 'p',
        S_IFSOCK => 's',
        _        => '?',
    });
    let bits: &[(u32, char)] = &[
        (0o400, 'r'), (0o200, 'w'), (0o100, 'x'),
        (0o040, 'r'), (0o020, 'w'), (0o010, 'x'),
        (0o004, 'r'), (0o002, 'w'), (0o001, 'x'),
    ];
    for &(mask, ch) in bits {
        s.push(if mode & mask != 0 { ch } else { '-' });
    }
    s
}

fn render_size(bytes: u64, human: bool) -> String {
    if !human { return format!("{}", bytes); }
    let units = ["", "K", "M", "G", "T"];
    let mut v = bytes;
    let mut idx = 0usize;
    let mut frac = 0u64;
    while v >= 1024 && idx + 1 < units.len() {
        frac = (v % 1024) * 10 / 1024;
        v /= 1024;
        idx += 1;
    }
    if idx == 0 {
        format!("{}", bytes)
    } else {
        format!("{}.{}{}", v, frac, units[idx])
    }
}

fn render_time(t: u64, now: u64) -> String {
    let half_year = 60u64 * 60 * 24 * 30 * 6;
    let recent = now > t && (now - t) < half_year;
    let (y, m, d, hh, mm) = unix_to_components(t);
    let months = ["Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"];
    let mon = months[(m as usize).saturating_sub(1).min(11)];
    if recent {
        format!("{} {:02} {:02}:{:02}", mon, d, hh, mm)
    } else {
        format!("{} {:02}  {}", mon, d, y)
    }
}

fn unix_to_components(t: u64) -> (u32, u32, u32, u32, u32) {
    let secs_in_day = 86_400u64;
    let days = t / secs_in_day;
    let rem  = t % secs_in_day;
    let hh = (rem / 3600) as u32;
    let mm = ((rem % 3600) / 60) as u32;

    // Civil date from days since 1970-01-01 (Proleptic Gregorian calendar)
    // Algorithm: http://howardhinnant.github.io/date_algorithms.html
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe/1460 + doe/36524 - doe/146_096) / 365;
    let y_off = yoe as i64 + era * 400;
    let doy = doe - (365*yoe + yoe/4 - yoe/100);
    let mp = (5*doy + 2) / 153;
    let d = (doy - (153*mp + 2)/5 + 1) as u32;
    let m = if mp < 10 { mp as u32 + 3 } else { mp as u32 - 9 };
    let y = (if m <= 2 { y_off + 1 } else { y_off }) as u32;
    (y, m, d, hh, mm)
}

fn entry_kind(mode: u32) -> &'static str {
    match mode & S_IFMT {
        S_IFDIR  => "d",
        S_IFLNK  => "l",
        _        => if mode & 0o111 != 0 { "x" } else { "f" },
    }
}

fn colorize(kind: &str, name: &str, enable: bool) -> String {
    if !enable { return String::from(name); }
    let prefix = match kind {
        "d" => "\x1b[1;34m",
        "x" => "\x1b[1;32m",
        "l" => "\x1b[1;36m",
        _   => "",
    };
    if prefix.is_empty() {
        String::from(name)
    } else {
        format!("{}{}\x1b[0m", prefix, name)
    }
}

/// Measure display width ignoring ANSI escape codes.
fn visible_len(s: &str) -> usize {
    let mut len = 0usize;
    let mut in_esc = false;
    for b in s.bytes() {
        if in_esc { if b == b'm' { in_esc = false; } continue; }
        if b == 0x1b { in_esc = true; continue; }
        len += 1;
    }
    len
}

fn column_layout(names: &[String], width: usize) -> String {
    if names.is_empty() { return String::new(); }
    let max = names.iter().map(|n| visible_len(n)).max().unwrap_or(0) + 2;
    let cols = (width / max.max(1)).max(1);
    let rows = (names.len() + cols - 1) / cols;
    let mut out = String::new();
    for r in 0..rows {
        for c in 0..cols {
            let idx = c * rows + r;
            if idx >= names.len() { break; }
            let n = &names[idx];
            out.push_str(n);
            if c + 1 < cols && idx + rows < names.len() {
                let pad = max - visible_len(n);
                for _ in 0..pad { out.push(' '); }
            }
        }
        out.push('\n');
    }
    out
}

// ──────────────────────────────────────────────────────────────────────
// Time helper
// ──────────────────────────────────────────────────────────────────────

fn current_unix_time() -> u64 {
    // Use clock_gettime CLOCK_REALTIME via C ABI.
    // If unavailable, return 0 (mtime display falls back to year-only form).
    #[repr(C)]
    struct Timespec { tv_sec: i64, tv_nsec: i64 }
    extern "C" {
        fn clock_gettime(clock_id: i32, tp: *mut Timespec) -> i32;
    }
    let mut ts = Timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe {
        clock_gettime(0 /* CLOCK_REALTIME */, &mut ts);
        if ts.tv_sec > 0 { ts.tv_sec as u64 } else { 0 }
    }
}

// ──────────────────────────────────────────────────────────────────────
// Env / TTY helpers
// ──────────────────────────────────────────────────────────────────────

fn getenv_str(name: &str) -> Option<String> {
    extern "C" { fn getenv(name: *const u8) -> *const u8; }
    let mut key = String::from(name);
    key.push('\0');
    unsafe {
        let ptr = getenv(key.as_ptr());
        if ptr.is_null() { return None; }
        let mut len = 0;
        while *ptr.add(len) != 0 { len += 1; }
        let bytes = core::slice::from_raw_parts(ptr, len);
        core::str::from_utf8(bytes).ok().map(String::from)
    }
}

fn stdout_is_tty() -> bool {
    extern "C" { fn isatty(fd: i32) -> i32; }
    unsafe { isatty(1) != 0 }
}

fn terminal_width() -> usize {
    getenv_str("COLUMNS")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(80)
}

fn color_enabled(mode: ColorMode) -> bool {
    match mode {
        ColorMode::Always => true,
        ColorMode::Never  => false,
        ColorMode::Auto   => {
            stdout_is_tty() && getenv_str("NO_COLOR").is_none()
        }
    }
}

// ──────────────────────────────────────────────────────────────────────
// Sort
// ──────────────────────────────────────────────────────────────────────

fn sort_entries(entries: &mut Vec<VfsDirEntry>, key: SortKey, reverse: bool) {
    match key {
        SortKey::Name  => entries.sort_by(|a, b| a.name.cmp(&b.name)),
        SortKey::Size  => entries.sort_by(|a, b| b.stat.size.cmp(&a.stat.size)),
        SortKey::Mtime => entries.sort_by(|a, b| b.stat.mtime.cmp(&a.stat.mtime)),
    }
    if reverse { entries.reverse(); }
}

// ──────────────────────────────────────────────────────────────────────
// Output helpers
// ──────────────────────────────────────────────────────────────────────

fn print_str(s: &str) {
    let _ = _write(1, s.as_ptr() as *const _, s.len());
}

fn eprint_str(s: &str) {
    let _ = _write(2, s.as_ptr() as *const _, s.len());
}

// ──────────────────────────────────────────────────────────────────────
// Listing logic
// ──────────────────────────────────────────────────────────────────────

/// List a single directory (non-recursive).
fn list_dir(
    vfs: &VfsClient,
    path: &str,
    opts: &LsOpts,
    color: bool,
    now: u64,
    width: usize,
    show_header: bool,
) {
    let mut entries = match vfs.readdir(path) {
        Ok(e) => e,
        Err(e) => {
            eprint_str(&format!("ls: {}: {:?}\n", path, e));
            return;
        }
    };

    // Filter dotfiles unless -a
    if !opts.all {
        entries.retain(|e| !e.name.starts_with('.'));
    }

    sort_entries(&mut entries, opts.sort, opts.reverse);

    if show_header {
        print_str(&format!("{}:\n", path));
    }

    if opts.long {
        print_long(&entries, opts, color, now);
    } else if opts.one_col {
        for entry in &entries {
            let kind = entry_kind(entry.stat.mode);
            let name = colorize(kind, &entry.name, color);
            print_str(&name);
            print_str("\n");
        }
    } else {
        let names: Vec<String> = entries.iter().map(|e| {
            let kind = entry_kind(e.stat.mode);
            colorize(kind, &e.name, color)
        }).collect();
        print_str(&column_layout(&names, width));
    }
}

fn print_long(entries: &[VfsDirEntry], opts: &LsOpts, color: bool, now: u64) {
    // Pre-compute column widths.
    let nlink_w = entries.iter().map(|e| digit_count(e.stat.nlink as u64)).max().unwrap_or(1);
    let uid_w   = entries.iter().map(|e| digit_count(e.stat.uid as u64)).max().unwrap_or(1);
    let gid_w   = entries.iter().map(|e| digit_count(e.stat.gid as u64)).max().unwrap_or(1);
    let size_w  = entries.iter().map(|e| render_size(e.stat.size, opts.human).len()).max().unwrap_or(1);

    for entry in entries {
        let mode_str = render_mode(entry.stat.mode);
        let size_str = render_size(entry.stat.size, opts.human);
        let time_str = render_time(entry.stat.mtime, now);
        let kind = entry_kind(entry.stat.mode);
        let name = colorize(kind, &entry.name, color);
        let line = format!(
            "{} {:>nlink$} {:>uid$} {:>gid$} {:>size$} {} {}\n",
            mode_str,
            entry.stat.nlink,
            entry.stat.uid,
            entry.stat.gid,
            size_str,
            time_str,
            name,
            nlink = nlink_w,
            uid = uid_w,
            gid = gid_w,
            size = size_w,
        );
        print_str(&line);
    }
}

fn digit_count(n: u64) -> usize {
    if n == 0 { return 1; }
    let mut d = 0;
    let mut v = n;
    while v > 0 { v /= 10; d += 1; }
    d
}

fn list_recursive(
    vfs: &VfsClient,
    path: &str,
    opts: &LsOpts,
    color: bool,
    now: u64,
    width: usize,
    depth: usize,
) {
    if depth > 16 { return; } // guard against infinite loops

    list_dir(vfs, path, opts, color, now, width, true);

    // Recurse into subdirectories.
    let mut entries = match vfs.readdir(path) {
        Ok(e) => e,
        Err(_) => return,
    };
    if !opts.all {
        entries.retain(|e| !e.name.starts_with('.'));
    }
    sort_entries(&mut entries, opts.sort, opts.reverse);

    for entry in &entries {
        if entry.is_dir {
            let sub = if path == "/" {
                format!("/{}", entry.name)
            } else {
                format!("{}/{}", path.trim_end_matches('/'), entry.name)
            };
            print_str("\n");
            list_recursive(vfs, &sub, opts, color, now, width, depth + 1);
        }
    }
}

// ──────────────────────────────────────────────────────────────────────
// main
// ──────────────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let argv = libcluu::args::args();
    let _ = debug_print(&format!("ls: enter argc={}", argv.len()));

    let mut opts = LsOpts::default();
    let mut paths: Vec<String> = Vec::new();

    let mut i = 1usize;
    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            i += 1;
            while i < argv.len() {
                paths.push(argv[i].clone());
                i += 1;
            }
            break;
        }
        if let Some(rest) = a.strip_prefix("--") {
            match rest {
                "all"            => opts.all = true,
                "long"           => opts.long = true,
                "human-readable" => opts.human = true,
                "recursive"      => opts.recursive = true,
                "color" | "color=auto"   => opts.color_mode = ColorMode::Auto,
                "color=always"   => opts.color_mode = ColorMode::Always,
                "color=never"    => opts.color_mode = ColorMode::Never,
                "help"    => { print_help(); return 0; }
                "version" => { print_str("ls (CLUU) 2.0\n"); return 0; }
                _         => { eprint_str(&format!("ls: unknown option --{}\n", rest)); return 2; }
            }
        } else if a.starts_with('-') && a.len() > 1 {
            for c in a[1..].chars() {
                match c {
                    '1' => opts.one_col = true,
                    'l' => opts.long = true,
                    'a' => opts.all = true,
                    'h' => opts.human = true,
                    'R' => opts.recursive = true,
                    'S' => opts.sort = SortKey::Size,
                    't' => opts.sort = SortKey::Mtime,
                    'r' => opts.reverse = true,
                    _   => { eprint_str(&format!("ls: unknown option -{}\n", c)); return 2; }
                }
            }
        } else {
            paths.push(a.clone());
        }
        i += 1;
    }

    // Default path: cwd
    if paths.is_empty() {
        paths.push(libcluu::posix::current_dir_string());
    }

    // Connect to VFS
    let vfs_endpoint = match registry::subscribe_output("vfs", "main") {
        Ok(e) => e,
        Err(_) => { eprint_str("ls: vfs not available\n"); return 1; }
    };
    let vfs = match VfsClient::new_from_registry(vfs_endpoint) {
        Ok(c) => c,
        Err(_) => { eprint_str("ls: failed to create vfs client\n"); return 1; }
    };

    let color = color_enabled(opts.color_mode);
    let now = current_unix_time();
    let width = terminal_width();
    let multi = paths.len() > 1;

    let mut exit_code = 0i32;
    let _ = debug_print(&format!("ls: paths={} cwd-resolved", paths.len()));
    for (idx, raw_path) in paths.iter().enumerate() {
        let path = libcluu::posix::resolve_path(raw_path);
        let _ = debug_print(&format!("ls: listing '{}'", path));

        if idx > 0 { print_str("\n"); }

        if opts.recursive {
            list_recursive(&vfs, &path, &opts, color, now, width, 0);
        } else {
            list_dir(&vfs, &path, &opts, color, now, width, multi);
        }
    }

    let _ = debug_print(&format!("ls: ok (exit {})", exit_code));
    exit_code
}

fn print_help() {
    print_str(
        "Usage: ls [OPTIONS] [FILE]...\n\
         List directory contents.\n\
         \n\
         Options:\n\
           -1          one entry per line\n\
           -a          show hidden entries (starting with .)\n\
           -h          human-readable sizes (-l only)\n\
           -l          long listing format\n\
           -r          reverse sort order\n\
           -R          list directories recursively\n\
           -S          sort by size (largest first)\n\
           -t          sort by modification time (newest first)\n\
           --color=auto|always|never  color output\n"
    );
}
