#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::mem::size_of;

#[allow(unused_imports)]
use libcluu::runtime as _;

use libcluu::boot::{process_info, PARAM_FB_HEIGHT, PARAM_FB_WIDTH, TOKEN_CLOCK, TOKEN_STDIN, TOKEN_STDOUT};
use libcluu::error::Error;
use libcluu::ipc::{
    call_with_reply_buf, parse_message, send_with_payload, PROCMGR_CONTAINER_STATS_LABEL,
    PROCMGR_QUERY_CTTY_LABEL, TTY_READ_LABEL, TTY_WRITE_LABEL,
};
use libcluu::types::Message;
use libcluu::{debug_print, registry, syscall};

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
    syscall::yield_cpu()?;

    let procmgr_endpoint = registry::subscribe_output("procmgr", "spawn")?;
    let info = process_info();
    let stdout = info.tokens[TOKEN_STDOUT];
    let stdin = info.tokens[TOKEN_STDIN];
    let clock_token = info.tokens[TOKEN_CLOCK];

    // Terminal width from framebuffer params (cols = fb_width / 8px-per-glyph).
    // Falls back to 80 if procmgr has not yet populated the params.
    let fb_width = info.params[PARAM_FB_WIDTH] as u32;
    let fb_height = info.params[PARAM_FB_HEIGHT] as u32;
    let cols = if fb_width > 0 { (fb_width / 8) as usize } else { 80 };
    let _rows = if fb_height > 0 { (fb_height / 16) as usize } else { 25 };

    // TSC frequency for CPU% normalisation (scheduler runs at 250 Hz).
    const SCHED_HZ: u64 = 250;
    let tsc_hz = syscall::clock_frequency(clock_token).unwrap_or(1_000_000_000);

    // Query our controlling terminal (VT) index once at startup.
    let my_vt: u8 = {
        let qmsg = Message::new(PROCMGR_QUERY_CTTY_LABEL, [0; 6], 0);
        let mut rbuf = [0u8; 64];
        match call_with_reply_buf(procmgr_endpoint, &qmsg, &[], &mut rbuf) {
            Ok((reply, _)) => reply.words[0] as u8,
            Err(_) => 0,
        }
    };

    let mut prev_ticks: BTreeMap<u64, u64> = BTreeMap::new();
    let mut prev_frame_tsc: u64 = 0;
    let mut first_frame = true;

    // Clear screen
    send_with_payload(stdout, TTY_WRITE_LABEL, b"\x1b[2J\x1b[H")?;

    loop {
        // 1. Query procmgr stats
        let msg = Message::new(PROCMGR_CONTAINER_STATS_LABEL, [0; 6], 0);
        let mut reply_buf = [0u8; 4096];
        let (reply_msg, payload_len) = match call_with_reply_buf(
            procmgr_endpoint,
            &msg,
            &[],
            &mut reply_buf,
        ) {
            Ok(r) => r,
            Err(_) => break,
        };

        let record_count = reply_msg.words[1];
        let total_containers = reply_msg.words[2];
        let total_sessions = reply_msg.words[3];

        // 2. Parse records
        let hdr_len = size_of::<Message>();
        let payload = &reply_buf[hdr_len..hdr_len + payload_len];
        let mut records: Vec<TopRecord> = Vec::new();
        for i in 0..record_count {
            let off = i * 64;
            if off + 64 <= payload.len() {
                records.push(TopRecord::parse(&payload[off..off + 64]));
            }
        }

        // 3. Build tree: parent → children, DFS traversal
        let mut children_map: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
        let mut cid_set: BTreeMap<u64, usize> = BTreeMap::new();
        for (idx, rec) in records.iter().enumerate() {
            cid_set.insert(rec.cid, idx);
            children_map
                .entry(rec.parent_cid)
                .or_insert_with(Vec::new)
                .push(idx);
        }

        // Find roots: parent_cid == 0 or parent not in cid_set
        let mut roots: Vec<usize> = Vec::new();
        for (idx, rec) in records.iter().enumerate() {
            if rec.parent_cid == 0 || !cid_set.contains_key(&rec.parent_cid) {
                roots.push(idx);
            }
        }
        roots.sort_by_key(|&i| records[i].cid);

        // DFS to build ordered list with tree prefixes
        let mut ordered: Vec<DfsEntry> = Vec::new();
        for (i, &root_idx) in roots.iter().enumerate() {
            let is_last = i == roots.len() - 1;
            dfs(
                root_idx,
                "",
                is_last,
                true,
                &records,
                &children_map,
                &mut ordered,
            );
        }

        // Sample TSC at start of render for CPU% denominator.
        let now_tsc = syscall::clock_now(clock_token).unwrap_or(0);

        // 4. Render frame
        let mut frame = String::new();
        frame.push_str("\x1b[H");

        // Header line (blue bg, bright white)
        frame.push_str(&format!(
            "\x1b[97;44m CLUU top   Containers: {}  Sessions: {}  [VT:{}]",
            total_containers, total_sessions, my_vt
        ));
        let hdr_content_len = 43
            + digit_count(total_containers)
            + digit_count(total_sessions)
            + digit_count(my_vt as usize);
        for _ in hdr_content_len..cols {
            frame.push(' ');
        }
        frame.push_str("\x1b[K\x1b[0m\n");

        // Column headers — CID, PCID, NAME, PID, PROFILE, HEAP, MEM, CPU%, ST
        frame.push_str(
            "\x1b[97m CID PCID NAME                  PID   PROFILE  HEAP    MEM   CPU%  ST  \x1b[K\x1b[0m\n",
        );
        // Separator: full terminal width using ASCII dash to avoid UTF-8 splitting
        frame.push_str("\x1b[0m");
        for _ in 0..cols {
            frame.push('-');
        }
        frame.push_str("\x1b[K\n");

        // Data rows
        for entry in &ordered {
            let rec = &records[entry.idx];
            let name_str = rec.name_str();

            let color = if rec.state == 3 {
                "\x1b[36m"
            } else if name_str.starts_with("su:") {
                "\x1b[32m"
            } else if name_str.starts_with("sudo:") {
                "\x1b[33m"
            } else if rec.state == 2 {
                "\x1b[91m"
            } else {
                "\x1b[0m"
            };

            let cid_str = format!("{:>4}", rec.cid);

            let pcid_str = if rec.parent_cid == 0 {
                String::from("   -")
            } else {
                format!("{:>4}", rec.parent_cid)
            };

            let full_name = format!("{}{}", entry.prefix, name_str);
            let display_name = if full_name.len() > 22 {
                &full_name[..22]
            } else {
                &full_name
            };

            let pid_str = if rec.pid == 0 {
                String::from("  -  ")
            } else {
                format!("{:>5}", rec.pid)
            };

            let profile_str = match rec.profile_bits {
                0x8F => String::from("admin    "),
                0xFF => String::from("super    "),
                0x0F => String::from("user     "),
                0x3F => String::from("service  "),
                0x1F => String::from("svc-dev  "),
                0x01 => String::from("sandbox  "),
                0 => String::from("  -      "),
                p => format!("0x{:04x}   ", p),
            };

            let heap_str = if rec.pid == 0 {
                String::from(" ---  ")
            } else {
                let kb = rec.heap_pages as usize * 4;
                if kb >= 1024 {
                    format!("{:>4}M ", kb / 1024)
                } else {
                    format!("{:>4}K ", kb)
                }
            };

            let mem_str = if rec.pid == 0 {
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
                // Convert elapsed TSC ticks to scheduler ticks (250 Hz APIC timer).
                let elapsed_tsc = now_tsc.saturating_sub(prev_frame_tsc);
                let elapsed_sched = (elapsed_tsc * SCHED_HZ / tsc_hz.max(1)).max(1);
                let pct = (delta * 100 / elapsed_sched).min(100);
                format!("{:>5}%", pct)
            };

            let st_str = match rec.state {
                0 => "RUN ",
                1 => "WAIT",
                2 => "DEAD",
                3 => "SESS",
                _ => " ?  ",
            };

            frame.push_str(&format!(
                "{}{} {} {:<22} {} {} {} {} {} {}\x1b[K\x1b[0m\n",
                color, cid_str, pcid_str, display_name, pid_str, profile_str, heap_str, mem_str, cpu_str, st_str
            ));
        }

        // Footer
        frame.push_str(
            "\x1b[90m q=quit  r=refresh                                      1s refresh \x1b[K\x1b[0m\n",
        );
        frame.push_str("\x1b[J");

        // Write in chunks, splitting on char boundaries to avoid
        // breaking multi-byte UTF-8 sequences across IPC messages.
        send_utf8_chunks(stdout, &frame)?;

        // Update prev_ticks and frame timestamp for next iteration
        prev_ticks.clear();
        for rec in &records {
            prev_ticks.insert(rec.cid, rec.cpu_ticks);
        }
        prev_frame_tsc = now_tsc;
        first_frame = false;

        // 5. Wait for input with 1000ms timeout
        let tokens = [stdin];
        let mut input_buf = [0u8; 128];
        match syscall::ipc_recv_any(&tokens, &mut input_buf, 1000) {
            Ok((_index, len)) => {
                if let Some((imsg, ipayload)) = parse_message(&input_buf[..len]) {
                    if imsg.tag.label == TTY_READ_LABEL {
                        if ipayload.contains(&0x03) {
                            break;
                        }
                        if ipayload.contains(&b'q') || ipayload.contains(&b'Q') {
                            break;
                        }
                    }
                }
            }
            Err(Error::Timeout) => { /* continue refresh loop */ }
            Err(_) => break,
        }
    }

    // Restore screen
    send_with_payload(stdout, TTY_WRITE_LABEL, b"\x1b[2J\x1b[H")?;
    Ok(())
}

