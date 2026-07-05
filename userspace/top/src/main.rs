//! `top` — live process monitor reading from /proc.
//!
//! Reads /proc/<tid>/stat for each TID, builds a container hierarchy tree
//! from cid/pcid, and renders a live updating display with htop-style
//! CPU/memory gauges and fixed-width aligned columns.
//!
//! Output goes through POSIX `_write(1, ...)` → VFS → PTS_WRITE_LABEL →
//! cluuterm → compositor SHM. Input is not handled (Ctrl-C via shell signal).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[allow(unused_imports)]
use libcluu::runtime as _;

use libcluu::boot::{process_info, TOKEN_CLOCK, TOKEN_SPACE};
use libcluu::debug_print;
use libcluu::fs::client::VfsClient;
use libcluu::registry;

const GRANT_SIZE: usize = 4096;

const W_CID: usize = 5;
const W_PCID: usize = 5;
const W_NAME: usize = 30;
const W_PID: usize = 7;
const W_HEAP: usize = 7;
const W_MEM: usize = 7;
const W_CPU: usize = 6;
const W_ST: usize = 4;

const MIN_COLS_FOR_DUAL_GAUGE: usize = 60;

extern "C" {
    fn _write(fd: i32, buf: *const u8, n: usize) -> isize;
    fn usleep(usec: u32) -> i32;
    fn _ioctl(fd: i32, request: usize, argp: *mut core::ffi::c_void) -> i32;
}

#[repr(C)]
struct WinSize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

const TIOCGWINSZ: usize = 0x5413;

fn terminal_cols() -> usize {
    let mut ws = WinSize { ws_row: 0, ws_col: 0, ws_xpixel: 0, ws_ypixel: 0 };
    let rc = unsafe { _ioctl(1, TIOCGWINSZ, &mut ws as *mut _ as *mut core::ffi::c_void) };
    if rc == 0 && ws.ws_col > 0 {
        ws.ws_col as usize
    } else {
        80
    }
}

fn write_stdout(bytes: &[u8]) {
    const MAX_CHUNK: usize = 900;
    let mut pos = 0;
    while pos < bytes.len() {
        let end = (pos + MAX_CHUNK).min(bytes.len());
        let _ = unsafe { _write(1, bytes[pos..end].as_ptr(), end - pos) };
        pos = end;
    }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(()) => 0,
        Err(_) => 1,
    }
}

