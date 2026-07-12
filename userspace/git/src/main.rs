//! `/bin/git` — local-only git-minimal (init/add/commit/log).
//!
//! No network. Uses FNV-1a for object IDs (local uniqueness only).

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use libcluu::debug_print;
use libcluu::fs::client::VfsClient;
use libcluu::posix::{_write, O_CREAT, O_WRONLY};
use libcluu::registry;

const CHUNK: usize = 64 * 1024;

fn write_fd(fd: i32, data: &[u8]) {
    let _ = _write(fd, data.as_ptr() as *const _, data.len());
}

fn fnv1a(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn hash_hex(h: u64) -> String {
    format!("{:016x}", h)
}

fn read_whole_file(vfs: &VfsClient, path: &str) -> Result<Vec<u8>, ()> {
    use libcluu::boot::{process_info, TOKEN_SPACE};
    let file = vfs.open(path).map_err(|_| ())?;
    let total = file.size;
    if total == 0 {
        let _ = vfs.close(file);
        return Ok(Vec::new());
    }
    let info = process_info();
    let tok = info.tokens[TOKEN_SPACE];
    let alloc_sz = ((CHUNK.min(total)) + 4095) & !4095;
    let base = libcluu::vspace::VSPACE
        .lock()
        .alloc(alloc_sz)
        .map_err(|_| { let _ = vfs.close(file); })?;
    let mut out: Vec<u8> = Vec::new();
    let mut off = 0usize;
    let mut res: Result<(), ()> = Ok(());
    while off < total {
        let want = (total - off).min(CHUNK);
        match vfs.read_grant(file, off, want, tok, base) {
            Ok(g) => {
                if g.len == 0 { break; }
                let s = unsafe { core::slice::from_raw_parts(base as *const u8, g.len) };
                out.extend_from_slice(s);
                off += g.len;
            }
            Err(_) => { res = Err(()); break; }
        }
    }
    let _ = libcluu::vspace::VSPACE.lock().free(base, alloc_sz);
    let _ = vfs.close(file);
    res?;
    Ok(out)
}

fn write_file(vfs: &VfsClient, path: &str, data: &[u8]) -> Result<(), ()> {
    let f = vfs
        .open_with(path, (O_WRONLY | O_CREAT) as usize, 0o644)
        .map_err(|_| ())?;
    let _ = vfs.write(f, 0, data).map_err(|_| { let _ = vfs.close(f); });
    let _ = vfs.close(f);
    Ok(())
}

fn read_text(vfs: &VfsClient, path: &str) -> Option<String> {
    let buf = read_whole_file(vfs, path).ok()?;
    core::str::from_utf8(&buf).ok().map(|s| s.trim().to_string())
}

fn git_init(vfs: &VfsClient, cwd: &str) -> i32 {
    let gd = format!("{}/.git", cwd.trim_end_matches('/'));
    let dirs = [
        gd.as_str(),
        &format!("{}/refs", gd),
        &format!("{}/refs/heads", gd),
        &format!("{}/objects", gd),
        &format!("{}/staging", gd),
    ];
    for d in &dirs {
        let _ = vfs.mkdir(d, 0o755);
    }
    if write_file(vfs, &format!("{}/HEAD", gd), b"ref: refs/heads/main\n").is_err() {
        write_fd(2, b"git: init failed\n");
        return 1;
    }
    write_fd(1, b"Initialized empty Git repository\n");
    let _ = debug_print("GIT_OK\n");
    0
}

fn git_add(vfs: &VfsClient, cwd: &str, file: &str) -> i32 {
    let resolved = libcluu::posix::resolve_path(file);
    let content = match read_whole_file(vfs, &resolved) {
        Ok(c) => c,
        Err(_) => {
            let m = format!("git: {}: cannot read\n", file);
            write_fd(2, m.as_bytes());
            return 1;
        }
    };
    let basename = resolved.rsplit('/').next().unwrap_or(&resolved);
    let staging = format!("{}/.git/staging/{}", cwd.trim_end_matches('/'), basename);
    if write_file(vfs, &staging, &content).is_err() {
        write_fd(2, b"git: staging failed\n");
        return 1;
    }
    let _ = debug_print("GIT_OK\n");
    0
}

fn git_commit(vfs: &VfsClient, cwd: &str, msg: &str) -> i32 {
    let gd = format!("{}/.git", cwd.trim_end_matches('/'));
    let staging = format!("{}/staging", gd);

    let entries = match vfs.readdir(&staging) {
        Ok(e) => e,
        Err(_) => {
            write_fd(2, b"git: no staging directory\n");
            return 1;
        }
    };

    let parent = read_text(vfs, &format!("{}/refs/heads/main", gd))
        .unwrap_or_else(|| String::from("none"));

    let mut blob: Vec<u8> = Vec::new();
    blob.extend_from_slice(b"parent ");
    blob.extend_from_slice(parent.as_bytes());
    blob.push(b'\n');
    blob.extend_from_slice(b"msg ");
    blob.extend_from_slice(msg.as_bytes());
    blob.push(b'\n');

    for entry in &entries {
        if entry.name == "." || entry.name == ".." {
            continue;
        }
        let fpath = format!("{}/{}", staging, entry.name);
        if let Ok(content) = read_whole_file(vfs, &fpath) {
            blob.extend_from_slice(b"file ");
            blob.extend_from_slice(entry.name.as_bytes());
            blob.push(b' ');
            blob.extend_from_slice(hash_hex(fnv1a(&content)).as_bytes());
            blob.push(b'\n');
        }
    }

    let hash_str = hash_hex(fnv1a(&blob));
    let obj_path = format!("{}/objects/{}", gd, hash_str);
    if write_file(vfs, &obj_path, &blob).is_err() {
        write_fd(2, b"git: commit failed\n");
        return 1;
    }
    let ref_path = format!("{}/refs/heads/main", gd);
    if write_file(vfs, &ref_path, hash_str.as_bytes()).is_err() {
        write_fd(2, b"git: update ref failed\n");
        return 1;
    }

    let out = format!("[{}]\nGIT_OK\n", hash_str);
    write_fd(1, out.as_bytes());
    0
}

fn git_log(vfs: &VfsClient, cwd: &str) -> i32 {
    let gd = format!("{}/.git", cwd.trim_end_matches('/'));
    let mut hash = match read_text(vfs, &format!("{}/refs/heads/main", gd)) {
        Some(h) => h,
        None => {
            let _ = debug_print("GIT_OK\n");
            return 0;
        }
    };

    while hash != "none" && !hash.is_empty() {
        let obj_path = format!("{}/objects/{}", gd, hash);
        let content = match read_whole_file(vfs, &obj_path) {
            Ok(c) => c,
            Err(_) => break,
        };
        let text = match core::str::from_utf8(&content) {
            Ok(s) => s,
            Err(_) => break,
        };
        let mut parent = String::new();
        let mut msg = String::new();
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("parent ") {
                parent = String::from(rest);
            } else if let Some(rest) = line.strip_prefix("msg ") {
                msg = String::from(rest);
            }
        }
        let entry = format!("commit {}\n    {}\n\n", hash, msg);
        write_fd(1, entry.as_bytes());
        if parent == "none" || parent.is_empty() {
            break;
        }
        hash = parent;
    }
    let _ = debug_print("GIT_OK\n");
    0
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let argv: Vec<String> = libcluu::args::args();
    if argv.len() < 2 {
        write_fd(2, b"git: usage: git <init|add|commit|log> [args]\n");
        return 1;
    }
    let sub = argv[1].as_str();
    let cwd = libcluu::posix::current_dir_string();
    let Ok(vfs_ep) = registry::subscribe_output("vfs", "main") else {
        write_fd(2, b"git: vfs unavailable\n");
        return 1;
    };
    let vfs = VfsClient::new(vfs_ep, registry::control_endpoint());

    let ec = match sub {
        "init" => git_init(&vfs, &cwd),
        "add" => {
            if argv.len() < 3 {
                write_fd(2, b"git: add: missing file\n");
                return 1;
            }
            git_add(&vfs, &cwd, &argv[2])
        }
        "commit" => {
            let mut msg = String::new();
            let mut i = 2;
            while i < argv.len() {
                if argv[i] == "-m" && i + 1 < argv.len() {
                    msg = argv[i + 1].clone();
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if msg.is_empty() {
                write_fd(2, b"git: commit: missing -m <msg>\n");
                return 1;
            }
            git_commit(&vfs, &cwd, &msg)
        }
        "log" => git_log(&vfs, &cwd),
        _ => {
            let m = format!("git: unknown command '{}'\n", sub);
            write_fd(2, m.as_bytes());
            1
        }
    };
    let _ = debug_print(&format!("git: exit {}", ec));
    ec
}
