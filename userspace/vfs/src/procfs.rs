//! Procfs virtual filesystem implementation.
//!
//! Provides system information through virtual files:
//! - /proc/version, uptime, meminfo, cpuinfo, mounts, fb, sched_overflow:
//!   static generators (no IPC needed)
//! - /proc/<pid>/{stat,status,cmdline,comm,exe}: per-PID process info via
//!   synchronous IPC to session-procmgr using `call_with_reply_buf`
//! - /proc/self/...: currently returns NotFound (requires TID→PID resolution,
//!   deferred — `top` uses explicit PIDs from readdir)
//! - /proc readdir: static entries + PID directories from procmgr `list_pids`

use crate::fd_table::OpenFile;
use crate::mount::{DirEntry, DirEntryStat, VirtualFile, AsyncMountBackend};
use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;
use libcluu::async_runtime::IpcCallFuture;
use libcluu::boot::{process_info, TOKEN_SELF};
use libcluu::ipc::{PROCMGR_LIST_PIDS_LABEL, PROCMGR_PROC_INFO_LABEL};
use libcluu::syscall::{
    pmm_get_stats, sched_get_overflow, PMM_STATS_TOTAL_FRAMES,
    PMM_STATS_USED_FRAMES, SCHED_OVERFLOW_DEFERRED_FAULT, SCHED_OVERFLOW_PENDING_WAKE,
};
use libcluu::types::Message;
use libcluu::{Error, Result};
use procmgr_common::wire::ProcInfo;

// ─── Static content generators (unchanged) ─────────────────────────────────

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

fn gen_version() -> Result<Vec<u8>> {
    let version = String::from(
        "CLUU microkernel v0.1.0\n\
         Built with Rust (no_std)\n\
         Architecture: x86_64\n",
    );
    Ok(version.into_bytes())
}

fn gen_uptime() -> Result<Vec<u8>> {
    let (sec, nsec) = libcluu::time::query(libcluu::time::TIME_GETCLOCK)
        .unwrap_or((0, 0));
    let centi = (nsec / 10_000_000) as u32;
    let text = format!("{}.{:02} 0.00\n", sec, centi);
    Ok(text.into_bytes())
}

