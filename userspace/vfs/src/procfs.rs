//! Procfs virtual filesystem implementation.
//!
//! Provides system information through virtual files:
//! - /proc/version, uptime, meminfo, cpuinfo, mounts, fb: static generators
//! - /proc/self/{status,stat,cmdline}: per-caller process info via procmgr IPC
//! - /proc/<pid>/{status,stat,cmdline}: per-PID process info via procmgr IPC
//! - /proc readdir: static entries + PID directories from procmgr

use crate::fd_table::OpenFile;
use crate::mount::{DirEntry, MountBackend, VirtualFile};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::mem::size_of;
use libcluu::boot::{process_info, TOKEN_SELF};
use libcluu::ipc::{call_with_reply_buf, PROCMGR_PROC_QUERY_LABEL};
use libcluu::syscall::{
    pmm_get_stats, sched_get_overflow, thread_enumerate, PMM_STATS_TOTAL_FRAMES,
    PMM_STATS_USED_FRAMES, SCHED_OVERFLOW_DEFERRED_FAULT, SCHED_OVERFLOW_PENDING_WAKE,
};
use libcluu::types::Message;
use libcluu::{Error, Result};

/// Query type constants for procmgr IPC.
const QUERY_STATUS: usize = 0;
const QUERY_STAT: usize = 1;
const QUERY_CMDLINE: usize = 2;
const QUERY_COMM: usize = 4;
const QUERY_EXE: usize = 5;

/// Names of static /proc files (no procmgr IPC needed).
const STATIC_FILES: &[&str] = &[
    "version",
    "uptime",
    "meminfo",
    "cpuinfo",
    "mounts",
    "fb",
    "sched_overflow",
];

/// Per-PID sub-files available under /proc/<pid>/ and /proc/self/.
const PID_SUBFILES: &[&str] = &["status", "stat", "cmdline", "comm", "exe"];

// ─── Static content generators (unchanged from original) ───────────────────

fn gen_version() -> Result<Vec<u8>> {
    let version = String::from(
        "CLUU microkernel v0.1.0\n\
         Built with Rust (no_std)\n\
         Architecture: x86_64\n",
    );
    Ok(version.into_bytes())
}

fn gen_uptime() -> Result<Vec<u8>> {
    // Linux format: "<uptime_seconds>.<centi> <idle_seconds>.<centi>\n"
    // Idle time tracking isn't wired through the kernel scheduler yet, so we
    // report it as 0 for now. Once per-CPU idle counters land, plumb them
    // through TIME_GETCLOCK or a dedicated query and replace the second field.
    let (sec, nsec) = libcluu::time::query(libcluu::time::TIME_GETCLOCK)
        .unwrap_or((0, 0));
    let centi = (nsec / 10_000_000) as u32; // 0..99
    let text = format!("{}.{:02} 0.00\n", sec, centi);
    Ok(text.into_bytes())
}

fn gen_meminfo() -> Result<Vec<u8>> {
    let self_token = process_info().tokens[TOKEN_SELF];
    let used = pmm_get_stats(self_token, PMM_STATS_USED_FRAMES).unwrap_or(0);
    let total = pmm_get_stats(self_token, PMM_STATS_TOTAL_FRAMES).unwrap_or(0);
    let free = total.saturating_sub(used);
    // PAGE_SIZE = 4 KB, so frames * 4 = kB directly.
    let total_kb = total * 4;
    let free_kb = free * 4;
    let used_kb = used * 4;
    let text = format!(
        "MemTotal:       {:>8} kB\n\
         MemFree:        {:>8} kB\n\
         MemAvailable:   {:>8} kB\n\
         MemUsed:        {:>8} kB\n\
         Buffers:               0 kB\n\
         Cached:                0 kB\n",
        total_kb, free_kb, free_kb, used_kb,
    );
    Ok(text.into_bytes())
}