/// Send a UTF-8 string in IPC-safe chunks (max 456 bytes payload),
/// never splitting a multi-byte character across chunks.
fn send_utf8_chunks(stdout: usize, s: &str) -> libcluu::Result<()> {
    const MAX_CHUNK: usize = 456;
    let bytes = s.as_bytes();
    let mut pos = 0;
    while pos < bytes.len() {
        let remaining = bytes.len() - pos;
        if remaining <= MAX_CHUNK {
            send_with_payload(stdout, TTY_WRITE_LABEL, &bytes[pos..])?;
            break;
        }
        // Back up from MAX_CHUNK boundary to a char boundary
        let mut end = pos + MAX_CHUNK;
        while end > pos && !is_utf8_char_start(bytes[end]) {
            end -= 1;
        }
        if end == pos {
            // Pathological: single char > 456 bytes; shouldn't happen, send raw
            end = pos + MAX_CHUNK;
        }
        send_with_payload(stdout, TTY_WRITE_LABEL, &bytes[pos..end])?;
        pos = end;
    }
    Ok(())
}

/// Check if a byte is the start of a UTF-8 character (not a continuation byte).
fn is_utf8_char_start(b: u8) -> bool {
    // Continuation bytes are 10xxxxxx (0x80..0xBF)
    (b & 0xC0) != 0x80
}

