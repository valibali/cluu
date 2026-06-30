//! `top` — live process monitor reading from /proc.
//!
//! Reads /proc/<tid>/stat for each TID, builds a container hierarchy tree
//! from cid/pcid, and renders a live updating display.
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

use libcluu::boot::{process_info, PARAM_FB_HEIGHT, PARAM_FB_WIDTH, TOKEN_CLOCK, TOKEN_SPACE};
use libcluu::debug_print;
use libcluu::fs::client::VfsClient;
use libcluu::registry;

const GRANT_SIZE: usize = 4096;

extern "C" {
    fn _write(fd: i32, buf: *const u8, n: usize) -> isize;
    fn usleep(usec: u32) -> i32;
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

    let fb_width = info.params[PARAM_FB_WIDTH] as u32;
    let _fb_height = info.params[PARAM_FB_HEIGHT] as u32;
    let cols = if fb_width > 0 { (fb_width / 8) as usize } else { 80 };

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

    write_stdout(b"\x1b[2J\x1b[H");

    loop {
        let now_tsc = libcluu::syscall::clock_now(clock_token).unwrap_or(0);

        let records = match read_all_proc_stats(&vfs, space_token, grant_base) {
            Ok(r) => r,
            Err(_) => break,
        };

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

        let mut frame = String::new();
        frame.push_str("\x1b[H");

        frame.push_str(&format!(
            "\x1b[97;44m CLUU top   Processes: {}",
            records.len()
        ));
        let hdr_content_len = 30 + digit_count(records.len());
        for _ in hdr_content_len..cols {
            frame.push(' ');
        }
        frame.push_str("\x1b[K\x1b[0m\n");

        frame.push_str(
            "\x1b[97m CID PCID NAME                  PID   HEAP    MEM   CPU%  ST  \x1b[K\x1b[0m\n",
        );
        frame.push_str("\x1b[0m");
        for _ in 0..cols {
            frame.push('-');
        }
        frame.push_str("\x1b[K\n");

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

            let cid_str = format!("{:>4}", rec.cid);
            let pcid_str = if rec.pcid == 0 {
                String::from("   -")
            } else {
                format!("{:>4}", rec.pcid)
            };

            let full_name = format!("{}{}", entry.prefix, rec.name);
            let display_name = if full_name.len() > 22 {
                &full_name[..22]
            } else {
                &full_name
            };

            let pid_str = format!("{:>5}", rec.pid);

            let heap_str = if rec.heap_pages == 0 {
                String::from(" ---  ")
            } else {
                let kb = rec.heap_pages as usize * 4;
                if kb >= 1024 {
                    format!("{:>4}M ", kb / 1024)
                } else {
                    format!("{:>4}K ", kb)
                }
            };

            let mem_str = if rec.heap_pages == 0 && rec.other_pages == 0 {
                String::from(" ---  ")
            } else {
                let kb = (rec.heap_pages as usize + rec.other_pages as usize) * 4;
                if kb >= 1024 {
                    format!("{:>4}M ", kb / 1024)
                } else {
                    format!("{:>4}K ", kb)
                }
            };

            let cpu_str = if first_frame || prev_frame_tsc == 0 {
                String::from("  --- ")
            } else {
                let prev = prev_ticks.get(&rec.cid).copied().unwrap_or(0);
                let delta = rec.cpu_ticks.saturating_sub(prev);
                let elapsed_tsc = now_tsc.saturating_sub(prev_frame_tsc);
                let elapsed_sched = (elapsed_tsc * SCHED_HZ / tsc_hz.max(1)).max(1);
                let pct = (delta * 100 / elapsed_sched).min(100);
                format!("{:>5}%", pct)
            };

            let st_str = match rec.state.as_str() {
                "R" => "RUN ",
                "Z" => "DEAD",
                _ => " ?  ",
            };

            frame.push_str(&format!(
                "{}{} {} {:<22} {} {} {} {} {}\x1b[K\x1b[0m\n",
                color, cid_str, pcid_str, display_name, pid_str, heap_str, mem_str, cpu_str, st_str
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
    write_stdout(b"\x1b[2J\x1b[H");
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
            if let Some(rec) = parse_stat_line(text) {
                records.push(rec);
            }
        }

        let _ = vfs.close(file);
    }

    Ok(records)
}

fn parse_stat_line(text: &str) -> Option<ProcRecord> {
    let paren_open = text.find('(')?;
    let paren_close = text.rfind(')')?;
    let pid = text[..paren_open].trim().parse::<u64>().ok()?;
    let name = text[paren_open + 1..paren_close].to_string();
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
        format!("{}\u{2514} ", prefix)
    } else {
        format!("{}\u{251C} ", prefix)
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
                format!("{}  ", prefix)
            } else {
                format!("{}\u{2502} ", prefix)
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