fn gen_cpuinfo() -> Result<Vec<u8>> {
    use core::arch::x86_64::__cpuid;

    let leaf0 = unsafe { __cpuid(0) };
    let mut vendor = [0u8; 12];
    vendor[0..4].copy_from_slice(&leaf0.ebx.to_le_bytes());
    vendor[4..8].copy_from_slice(&leaf0.edx.to_le_bytes());
    vendor[8..12].copy_from_slice(&leaf0.ecx.to_le_bytes());
    let vendor_str = core::str::from_utf8(&vendor).unwrap_or("?");

    let leaf1 = unsafe { __cpuid(1) };
    let stepping = leaf1.eax & 0xF;
    let base_model = (leaf1.eax >> 4) & 0xF;
    let base_family = (leaf1.eax >> 8) & 0xF;
    let ext_model = (leaf1.eax >> 16) & 0xF;
    let ext_family = (leaf1.eax >> 20) & 0xFF;
    let display_family = if base_family == 0xF { base_family + ext_family } else { base_family };
    let display_model = if base_family == 0x6 || base_family == 0xF {
        (ext_model << 4) | base_model
    } else {
        base_model
    };

    // Brand string: extended leaves 0x80000002-04 give 48 bytes ASCII.
    let ext_max = unsafe { __cpuid(0x80000000) }.eax;
    let mut brand = [0u8; 48];
    let brand_str = if ext_max >= 0x80000004 {
        for i in 0..3 {
            let r = unsafe { __cpuid(0x80000002 + i as u32) };
            let off = i * 16;
            brand[off..off + 4].copy_from_slice(&r.eax.to_le_bytes());
            brand[off + 4..off + 8].copy_from_slice(&r.ebx.to_le_bytes());
            brand[off + 8..off + 12].copy_from_slice(&r.ecx.to_le_bytes());
            brand[off + 12..off + 16].copy_from_slice(&r.edx.to_le_bytes());
        }
        let end = brand.iter().position(|&b| b == 0).unwrap_or(48);
        core::str::from_utf8(&brand[..end]).unwrap_or("unknown").trim()
    } else {
        "unknown"
    };

    // Decode a useful subset of feature flags from leaf 1 EDX/ECX.
    let mut flags = String::new();
    let edx_flags: &[(u32, &str)] = &[
        (0, "fpu"), (4, "tsc"), (5, "msr"), (9, "apic"),
        (15, "cmov"), (19, "clflush"), (23, "mmx"),
        (24, "fxsr"), (25, "sse"), (26, "sse2"),
    ];
    for &(bit, name) in edx_flags {
        if leaf1.edx & (1 << bit) != 0 {
            if !flags.is_empty() { flags.push(' '); }
            flags.push_str(name);
        }
    }
    let ecx_flags: &[(u32, &str)] = &[
        (0, "sse3"), (9, "ssse3"), (19, "sse4_1"), (20, "sse4_2"),
        (23, "popcnt"), (25, "aes"), (26, "xsave"), (28, "avx"),
        (30, "rdrand"), (31, "hypervisor"),
    ];
    for &(bit, name) in ecx_flags {
        if leaf1.ecx & (1 << bit) != 0 {
            if !flags.is_empty() { flags.push(' '); }
            flags.push_str(name);
        }
    }

    let text = format!(
        "processor\t: 0\n\
         vendor_id\t: {}\n\
         cpu family\t: {}\n\
         model\t\t: {}\n\
         stepping\t: {}\n\
         model name\t: {}\n\
         flags\t\t: {}\n\n",
        vendor_str, display_family, display_model, stepping, brand_str, flags,
    );
    Ok(text.into_bytes())
}

fn gen_mounts() -> Result<Vec<u8>> {
    let mounts = String::from(
        "/dev/initrd /dev/initrd initrd ro 0 0\n\
         /dev/vda /mnt/disk ext2 rw 0 0\n\
         proc /proc proc rw 0 0\n",
    );
    Ok(mounts.into_bytes())
}

pub struct FbInfo {
    pub phys: u64,
    pub size: u64,
    pub width: u64,
    pub height: u64,
    pub pitch: u64,
}

static mut FB_INFO: Option<FbInfo> = None;

pub fn set_fb_info(info: FbInfo) {
    unsafe {
        let ptr = &raw mut FB_INFO;
        *ptr = Some(info);
    }
}