// ---------------------------------------------------------------------------
// Record parsing & tree helpers
// ---------------------------------------------------------------------------

struct TopRecord {
    cid: u64,
    parent_cid: u64,
    pid: u64,
    profile_bits: u16,
    state: u8,
    vt_index: u8,
    heap_pages: u16,
    other_pages: u16, // code + stack
    cpu_ticks: u64,
    name: [u8; 24],
}

impl TopRecord {
    fn parse(data: &[u8]) -> Self {
        let mut r = Self {
            cid: 0,
            parent_cid: 0,
            pid: 0,
            profile_bits: 0,
            state: 0,
            vt_index: 0xFF,
            heap_pages: 0,
            other_pages: 0,
            cpu_ticks: 0,
            name: [0u8; 24],
        };
        if data.len() >= 64 {
            r.cid = u64::from_le_bytes([
                data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
            ]);
            r.parent_cid = u64::from_le_bytes([
                data[8], data[9], data[10], data[11], data[12], data[13], data[14], data[15],
            ]);
            r.pid = u64::from_le_bytes([
                data[16], data[17], data[18], data[19], data[20], data[21], data[22], data[23],
            ]);
            r.profile_bits = u16::from_le_bytes([data[24], data[25]]);
            r.state = data[26];
            r.vt_index = data[27];
            r.heap_pages = u16::from_le_bytes([data[28], data[29]]);
            r.other_pages = u16::from_le_bytes([data[30], data[31]]);
            r.cpu_ticks = u64::from_le_bytes([
                data[32], data[33], data[34], data[35], data[36], data[37], data[38], data[39],
            ]);
            r.name[..24].copy_from_slice(&data[40..64]);
        }
        r
    }

    fn name_str(&self) -> &str {
        let end = self.name.iter().position(|&b| b == 0).unwrap_or(24);
        core::str::from_utf8(&self.name[..end]).unwrap_or("?")
    }
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
    records: &[TopRecord],
    children_map: &BTreeMap<u64, Vec<usize>>,
    ordered: &mut Vec<DfsEntry>,
) {
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
        sorted.sort_by_key(|&i| records[i].cid);
        for (i, &kid) in sorted.iter().enumerate() {
            let child_prefix = if is_root {
                String::new()
            } else if is_last {
                format!("{}  ", prefix)
            } else {
                format!("{}\u{2502} ", prefix)
            };
            let kid_is_last = i == sorted.len() - 1;
            dfs(kid, &child_prefix, kid_is_last, false, records, children_map, ordered);
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
