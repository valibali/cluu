//! Userspace runtime - entry point and panic handler

#[cfg(all(not(feature = "std"), not(test), target_os = "none"))]
use core::panic::PanicInfo;

#[cfg(all(not(feature = "std"), not(test), not(feature = "c-runtime"), target_os = "none"))]
use crate::allocator;
#[cfg(all(not(feature = "std"), not(test), not(feature = "c-runtime"), target_os = "none"))]
use crate::{ipc::notify_exit, syscall::debug_print, syscall::yield_cpu};
#[cfg(all(not(feature = "std"), not(test), not(feature = "c-runtime"), target_os = "none"))]
use core::fmt::Write;

#[cfg(all(not(feature = "std"), not(test), not(feature = "c-runtime"), target_os = "none"))]
mod stack_string {
    use core::fmt::{self, Write};

    pub struct StackString<const N: usize> {
        buf: [u8; N],
        len: usize,
    }

    impl<const N: usize> StackString<N> {
        pub const fn new() -> Self {
            Self {
                buf: [0u8; N],
                len: 0,
            }
        }

        pub fn as_str(&self) -> &str {
            // Safety: we only write valid UTF-8 through core::fmt::Write.
            unsafe { core::str::from_utf8_unchecked(&self.buf[..self.len]) }
        }
    }

    impl<const N: usize> Write for StackString<N> {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            let bytes = s.as_bytes();
            let available = N.saturating_sub(self.len);
            let to_copy = bytes.len().min(available);
            self.buf[self.len..self.len + to_copy].copy_from_slice(&bytes[..to_copy]);
            self.len += to_copy;
            Ok(())
        }
    }
}

#[cfg(all(not(feature = "std"), not(test), not(feature = "c-runtime"), target_os = "none"))]
use stack_string::StackString;

/// Entry point for userspace programs
///
/// This is called by the kernel after loading the program.
/// It sets up the runtime and calls main().
///
/// Note: This is disabled when the `c-runtime` feature is enabled,
/// as C programs use crt0.S for their entry point instead.
#[cfg(all(
    not(feature = "std"),
    not(test),
    not(feature = "c-runtime"),
    target_os = "none"
))]
#[no_mangle]
#[link_section = ".text._start"]
pub extern "C" fn _start() -> ! {
    extern "C" {
        fn main() -> i32;
    }

    allocator::init();

    // When the posix feature is enabled, do full unified init (TLS + fd table + registry + cwd + env).
    // This mirrors __cluu_init() used by C programs via crt0.S.
    #[cfg(feature = "posix")]
    {
        crate::posix::init_tls();
        crate::fd_table::init_stdio();
        let _ = crate::registry::init("app");
        crate::posix::init_cwd();
        crate::posix::init_env();
        crate::args::init();

        // Apply file redirections written by the shell into the ProcessInfo page.
        // Must run AFTER init_stdio (so entries exist to replace) and AFTER
        // registry::init (so VFS lookups work), but BEFORE main().
        {
            let info = crate::boot::process_info();
            let redir_offset = info.params[crate::boot::PARAM_REDIR_OFFSET] as usize;
            let redir_len = info.params[crate::boot::PARAM_REDIR_LEN] as usize;
            if redir_offset > 0 && redir_len > 0 {
                let page_base = crate::boot::PROCESS_INFO_ADDR & !(crate::mem::PAGE_SIZE - 1);
                let redir_ptr = (page_base + redir_offset) as *const u8;
                let redir_slice = unsafe { core::slice::from_raw_parts(redir_ptr, redir_len) };
                apply_redirections(redir_slice);
            }
        }
    }

    #[cfg(not(feature = "posix"))]
    crate::args::init();

    let exit_code = unsafe { main() };
    if exit_code != 0 {
        let _ = debug_print("userspace: exiting with error");
    }

    // Close pipe write ends before notifying exit so downstream readers
    // receive EOF before the shell's wait loop sees the exit notification.
    // We use process_info directly (no Vec allocation) to avoid heap use
    // during the exit path.
    #[cfg(feature = "posix")]
    {
        let info = crate::boot::process_info();
        let pipe_mask = info.params[crate::boot::PARAM_FD_PIPE_MASK] as u8;
        // Check each of stdin(0), stdout(1), stderr(2), stdlog(3) for write-pipe.
        // stdin is always read-only; stdout/stderr/stdlog may be write-pipe.
        for fd_idx in 1u8..=3u8 {
            if pipe_mask & (1 << fd_idx) != 0 {
                let token_idx = fd_idx as usize; // TOKEN_STDOUT=1, TOKEN_STDERR=2, TOKEN_STDLOG=3
                let ep = info.tokens[token_idx];
                if ep != 0 {
                    crate::posix::pipe::close_pipe_write(ep);
                }
            }
        }
    }

    let _ = notify_exit(exit_code);

    // Block forever waiting for procmgr to destroy us
    // Create a temporary endpoint to block on (prevents CPU consumption)
    let info = crate::boot::process_info();
    let ipc_cap = info.tokens[crate::boot::TOKEN_IPC];
    if ipc_cap != 0 {
        if let Ok(ep) = crate::syscall::endpoint_create(ipc_cap) {
            let mut buf = [0u8; 64];
            let _ = crate::syscall::ipc_recv(ep, &mut buf);
        }
    }
    // Fallback: yield loop (only if endpoint creation failed)
    loop {
        let _ = yield_cpu();
    }
}

