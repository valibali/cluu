//! `history` builtin + persistent history load/save helpers.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use libcluu::Result;

use super::registry::CommandContext;
use super::registry::{BuiltinCommand, BuiltinRegistry, WriteSink};

pub fn register(registry: &mut BuiltinRegistry) {
    registry.register(Box::new(HistoryBuiltin));
}

pub(crate) struct HistoryBuiltin;

impl BuiltinCommand for HistoryBuiltin {
    fn name(&self) -> &'static str {
        "history"
    }

    fn run_with_sink(
        &self,
        stdout: &WriteSink,
        context: &mut CommandContext,
        args: &[String],
    ) -> Result<()> {
        let n: usize = args
            .first()
            .and_then(|s| s.parse().ok())
            .unwrap_or(usize::MAX);
        let total = context.history.len();
        for (idx, line) in context.history.iter().enumerate() {
            if total.saturating_sub(idx) > n {
                continue;
            }
            stdout.write_all(format!("{:>5}  {}\n", idx + 1, line).as_bytes())?;
        }
        Ok(())
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        self.run_with_sink(&WriteSink::Tty(stdout), context, args)
    }
}

// ─── Persistent history helpers ───────────────────────────────────────────────

const HISTORY_PATH_SUFFIX: &str = "/.cluu_history";
const HOME_FALLBACK: &str = "/home/root";

fn history_path() -> String {
    // Walk process env for HOME; fall back to /home/root.
    let home = libcluu::posix::snapshot_env()
        .into_iter()
        .find(|(k, _)| k == "HOME")
        .map(|(_, v)| v)
        .unwrap_or_else(|| String::from(HOME_FALLBACK));
    format!("{}{}", home, HISTORY_PATH_SUFFIX)
}

fn vfs_client() -> Option<libcluu::fs::client::VfsClient> {
    let ep = libcluu::registry::subscribe_output("vfs", "main").ok()?;
    libcluu::fs::client::VfsClient::new_from_registry(ep).ok()
}

/// Load history from ~/.cluu_history into `ctx.history`.
/// Silent on any error (missing file, missing VFS endpoint, etc.).
pub fn load_history(ctx: &mut CommandContext) {
    let path = history_path();
    let Some(vfs) = vfs_client() else { return; };
    let Ok(file) = vfs.open(&path) else { return; };
    if file.size == 0 {
        crate::io::report_err(vfs.close(file), "vfs.close");
        return;
    }

    // Read via grant like shellrc does.
    let info = libcluu::boot::process_info();
    let space_token = info.tokens[libcluu::boot::TOKEN_SPACE];
    if space_token == 0 {
        crate::io::report_err(vfs.close(file), "vfs.close");
        return;
    }
    let total = file.size;
    let chunk_alloc = (total.min(4096) + 4095) & !4095;
    let scratch_base = match libcluu::vspace::VSPACE.lock().alloc(chunk_alloc) {
        Ok(b) => b,
        Err(_) => {
            crate::io::report_err(vfs.close(file), "vfs.close");
            return;
        }
    };

    let mut buf: Vec<u8> = Vec::with_capacity(total);
    let mut offset = 0usize;
    while offset < total {
        let want = (total - offset).min(4096);
        match vfs.read_grant(file, offset, want, space_token, scratch_base) {
            Ok(grant) => {
                if grant.len == 0 {
                    break;
                }
                let slice = unsafe {
                    core::slice::from_raw_parts(scratch_base as *const u8, grant.len)
                };
                buf.extend_from_slice(slice);
                offset += grant.len;
            }
            Err(_) => break,
        }
    }
    let _ = libcluu::vspace::VSPACE.lock().free(scratch_base, chunk_alloc);
    crate::io::report_err(vfs.close(file), "vfs.close");

    if let Ok(s) = core::str::from_utf8(&buf) {
        let lines: Vec<String> = s.lines().map(|l| l.to_string()).collect();
        ctx.history.replace_all(lines);
    }
}

/// Save history to ~/.cluu_history. Overwrites the file.
/// Silent on any error.
pub fn save_history(ctx: &CommandContext) {
    let path = history_path();
    let Some(vfs) = vfs_client() else { return; };

    // Build content first.
    let mut content = String::new();
    for l in ctx.history.iter() {
        content.push_str(l);
        content.push('\n');
    }
    let bytes = content.as_bytes();

    // O_WRONLY=1, O_CREAT=0o100, O_TRUNC=0o1000
    const FLAGS: usize = 1 | 0o100 | 0o1000;
    let Ok(file) = vfs.open_with(&path, FLAGS, 0o644) else { return; };
    crate::io::report_err(vfs.write(file, 0, bytes), "vfs.write");
    crate::io::report_err(vfs.close(file), "vfs.close");
}