fn run() -> libcluu::Result<()> {
    debug_print("top: start")?;

    registry::init("top")?;
    libcluu::syscall::yield_cpu()?;

    let info = process_info();
    let clock_token = info.tokens[TOKEN_CLOCK];
    let space_token = info.tokens[TOKEN_SPACE];

    const SCHED_HZ: u64 = 250;
    let tsc_hz = libcluu::syscall::clock_frequency(clock_token).unwrap_or(1_000_000_000);

    let vfs_endpoint = match registry::subscribe_output("vfs", "main") {
        Ok(e) => e,
        Err(_) => {
            let _ = debug_print("top: vfs subscribe failed");
            return Err(libcluu::Error::NotFound);
        }
    };
    let vfs = match VfsClient::new_from_registry(vfs_endpoint) {
        Ok(c) => c,
        Err(_) => {
            let _ = debug_print("top: vfs client failed");
            return Err(libcluu::Error::NotFound);
        }
    };

    let grant_base = match libcluu::vspace::VSPACE.lock().alloc(GRANT_SIZE) {
        Ok(addr) => addr,
        Err(_) => return Err(libcluu::Error::OutOfMemory),
    };

    let mut prev_ticks: BTreeMap<u64, u64> = BTreeMap::new();
    let mut prev_frame_tsc: u64 = 0;
    let mut first_frame = true;

    write_stdout(b"\x1b[2J\x1b[H\x1b[?25l");

    loop {
        let now_tsc = libcluu::syscall::clock_now(clock_token).unwrap_or(0);

        let cols = {
            let c = terminal_cols();
            c.min(120)
        };
        let cols = cols.saturating_sub(1);

        // Read system memory info (total/used in kB) from /proc/meminfo.
        let (mem_total_kb, mem_used_kb) =
            read_meminfo(&vfs, space_token, grant_base).unwrap_or((0, 0));

        let records = match read_all_proc_stats(&vfs, space_token, grant_base) {
            Ok(r) => r,
            Err(_) => break,
        };

        // Build container hierarchy tree from cid/pcid.
        let mut children_map: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
        let mut cid_set: BTreeMap<u64, usize> = BTreeMap::new();
        for (idx, rec) in records.iter().enumerate() {
            if rec.cid != 0 {
                cid_set.insert(rec.cid, idx);
            }
            if rec.pcid != 0 {
                children_map
                    .entry(rec.pcid)
                    .or_insert_with(Vec::new)
                    .push(idx);
            }
        }

        let mut roots: Vec<usize> = Vec::new();
        for (idx, rec) in records.iter().enumerate() {
            if rec.pcid == 0 || !cid_set.contains_key(&rec.pcid) {
                roots.push(idx);
            }
        }
        roots.sort_unstable_by_key(|&i| records[i].cid);

        let mut ordered: Vec<DfsEntry> = Vec::new();
        let mut visited: BTreeMap<usize, ()> = BTreeMap::new();
        for (i, &root_idx) in roots.iter().enumerate() {
            let is_last = i == roots.len() - 1;
            dfs(root_idx, "", is_last, true, &records, &children_map, &mut ordered, &mut visited);
        }

        // System CPU%: sum of per-process tick deltas / elapsed sched ticks.
        let sys_cpu_pct = if first_frame || prev_frame_tsc == 0 {
            0u32
        } else {
            let total_delta: u64 = records
                .iter()
                .map(|r| r.cpu_ticks.saturating_sub(prev_ticks.get(&r.cid).copied().unwrap_or(0)))
                .sum();
            let elapsed_tsc = now_tsc.saturating_sub(prev_frame_tsc);
            let elapsed_sched = (elapsed_tsc * SCHED_HZ / tsc_hz.max(1)).max(1);
            ((total_delta * 100) / elapsed_sched).min(100) as u32
        };

        let mem_pct = if mem_total_kb > 0 {
            ((mem_used_kb * 100) / mem_total_kb).min(100) as u32
        } else {
            0
        };

        let mut frame = String::new();
        frame.push_str("\x1b[H");

        // Title bar.
        frame.push_str(&format!(
            "\x1b[97;44m CLUU top   Processes: {}",
            records.len()
        ));
        let hdr_content_len = 23 + digit_count(records.len());
        for _ in hdr_content_len..cols {
            frame.push(' ');
        }
        frame.push_str("\x1b[K\x1b[0m\n");

        // htop-style CPU + memory gauges (█/░ block elements via the u32
        // codepoint pipeline). Bar widths scale to terminal width.
        let cpu_color = gauge_color(sys_cpu_pct);
        let mem_color = gauge_color(mem_pct);
        let cpu_pct_str = format!("{}%", sys_cpu_pct);
        let mem_str = format!("{}/{}", format_mem_kb(mem_used_kb), format_mem_kb(mem_total_kb));

        if cols >= MIN_COLS_FOR_DUAL_GAUGE {
            // Visible layout: "CPU [bar] PCT  Mem [bar] MEM"
            // Fixed: "CPU ["=5 "] "=2 PCT=4 "  "=2 "Mem ["=5 "] "=2 MEM=var
            let overhead = 5 + 2 + 4 + 2 + 5 + 2 + mem_str.len();
            let remaining = cols.saturating_sub(overhead);
            let bar_w = remaining / 2;
            let cpu_bar = render_bar(sys_cpu_pct, bar_w);
            let mem_bar = render_bar(mem_pct, bar_w);
            frame.push_str(&format!(
                "\x1b[97mCPU\x1b[0m {}[{}]\x1b[0m {:<4}  \x1b[97mMem\x1b[0m {}[{}]\x1b[0m {}\x1b[K\n",
                cpu_color, cpu_bar, cpu_pct_str,
                mem_color, mem_bar, mem_str,
            ));
        } else {
            let overhead = 5 + 2 + 4;
            let bar_w = cols.saturating_sub(overhead);
            let cpu_bar = render_bar(sys_cpu_pct, bar_w);
            frame.push_str(&format!(
                "\x1b[97mCPU\x1b[0m {}[{}]\x1b[0m {}\x1b[K\n",
                cpu_color, cpu_bar, cpu_pct_str,
            ));
        }

        // Column header — widths match data rows exactly.
        frame.push_str(&format!(
            "\x1b[97m{:>W_CID$} {:>W_PCID$} {:<W_NAME$} {:>W_PID$} {:>W_HEAP$} {:>W_MEM$} {:>W_CPU$} {:<W_ST$}\x1b[K\x1b[0m\n",
            "CID", "PCID", "NAME", "PID", "HEAP", "MEM", "CPU%", "ST",
        ));
        frame.push_str("\x1b[0m");
        for _ in 0..cols {
            frame.push('-');
        }
        frame.push_str("\x1b[K\n");

        // Data rows.
        for entry in &ordered {
            let rec = &records[entry.idx];

            let color = if rec.state == "R" {
                "\x1b[36m"
            } else if rec.name.starts_with("su:") {
                "\x1b[32m"
            } else if rec.name.starts_with("sudo:") {
                "\x1b[33m"
            } else if rec.state == "Z" {
                "\x1b[91m"
            } else {
                "\x1b[0m"
            };

            let cid_str = format!("{:>1$}", rec.cid, W_CID);
            let pcid_str = if rec.pcid == 0 {
                format!("{:>1$}", '-', W_PCID)
            } else {
                format!("{:>1$}", rec.pcid, W_PCID)
            };

            let full_name = format!("{}{}", entry.prefix, rec.name);
            let name_str = fit_chars(&full_name, W_NAME);

            let pid_str = format!("{:>1$}", rec.pid, W_PID);

            let heap_str = if rec.heap_pages == 0 {
                String::from("---")
            } else {
                format_mem_kb(rec.heap_pages as u64 * 4)
            };
            let heap_col = format!("{:>1$}", heap_str, W_HEAP);

            let mem_str = if rec.heap_pages == 0 && rec.other_pages == 0 {
                String::from("---")
            } else {
                format_mem_kb((rec.heap_pages as u64 + rec.other_pages as u64) * 4)
            };
            let mem_col = format!("{:>1$}", mem_str, W_MEM);

            let cpu_str = if first_frame || prev_frame_tsc == 0 {
                String::from("---")
            } else {
                let prev = prev_ticks.get(&rec.cid).copied().unwrap_or(0);
                let delta = rec.cpu_ticks.saturating_sub(prev);
                let elapsed_tsc = now_tsc.saturating_sub(prev_frame_tsc);
                let elapsed_sched = (elapsed_tsc * SCHED_HZ / tsc_hz.max(1)).max(1);
                let pct = (delta * 100 / elapsed_sched).min(100);
                format!("{}%", pct)
            };
            let cpu_col = format!("{:>1$}", cpu_str, W_CPU);

            let st_str = match rec.state.as_str() {
                "R" => "RUN ",
                "Z" => "DEAD",
                _ => "UN  ",
            };

            frame.push_str(&format!(
                "{}{} {} {} {} {} {} {} {}\x1b[K\x1b[0m\n",
                color, cid_str, pcid_str, name_str, pid_str, heap_col, mem_col, cpu_col, st_str
            ));
        }

        frame.push_str(
            "\x1b[90m Ctrl-C to quit                                   1s refresh \x1b[K\x1b[0m\n",
        );
        frame.push_str("\x1b[J");

        write_stdout(frame.as_bytes());

        prev_ticks.clear();
        for rec in &records {
            prev_ticks.insert(rec.cid, rec.cpu_ticks);
        }
        prev_frame_tsc = now_tsc;
        first_frame = false;

        unsafe { let _ = usleep(1_000_000); }
    }

    let _ = libcluu::vspace::VSPACE.lock().free(grant_base, GRANT_SIZE);
    write_stdout(b"\x1b[2J\x1b[H\x1b[?25h");
    Ok(())
}

