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

    // GNU-close fixed-width columns, whitespace-separated. Default carries
    // the CLUU-specific PPID / SID / CID columns so `ps | grep CID` lookups
    // work without -f. -f adds the BSD/GNU long form. -l keeps the verbose
    // memory/CPU layout.
    if long_format {
        write_fd(1, b"  PID NAME             STATE      CPU  HEAP OTHER\n");
    } else if full_format {
        write_fd(1, b"UID         PID  PPID  SID  CID PCID STATE      TIME CMD\n");
    } else {
        write_fd(1, b"  PID  PPID  SID  CID PCID STATE      TIME CMD\n");
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
                            "{:<10} {:>4} {:>5} {:>4} {:>4} {:>4} {:<5} {:>9} {}\n",
                            "root",
                            line.pid,
                            line.ppid,
                            line.sid,
                            line.cid,
                            line.pcid,
                            line.state,
                            line.cpu_ticks,
                            line.name,
                        )
                    } else {
                        format!(
                            "{:>5} {:>5} {:>4} {:>4} {:>4} {:<5} {:>9} {}\n",
                            line.pid,
                            line.ppid,
                            line.sid,
                            line.cid,
                            line.pcid,
                            line.state,
                            line.cpu_ticks,
                            line.name,
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
    ppid: String,
    sid: String,
    cid: String,
    pcid: String,
}

fn parse_stat_line(text: &str) -> Option<StatLine> {
    // CLUU stat layout (extended past the original 6 fields):
    //   pid (name) state cpu_ticks heap_pages other_pages ppid sid cid pcid
    // Older procmgr builds emit only the first 6 fields; missing trailing
    // columns default to "0" so older logs / mixed-version trees still parse.
    let paren_open = text.find('(')?;
    let paren_close = text.rfind(')')?;
    let pid = text[..paren_open].trim().to_string();
    let name = text[paren_open + 1..paren_close].to_string();
    let rest = text[paren_close + 1..].trim();
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.len() < 4 {
        return None;
    }
    let state = parts[0].to_string();
    let cpu_ticks = parts[1].to_string();
    let heap_pages = parts[2].parse::<u32>().unwrap_or(0);
    let other_pages = parts.get(3).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
    let ppid = parts.get(4).map(|s| s.to_string()).unwrap_or_else(|| "0".to_string());
    let sid = parts.get(5).map(|s| s.to_string()).unwrap_or_else(|| "0".to_string());
    let cid = parts.get(6).map(|s| s.to_string()).unwrap_or_else(|| "0".to_string());
    let pcid = parts.get(7).map(|s| s.to_string()).unwrap_or_else(|| "0".to_string());
    Some(StatLine { pid, name, state, cpu_ticks, heap_pages, other_pages, ppid, sid, cid, pcid })
}

fn write_fd(fd: i32, data: &[u8]) {
    let _ = _write(fd, data.as_ptr() as *const _, data.len());
}
