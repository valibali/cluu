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
use libcluu::ipc::{call_with_reply_buf, PROCMGR_PROC_QUERY_LABEL};
use libcluu::types::Message;
use libcluu::{Error, Result};

/// Query type constants for procmgr IPC.
const QUERY_STATUS: usize = 0;
const QUERY_STAT: usize = 1;
const QUERY_CMDLINE: usize = 2;
const QUERY_LIST: usize = 3;

/// Names of static /proc files (no procmgr IPC needed).
const STATIC_FILES: &[&str] = &["version", "uptime", "meminfo", "cpuinfo", "mounts", "fb"];

/// Per-PID sub-files available under /proc/<pid>/ and /proc/self/.
const PID_SUBFILES: &[&str] = &["status", "stat", "cmdline"];

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
    let uptime = String::from("0.00 0.00\n");
    Ok(uptime.into_bytes())
}

fn gen_meminfo() -> Result<Vec<u8>> {
    let meminfo = String::from(
        "MemTotal:       131072 kB\n\
         MemFree:        100000 kB\n\
         MemAvailable:   100000 kB\n\
         Buffers:            0 kB\n\
         Cached:             0 kB\n",
    );
    Ok(meminfo.into_bytes())
}

fn gen_cpuinfo() -> Result<Vec<u8>> {
    let cpuinfo = String::from(
        "processor\t: 0\n\
         vendor_id\t: CLUU\n\
         model name\t: QEMU Virtual CPU\n\
         cpu MHz\t\t: 2000.000\n\
         cache size\t: 4096 KB\n\
         flags\t\t: fpu vme de pse tsc msr pae mce cx8 apic\n\n",
    );
    Ok(cpuinfo.into_bytes())
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

fn gen_static(name: &str) -> Result<Vec<u8>> {
    match name {
        "version" => gen_version(),
        "uptime" => gen_uptime(),
        "meminfo" => gen_meminfo(),
        "cpuinfo" => gen_cpuinfo(),
        "mounts" => gen_mounts(),
        "fb" => gen_fb(),
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
        let (reply, payload_len) =
            call_with_reply_buf(self.procmgr_endpoint, &req, &[], &mut reply_buf)?;

        // words[0] = errno (0 = success)
        let errno = reply.words[0] as isize;
        if errno != 0 {
            return if errno == -2 {
                Err(Error::NotFound)
            } else if errno == -1 {
                Err(Error::PermissionDenied)
            } else {
                Err(Error::InvalidOperation)
            };
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
            _ => QUERY_STATUS,
        }
    }

    /// Query procmgr for the PID list (visible to caller).
    fn query_pid_list(&self, caller_tid: usize) -> Result<Vec<u32>> {
        let req = Message::new(
            PROCMGR_PROC_QUERY_LABEL,
            [QUERY_LIST, 0, caller_tid, 0, 0, 0],
            3,
        );
        let mut reply_buf = [0u8; 4096];
        let (reply, _payload_len) =
            call_with_reply_buf(self.procmgr_endpoint, &req, &[], &mut reply_buf)?;

        let errno = reply.words[0] as isize;
        if errno != 0 {
            return Err(Error::InvalidOperation);
        }

        let pid_count = reply.words[1];
        let data_start = size_of::<Message>();
        let mut pids = Vec::with_capacity(pid_count);
        for i in 0..pid_count {
            let offset = data_start + i * 4;
            if offset + 4 > reply_buf.len() {
                break;
            }
            let pid = u32::from_le_bytes([
                reply_buf[offset],
                reply_buf[offset + 1],
                reply_buf[offset + 2],
                reply_buf[offset + 3],
            ]);
            pids.push(pid);
        }
        Ok(pids)
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
            }));
        }

        // Dynamic: "self/subfile" or "<pid>/subfile"
        if let Some((pid, subfile)) = Self::parse_pid_path(rel) {
            let query_type = Self::subfile_to_query(subfile);
            let data = self.query_procmgr(query_type, pid, caller_tid)?;
            return Ok(OpenFile::Virtual(VirtualFile {
                data,
                path: String::from(full_path),
            }));
        }

        Err(Error::NotFound)
    }

    fn readdir(&self, rel_path: &str, caller_tid: usize) -> Result<Vec<DirEntry>> {
        let rel = rel_path.trim_start_matches('/');

        if rel.is_empty() {
            // Root /proc directory: static entries + PID directories
            let mut entries: Vec<DirEntry> = STATIC_FILES
                .iter()
                .map(|&name| DirEntry {
                    name: String::from(name),
                    is_dir: false,
                })
                .collect();

            entries.push(DirEntry {
                name: String::from("self"),
                is_dir: true,
            });

            // Query procmgr for PID list
            if let Ok(pids) = self.query_pid_list(caller_tid) {
                for pid in pids {
                    entries.push(DirEntry {
                        name: format!("{}", pid),
                        is_dir: true,
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
                })
                .collect());
        }

        Err(Error::NotFound)
    }
}
