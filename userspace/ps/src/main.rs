//! `/bin/ps` — report process status.
//!
//! Flags: -e/-A (all procs), -f (full-format), -l (long), -u USER (by user)
//!
//! Data source: /proc/<pid>/stat via VFS → procmgr

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use libcluu::boot::{process_info, TOKEN_SPACE};
use libcluu::cli::{parse, render_help, CliError, Spec};
use libcluu::fs::client::VfsClient;
use libcluu::posix::_write;
use libcluu::registry;

const GRANT_SIZE: usize = 4096;

fn spec() -> Spec {
    Spec::new()
        .program("ps")
        .version("0.1.0")
        .usage("[-eAfl] [-u USER]")
        .flag('e', "every", "select all processes (same as -A)")
        .flag('A', "all", "select all processes")
        .flag('f', "full", "do full-format listing")
        .flag('l', "long", "long format")
        .required('u', "user", "select by effective user name")
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
            write_fd(1, b"ps 0.1.0\n");
            return 0;
        }
        Err(e) => {
            let msg = format!("ps: {}\n", e);
            write_fd(2, msg.as_bytes());
            return 2;
        }
    };

    let full_format = parsed.is_set("full");
    let long_format = parsed.is_set("long");
    // -u USER: filter by user name (best-effort; CLUU has no per-process user info yet)
    let filter_user = parsed.value("user");

    let Ok(vfs_endpoint) = registry::subscribe_output("vfs", "main") else {
        write_fd(2, b"ps: vfs not available\n");
        return 1;
    };
    let vfs = match VfsClient::new_from_registry(vfs_endpoint) {
        Ok(c) => c,
        Err(_) => {
            write_fd(2, b"ps: failed to connect to vfs\n");
            return 1;
        }
    };

    // GNU-style headers — fixed-width columns, plain whitespace-separated so
    // common pipelines (`ps | grep`, `ps | awk`) work. Default mirrors the
    // BSD/GNU `ps` short form: PID TTY TIME CMD. -f adds UID/PPID/C/STIME.
    // -l keeps the verbose CLUU-specific layout for now.
    if long_format {
        write_fd(1, b"  PID NAME             STATE      CPU  HEAP OTHER\n");
    } else if full_format {
        write_fd(1, b"UID        PID  PPID C STIME TTY          TIME CMD\n");
    } else {
        write_fd(1, b"  PID TTY          TIME CMD\n");
    }

    let entries = match vfs.readdir("/proc") {
        Ok(e) => e,
        Err(_) => {
            write_fd(2, b"ps: failed to read /proc\n");
            return 1;
        }
    };

    let info = process_info();
    let space_token = info.tokens[TOKEN_SPACE];
    let grant_base = match libcluu::vspace::VSPACE.lock().alloc(GRANT_SIZE) {
        Ok(addr) => addr,
        Err(_) => {
            write_fd(2, b"ps: out of virtual memory\n");
            return 1;
        }
    };

    for entry in &entries {
        if !entry.is_dir
            || entry.name.is_empty()
            || !entry.name.bytes().all(|b| b.is_ascii_digit())
        {
            continue;
        }

        let stat_path = format!("/proc/{}/stat", entry.name);
        let file = match vfs.open(&stat_path) {
            Ok(f) => f,
            Err(_) => continue,
        };
        if file.size == 0 {
            let _ = vfs.close(file);
            continue;
        }

        let read_size = file.size.min(GRANT_SIZE);
        if let Ok(grant) = vfs.read_grant(file, 0, read_size, space_token, grant_base) {
            if grant.len > 0 && grant.offset + grant.len <= GRANT_SIZE {
                let addr = grant.base + grant.offset;
                let data = unsafe {
                    core::slice::from_raw_parts(addr as *const u8, grant.len)
                };
                let text = core::str::from_utf8(data).unwrap_or("").trim();
                // Format: "pid (name) state cpu_ticks heap_pages other_pages"
                if let Some(line) = parse_stat_line(text) {
                    // -u filter: no per-process user info, so -u root shows all,
                    // -u anything-else shows nothing. Best-effort.
                    if let Some(user) = filter_user {
                        if user != "root" {
                            // CLUU has no user ACL per process — skip non-root filter.
                            continue;
                        }
                    }

                    let _mem_kb = (line.heap_pages as u64 + line.other_pages as u64) * 4;
                    // CLUU has no controlling-tty per-process model yet;
                    // ?+state means "no tty". Stub out -f STIME and CMD to
                    // mirror real ps formatting until those fields land.
                    let row = if long_format {
                        format!(
                            "{:>5} {:<16} {:>5}  {:>8}  {:>4}  {:>5}\n",
                            line.pid,
                            line.name,
                            line.state,
                            line.cpu_ticks,
                            line.heap_pages,
                            line.other_pages
                        )
                    } else if full_format {
                        format!(
                            "{:<10} {:>4} {:>5} {} {:<5} {:<10} {:>5} {}\n",
                            "root",
                            line.pid,
                            "?",
                            "0",
                            "?",
                            "?",
                            line.cpu_ticks,
                            line.name,
                        )
                    } else {
                        format!(
                            "{:>5} {:<12} {:>4} {}\n",
                            line.pid, "?", line.cpu_ticks, line.name,
                        )
                    };
                    write_fd(1, row.as_bytes());
                }
            }
        }

        let _ = vfs.close(file);
    }

    let _ = libcluu::vspace::VSPACE.lock().free(grant_base, GRANT_SIZE);
    0
}

struct StatLine {
    pid: String,
    name: String,
    state: String,
    cpu_ticks: String,
    heap_pages: u32,
    other_pages: u32,
}

fn parse_stat_line(text: &str) -> Option<StatLine> {
    // Format: "pid (name) state cpu_ticks heap_pages other_pages"
    // Split on the first '(' and last ')' to handle names with spaces.
    let paren_open = text.find('(')?;
    let paren_close = text.rfind(')')?;
    let pid = text[..paren_open].trim().to_string();
    let name = text[paren_open + 1..paren_close].to_string();
    let rest = text[paren_close + 1..].trim();
    // rest: "state cpu_ticks heap_pages other_pages"
    let parts: Vec<&str> = rest.splitn(4, ' ').collect();
    if parts.len() < 4 {
        // Try with fewer parts (e.g. missing other_pages).
        if parts.len() >= 3 {
            let state = parts[0].to_string();
            let cpu_ticks = parts[1].to_string();
            let heap_pages = parts[2].parse::<u32>().unwrap_or(0);
            return Some(StatLine { pid, name, state, cpu_ticks, heap_pages, other_pages: 0 });
        }
        return None;
    }
    let state = parts[0].to_string();
    let cpu_ticks = parts[1].to_string();
    let heap_pages = parts[2].parse::<u32>().unwrap_or(0);
    let other_pages = parts[3].trim().parse::<u32>().unwrap_or(0);
    Some(StatLine { pid, name, state, cpu_ticks, heap_pages, other_pages })
}

fn write_fd(fd: i32, data: &[u8]) {
    let _ = _write(fd, data.as_ptr() as *const _, data.len());
}
