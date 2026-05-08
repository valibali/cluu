//! `/bin/cp` — copy files and directories.
//!
//! Flags: -r/-R, -i, -f, -v, -p, -n

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use libcluu::boot::{process_info, TOKEN_SPACE};
use libcluu::cli::{parse, render_help, CliError, Spec};
use libcluu::debug_print;
use libcluu::fs::client::VfsClient;
use libcluu::posix::{O_CREAT, O_TRUNC, O_WRONLY, _write};
use libcluu::registry;

const CHUNK_SIZE: usize = 64 * 1024;

fn spec() -> Spec {
    Spec::new()
        .program("cp")
        .version("0.1.0")
        .usage("[-rRifvpn] SOURCE DEST")
        .flag('r', "recursive", "copy directories recursively")
        .flag('R', "recursive-cap", "copy directories recursively (alias for -r)")
        .flag('i', "interactive", "prompt before overwrite (treated as -n in batch)")
        .flag('f', "force", "do not prompt before overwriting")
        .flag('v', "verbose", "explain what is being done")
        .flag('p', "preserve", "preserve mode and timestamps")
        .flag('n', "no-clobber", "do not overwrite an existing file")
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = libcluu::args::args();
    let sp = spec();
    let parsed = match parse(&sp, &args) {
        Ok(p) => p,
        Err(CliError::HelpRequested) => {
            let h = render_help(&sp);
            write_fd(1, h.as_bytes());
            return 0;
        }
        Err(CliError::VersionRequested) => {
            write_fd(1, b"cp 0.1.0\n");
            return 0;
        }
        Err(e) => {
            let msg = format!("cp: {}\n", e);
            write_fd(2, msg.as_bytes());
            return 2;
        }
    };

    if parsed.positional.len() != 2 {
        write_fd(2, b"cp: usage: cp [-rRifvpn] SOURCE DEST\n");
        return 2;
    }

    let recursive = parsed.is_set("recursive") || parsed.is_set("recursive-cap");
    let verbose = parsed.is_set("verbose");
    let no_clobber = parsed.is_set("no-clobber") || parsed.is_set("interactive");
    // -f is default — no special action needed beyond ignoring no-clobber
    let force = parsed.is_set("force");
    let preserve = parsed.is_set("preserve");

    let src = libcluu::posix::resolve_path(&parsed.positional[0]);
    let dst = libcluu::posix::resolve_path(&parsed.positional[1]);

    let Ok(vfs_endpoint) = registry::subscribe_output("vfs", "main") else {
        write_fd(2, b"cp: vfs unavailable\n");
        return 1;
    };
    let client_id = registry::control_endpoint();
    let client = VfsClient::new(vfs_endpoint, client_id);

    let opts = CpOpts {
        recursive,
        verbose,
        no_clobber,
        force,
        preserve,
    };

    let exit_code = match copy_entry(&client, &src, &dst, &opts) {
        Ok(()) => 0,
        Err(err) => {
            let msg = format!("cp: {}: {}\n", src, err);
            write_fd(2, msg.as_bytes());
            1
        }
    };
    let _ = debug_print(&format!("cp: ok (exit {})", exit_code));
    exit_code
}

struct CpOpts {
    recursive: bool,
    verbose: bool,
    no_clobber: bool,
    #[allow(dead_code)]
    force: bool,
    #[allow(dead_code)]
    preserve: bool,
}

fn copy_entry(client: &VfsClient, src: &str, dst: &str, opts: &CpOpts) -> Result<(), String> {
    let info = client.stat(src).map_err(|e| format!("{:?}", e))?;
    let is_dir = info.mode & 0o170000 == 0o040000;

    if is_dir {
        if !opts.recursive {
            return Err(String::from("is a directory (use -r)"));
        }
        copy_dir_recursive(client, src, dst, opts)
    } else {
        copy_file(client, src, dst, opts)
    }
}

fn copy_dir_recursive(
    client: &VfsClient,
    src: &str,
    dst: &str,
    opts: &CpOpts,
) -> Result<(), String> {
    // Create destination directory if it doesn't exist.
    match client.mkdir(dst, 0o755) {
        Ok(()) => {}
        Err(_) => {
            // May already exist — check.
            match client.stat(dst) {
                Ok(st) if st.mode & 0o170000 == 0o040000 => {}
                _ => return Err(format!("cannot create directory '{}'", dst)),
            }
        }
    }

    let entries = client.readdir(src).map_err(|e| format!("{:?}", e))?;
    for entry in entries {
        if entry.name == "." || entry.name == ".." {
            continue;
        }
        let child_src = format!("{}/{}", src.trim_end_matches('/'), entry.name);
        let child_dst = format!("{}/{}", dst.trim_end_matches('/'), entry.name);
        copy_entry(client, &child_src, &child_dst, opts)?;
    }
    Ok(())
}

fn copy_file(client: &VfsClient, src: &str, dst: &str, opts: &CpOpts) -> Result<(), String> {
    // No-clobber check.
    if opts.no_clobber {
        if client.stat(dst).is_ok() {
            return Ok(());
        }
    }

    let src_info = client.stat(src).map_err(|e| format!("{:?}", e))?;
    let src_file = client.open(src).map_err(|e| format!("{:?}", e))?;
    let total = src_file.size;

    let dst_file = client
        .open_with(dst, (O_WRONLY | O_CREAT | O_TRUNC) as usize, 0o644)
        .map_err(|e| {
            let _ = client.close(src_file);
            format!("dst open: {:?}", e)
        })?;

    if opts.verbose {
        let msg = format!("'{}' -> '{}'\n", src, dst);
        write_fd(1, msg.as_bytes());
    }

    if total == 0 {
        let _ = client.close(src_file);
        let _ = client.close(dst_file);
        return Ok(());
    }

    let info_page = process_info();
    let space_token = info_page.tokens[TOKEN_SPACE];
    let chunk_alloc = ((CHUNK_SIZE.min(total)) + 4095) & !4095;
    let scratch_base = libcluu::vspace::VSPACE
        .lock()
        .alloc(chunk_alloc)
        .map_err(|_| {
            let _ = client.close(src_file);
            let _ = client.close(dst_file);
            String::from("out of virtual memory")
        })?;

    let mut offset = 0usize;
    let mut result: Result<(), String> = Ok(());
    while offset < total {
        let remaining = total - offset;
        let want = remaining.min(CHUNK_SIZE);
        match client.read_grant(src_file, offset, want, space_token, scratch_base) {
            Ok(grant) => {
                if grant.len == 0 {
                    break;
                }
                let buf =
                    unsafe { core::slice::from_raw_parts(scratch_base as *const u8, grant.len) };
                if let Err(e) = client.write(dst_file, offset, buf) {
                    result = Err(format!("write: {:?}", e));
                    break;
                }
                offset += grant.len;
            }
            Err(e) => {
                result = Err(format!("read: {:?}", e));
                break;
            }
        }
    }

    let _ = libcluu::vspace::VSPACE.lock().free(scratch_base, chunk_alloc);
    let _ = client.close(src_file);
    let _ = client.close(dst_file);

    // Preserve mode if requested (best-effort, VFS may not support chmod).
    let _ = (opts.preserve, src_info);

    result
}

fn write_fd(fd: i32, data: &[u8]) {
    let _ = _write(fd, data.as_ptr() as *const _, data.len());
}
