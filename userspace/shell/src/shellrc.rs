//! shellrc — source a shell rc file at startup.
//!
//! Used by `main.rs` to load `/etc/shellrc` and `$HOME/.shellrc` after
//! login but before the prompt fires. Each non-empty non-comment line
//! is fed through the existing `cluu_lang::parse_program` + builtin
//! `execute()` pair, exactly as if the user had typed it. Missing files
//! are silently skipped (so users without a `~/.shellrc` still get a
//! shell). Per-line parse / executor errors are logged via
//! `debug_print` but never abort sourcing — a broken line shouldn't
//! lock the user out of their shell.
//!
//! Why grant-based reads: shellrc files are typically short (< 1 KB)
//! but live on the ext2 userdisk, so we go through VFS like any other
//! file. We mirror the read pattern in `/bin/cat` — a single
//! page-aligned scratch region in our own vspace, filled in chunks
//! through `read_grant` and copied into a Vec for parsing.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libcluu::boot::{process_info, TOKEN_SPACE};
use libcluu::debug_print;
use libcluu::fs::client::VfsClient;
use libcluu::Result as LibResult;

use crate::commands::{BuiltinRegistry, CommandContext, CommandExecutor, ExecResult};

const READ_CHUNK: usize = 4096;
/// Hard cap on rc-file size we will source. Anything bigger is treated
/// as a corrupt / malicious file and ignored after a debug_print.
const MAX_RC_BYTES: usize = 64 * 1024;

/// Source `path` if it exists. Always returns `Ok(())` — the only
/// reason this returns a `Result` at all is so the call site stays
/// uniform with other shell helpers; missing files / parse errors /
/// executor errors are all logged-and-continued.
pub fn source_file(
    path: &str,
    stdout: usize,
    context: &mut CommandContext,
    registry: &BuiltinRegistry,
    vfs: &VfsClient,
) -> LibResult<()> {
    let bytes = match read_file_via_vfs(vfs, path) {
        Ok(b) => b,
        Err(_) => {
            let _ = debug_print(&format!("shellrc: {} not found, skipping", path));
            return Ok(());
        }
    };

    let text = match core::str::from_utf8(&bytes) {
        Ok(s) => s,
        Err(_) => {
            let _ = debug_print(&format!("shellrc: {} is not UTF-8, skipping", path));
            return Ok(());
        }
    };

    let _ = debug_print(&format!("shellrc: sourcing {}", path));
    for (idx, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        match cluu_lang::parse_program(trimmed) {
            Ok(program) => match registry.execute(stdout, context, &program) {
                Ok(ExecResult::Handled) => {}
                Ok(ExecResult::NotHandled) => {
                    let _ = debug_print(&format!(
                        "shellrc: {}:{} unsupported command",
                        path,
                        idx + 1
                    ));
                }
                Err(e) => {
                    let _ = debug_print(&format!(
                        "shellrc: {}:{} executor error: {:?}",
                        path,
                        idx + 1,
                        e
                    ));
                }
            },
            Err(e) => {
                let _ = debug_print(&format!(
                    "shellrc: {}:{} parse error: {}",
                    path,
                    idx + 1,
                    e
                ));
            }
        }
    }
    Ok(())
}

/// Read an entire file via VFS into a Vec<u8>. Mirrors the read loop
/// in `/bin/cat`: page-aligned scratch in vspace, refill via
/// `read_grant`, copy out per chunk. Returns the file's bytes or
/// surfaces the first VFS error encountered.
fn read_file_via_vfs(vfs: &VfsClient, path: &str) -> LibResult<Vec<u8>> {
    let file = vfs.open(path)?;
    let total = file.size;
    if total == 0 {
        let _ = vfs.close(file);
        return Ok(Vec::new());
    }
    if total > MAX_RC_BYTES {
        let _ = vfs.close(file);
        let _ = debug_print(&format!(
            "shellrc: {} is {} bytes, exceeds {} cap — refusing to source",
            path, total, MAX_RC_BYTES
        ));
        return Err(libcluu::Error::InvalidArgument);
    }

    let info = process_info();
    let space_token = info.tokens[TOKEN_SPACE];
    if space_token == 0 {
        let _ = vfs.close(file);
        return Err(libcluu::Error::InvalidState);
    }

    let chunk_alloc = ((READ_CHUNK.min(total)) + 4095) & !4095;
    let scratch_base = match libcluu::vspace::VSPACE.lock().alloc(chunk_alloc) {
        Ok(base) => base,
        Err(e) => {
            let _ = vfs.close(file);
            return Err(e);
        }
    };

    let mut out = Vec::with_capacity(total);
    let mut offset = 0usize;
    let mut result: LibResult<()> = Ok(());
    while offset < total {
        let remaining = total - offset;
        let want = remaining.min(READ_CHUNK);
        match vfs.read_grant(file, offset, want, space_token, scratch_base) {
            Ok(grant) => {
                if grant.len == 0 {
                    break;
                }
                let slice = unsafe {
                    core::slice::from_raw_parts(scratch_base as *const u8, grant.len)
                };
                out.extend_from_slice(slice);
                offset += grant.len;
            }
            Err(e) => {
                result = Err(e);
                break;
            }
        }
    }

    let _ = libcluu::vspace::VSPACE.lock().free(scratch_base, chunk_alloc);
    let _ = vfs.close(file);
    result.map(|_| out)
}

/// Read `HOME` from libcluu's env table (populated at `_start` from
/// the envelope-resolved env trailer in ProcessInfo). Returns `None`
/// if HOME is unset, which causes the caller to silently skip
/// `~/.shellrc`.
pub fn home_from_env() -> Option<String> {
    // Walk `snapshot_env()` instead of calling C `getenv`, to avoid
    // having to round-trip through CStr buffers from Rust.
    for (k, v) in libcluu::posix::snapshot_env() {
        if k == "HOME" {
            return Some(v);
        }
    }
    None
}