struct ProcRecord {
    cid: u64,
    pcid: u64,
    pid: u64,
    name: String,
    state: String,
    cpu_ticks: u64,
    heap_pages: u32,
    other_pages: u32,
}

fn read_all_proc_stats(
    vfs: &VfsClient,
    space_token: usize,
    grant_base: usize,
) -> libcluu::Result<Vec<ProcRecord>> {
    let entries = vfs.readdir("/proc")?;

    let mut records: Vec<ProcRecord> = Vec::new();

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
        let grant = match vfs.read_grant(file, 0, read_size, space_token, grant_base) {
            Ok(g) => g,
            Err(_) => {
                let _ = vfs.close(file);
                continue;
            }
        };

        if grant.len > 0 && grant.offset + grant.len <= GRANT_SIZE {
            let addr = grant_base + grant.offset;
            let data = unsafe { core::slice::from_raw_parts(addr as *const u8, grant.len) };
            let text = core::str::from_utf8(data).unwrap_or("").trim();
            if let Some(rec) = parse_stat_line(text, &entry.name) {
                records.push(rec);
            }
        }

        let _ = vfs.close(file);
    }

    Ok(records)
}

fn parse_stat_line(text: &str, tid: &str) -> Option<ProcRecord> {
    let paren_open = text.find('(')?;
    let paren_close = text.rfind(')')?;
    let pid = text[..paren_open].trim().parse::<u64>().ok()?;
    let raw_name = text[paren_open + 1..paren_close].to_string();
    // Never display ? / empty / ??? names — substitute a stable tid reference.
    let name = if raw_name.is_empty() || raw_name.chars().all(|c| c == '?') {
        format!("[tid:{}]", tid)
    } else {
        raw_name
    };
    let rest = text[paren_close + 1..].trim();
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.len() < 4 {
        return None;
    }
    let state = parts[0].to_string();
    let cpu_ticks = parts[1].parse::<u64>().unwrap_or(0);
    let heap_pages = parts[2].parse::<u32>().unwrap_or(0);
    let other_pages = parts.get(3).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
    let cid: u64 = parts.get(6).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
    let pcid: u64 = parts.get(7).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);

    Some(ProcRecord {
        cid,
        pcid,
        pid,
        name,
        state,
        cpu_ticks,
        heap_pages,
        other_pages,
    })
}

