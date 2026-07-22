//! `/proc/devices` procfs backend (D1.4).
//!
//! Async `MountBackend` that IPCs drivermgr to render the discovered device
//! tree.  Mounted at `/proc/devices`, taking longest-prefix precedence over
//! the existing `/proc` (ProcfsBackend) mount.
//!
//! Path model:
//! - `cat /proc/devices`          → open("")         → full device listing
//! - `ls /proc/devices`           → readdir("")      → ["pci", "acpi"] dirs
//! - `ls /proc/devices/pci`       → readdir("pci")   → ["00:04.0", ...]
//! - `cat /proc/devices/pci/00:04.0` → open("pci/00:04.0") → per-device detail
//!
//! Per AGENTS.md §7, procfs is an `AsyncMountBackend` — all reads cross a
//! process boundary (VFS → drivermgr) and must go through the async runtime
//! to avoid the single-threaded mutual-blocking IPC deadlock class.

extern crate alloc;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::ToString;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::future::Future;
use core::pin::Pin;

use libcluu::async_runtime::IpcCallFuture;
use libcluu::ipc::{DRIVERMGR_QUERY_DEVICE_LABEL, DRIVERMGR_QUERY_DEVICES_LABEL};
use libcluu::types::Message;
use libcluu::{Error, Result};

use crate::fd_table::OpenFile;
use crate::mount::{AsyncMountBackend, DirEntry, DirEntryStat, VirtualFile};

const REPLY_OK: usize = 0;

pub struct DevicesProcfsBackend {
    drivermgr_endpoint: usize,
}

impl DevicesProcfsBackend {
    pub fn new(drivermgr_endpoint: usize) -> Self {
        Self { drivermgr_endpoint }
    }

    async fn query_devices(&self) -> Result<Vec<u8>> {
        let req = Message::new(DRIVERMGR_QUERY_DEVICES_LABEL, [0, 0, 0, 0, 0, 0], 1);
        let (reply, payload) = IpcCallFuture::new(self.drivermgr_endpoint, req).await?;
        if reply.words[1] != REPLY_OK && payload.is_empty() {
            return Err(Error::InvalidState);
        }
        Ok(payload)
    }

    async fn query_device(&self, canonical_path: &str) -> Result<Vec<u8>> {
        let mut req = Message::new(DRIVERMGR_QUERY_DEVICE_LABEL, [0, 0, 0, 0, 0, 0], 1);
        let (reply, payload) =
            IpcCallFuture::new_with_payload(self.drivermgr_endpoint, &mut req, canonical_path.as_bytes()).await?;
        if reply.words[1] != REPLY_OK {
            return Err(Error::NotFound);
        }
        Ok(payload)
    }

    fn parse_device_names(listing: &[u8], bus_prefix: &str) -> Vec<DirEntry> {
        let file_stat = DirEntryStat { mode: 0o100444u32, nlink: 1, ..Default::default() };
        let mut entries = Vec::new();
        let text = match core::str::from_utf8(listing) {
            Ok(s) => s,
            Err(_) => return entries,
        };
        let prefix = format!("/{}/", bus_prefix);
        for line in text.lines() {
            let path = line.split_whitespace().next().unwrap_or("");
            if let Some(suffix) = path.strip_prefix(&prefix) {
                if !suffix.is_empty() && !suffix.contains('/') {
                    entries.push(DirEntry {
                        name: suffix.to_string(),
                        is_dir: false,
                        stat: file_stat,
                    });
                }
            }
        }
        entries
    }
}

impl AsyncMountBackend for DevicesProcfsBackend {
    fn name(&self) -> &'static str {
        "devices_procfs"
    }

    fn open_async(
        &self,
        rel_path: &str,
        full_path: &str,
        _caller_tid: usize,
    ) -> Pin<Box<dyn Future<Output = Result<OpenFile>> + '_>> {
        let rel_path = rel_path.to_string();
        let full_path = full_path.to_string();
        Box::pin(async move {
            let rel = rel_path.trim_start_matches('/');
            let data = if rel.is_empty() {
                self.query_devices().await?
            } else {
                let canonical = format!("/{}", rel);
                self.query_device(&canonical).await?
            };
            Ok(OpenFile::Virtual(VirtualFile {
                data,
                path: full_path,
                rights: u64::MAX,
            }))
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
            let dir_stat = DirEntryStat { mode: 0o040555u32, nlink: 1, ..Default::default() };

            if rel.is_empty() {
                return Ok(vec![
                    DirEntry { name: String::from("pci"), is_dir: true, stat: dir_stat },
                    DirEntry { name: String::from("acpi"), is_dir: true, stat: dir_stat },
                ]);
            }

            let listing = self.query_devices().await?;
            Ok(Self::parse_device_names(&listing, rel))
        })
    }

    fn stat_async(
        &self,
        rel_path: &str,
        _full_path: &str,
        _caller_tid: usize,
    ) -> Pin<Box<dyn Future<Output = Result<DirEntryStat>> + '_>> {
        let rel_path = rel_path.to_string();
        Box::pin(async move {
            let rel = rel_path.trim_start_matches('/');
            let dir_stat = DirEntryStat { mode: 0o040555u32, nlink: 1, ..Default::default() };

            if rel.is_empty() || rel == "pci" || rel == "acpi" {
                return Ok(dir_stat);
            }

            let canonical = format!("/{}", rel);
            let data = self.query_device(&canonical).await?;
            Ok(DirEntryStat {
                size: data.len() as u64,
                mode: 0o100444u32,
                nlink: 1,
                ..Default::default()
            })
        })
    }
}