/// Open files listed in the REDIR trailer and replace the corresponding fd_table
/// entries so that subsequent reads/writes go to the redirected files.
///
/// Called from `_start` after `init_stdio` and `registry::init`, before `main`.
/// Best-effort: if a file cannot be opened, the original fd entry is left intact
/// and the child will see EBADF/EIO on that fd (matching shell error semantics).
#[cfg(all(
    not(feature = "std"),
    not(test),
    not(feature = "c-runtime"),
    feature = "posix",
    target_os = "none"
))]
fn apply_redirections(redir_bytes: &[u8]) {
    use crate::posix::file::{O_APPEND, O_CREAT, O_RDONLY, O_TRUNC, O_WRONLY};

    let vfs_endpoint = match crate::registry::lookup_service("vfs:main") {
        Some(ep) => ep,
        None => return,
    };
    let client_id = crate::registry::control_endpoint();
    if client_id == 0 {
        return;
    }
    let vfs = crate::fs::client::VfsClient::new(vfs_endpoint, client_id);

    let mut cursor = 0usize;
    while cursor + 4 <= redir_bytes.len() {
        let target_fd = redir_bytes[cursor];
        let flags = redir_bytes[cursor + 1];
        let path_len = u16::from_le_bytes([redir_bytes[cursor + 2], redir_bytes[cursor + 3]]) as usize;
        cursor += 4;
        if cursor + path_len > redir_bytes.len() {
            break;
        }
        let path_bytes = &redir_bytes[cursor..cursor + path_len];
        cursor += path_len;

        let path = match core::str::from_utf8(path_bytes) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let posix_flags: usize = match flags {
            1 => (O_WRONLY | O_CREAT | O_TRUNC) as usize,
            2 => (O_WRONLY | O_CREAT | O_APPEND) as usize,
            3 => O_RDONLY as usize,
            _ => continue,
        };

        let resolved = crate::posix::resolve_path(path);
        let file = match vfs.open_with(&resolved, posix_flags, 0o644) {
            Ok(f) => f,
            Err(_) => continue,
        };

        let readable = flags == 3;
        let writable = flags == 1 || flags == 2;
        let mut entry = crate::fd_table::FdEntry::file(
            vfs_endpoint,
            file.fd,
            client_id,
            readable,
            writable,
        );
        if flags == 2 {
            // O_APPEND: position at end of file so writes go there.
            entry.position = file.size as u64;
        }
        crate::fd_table::FD_TABLE.lock().replace(target_fd as i32, entry);
    }
}

/// Panic handler for Rust userspace programs
///
/// Note: This is only active when building for the target (not during testing)
/// and when not using the C runtime (which has its own simpler panic handler).
#[cfg(all(
    not(feature = "std"),
    not(test),
    not(feature = "c-runtime"),
    target_os = "none"
))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    #[cfg(feature = "posix")]
    crate::posix::tty::restore_on_panic();

    let proc_info = crate::boot::process_info();
    let heap_stats = allocator::stats();
    let mut line = StackString::<512>::new();
    let _ = write!(
        &mut line,
        "userspace: panic cookie={} stdin={} stdout={} stderr={} stdlog={} reg={} ipc={} space={} heap_used={}/{} peak={} free={}",
        proc_info.exit_cookie,
        proc_info.tokens[crate::boot::TOKEN_STDIN],
        proc_info.tokens[crate::boot::TOKEN_STDOUT],
        proc_info.tokens[crate::boot::TOKEN_STDERR],
        proc_info.tokens[crate::boot::TOKEN_STDLOG],
        proc_info.tokens[crate::boot::TOKEN_REGISTRY],
        proc_info.tokens[crate::boot::TOKEN_IPC],
        proc_info.tokens[crate::boot::TOKEN_SPACE],
        heap_stats.used,
        heap_stats.total,
        heap_stats.peak,
        heap_stats.free,
    );
    if let Some(loc) = info.location() {
        let _ = write!(
            &mut line,
            " at {}:{}:{}",
            loc.file(),
            loc.line(),
            loc.column()
        );
    }
    let _ = write!(&mut line, " msg={}", info.message());
    let _ = debug_print(line.as_str());
    let _ = notify_exit(-1);

    // Block forever waiting for procmgr to destroy us
    let ipc_cap = proc_info.tokens[crate::boot::TOKEN_IPC];
    if ipc_cap != 0 {
        if let Ok(ep) = crate::syscall::endpoint_create(ipc_cap) {
            let mut buf = [0u8; 64];
            let _ = crate::syscall::ipc_recv(ep, &mut buf);
        }
    }
    // Fallback: yield loop
    loop {
        let _ = yield_cpu();
    }
}

/// Simple panic handler for C runtime
///
/// Used when building for C programs. Just halts the CPU.
#[cfg(all(
    not(feature = "std"),
    not(test),
    feature = "c-runtime",
    target_os = "none"
))]
#[panic_handler]
fn panic_c(_info: &PanicInfo) -> ! {
    // For C programs, just halt. The C program should use _exit() for clean shutdown.
    loop {
        unsafe {
            core::arch::asm!("hlt");
        }
    }
}
