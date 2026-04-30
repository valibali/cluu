//! VFS load + atomic save. Mirror shellrc.rs::read_file_via_vfs (UE18).
//!
//! Atomic save sequence (T0 finding 7):
//!   open_with(tmp, O_WRONLY|O_CREAT|O_TRUNC, 0o644)
//!     -> write(file, 0, &bytes) -> close(file) -> rename(tmp, final)
//! On rename failure, best-effort unlink(tmp) so we don't leave .edit~ litter.

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;
use crate::buffer::EditBuffer;
use crate::mode::Editor;

use libcluu::boot::{process_info, TOKEN_SPACE};
use libcluu::fs::client::{VfsClient, VfsFile};
use libcluu::posix::{O_CREAT, O_TRUNC, O_WRONLY};
use libcluu::registry;

pub const MAX_FILE_BYTES: usize = 1 << 20; // 1 MiB cap (spec §12)
const READ_CHUNK: usize = 4096;

fn connect_vfs() -> Result<VfsClient, String> {
    let endpoint = registry::subscribe_output("vfs", "main")
        .map_err(|_| String::from("E484: Cannot open VFS"))?;
    VfsClient::new_from_registry(endpoint)
        .map_err(|_| String::from("E484: Cannot open VFS"))
}

pub fn load(state: &mut Editor, path: &str) {
    if path.is_empty() {
        state.message = "E32: No file name".into();
        return;
    }
    // Resolve relative paths against CWD so `edit hello.txt` from /home
    // talks to the VFS about /home/hello.txt, not a bare "hello.txt".
    let resolved = libcluu::posix::resolve_path(path);
    let mut vfs = match connect_vfs() {
        Ok(v) => v,
        Err(e) => { state.message = e; return; }
    };
    match read_file(&mut vfs, &resolved) {
        Ok(bytes) => {
            state.buf = EditBuffer::new(bytes, Some(resolved.clone()));
            state.message = alloc::format!("\"{}\" loaded", resolved);
        }
        Err(e) => {
            // Vim convention: opening a non-existent path creates a new
            // buffer named after the path so `:w` writes to it. We can't
            // distinguish NotFound from other errors via the current Vfs
            // API surface, so for any open failure we still attach the
            // path to the buffer and let the user see the error message.
            // Save will hit the real error if it's anything but NotFound.
            state.buf = EditBuffer::new(Vec::new(), Some(resolved.clone()));
            state.message = if e.contains("Cannot open") {
                alloc::format!("\"{}\" [New File]", resolved)
            } else {
                e
            };
        }
    }
}

pub fn save(state: &mut Editor, override_path: Option<&str>) {
    let target_path = override_path
        .map(libcluu::posix::resolve_path)
        .or_else(|| state.buf.path.clone());
    let Some(path) = target_path else {
        state.message = "E32: No file name".into();
        return;
    };
    let bytes = state.buf.pieces.read_all();
    let mut vfs = match connect_vfs() {
        Ok(v) => v,
        Err(e) => { state.message = e; return; }
    };
    if let Err(e) = save_atomic(&mut vfs, &path, &bytes) {
        state.message = e;
        return;
    }
    state.buf.path = Some(path.clone());
    state.buf.mark_clean();
    let line_count = state.buf.pieces.line_count();
    state.message = alloc::format!("\"{}\" {}L written", path, line_count);
}

/// Read entire file into a Vec<u8> via the VFS read-grant zero-copy
/// pattern. Adapted from `userspace/shell/src/shellrc.rs::read_file_via_vfs`
/// (UE18). 4 KiB chunks; 1 MiB cap per spec §12.
fn read_file(vfs: &mut VfsClient, path: &str) -> Result<Vec<u8>, String> {
    let file = vfs.open(path)
        .map_err(|e| alloc::format!("E484: Cannot open \"{}\": {:?}", path, e))?;
    let total = file.size;
    if total == 0 {
        let _ = vfs.close(file);
        return Ok(Vec::new());
    }
    if total > MAX_FILE_BYTES {
        let _ = vfs.close(file);
        return Err(alloc::format!(
            "E5: File \"{}\" is {} bytes, exceeds {} cap",
            path, total, MAX_FILE_BYTES
        ));
    }

    let info = process_info();
    let space_token = info.tokens[TOKEN_SPACE];
    if space_token == 0 {
        let _ = vfs.close(file);
        return Err(String::from("E484: No space token"));
    }

    let chunk_alloc = ((READ_CHUNK.min(total)) + 4095) & !4095;
    let scratch_base = match libcluu::vspace::VSPACE.lock().alloc(chunk_alloc) {
        Ok(base) => base,
        Err(e) => {
            let _ = vfs.close(file);
            return Err(alloc::format!("E484: vspace alloc failed: {:?}", e));
        }
    };

    let mut out: Vec<u8> = Vec::with_capacity(total);
    let mut offset = 0usize;
    let mut err: Option<String> = None;
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
                err = Some(alloc::format!("E484: read failed: {:?}", e));
                break;
            }
        }
    }

    let _ = libcluu::vspace::VSPACE.lock().free(scratch_base, chunk_alloc);
    let _ = vfs.close(file);
    match err {
        Some(e) => Err(e),
        None => Ok(out),
    }
}

fn save_atomic(vfs: &mut VfsClient, path: &str, bytes: &[u8]) -> Result<(), String> {
    let tmp = alloc::format!("{}.edit~", path);
    if let Err(e) = write_all(vfs, &tmp, bytes) {
        return Err(alloc::format!("E212: Cannot open for writing: {:?}", e));
    }
    if let Err(e) = vfs.rename(&tmp, path) {
        let _ = vfs.unlink(&tmp); // best-effort cleanup
        return Err(alloc::format!("E212: Cannot rename: {:?}", e));
    }
    Ok(())
}

/// Open `path` with O_WRONLY|O_CREAT|O_TRUNC (mode 0o644), single
/// `vfs.write(file, 0, bytes)` call, then close. Per T0 finding 7:
/// no whole-file write helper exists; this is the canonical sequence.
/// Pattern modeled on `userspace/touch/src/main.rs:42-46`.
fn write_all(vfs: &mut VfsClient, path: &str, bytes: &[u8]) -> Result<(), libcluu::Error> {
    let file: VfsFile = vfs.open_with(
        path,
        (O_WRONLY | O_CREAT | O_TRUNC) as usize,
        0o644,
    )?;
    let write_res = vfs.write(file, 0, bytes);
    let close_res = vfs.close(file);
    match write_res {
        Ok(written) if written == bytes.len() => close_res,
        Ok(_) => Err(libcluu::Error::InvalidState),
        Err(e) => {
            // Surface the write error; close error is secondary.
            let _ = close_res;
            Err(e)
        }
    }
}