struct DfsEntry {
    idx: usize,
    prefix: String,
}
fn dfs(
    idx: usize,
    prefix: &str,
    is_last: bool,
    is_root: bool,
    records: &[ProcRecord],
    children_map: &BTreeMap<u64, Vec<usize>>,
    ordered: &mut Vec<DfsEntry>,
    visited: &mut BTreeMap<usize, ()>,
) {
    if visited.contains_key(&idx) {
        return;
    }
    visited.insert(idx, ());

    let connector = if is_root {
        String::new()
    } else if is_last {
        format!("{}\u{2514}\u{2500}\u{2500} ", prefix)
    } else {
        format!("{}\u{251C}\u{2500}\u{2500} ", prefix)
    };
    ordered.push(DfsEntry {
        idx,
        prefix: connector,
    });

    let cid = records[idx].cid;
    if let Some(kids) = children_map.get(&cid) {
        let mut sorted = kids.clone();
        sorted.sort_unstable_by_key(|&i| records[i].cid);
        for (i, &kid) in sorted.iter().enumerate() {
            let child_prefix = if is_root {
                String::new()
            } else if is_last {
                format!("{}    ", prefix)
            } else {
                format!("{}\u{2502}   ", prefix)
            };
            let kid_is_last = i == sorted.len() - 1;
            dfs(kid, &child_prefix, kid_is_last, false, records, children_map, ordered, visited);
        }
    }
}

fn digit_count(n: usize) -> usize {
    if n == 0 {
        return 1;
    }
    let mut count = 0;
    let mut v = n;
    while v > 0 {
        v /= 10;
        count += 1;
    }
    count
}

/// Read /proc/meminfo and return (total_kb, used_kb).
fn read_meminfo(vfs: &VfsClient, space_token: usize, grant_base: usize) -> Option<(u64, u64)> {
    let file = vfs.open("/proc/meminfo").ok()?;
    if file.size == 0 {
        let _ = vfs.close(file);
        return None;
    }
    let read_size = file.size.min(GRANT_SIZE);
    let grant = vfs.read_grant(file, 0, read_size, space_token, grant_base).ok();
    let result = grant.and_then(|g| {
        if g.len > 0 && g.offset + g.len <= GRANT_SIZE {
            let addr = grant_base + g.offset;
            let data = unsafe { core::slice::from_raw_parts(addr as *const u8, g.len) };
            let text = core::str::from_utf8(data).unwrap_or("");
            parse_meminfo(text)
        } else {
            None
        }
    });
    let _ = vfs.close(file);
    result
}

fn parse_meminfo(text: &str) -> Option<(u64, u64)> {
    let mut total: Option<u64> = None;
    let mut used: Option<u64> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("MemTotal:") {
            total = parse_kb(rest);
        } else if let Some(rest) = trimmed.strip_prefix("MemUsed:") {
            used = parse_kb(rest);
        }
    }
    total.zip(used)
}

fn parse_kb(s: &str) -> Option<u64> {
    s.trim()
        .trim_end_matches("kB")
        .trim()
        .parse::<u64>()
        .ok()
}

/// Format kB as a human-readable string: 64K, 128M, 2G.
fn format_mem_kb(kb: u64) -> String {
    if kb >= 1024 * 1024 {
        format!("{}G", kb / (1024 * 1024))
    } else if kb >= 1024 {
        format!("{}M", kb / 1024)
    } else {
        format!("{}K", kb)
    }
}

fn render_bar(pct: u32, width: usize) -> String {
    let filled = ((pct as usize) * width / 100).min(width);
    let empty = width - filled;
    let mut bar = String::with_capacity(width);
    for _ in 0..filled {
        bar.push('\u{2588}');
    }
    for _ in 0..empty {
        bar.push('\u{2591}');
    }
    bar
}

/// Green under 50%, yellow under 80%, red at/above 80%.
fn gauge_color(pct: u32) -> &'static str {
    if pct < 50 {
        "\x1b[32m"
    } else if pct < 80 {
        "\x1b[33m"
    } else {
        "\x1b[31m"
    }
}

/// Truncate or pad `s` to exactly `width` display characters (char-safe for
/// multi-byte UTF-8 like the tree connectors └├│).
fn fit_chars(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut count = 0usize;
    for c in s.chars() {
        if count >= width {
            break;
        }
        out.push(c);
        count += 1;
    }
    while count < width {
        out.push(' ');
        count += 1;
    }
    out
}
