//! Userspace runtime - entry point and panic handler

#[cfg(all(not(feature = "std"), not(test)))]
use core::panic::PanicInfo;

#[cfg(all(not(feature = "std"), not(test), target_os = "none"))]
use crate::allocator;
#[cfg(all(not(feature = "std"), not(test)))]
use crate::{ipc::notify_exit, syscall::debug_print, syscall::yield_cpu};
#[cfg(all(not(feature = "std"), not(test)))]
use core::fmt::{self, Write};

struct StackString<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> StackString<N> {
    const fn new() -> Self {
        Self { buf: [0u8; N], len: 0 }
    }

    fn as_str(&self) -> &str {
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

/// Entry point for userspace programs
///
/// This is called by the kernel after loading the program.
/// It sets up the runtime and calls main().
#[cfg(all(not(feature = "std"), not(test), target_os = "none"))]
#[no_mangle]
#[link_section = ".text._start"]
pub extern "C" fn _start() -> ! {
    extern "Rust" {
        fn main() -> i32;
    }

    allocator::init();

    let exit_code = unsafe { main() };
    if exit_code != 0 {
        let _ = debug_print("userspace: exiting with error");
    }
    let _ = notify_exit(exit_code);

    // Block forever waiting for procmgr to destroy us
    // Create a temporary endpoint to block on (prevents CPU consumption)
    let info = crate::boot::process_info();
    let proc_cap = info.tokens[crate::boot::TOKEN_PROC_CAP];
    if proc_cap != 0 {
        if let Ok(ep) = crate::syscall::endpoint_create(proc_cap) {
            let mut buf = [0u8; 64];
            let _ = crate::syscall::ipc_recv(ep, &mut buf);
        }
    }
    // Fallback: yield loop (only if endpoint creation failed)
    loop {
        let _ = yield_cpu();
    }
}

/// Panic handler for userspace
///
/// Note: This is only active when building for the target (not during testing)
#[cfg(all(not(feature = "std"), not(test)))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let proc_info = crate::boot::process_info();
    let heap_stats = allocator::stats();
    let mut line = StackString::<512>::new();
    let _ = write!(
        &mut line,
        "userspace: panic cookie={} stdin={} stdout={} stderr={} stdlog={} reg={} proc_cap={} space={} heap_used={}/{} peak={} free={}",
        proc_info.exit_cookie,
        proc_info.tokens[crate::boot::TOKEN_STDIN],
        proc_info.tokens[crate::boot::TOKEN_STDOUT],
        proc_info.tokens[crate::boot::TOKEN_STDERR],
        proc_info.tokens[crate::boot::TOKEN_STDLOG],
        proc_info.tokens[crate::boot::TOKEN_REGISTRY],
        proc_info.tokens[crate::boot::TOKEN_PROC_CAP],
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
    let proc_cap = proc_info.tokens[crate::boot::TOKEN_PROC_CAP];
    if proc_cap != 0 {
        if let Ok(ep) = crate::syscall::endpoint_create(proc_cap) {
            let mut buf = [0u8; 64];
            let _ = crate::syscall::ipc_recv(ep, &mut buf);
        }
    }
    // Fallback: yield loop
    loop {
        let _ = yield_cpu();
    }
}
