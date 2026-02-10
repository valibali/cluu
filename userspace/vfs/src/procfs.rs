//! Procfs virtual filesystem implementation.
//!
//! Provides system information through virtual files:
//! - /proc/version: Kernel/OS version info
//! - /proc/uptime: System uptime
//! - /proc/meminfo: Memory statistics
//! - /proc/cpuinfo: CPU information
//! - /proc/self: Current process info (directory)

use crate::mount::{DirEntry, VirtualEntry};
use alloc::format;
use alloc::vec::Vec;
use libcluu::Result;

/// Static procfs entries table.
pub static ENTRIES: &[(&str, VirtualEntry)] = &[
    ("version", VirtualEntry::File(gen_version)),
    ("uptime", VirtualEntry::File(gen_uptime)),
    ("meminfo", VirtualEntry::File(gen_meminfo)),
    ("cpuinfo", VirtualEntry::File(gen_cpuinfo)),
    ("mounts", VirtualEntry::File(gen_mounts)),
    ("self", VirtualEntry::Dir(gen_self_dir)),
];

fn gen_version() -> Result<Vec<u8>> {
    let version = format!(
        "CLUU microkernel v0.1.0\n\
         Built with Rust (no_std)\n\
         Architecture: x86_64\n"
    );
    Ok(version.into_bytes())
}

fn gen_uptime() -> Result<Vec<u8>> {
    // TODO: Get actual uptime from timeserver
    let uptime = format!("0.00 0.00\n");
    Ok(uptime.into_bytes())
}

fn gen_meminfo() -> Result<Vec<u8>> {
    // TODO: Get actual memory stats from kernel
    let meminfo = format!(
        "MemTotal:       131072 kB\n\
         MemFree:        100000 kB\n\
         MemAvailable:   100000 kB\n\
         Buffers:            0 kB\n\
         Cached:             0 kB\n"
    );
    Ok(meminfo.into_bytes())
}

fn gen_cpuinfo() -> Result<Vec<u8>> {
    let cpuinfo = format!(
        "processor\t: 0\n\
         vendor_id\t: CLUU\n\
         model name\t: QEMU Virtual CPU\n\
         cpu MHz\t\t: 2000.000\n\
         cache size\t: 4096 KB\n\
         flags\t\t: fpu vme de pse tsc msr pae mce cx8 apic\n\n"
    );
    Ok(cpuinfo.into_bytes())
}

fn gen_mounts() -> Result<Vec<u8>> {
    // List mounted filesystems
    let mounts = format!(
        "/dev/initrd /dev/initrd initrd ro 0 0\n\
         /dev/vda /mnt/disk ext2 rw 0 0\n\
         proc /proc proc rw 0 0\n"
    );
    Ok(mounts.into_bytes())
}

fn gen_self_dir() -> Result<Vec<DirEntry>> {
    // /proc/self directory entries
    Ok(alloc::vec![
        DirEntry {
            name: alloc::string::String::from("status"),
            is_dir: false,
        },
        DirEntry {
            name: alloc::string::String::from("cmdline"),
            is_dir: false,
        },
        DirEntry {
            name: alloc::string::String::from("fd"),
            is_dir: true,
        },
    ])
}