fn gen_meminfo() -> Result<Vec<u8>> {
    let self_token = process_info().tokens[TOKEN_SELF];
    let used = pmm_get_stats(self_token, PMM_STATS_USED_FRAMES).unwrap_or(0);
    let total = pmm_get_stats(self_token, PMM_STATS_TOTAL_FRAMES).unwrap_or(0);
    let free = total.saturating_sub(used);
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

// ─── ProcInfo formatting ───────────────────────────────────────────────────

/// Format a ProcInfo into the requested subfile format.
fn format_proc_info(subfile: &str, info: &ProcInfo) -> Result<Vec<u8>> {
    match subfile {
        "stat" => {
            // Linux-style stat: "pid (name) state rest..."
            // Fields: pid, name, state, cpu_ticks, heap_pages, other_pages,
            //         ppid, sid, cid, pcid
            let text = format!(
                "{} ({}) R 0 0 0 {} 0 {} {}\n",
                info.pid, info.command, info.ppid, info.pid, info.ppid,
            );
            Ok(text.into_bytes())
        }
        "status" => {
            let text = format!(
                "Name:\t{}\n\
                 State:\tR (running)\n\
                 Pid:\t{}\n\
                 PPid:\t{}\n",
                info.command, info.pid, info.ppid,
            );
            Ok(text.into_bytes())
        }
        "cmdline" => {
            // Null-terminated argv[0]
            let mut data = info.argv0.as_bytes().to_vec();
            data.push(0);
            Ok(data)
        }
        "comm" => {
            let mut text = info.command.clone();
            text.push('\n');
            Ok(text.into_bytes())
        }
        "exe" => {
            // Path to executable
            Ok(info.argv0.as_bytes().to_vec())
        }
        _ => Err(Error::NotFound),
    }
}

/// Parse "self/subfile" or "<pid>/subfile" from a relative path.
/// Returns (pid, subfile) where pid=0 means "self".
fn parse_pid_path(rel: &str) -> Option<(i32, &str)> {
    let (first, rest) = rel.split_once('/')?;
    if !PID_SUBFILES.contains(&rest) {
        return None;
    }
    if first == "self" {
        Some((0, rest))
    } else {
        first.parse::<i32>().ok().map(|pid| (pid, rest))
    }
}

// ─── ProcfsBackend ─────────────────────────────────────────────────────────

pub struct ProcfsBackend {
    procmgr_endpoint: usize,
}

impl ProcfsBackend {
    pub fn new(procmgr_endpoint: usize) -> Self {
        Self { procmgr_endpoint }
    }

    async fn list_pids_async(&self) -> Result<Vec<u32>> {
        let req = Message::new(
            PROCMGR_LIST_PIDS_LABEL,
            [0, 0, 0, 0, 0, 0],
            6,
        );
        let (reply, payload) = IpcCallFuture::new(self.procmgr_endpoint, req).await?;

        let (errno, pid_count) = if !payload.is_empty() {
            (reply.words[1] as isize, reply.words[2])
        } else {
            (reply.words[0] as isize, reply.words[1])
        };

        if errno != 0 {
            return Err(Error::from_errno(errno));
        }

        let mut pids = Vec::with_capacity(pid_count);
        for i in 0..pid_count {
            let offset = i * 4;
            if offset + 4 > payload.len() {
                break;
            }
            let pid = u32::from_le_bytes(
                payload[offset..offset + 4].try_into().unwrap_or([0u8; 4]),
            );
            pids.push(pid);
        }
        Ok(pids)
    }

    async fn proc_info_async(&self, pid: i32, caller_tid: usize) -> Result<ProcInfo> {
        let req = Message::new(
            PROCMGR_PROC_INFO_LABEL,
            [pid as usize, 0, caller_tid, 0, 0, 0],
            6,
        );
        let (reply, payload) = IpcCallFuture::new(self.procmgr_endpoint, req).await?;

        let errno = if !payload.is_empty() {
            reply.words[1] as isize
        } else {
            reply.words[0] as isize
        };

        if errno != 0 {
            return Err(Error::from_errno(errno));
        }

        postcard::from_bytes::<ProcInfo>(&payload)
            .map_err(|_| Error::InvalidArgument)
    }
}

impl AsyncMountBackend for ProcfsBackend {
    fn name(&self) -> &'static str {
        "procfs"
    }

    fn open_async(
        &self,
        rel_path: &str,
        full_path: &str,
        caller_tid: usize,
    ) -> Pin<Box<dyn Future<Output = Result<OpenFile>> + '_>> {
        let rel_path = rel_path.to_string();
        let full_path = full_path.to_string();
        Box::pin(async move {
            let rel = rel_path.trim_start_matches('/');

            if STATIC_FILES.contains(&rel) {
                let data = gen_static(rel)?;
                return Ok(OpenFile::Virtual(VirtualFile {
                    data,
                    path: full_path.to_string(),
                    rights: u64::MAX,
                }));
            }

            if let Some((pid, subfile)) = parse_pid_path(rel) {
                if pid == 0 {
                    return Err(Error::NotFound);
                }

                let info = self.proc_info_async(pid, caller_tid).await?;
                let data = format_proc_info(subfile, &info)?;
                return Ok(OpenFile::Virtual(VirtualFile {
                    data,
                    path: full_path.to_string(),
                    rights: u64::MAX,
                }));
            }

            Err(Error::NotFound)
        })
    }

    fn readdir_async(
        &self,
        rel_path: &str,
        _caller_tid: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<DirEntry>>> + '_>> {
        let rel_path = rel_path.to_string();
        Box::pin(async move {
            let rel = rel_path.trim_start_matches('/');
            let file_stat = DirEntryStat { mode: 0o100444u32, nlink: 1, ..Default::default() };
            let dir_stat  = DirEntryStat { mode: 0o040555u32, nlink: 1, ..Default::default() };

            if rel.is_empty() {
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

                if let Ok(pids) = self.list_pids_async().await {
                    for pid in pids {
                        entries.push(DirEntry {
                            name: format!("{}", pid),
                            is_dir: true,
                            stat: dir_stat,
                        });
                    }
                }

                return Ok(entries);
            }

            if rel == "self" || rel.parse::<i32>().is_ok() {
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
        })
    }
}
