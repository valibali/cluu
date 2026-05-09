//! ext2io probe: ext2 filesystem I/O smoke tests.
//!
//! Subcmds (argv[1]):
//!   write   — write one byte to a file (default: /home/root/ext2io_scratch)
//!   append  — append one byte past EOF (default: /home/root/ext2io_scratch)
//!   mutate  — mkdir + rename + rmdir
//!   unlink  — create + unlink + verify
//!
//! Lifted from Ext2WriteBuiltin, Ext2AppendBuiltin, Ext2MutateBuiltin,
//! Ext2UnlinkBuiltin (jobs.rs).

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use libcluu::fs::client::VfsClient;
use libcluu::{registry, Error};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = libcluu::args::args();
    let subcmd = args.get(1).map_or("write", |s| s.as_str());

    match subcmd {
        "write" => run_write(&args),
        "append" => run_append(&args),
        "mutate" => run_mutate(),
        "unlink" => run_unlink(),
        _ => {
            let _ = libcluu::debug_print("ext2io: usage: ext2io write|append|mutate|unlink");
            1
        }
    }
}

fn get_vfs(tag: &str) -> Option<VfsClient> {
    let ep = match registry::subscribe_output("vfs", "main") {
        Ok(ep) => ep,
        Err(err) => {
            let line = format!("{}: FAIL vfs unavailable {:?}", tag, err);
            let _ = libcluu::debug_print(&line);
            return None;
        }
    };
    match VfsClient::new_from_registry(ep) {
        Ok(client) => Some(client),
        Err(err) => {
            let line = format!("{}: FAIL client {:?}", tag, err);
            let _ = libcluu::debug_print(&line);
            None
        }
    }
}

fn run_write(args: &[alloc::string::String]) -> i32 {
    let path = args.get(2).map_or("/home/root/ext2io_scratch", |s| s.as_str());
    let Some(vfs) = get_vfs("ext2write") else {
        return 1;
    };

    // O_RDWR | O_CREAT — scratch path may not exist on a fresh disk.
    let file = match vfs.open_with(path, 2 | 0o1000, 0o644) {
        Ok(file) => file,
        Err(err) => {
            let line = format!("ext2write: FAIL open {} {:?}", path, err);
            let _ = libcluu::debug_print(&line);
            return 1;
        }
    };

    let result = match vfs.write(file, 0, &[0x7f]) {
        Ok(1) => {
            let line = format!("ext2write: PASS path={}", path);
            let _ = libcluu::debug_print(&line);
            0
        }
        Ok(written) => {
            let line = format!("ext2write: FAIL short-write {}", written);
            let _ = libcluu::debug_print(&line);
            1
        }
        Err(err) => {
            let line = format!("ext2write: FAIL write {:?}", err);
            let _ = libcluu::debug_print(&line);
            1
        }
    };
    let _ = vfs.close(file);
    result
}

fn run_append(args: &[alloc::string::String]) -> i32 {
    let path = args.get(2).map_or("/home/root/ext2io_scratch", |s| s.as_str());
    let Some(vfs) = get_vfs("ext2append") else {
        return 1;
    };

    // O_RDWR | O_CREAT — scratch path may not exist on a fresh disk.
    let file = match vfs.open_with(path, 2 | 0o1000, 0o644) {
        Ok(file) => file,
        Err(err) => {
            let line = format!("ext2append: FAIL open {} {:?}", path, err);
            let _ = libcluu::debug_print(&line);
            return 1;
        }
    };

    let append_offset = file.size;
    let result = match vfs.write(file, append_offset, &[0]) {
        Ok(1) => {
            let line = format!(
                "ext2append: PASS path={} offset={}",
                path, append_offset
            );
            let _ = libcluu::debug_print(&line);
            0
        }
        Ok(written) => {
            let line = format!("ext2append: FAIL short-write {}", written);
            let _ = libcluu::debug_print(&line);
            1
        }
        Err(err) => {
            let line = format!("ext2append: FAIL write {:?}", err);
            let _ = libcluu::debug_print(&line);
            1
        }
    };
    let _ = vfs.close(file);
    result
}

fn run_mutate() -> i32 {
    let Some(vfs) = get_vfs("ext2mutate") else {
        return 1;
    };

    let from = "/l2a_dir";
    let to = "/l2a_dir_renamed";
    let mut op = "mkdir";
    let result = (|| -> libcluu::Result<()> {
        vfs.mkdir(from, 0o755)?;
        op = "rename";
        vfs.rename(from, to)?;
        op = "rmdir";
        vfs.rmdir(to)?;
        Ok(())
    })();

    match result {
        Ok(()) => {
            let _ = libcluu::debug_print("ext2mutate: PASS mkdir+rename+rmdir");
            0
        }
        Err(err) => {
            let line = format!("ext2mutate: FAIL op={} err={:?}", op, err);
            let _ = libcluu::debug_print(&line);
            1
        }
    }
}

fn run_unlink() -> i32 {
    let path = "/l2a_tmp_unlink";
    let Some(vfs) = get_vfs("ext2unlink") else {
        return 1;
    };

    if let Err(err) = vfs.mkdir("/tmp", 0o755) {
        if err != Error::AlreadyExists {
            let line = format!("ext2unlink: FAIL mkdir /tmp {:?}", err);
            let _ = libcluu::debug_print(&line);
            return 1;
        }
    }

    let created = match vfs.open_with(path, 0o1000 | 2, 0o644) {
        Ok(file) => file,
        Err(err) => {
            let line = format!("ext2unlink: FAIL create/open {:?}", err);
            let _ = libcluu::debug_print(&line);
            return 1;
        }
    };
    let _ = vfs.close(created);

    if let Err(err) = vfs.unlink(path) {
        let line = format!("ext2unlink: FAIL unlink {:?}", err);
        let _ = libcluu::debug_print(&line);
        return 1;
    }

    match vfs.stat(path) {
        Err(Error::NotFound) => {
            let _ = libcluu::debug_print("ext2unlink: PASS create+unlink+verify");
            0
        }
        Err(err) => {
            let line = format!("ext2unlink: FAIL verify {:?}", err);
            let _ = libcluu::debug_print(&line);
            1
        }
        Ok(_) => {
            let _ = libcluu::debug_print("ext2unlink: FAIL still-exists");
            1
        }
    }
}