fn gen_fb() -> Result<Vec<u8>> {
    let text = unsafe {
        let ptr = &raw const FB_INFO;
        match &*ptr {
            Some(fb) => format!(
                "phys=0x{:x}\nsize={}\nwidth={}\nheight={}\npitch={}\n",
                fb.phys, fb.size, fb.width, fb.height, fb.pitch
            ),
            None => String::from("# no framebuffer info available\n"),
        }
    };
    Ok(text.into_bytes())
}

/// Read scheduler overflow counters (H9 deferred-fault queue, H10 pending-wake
/// queue) directly from the kernel and format them as a tiny key=value file.
/// Each line carries one counter; both start at 0 on a healthy system.
fn gen_sched_overflow() -> Result<Vec<u8>> {
    let self_token = process_info().tokens[TOKEN_SELF];
    let deferred_fault = sched_get_overflow(self_token, SCHED_OVERFLOW_DEFERRED_FAULT).unwrap_or(0);
    let pending_wake = sched_get_overflow(self_token, SCHED_OVERFLOW_PENDING_WAKE).unwrap_or(0);
    let text = format!(
        "deferred_fault_overflow {}\npending_wake_overflow {}\n",
        deferred_fault, pending_wake
    );
    Ok(text.into_bytes())
}

fn gen_static(name: &str) -> Result<Vec<u8>> {
    match name {
        "version" => gen_version(),
        "uptime" => gen_uptime(),
        "meminfo" => gen_meminfo(),
        "cpuinfo" => gen_cpuinfo(),
        "mounts" => gen_mounts(),
        "fb" => gen_fb(),
        "sched_overflow" => gen_sched_overflow(),
        _ => Err(Error::NotFound),
    }
}

// ─── ProcfsBackend ─────────────────────────────────────────────────────────

/// Procfs backend — combines static generators with dynamic procmgr IPC.
pub struct ProcfsBackend {
    procmgr_endpoint: usize,
}

impl ProcfsBackend {
    pub fn new(procmgr_endpoint: usize) -> Self {
        Self { procmgr_endpoint }
    }

    /// Query procmgr for process data. Returns payload bytes on success.
    ///
    /// Wire format (after `reply_with_payload` on the procmgr side):
    /// - `reply.words[0]` = payload byte length (overwritten by the IPC layer).
    /// - `reply.words[1]` = errno (0 = success, negative = errno).
    /// - `reply.words[2]` = type-specific count, redundant with `payload_len`.
    fn query_procmgr(
        &self,
        query_type: usize,
        target_pid: usize,
        caller_tid: usize,
    ) -> Result<Vec<u8>> {
        let req = Message::new(
            PROCMGR_PROC_QUERY_LABEL,
            [query_type, target_pid, caller_tid, 0, 0, 0],
            3,
        );
        let mut reply_buf = [0u8; 4096];
        let bytes_received =
            libcluu::syscall::ipc_call_timeout(self.procmgr_endpoint, req.as_bytes(), &mut reply_buf, 500)
                .map_err(|_| Error::NotFound)?;

        if bytes_received < size_of::<Message>() {
            return Err(Error::InvalidState);
        }
        let reply_hdr = &reply_buf[..size_of::<Message>()];
        let errno = unsafe {
            let words_ptr = reply_hdr.as_ptr().add(8) as *const usize;
            *words_ptr as isize
        };
        let payload_len = bytes_received - size_of::<Message>();

        if errno != 0 {
            return Err(Error::from_errno(errno));
        }

        let data_start = size_of::<Message>();
        let data_end = data_start + payload_len;
        Ok(reply_buf[data_start..data_end].to_vec())
    }

    /// Parse "self/subfile" or "<pid>/subfile" from a relative path.
    /// Returns (pid, subfile) where pid=0 means "self".
    fn parse_pid_path(rel: &str) -> Option<(usize, &str)> {
        let (first, rest) = rel.split_once('/')?;
        if !PID_SUBFILES.contains(&rest) {
            return None;
        }
        if first == "self" {
            Some((0, rest))
        } else {
            first.parse::<usize>().ok().map(|pid| (pid, rest))
        }
    }

    /// Map a sub-file name to a query type.
    fn subfile_to_query(subfile: &str) -> usize {
        match subfile {
            "status" => QUERY_STATUS,
            "stat" => QUERY_STAT,
            "cmdline" => QUERY_CMDLINE,
            "comm" => QUERY_COMM,
            "exe" => QUERY_EXE,
            _ => QUERY_STATUS,
        }
    }

    /// Enumerate live thread IDs directly from the kernel.
    ///
    /// This bypasses procmgr IPC — the kernel is the source of truth for
    /// threads. The TIDs returned here are used as directory names in
    /// /proc readdir. Per-PID detail files (stat, status, etc.) still
    /// query procmgr, which resolves TID→PID via its tid_to_pid map.
    fn query_tid_list(&self) -> Result<Vec<u32>> {
        let self_token = process_info().tokens[TOKEN_SELF];
        let mut buf = [0u64; 256];
        let count = thread_enumerate(self_token, &mut buf).unwrap_or(0);
        Ok(buf[..count].iter().map(|&tid| tid as u32).collect())
    }
}

impl MountBackend for ProcfsBackend {
    fn name(&self) -> &'static str {
        "procfs"
    }

    fn open(&self, rel_path: &str, full_path: &str, caller_tid: usize) -> Result<OpenFile> {
        let rel = rel_path.trim_start_matches('/');

        // Static files
        if STATIC_FILES.contains(&rel) {
            let data = gen_static(rel)?;
            return Ok(OpenFile::Virtual(VirtualFile {
                data,
                path: String::from(full_path),
                rights: u64::MAX,
            }));
        }

        // Dynamic: "self/subfile" or "<pid>/subfile"
        if let Some((pid, subfile)) = Self::parse_pid_path(rel) {
            let query_type = Self::subfile_to_query(subfile);
            let data = self.query_procmgr(query_type, pid, caller_tid)?;
            return Ok(OpenFile::Virtual(VirtualFile {
                data,
                path: String::from(full_path),
                rights: u64::MAX,
            }));
        }

        Err(Error::NotFound)
    }

    fn readdir(&self, rel_path: &str, _caller_tid: usize) -> Result<Vec<DirEntry>> {
        use crate::mount::DirEntryStat;
        let rel = rel_path.trim_start_matches('/');

        let file_stat = DirEntryStat { mode: 0o100444u32, nlink: 1, ..Default::default() };
        let dir_stat  = DirEntryStat { mode: 0o040555u32, nlink: 1, ..Default::default() };

        if rel.is_empty() {
            // Root /proc directory: static entries + PID directories
            let mut entries: Vec<DirEntry> = STATIC_FILES
                .iter()
                .map(|&name| DirEntry {
                    name: String::from(name),
                    is_dir: false,
                    stat: file_stat,
                })
                .collect();

            entries.push(DirEntry {
                name: String::from("self"),
                is_dir: true,
                stat: dir_stat,
            });

            // Enumerate live TIDs directly from the kernel (no procmgr IPC).
            if let Ok(tids) = self.query_tid_list() {
                for tid in tids {
                    entries.push(DirEntry {
                        name: format!("{}", tid),
                        is_dir: true,
                        stat: dir_stat,
                    });
                }
            }

            return Ok(entries);
        }

        // "self" or "<pid>" directory
        if rel == "self" {
            return Ok(PID_SUBFILES
                .iter()
                .map(|&name| DirEntry {
                    name: String::from(name),
                    is_dir: false,
                    stat: file_stat,
                })
                .collect());
        }

        if rel.parse::<usize>().is_ok() {
            // Numeric PID directory — return sub-files.
            // Access control is enforced at open() time by procmgr.
            return Ok(PID_SUBFILES
                .iter()
                .map(|&name| DirEntry {
                    name: String::from(name),
                    is_dir: false,
                    stat: file_stat,
                })
                .collect());
        }

        Err(Error::NotFound)
    }
}
