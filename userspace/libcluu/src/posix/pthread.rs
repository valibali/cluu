//! POSIX pthreads implementation for CLUU.
//!
//! Provides thread lifecycle (create/join/detach), synchronization primitives
//! (mutex, condvar, once), and thread-specific data (key/getspecific/setspecific).
//! Built on CLUU's ThreadCreate invoke op and futex-based synchronization.
//!
//! # Thread creation flow
//!
//! 1. Parent allocates stack (SpaceMap) and TLS block (heap)
//! 2. Parent writes PthreadStartup on child's stack
//! 3. Parent calls ThreadCreate → gets child thread token
//! 4. Parent stores token in PthreadInternal + TLS block (FS:8)
//! 5. Parent calls ThreadSetFSBase on child token
//! 6. Parent stores ready=1, futex_wake
//! 7. Child trampoline waits for ready, then FS base is already correct
//!
//! # TLS layout (x86_64 variant II)
//!
//! ```text
//! [.tdata copy][.tbss zeroed][padding][TCB: self-ptr(8), token(8), keys(64×8)]
//!                                      ^-- FS base points here
//! ```
//!
//! - FS:0 = TCB self-pointer (required by x86_64 ABI)
//! - FS:8 = thread token (custom CLUU slot, used by pthread_self)
//! - FS:16..FS:528 = pthread_key values (64 slots × 8 bytes)
//! - Negative offsets from FS = __thread variables

extern crate alloc;

use super::{c_int, c_void};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;

// ═══════════════════════════════════════════════════════════════════════════
// Types
// ═══════════════════════════════════════════════════════════════════════════

/// pthread_t is the thread token handle returned by ThreadCreate.
pub type pthread_t = usize;

/// pthread_attr_t — stores stack size in bytes (0 = use default 64 KB).
pub type pthread_attr_t = usize;

/// Offset within the TCB where the thread token is stored.
/// Accessed as FS:8 (8 bytes after the self-pointer at FS:0).
const TLS_THREAD_TOKEN_OFFSET: usize = 8;

/// Offset within the TCB where pthread_key values start.
/// 64 key slots × 8 bytes each = 512 bytes at FS:16..FS:528.
const TLS_KEYS_OFFSET: usize = 16;

/// Maximum number of pthread keys (POSIX minimum is 128, we use 64).
const PTHREAD_KEYS_MAX: usize = 64;

/// Total TCB size: self-ptr(8) + token(8) + keys(64×8).
const TCB_SIZE: usize = 8 + 8 + PTHREAD_KEYS_MAX * 8;

/// Short-lived startup info consumed by the trampoline.
/// Placed on the child's stack before thread creation.
#[repr(C)]
struct PthreadStartup {
    start_routine: usize,           // fn(*mut c_void) -> *mut c_void
    arg: usize,                     // argument to start_routine
    internal: *mut PthreadInternal, // pointer to long-lived metadata
    ready: AtomicU32,               // futex word: 0=wait, 1=go
}

/// Long-lived thread metadata, exists until join/detach cleanup.
struct PthreadInternal {
    token: usize,
    stack_base: usize,
    stack_size: usize,
    tls_block: usize,
    tls_block_size: usize,
    exit_value: AtomicUsize,
    exited: AtomicU32, // futex word: 0=running, 1=exited
    detached: AtomicU32,
    /// Prevents double-free across join/detach/reap paths.
    /// 0 = unclaimed, 1 = claimed by a cleanup path.
    cleanup_claimed: AtomicU32,
}

// ═══════════════════════════════════════════════════════════════════════════
// Global state
// ═══════════════════════════════════════════════════════════════════════════

/// Type alias for key destructor function.
type KeyDestructor = extern "C" fn(*mut c_void);

lazy_static! {
    /// Active threads keyed by pthread_t (= thread token).
    /// Value is raw pointer to PthreadInternal (Box::into_raw).
    static ref THREADS: Mutex<BTreeMap<pthread_t, usize>> = Mutex::new(BTreeMap::new());

    /// Global pthread_key destructor table.
    /// Each slot is Some(destructor_fn) if the key is allocated, None if free.
    static ref KEY_DESTRUCTORS: Mutex<[Option<KeyDestructor>; PTHREAD_KEYS_MAX]> =
        Mutex::new([None; PTHREAD_KEYS_MAX]);
}

/// Bitmap tracking which key slots are allocated (1 = allocated).
static KEY_ALLOCATED: AtomicUsize = AtomicUsize::new(0);

const PAGE_SIZE: usize = 4096;

/// Deferred-reclaim entry for detached threads that exited on their own.
/// A detached thread can't free its own stack (it's running on it), so
/// pthread_entry pushes an entry here and pthread_create drains it.
struct ReapEntry {
    stack_base: usize,
    stack_pages: usize,
    tls_block: usize,
    tls_size: usize,
    internal_ptr: usize,
}

static REAP_QUEUE: Mutex<Vec<ReapEntry>> = Mutex::new(Vec::new());

fn reap_dead_threads() {
    let entries: Vec<ReapEntry> = core::mem::take(&mut *REAP_QUEUE.lock());
    let space = crate::boot::space_token();
    for e in entries {
        let _ = crate::syscall::space_unmap(space, e.stack_base, e.stack_pages);
        if let Ok(layout) = core::alloc::Layout::from_size_align(e.tls_size, 16) {
            unsafe {
                alloc::alloc::dealloc(e.tls_block as *mut u8, layout);
            }
        }
        unsafe {
            drop(Box::from_raw(e.internal_ptr as *mut PthreadInternal));
        }
    }
}

/// Default thread stack: 64 KB (16 pages).
const DEFAULT_STACK_PAGES: usize = 16;
const DEFAULT_STACK_SIZE: usize = DEFAULT_STACK_PAGES * PAGE_SIZE;

/// Thread stack region: 0x6000_0000 .. 0x7000_0000 (256 MB).
/// Above grant region (0x5000_0000..0x6000_0000), below initrd (0x7000_0000).
const THREAD_STACK_REGION_START: usize = 0x6000_0000;
const THREAD_STACK_REGION_END: usize = 0x7000_0000;

/// Bump allocator for thread stack addresses.
static NEXT_STACK_ADDR: AtomicUsize = AtomicUsize::new(THREAD_STACK_REGION_START);

// ═══════════════════════════════════════════════════════════════════════════
// TLS template (parsed once from linker symbols)
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(not(all(feature = "host-test", feature = "posix")))]
extern "C" {
    static __tdata_start: u8;
    static __tdata_end: u8;
    static __tbss_start: u8;
    static __tbss_end: u8;
}

#[cfg(all(feature = "host-test", feature = "posix"))]
mod host_tls_symbols {
    #[no_mangle]
    pub(super) static __tdata_start: u8 = 0;
    #[no_mangle]
    pub(super) static __tdata_end: u8 = 0;
    #[no_mangle]
    pub(super) static __tbss_start: u8 = 0;
    #[no_mangle]
    pub(super) static __tbss_end: u8 = 0;
}
#[cfg(all(feature = "host-test", feature = "posix"))]
use host_tls_symbols::*;

struct TlsTemplate {
    tdata_src: usize,
    tdata_size: usize,
    total_size: usize, // tdata + tbss
}

fn get_tls_template() -> TlsTemplate {
    let tdata_src = unsafe { &__tdata_start as *const u8 as usize };
    let tdata_size =
        unsafe { &__tdata_end as *const u8 as usize - &__tdata_start as *const u8 as usize };
    let tbss_size =
        unsafe { &__tbss_end as *const u8 as usize - &__tbss_start as *const u8 as usize };
    TlsTemplate {
        tdata_src,
        tdata_size,
        total_size: tdata_size + tbss_size,
    }
}

/// Allocate a TLS block with .tdata copied and .tbss zeroed.
/// Returns (block_addr, tcb_addr) where tcb_addr is the FS base value.
///
/// Layout (variant II):
/// ```text
/// [.tdata copy][.tbss zeroed][padding][TCB: self-ptr(8) + token(8) + keys(512)]
///                                      ^-- FS base (tcb_addr)
/// ```
///
/// Used by both init_tls (main thread) and pthread_create (child threads).
#[repr(align(64))]
struct MainTlsBlock([u8; 4096]);
static mut MAIN_TLS_BLOCK: MainTlsBlock = MainTlsBlock([0; 4096]);
static mut MAIN_TLS_USED: bool = false;

pub fn alloc_tls_block() -> Option<(usize, usize)> {
    let tmpl = get_tls_template();

    let tls_aligned = (tmpl.total_size + 15) & !15;
    let block_size = tls_aligned + TCB_SIZE;

    let layout = core::alloc::Layout::from_size_align(block_size, 16).ok()?;
    let block = unsafe { alloc::alloc::alloc_zeroed(layout) };
    if block.is_null() {
        return alloc_tls_block_static(&tmpl, tls_aligned, block_size);
    }
    let block_addr = block as usize;
    // In x86_64 TLS variant II, the compiler accesses TLS variables at
    // negative offsets from the TCB (FS base). The linker uses the aligned
    // TLS segment size for @tpoff, so variables are at tcb - tls_aligned + offset.
    // Since tcb = block + tls_aligned, this maps to block + offset — exactly
    // where we place the .tdata copy.
    if tmpl.tdata_size > 0 {
        unsafe {
            core::ptr::copy_nonoverlapping(tmpl.tdata_src as *const u8, block, tmpl.tdata_size);
        }
    }
    // .tbss follows .tdata in the TLS image and is already zeroed by alloc_zeroed.

    // TCB at aligned offset after TLS data.
    let tcb_addr = block_addr + tls_aligned;

    // Write self-pointer at FS:0 (required by x86_64 ABI).
    unsafe {
        core::ptr::write(tcb_addr as *mut usize, tcb_addr);
    }
    // FS:8 (thread token) left as 0 — caller writes it.
    // FS:16..FS:528 (key values) already zeroed by alloc_zeroed.

    Some((block_addr, tcb_addr))
}

fn alloc_tls_block_static(
    tmpl: &TlsTemplate,
    tls_aligned: usize,
    block_size: usize,
) -> Option<(usize, usize)> {
    if block_size > 4096 {
        return None;
    }
    unsafe {
        if MAIN_TLS_USED {
            return None;
        }
        MAIN_TLS_USED = true;
        let block = MAIN_TLS_BLOCK.0.as_mut_ptr();
        for i in 0..block_size {
            *block.add(i) = 0;
        }
        let block_addr = block as usize;
        if tmpl.tdata_size > 0 {
            core::ptr::copy_nonoverlapping(
                tmpl.tdata_src as *const u8,
                block,
                tmpl.tdata_size,
            );
        }
        let tcb_addr = block_addr + tls_aligned;
        core::ptr::write(tcb_addr as *mut usize, tcb_addr);
        Some((block_addr, tcb_addr))
    }
}

/// Size of allocated TLS block (for deallocation).
pub fn tls_block_alloc_size() -> usize {
    let tmpl = get_tls_template();
    let tls_aligned = (tmpl.total_size + 15) & !15;
    tls_aligned + TCB_SIZE
}

// ═══════════════════════════════════════════════════════════════════════════
// Stack allocation
// ═══════════════════════════════════════════════════════════════════════════

/// Allocate a thread stack with a guard page below.
/// Returns (stack_base, stack_top) where stack_base is the first mapped page
/// (the guard page at stack_base - PAGE_SIZE is left unmapped).
pub fn alloc_thread_stack(num_pages: usize) -> Option<(usize, usize)> {
    let size = num_pages * PAGE_SIZE;
    // Reserve: 1 guard page (unmapped) + num_pages stack pages.
    let total = PAGE_SIZE + size;

    let region_base = NEXT_STACK_ADDR.fetch_add(total, Ordering::SeqCst);
    if region_base + total > THREAD_STACK_REGION_END {
        NEXT_STACK_ADDR.fetch_sub(total, Ordering::SeqCst);
        return None;
    }

    // Guard page at region_base is left unmapped (page fault on overflow).
    let stack_base = region_base + PAGE_SIZE;

    // Map the stack pages as RW.
    let space = crate::boot::space_token();
    crate::syscall::space_map_range(space, stack_base, 0, 0x03, num_pages, 0).ok()?;

    let stack_top = stack_base + size;
    Some((stack_base, stack_top))
}

// ═══════════════════════════════════════════════════════════════════════════
// Trampoline
// ═══════════════════════════════════════════════════════════════════════════

/// Naked trampoline — first code the child thread executes.
///
/// At entry (from for_new_thread): rsp = startup_addr - 8, all GPRs = 0.
/// PthreadStartup lives at startup_addr = (original rsp) + 8.
///
/// We fix stack alignment before `call`: SysV ABI requires rsp ≡ 0 mod 16
/// at the point of a `call` instruction (so callee sees rsp ≡ 8 mod 16).
#[unsafe(naked)]
extern "C" fn pthread_trampoline() {
    core::arch::naked_asm!(
        "sub rsp, 8",            // rsp was 8 mod 16, now 0 mod 16
        "lea rdi, [rsp + 16]",   // PthreadStartup at original (rsp + 8) = new (rsp + 16)
        "call {entry}",
        "ud2",
        entry = sym pthread_entry,
    );
}

/// Rust entry point called by the trampoline.
///
/// This function does NOT use TLS before the ready handshake completes.
/// The parent sets FS base via ThreadSetFSBase before signaling ready,
/// and the context switch that wakes us applies the correct FS base.
extern "C" fn pthread_entry(startup: *mut PthreadStartup) -> ! {
    let info = unsafe { &*startup };
    let space = crate::boot::space_token();

    // Wait for parent to finish setup (ThreadSetFSBase + ready=1).
    // No TLS access here — futex_wait uses only syscalls and ProcessInfo.
    while info.ready.load(Ordering::Acquire) == 0 {
        let _ = crate::syscall::futex_wait(
            space,
            &info.ready as *const AtomicU32 as usize,
            0,
            0, // infinite timeout
        );
    }

    // FS base is now set (parent called ThreadSetFSBase, applied by context switch).
    // TLS works from here on.

    // Read fields from startup struct.
    let start_routine: extern "C" fn(*mut c_void) -> *mut c_void =
        unsafe { core::mem::transmute(info.start_routine) };
    let arg = info.arg as *mut c_void;
    let internal = info.internal;

    // Call user's thread function.
    let retval = start_routine(arg);

    // Run key destructors (POSIX requires up to 4 iterations).
    run_key_destructors();

    // Store return value and signal exit.
    let internal_ref = unsafe { &*internal };
    internal_ref
        .exit_value
        .store(retval as usize, Ordering::Release);
    internal_ref.exited.store(1, Ordering::Release);
    let _ = crate::syscall::futex_wake(
        space,
        &internal_ref.exited as *const AtomicU32 as usize,
        usize::MAX,
    );

    // Clean up errno entry for this thread.
    let my_token = internal_ref.token;
    {
        let mut table = crate::errno::ERRNO_BY_THREAD.lock();
        table.remove(&my_token);
    }

    // If detached and not yet claimed by pthread_detach, push to reap queue.
    // A detached thread can't free its own stack (running on it), so
    // pthread_create drains REAP_QUEUE on the next call.
    if internal_ref.detached.load(Ordering::Acquire) != 0 {
        if internal_ref
            .cleanup_claimed
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            REAP_QUEUE.lock().push(ReapEntry {
                stack_base: internal_ref.stack_base,
                stack_pages: internal_ref.stack_size / PAGE_SIZE,
                tls_block: internal_ref.tls_block,
                tls_size: internal_ref.tls_block_size,
                internal_ptr: internal as usize,
            });
        }
    }

    // Destroy ourselves — the kernel marks thread dead and switches away.
    let _ = crate::syscall::thread_destroy(my_token);

    // Fallback if thread_destroy somehow returns.
    loop {
        let _ = crate::syscall::yield_cpu();
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Public API
// ═══════════════════════════════════════════════════════════════════════════

/// Create a new thread.
///
/// # Arguments
/// - `thread`: Output — receives the new thread's pthread_t
/// - `attr`: Thread attributes (may be NULL; stack size from `pthread_attr_setstacksize`)
/// - `start_routine`: Function the new thread will execute
/// - `arg`: Argument passed to start_routine
///
/// # Returns
/// 0 on success, positive errno on error.
#[no_mangle]
pub extern "C" fn pthread_create(
    thread: *mut pthread_t,
    attr: *const pthread_attr_t,
    start_routine: extern "C" fn(*mut c_void) -> *mut c_void,
    arg: *mut c_void,
) -> c_int {
    if thread.is_null() {
        return crate::errno::EINVAL;
    }

    reap_dead_threads();

    let space = crate::boot::space_token();

    let stack_size = if !attr.is_null() {
        let sz = unsafe { *attr };
        if sz == 0 { DEFAULT_STACK_SIZE } else { sz }
    } else {
        DEFAULT_STACK_SIZE
    };
    let stack_pages = (stack_size + PAGE_SIZE - 1) / PAGE_SIZE;

    // 1. Allocate stack with guard page.
    let (stack_base, stack_top) = match alloc_thread_stack(stack_pages) {
        Some(s) => s,
        None => return crate::errno::EAGAIN,
    };

    // 2. Allocate TLS block.
    let (tls_block, tls_tcb_addr) = match alloc_tls_block() {
        Some(t) => t,
        None => {
            let _ = crate::syscall::space_unmap(space, stack_base, stack_pages);
            return crate::errno::EAGAIN;
        }
    };

    // 3. Allocate PthreadInternal (long-lived).
    let internal = Box::new(PthreadInternal {
        token: 0, // set after thread_create
        stack_base,
        stack_size: stack_pages * PAGE_SIZE,
        tls_block,
        tls_block_size: tls_block_alloc_size(),
        exit_value: AtomicUsize::new(0),
        exited: AtomicU32::new(0),
        detached: AtomicU32::new(0),
        cleanup_claimed: AtomicU32::new(0),
    });
    let internal_ptr = Box::into_raw(internal);

    // 4. Place PthreadStartup at the top of the stack.
    //    Align down to 16 bytes for ABI compliance.
    let startup_size = core::mem::size_of::<PthreadStartup>();
    let startup_aligned = (startup_size + 15) & !15;
    let startup_addr = stack_top - startup_aligned;

    let startup_ptr = startup_addr as *mut PthreadStartup;
    unsafe {
        core::ptr::write(
            startup_ptr,
            PthreadStartup {
                start_routine: start_routine as usize,
                arg: arg as usize,
                internal: internal_ptr,
                ready: AtomicU32::new(0),
            },
        );
    }

    // 5. Create the thread.
    //    entry = trampoline, stack = startup_addr.
    //    for_new_thread sets rsp = startup_addr - 8.
    //    Trampoline does: sub rsp,8 → lea rdi,[rsp+16] = startup_addr.
    let child_token = match crate::syscall::thread_create(
        space,
        pthread_trampoline as *const () as usize,
        startup_addr,
        128, // default priority
        0,   // flags — pthreads start running
    ) {
        Ok(t) => t,
        Err(_) => {
            unsafe {
                drop(Box::from_raw(internal_ptr));
            }
            let layout = core::alloc::Layout::from_size_align(tls_block_alloc_size(), 16).unwrap();
            unsafe {
                alloc::alloc::dealloc(tls_block as *mut u8, layout);
            }
            let _ = crate::syscall::space_unmap(space, stack_base, stack_pages);
            return crate::errno::EAGAIN;
        }
    };

    // 6. Store child token in PthreadInternal.
    unsafe {
        (*internal_ptr).token = child_token;
    }

    // 7. Write child token to TLS block at FS:8 (for pthread_self).
    unsafe {
        core::ptr::write(
            (tls_tcb_addr + TLS_THREAD_TOKEN_OFFSET) as *mut usize,
            child_token,
        );
    }

    // 8. Set FS base on the child (updates saved context.fs_base).
    //    Applied by context switch when child is next scheduled.
    let _ = crate::syscall::thread_set_fs_base(child_token, tls_tcb_addr);

    // 9. Signal ready — child can now proceed.
    unsafe {
        (*startup_ptr).ready.store(1, Ordering::Release);
    }
    let _ = crate::syscall::futex_wake(
        space,
        unsafe { &(*startup_ptr).ready as *const AtomicU32 as usize },
        1,
    );

    // 10. Record in global table.
    {
        let mut threads = THREADS.lock();
        threads.insert(child_token, internal_ptr as usize);
    }

    // 11. Write pthread_t to caller.
    unsafe {
        *thread = child_token;
    }

    0
}

/// Wait for a thread to terminate and retrieve its return value.
///
/// # Arguments
/// - `thread`: Thread to wait for
/// - `retval`: If non-null, receives the thread's return value
///
/// # Returns
/// 0 on success, positive errno on error.
#[no_mangle]
pub extern "C" fn pthread_join(thread: pthread_t, retval: *mut *mut c_void) -> c_int {
    let internal_raw = {
        let threads = THREADS.lock();
        match threads.get(&thread) {
            Some(&ptr) => ptr,
            None => return crate::errno::ESRCH,
        }
    };

    let internal_ptr = internal_raw as *mut PthreadInternal;
    let internal = unsafe { &*internal_ptr };

    // Can't join a detached thread.
    if internal.detached.load(Ordering::Acquire) != 0 {
        return crate::errno::EINVAL;
    }

    let space = crate::boot::space_token();

    // Wait for thread to exit.
    while internal.exited.load(Ordering::Acquire) == 0 {
        let _ = crate::syscall::futex_wait(
            space,
            &internal.exited as *const AtomicU32 as usize,
            0,
            0, // infinite timeout
        );
    }

    // Read return value.
    if !retval.is_null() {
        unsafe {
            *retval = internal.exit_value.load(Ordering::Acquire) as *mut c_void;
        }
    }

    // Cleanup.
    let token = internal.token;
    let stack_base = internal.stack_base;
    let stack_pages = internal.stack_size / PAGE_SIZE;
    let tls_block = internal.tls_block;
    let tls_size = internal.tls_block_size;

    // Remove from table.
    {
        let mut threads = THREADS.lock();
        threads.remove(&thread);
    }

    // Free PthreadInternal.
    unsafe {
        drop(Box::from_raw(internal_ptr));
    }

    // Destroy thread (kernel-side).
    let _ = crate::syscall::thread_destroy(token);

    // Free stack pages.
    let _ = crate::syscall::space_unmap(space, stack_base, stack_pages);

    // Free TLS block.
    if let Ok(layout) = core::alloc::Layout::from_size_align(tls_size, 16) {
        unsafe {
            alloc::alloc::dealloc(tls_block as *mut u8, layout);
        }
    }

    0
}

/// Mark a thread as detached.
///
/// A detached thread's resources are freed when it exits (with a known
/// limitation: stack/TLS leak in the current implementation).
///
/// # Returns
/// 0 on success, ESRCH if thread not found, EINVAL if already detached.
#[no_mangle]
pub extern "C" fn pthread_detach(thread: pthread_t) -> c_int {
    let internal_raw = {
        let threads = THREADS.lock();
        match threads.get(&thread) {
            Some(&ptr) => ptr,
            None => return crate::errno::ESRCH,
        }
    };

    let internal = unsafe { &*(internal_raw as *const PthreadInternal) };

    if internal
        .detached
        .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return crate::errno::EINVAL;
    }

    // If the thread has already exited, clean up now.
    if internal.exited.load(Ordering::Acquire) != 0 {
        if internal
            .cleanup_claimed
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let token = internal.token;
            let stack_base = internal.stack_base;
            let stack_pages = internal.stack_size / PAGE_SIZE;
            {
                let mut threads = THREADS.lock();
                threads.remove(&thread);
            }

            let _ = crate::syscall::thread_destroy(token);
            let _ = crate::syscall::space_unmap(
                crate::boot::space_token(),
                stack_base,
                stack_pages,
            );
            let tls_block = internal.tls_block;
            let tls_size = internal.tls_block_size;
            if let Ok(layout) = core::alloc::Layout::from_size_align(tls_size, 16) {
                unsafe {
                    alloc::alloc::dealloc(tls_block as *mut u8, layout);
                }
            }

            unsafe {
                drop(Box::from_raw(internal_raw as *mut PthreadInternal));
            }
        }
    }

    0
}

/// Get the calling thread's pthread_t.
///
/// Reads the thread token from the TLS block at FS:8.
/// For the main thread, init_tls writes token_self() there.
/// For child threads, pthread_create writes the child token.
#[no_mangle]
pub extern "C" fn pthread_self() -> pthread_t {
    let val: usize;
    unsafe {
        core::arch::asm!("mov {}, qword ptr fs:[8]", out(reg) val, options(nostack, readonly));
    }
    // Fallback if FS:8 wasn't initialized (shouldn't happen after init_tls).
    if val != 0 {
        val
    } else {
        crate::boot::token_self()
    }
}

/// pthread_equal — compare two thread IDs.
#[no_mangle]
pub extern "C" fn pthread_equal(t1: pthread_t, t2: pthread_t) -> c_int {
    if t1 == t2 {
        1
    } else {
        0
    }
}

/// Get a thread's stack region [base, base+size) for GC stack scanning.
/// Returns 0 on success, -1 if thread not found.
#[no_mangle]
pub extern "C" fn cluu_thread_stack_region(
    tid: pthread_t,
    base: *mut usize,
    size: *mut usize,
) -> c_int {
    let threads = THREADS.lock();
    match threads.get(&tid) {
        Some(&raw) => {
            let internal = unsafe { &*(raw as *const PthreadInternal) };
            unsafe {
                *base = internal.stack_base;
                *size = internal.stack_size;
            }
            0
        }
        None => -1,
    }
}

/// Suspend a thread (for GC stack scanning). Returns 0 on success.
#[no_mangle]
pub extern "C" fn cluu_thread_suspend(tid: pthread_t) -> c_int {
    match crate::syscall::thread_suspend(tid) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

/// Resume a previously suspended thread. Returns 0 on success.
#[no_mangle]
pub extern "C" fn cluu_thread_resume(tid: pthread_t) -> c_int {
    match crate::syscall::thread_resume(tid) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CluuCalleeSavedRegs {
    pub rbx: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rbp: u64,
    pub rsp: u64,
}

#[no_mangle]
pub extern "C" fn cluu_thread_get_regs(tid: pthread_t, regs: *mut CluuCalleeSavedRegs) -> c_int {
    if regs.is_null() {
        return -1;
    }
    match unsafe {
        crate::syscall::invoke(
            tid,
            crate::syscall::InvokeOp::ThreadGetStats,
            regs as usize,
            0,
            0,
            0,
        )
    } {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

/// pthread_attr_init — initialize thread attributes (stub).
#[no_mangle]
pub extern "C" fn pthread_attr_init(_attr: *mut pthread_attr_t) -> c_int {
    if !_attr.is_null() {
        unsafe {
            *_attr = 0;
        }
    }
    0
}

/// pthread_attr_destroy — destroy thread attributes (stub).
#[no_mangle]
pub extern "C" fn pthread_attr_destroy(_attr: *mut pthread_attr_t) -> c_int {
    0
}

/// pthread_attr_setstacksize — set stack size attribute (stub).
#[no_mangle]
pub extern "C" fn pthread_attr_setstacksize(attr: *mut pthread_attr_t, stacksize: usize) -> c_int {
    if attr.is_null() || stacksize < PAGE_SIZE {
        return crate::errno::EINVAL;
    }
    unsafe {
        *attr = stacksize;
    }
    0
}

/// pthread_attr_setdetachstate — set detach state attribute.
///
/// PTHREAD_CREATE_JOINABLE (1) is the default; threads are joinable.
/// PTHREAD_CREATE_DETACHED (0) marks the thread for automatic cleanup.
///
/// The current implementation treats all threads as joinable by default.
/// This function accepts both values but only JOINABLE is fully supported
/// (DETACHED is recorded but pthread_create does not auto-detach).
#[no_mangle]
pub extern "C" fn pthread_attr_setdetachstate(attr: *mut pthread_attr_t, detachstate: c_int) -> c_int {
    if attr.is_null() {
        return crate::errno::EINVAL;
    }
    match detachstate {
        PTHREAD_CREATE_JOINABLE | PTHREAD_CREATE_DETACHED => 0,
        _ => crate::errno::EINVAL,
    }
}

pub const PTHREAD_CREATE_JOINABLE: c_int = 1;
pub const PTHREAD_CREATE_DETACHED: c_int = 0;

/// pthread_attr_getstacksize — get stack size attribute (stub).
#[no_mangle]
pub extern "C" fn pthread_attr_getstacksize(
    attr: *const pthread_attr_t,
    stacksize: *mut usize,
) -> c_int {
    if attr.is_null() || stacksize.is_null() {
        return crate::errno::EINVAL;
    }
    let sz = unsafe { *attr };
    unsafe {
        *stacksize = if sz == 0 { DEFAULT_STACK_SIZE } else { sz };
    }
    0
}

// ═══════════════════════════════════════════════════════════════════════════
// Mutex (futex-based, 3-state per Drepper's "Futexes Are Tricky")
// ═══════════════════════════════════════════════════════════════════════════

// Mutex states: 0 = unlocked, 1 = locked (no waiters), 2 = locked (with waiters).
const MUTEX_UNLOCKED: u32 = 0;
const MUTEX_LOCKED: u32 = 1;
const MUTEX_CONTENDED: u32 = 2;

/// Initialize a mutex. Sets state to unlocked (0).
#[no_mangle]
pub extern "C" fn pthread_mutex_init(mutex: *mut AtomicU32, _attr: *const c_void) -> c_int {
    if mutex.is_null() {
        return crate::errno::EINVAL;
    }
    unsafe {
        (*mutex).store(MUTEX_UNLOCKED, Ordering::Release);
    }
    0
}

/// Destroy a mutex (no-op — statically allocated).
#[no_mangle]
pub extern "C" fn pthread_mutex_destroy(_mutex: *mut AtomicU32) -> c_int {
    0
}

/// Lock a mutex. Blocks until the lock is acquired.
#[no_mangle]
pub extern "C" fn pthread_mutex_lock(mutex: *mut AtomicU32) -> c_int {
    if mutex.is_null() {
        return crate::errno::EINVAL;
    }
    let m = unsafe { &*mutex };

    // Fast path: CAS 0 → 1 (uncontended).
    if m.compare_exchange(
        MUTEX_UNLOCKED,
        MUTEX_LOCKED,
        Ordering::Acquire,
        Ordering::Relaxed,
    )
    .is_ok()
    {
        return 0;
    }

    // Slow path: set state to 2 (contended) and wait.
    let space = crate::boot::space_token();
    loop {
        // If state is already 2, or we swap it to 2, wait on the futex.
        let old = m.swap(MUTEX_CONTENDED, Ordering::Acquire);
        if old == MUTEX_UNLOCKED {
            // We got the lock (it was unlocked before our swap).
            return 0;
        }
        // Wait until state changes from 2.
        let _ = crate::syscall::futex_wait(space, mutex as usize, MUTEX_CONTENDED, 0);
    }
}

/// Try to lock a mutex without blocking.
#[no_mangle]
pub extern "C" fn pthread_mutex_trylock(mutex: *mut AtomicU32) -> c_int {
    if mutex.is_null() {
        return crate::errno::EINVAL;
    }
    let m = unsafe { &*mutex };
    if m.compare_exchange(
        MUTEX_UNLOCKED,
        MUTEX_LOCKED,
        Ordering::Acquire,
        Ordering::Relaxed,
    )
    .is_ok()
    {
        0
    } else {
        crate::errno::EBUSY
    }
}

/// Unlock a mutex.
#[no_mangle]
pub extern "C" fn pthread_mutex_unlock(mutex: *mut AtomicU32) -> c_int {
    if mutex.is_null() {
        return crate::errno::EINVAL;
    }
    let m = unsafe { &*mutex };

    // Swap to unlocked. If previous state was 2 (contended), wake one waiter.
    let prev = m.swap(MUTEX_UNLOCKED, Ordering::Release);
    if prev == MUTEX_CONTENDED {
        let space = crate::boot::space_token();
        let _ = crate::syscall::futex_wake(space, mutex as usize, 1);
    }
    0
}

// ═══════════════════════════════════════════════════════════════════════════
// Condition Variable (futex-based, sequence counter)
// ═══════════════════════════════════════════════════════════════════════════

/// Initialize a condition variable. Sets sequence counter to 0.
#[no_mangle]
pub extern "C" fn pthread_cond_init(cond: *mut AtomicU32, _attr: *const c_void) -> c_int {
    if cond.is_null() {
        return crate::errno::EINVAL;
    }
    unsafe {
        (*cond).store(0, Ordering::Release);
    }
    0
}

/// Destroy a condition variable (no-op).
#[no_mangle]
pub extern "C" fn pthread_cond_destroy(_cond: *mut AtomicU32) -> c_int {
    0
}

/// Wait on a condition variable. Releases mutex, waits, re-acquires mutex.
#[no_mangle]
pub extern "C" fn pthread_cond_wait(cond: *mut AtomicU32, mutex: *mut AtomicU32) -> c_int {
    if cond.is_null() || mutex.is_null() {
        return crate::errno::EINVAL;
    }
    let c = unsafe { &*cond };

    // Read current sequence number.
    let seq = c.load(Ordering::Acquire);

    // Release the mutex before waiting.
    pthread_mutex_unlock(mutex);

    // Wait until sequence number changes (signal/broadcast increments it).
    let space = crate::boot::space_token();
    let _ = crate::syscall::futex_wait(space, cond as usize, seq, 0);

    // Re-acquire the mutex.
    pthread_mutex_lock(mutex);
    0
}

/// Wake one thread waiting on a condition variable.
#[no_mangle]
pub extern "C" fn pthread_cond_signal(cond: *mut AtomicU32) -> c_int {
    if cond.is_null() {
        return crate::errno::EINVAL;
    }
    let c = unsafe { &*cond };
    c.fetch_add(1, Ordering::Release);
    let space = crate::boot::space_token();
    let _ = crate::syscall::futex_wake(space, cond as usize, 1);
    0
}

/// Wake all threads waiting on a condition variable.
#[no_mangle]
pub extern "C" fn pthread_cond_broadcast(cond: *mut AtomicU32) -> c_int {
    if cond.is_null() {
        return crate::errno::EINVAL;
    }
    let c = unsafe { &*cond };
    c.fetch_add(1, Ordering::Release);
    let space = crate::boot::space_token();
    let _ = crate::syscall::futex_wake(space, cond as usize, usize::MAX);
    0
}

/// Compute remaining milliseconds from `now` to absolute deadline `abs`.
/// Returns 0 if the deadline has already passed.
fn compute_remaining_ms(now: &super::time::Timespec, abs: &super::time::Timespec) -> u64 {
    let now_sec = now.tv_sec as i64;
    let now_nsec = now.tv_nsec as i64;
    let abs_sec = abs.tv_sec as i64;
    let abs_nsec = abs.tv_nsec as i64;

    let diff_sec = abs_sec - now_sec;
    let diff_nsec = abs_nsec - now_nsec;

    if diff_sec < 0 || (diff_sec == 0 && diff_nsec <= 0) {
        return 0;
    }

    let ms = (diff_sec as u64) * 1000;
    let nsec_ms = (diff_nsec as u64).div_ceil(1_000_000);
    ms + nsec_ms
}

/// Wait on a condition variable with an absolute timeout.
///
/// Atomically releases `mutex`, blocks until either the condition is
/// signalled/broadcast or the absolute time `abstime` is reached, then
/// re-acquires `mutex`.
///
/// # Arguments
/// - `cond`: Condition variable to wait on
/// - `mutex`: Mutex to release/re-acquire (must be locked by caller)
/// - `abstime`: Absolute timeout, CLOCK_REALTIME-based (per POSIX)
///
/// # Returns
/// 0 on success (woken by signal/broadcast), `ETIMEDOUT` if the deadline
/// expired before a signal arrived, `EINVAL` for invalid arguments.
///
/// # Real timing
///
/// Uses the kernel futex with a bounded timeout — no busy wait, no fake
/// success. The timeout is computed as the remaining time from the current
/// CLOCK_REALTIME to `abstime`, rounded up to the nearest millisecond.
#[no_mangle]
pub extern "C" fn pthread_cond_timedwait(
    cond: *mut AtomicU32,
    mutex: *mut AtomicU32,
    abstime: *const super::time::Timespec,
) -> c_int {
    if cond.is_null() || mutex.is_null() || abstime.is_null() {
        return crate::errno::EINVAL;
    }

    let c = unsafe { &*cond };
    let abs = unsafe { &*abstime };

    if abs.tv_nsec < 0 || abs.tv_nsec >= 1_000_000_000 {
        return crate::errno::EINVAL;
    }

    let seq = c.load(Ordering::Acquire);

    let mut now = super::time::Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if super::time::clock_gettime(super::time::CLOCK_REALTIME, &mut now) != 0 {
        return crate::errno::EINVAL;
    }
    let timeout_ms = compute_remaining_ms(&now, abs);

    // Release the mutex before waiting.
    pthread_mutex_unlock(mutex);

    if timeout_ms > 0 {
        let space = crate::boot::space_token();
        let _ = crate::syscall::futex_wait(space, cond as usize, seq, timeout_ms);
    }
    // timeout_ms == 0: deadline already passed, skip waiting but still
    // check for a pending signal via the sequence counter below.

    pthread_mutex_lock(mutex);

    // futex_wait returns Ok for both timeout and signal — distinguish
    // by checking if the sequence counter changed.
    if c.load(Ordering::Acquire) == seq {
        crate::errno::ETIMEDOUT
    } else {
        0
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// pthread_once
// ═══════════════════════════════════════════════════════════════════════════

// Once states: 0 = not started, 1 = in progress, 2 = complete.
const ONCE_INIT: u32 = 0;
const ONCE_RUNNING: u32 = 1;
const ONCE_DONE: u32 = 2;

/// Execute init_routine exactly once, regardless of how many threads call this.
///
/// pthread_once_t is treated as a single AtomicU32. Note: newlib's
/// pthread_once_t is `{ int is_initialized; int init_executed; }` but since
/// `_POSIX_THREADS` isn't defined, C code declares it manually.
#[no_mangle]
pub extern "C" fn pthread_once(once: *mut AtomicU32, init_routine: extern "C" fn()) -> c_int {
    if once.is_null() {
        return crate::errno::EINVAL;
    }
    let o = unsafe { &*once };

    // Fast path: already done.
    if o.load(Ordering::Acquire) == ONCE_DONE {
        return 0;
    }

    // Try to become the initializer.
    match o.compare_exchange(ONCE_INIT, ONCE_RUNNING, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => {
            // We won the race — run the init routine.
            init_routine();
            o.store(ONCE_DONE, Ordering::Release);
            let space = crate::boot::space_token();
            let _ = crate::syscall::futex_wake(space, once as usize, usize::MAX);
        }
        Err(_) => {
            // Another thread is running or has run the init routine.
            let space = crate::boot::space_token();
            loop {
                let state = o.load(Ordering::Acquire);
                if state == ONCE_DONE {
                    break;
                }
                // Still running — wait for completion.
                let _ = crate::syscall::futex_wait(space, once as usize, ONCE_RUNNING, 0);
            }
        }
    }
    0
}

// ═══════════════════════════════════════════════════════════════════════════
// Thread-Specific Data (pthread_key)
// ═══════════════════════════════════════════════════════════════════════════

/// Create a thread-specific data key with an optional destructor.
#[no_mangle]
pub extern "C" fn pthread_key_create(
    key: *mut u32,
    destructor: Option<extern "C" fn(*mut c_void)>,
) -> c_int {
    if key.is_null() {
        return crate::errno::EINVAL;
    }

    let mut dtors = KEY_DESTRUCTORS.lock();
    // Find first free slot.
    let alloc_bits = KEY_ALLOCATED.load(Ordering::Acquire);
    for i in 0..PTHREAD_KEYS_MAX {
        if alloc_bits & (1 << i) == 0 {
            // Claim the slot.
            KEY_ALLOCATED.fetch_or(1 << i, Ordering::Release);
            dtors[i] = destructor;
            unsafe {
                *key = i as u32;
            }
            return 0;
        }
    }
    crate::errno::EAGAIN
}

/// Delete a thread-specific data key.
#[no_mangle]
pub extern "C" fn pthread_key_delete(key: u32) -> c_int {
    let idx = key as usize;
    if idx >= PTHREAD_KEYS_MAX {
        return crate::errno::EINVAL;
    }
    if KEY_ALLOCATED.load(Ordering::Acquire) & (1 << idx) == 0 {
        return crate::errno::EINVAL;
    }

    let mut dtors = KEY_DESTRUCTORS.lock();
    dtors[idx] = None;
    KEY_ALLOCATED.fetch_and(!(1 << idx), Ordering::Release);
    0
}

/// Get the value for a thread-specific data key.
/// Returns the value previously set by pthread_setspecific, or NULL.
#[no_mangle]
pub extern "C" fn pthread_getspecific(key: u32) -> *mut c_void {
    let idx = key as usize;
    if idx >= PTHREAD_KEYS_MAX {
        return core::ptr::null_mut();
    }
    let offset = TLS_KEYS_OFFSET + idx * 8;
    let val: usize;
    unsafe {
        // Read from FS:(16 + key * 8).
        core::arch::asm!(
            "mov {out}, qword ptr fs:[{offset}]",
            offset = in(reg) offset,
            out = out(reg) val,
            options(nostack, readonly),
        );
    }
    val as *mut c_void
}

/// Set the value for a thread-specific data key.
#[no_mangle]
pub extern "C" fn pthread_setspecific(key: u32, value: *const c_void) -> c_int {
    let idx = key as usize;
    if idx >= PTHREAD_KEYS_MAX {
        return crate::errno::EINVAL;
    }
    if KEY_ALLOCATED.load(Ordering::Relaxed) & (1 << idx) == 0 {
        return crate::errno::EINVAL;
    }
    let offset = TLS_KEYS_OFFSET + idx * 8;
    unsafe {
        // Write to FS:(16 + key * 8).
        core::arch::asm!(
            "mov qword ptr fs:[{offset}], {val}",
            offset = in(reg) offset,
            val = in(reg) value as usize,
            options(nostack),
        );
    }
    0
}

/// Run key destructors for the current thread.
/// POSIX requires up to PTHREAD_DESTRUCTOR_ITERATIONS (4) rounds.
fn run_key_destructors() {
    // POSIX: call destructors up to PTHREAD_DESTRUCTOR_ITERATIONS (4) rounds.
    for _round in 0..4 {
        let alloc_bits = KEY_ALLOCATED.load(Ordering::Acquire);
        if alloc_bits == 0 {
            return;
        }

        let mut any_called = false;

        // Snapshot destructors under the lock, then release before calling them.
        // This avoids holding the lock during user destructor callbacks.
        let dtors: [Option<KeyDestructor>; PTHREAD_KEYS_MAX] = *KEY_DESTRUCTORS.lock();

        for (i, dtor_opt) in dtors.iter().enumerate().take(PTHREAD_KEYS_MAX) {
            if alloc_bits & (1 << i) == 0 {
                continue;
            }
            if let Some(dtor) = *dtor_opt {
                let offset = TLS_KEYS_OFFSET + i * 8;
                let val: usize;
                unsafe {
                    core::arch::asm!(
                        "mov {out}, qword ptr fs:[{offset}]",
                        offset = in(reg) offset,
                        out = out(reg) val,
                        options(nostack, readonly),
                    );
                }
                if val != 0 {
                    // Clear the value before calling destructor (POSIX requirement).
                    unsafe {
                        core::arch::asm!(
                            "mov qword ptr fs:[{offset}], {zero}",
                            offset = in(reg) offset,
                            zero = in(reg) 0usize,
                            options(nostack),
                        );
                    }
                    dtor(val as *mut c_void);
                    any_called = true;
                }
            }
        }
        if !any_called {
            break;
        }
    }
}

/// pthread_exit — exit the calling thread with a return value.
///
/// For child threads, stores the return value and terminates.
/// For the main thread, this is a process exit.
#[no_mangle]
pub extern "C" fn pthread_exit(retval: *mut c_void) -> ! {
    // Run key destructors.
    run_key_destructors();

    // Try to find our PthreadInternal via thread token.
    let my_token = pthread_self();
    let internal_raw = {
        let threads = THREADS.lock();
        threads.get(&my_token).copied()
    };

    if let Some(raw) = internal_raw {
        let internal = unsafe { &*(raw as *const PthreadInternal) };
        internal
            .exit_value
            .store(retval as usize, Ordering::Release);
        internal.exited.store(1, Ordering::Release);
        let space = crate::boot::space_token();
        let _ = crate::syscall::futex_wake(
            space,
            &internal.exited as *const AtomicU32 as usize,
            usize::MAX,
        );
    }

    // Clean up errno entry.
    {
        let mut table = crate::errno::ERRNO_BY_THREAD.lock();
        table.remove(&my_token);
    }

    // Destroy ourselves.
    let _ = crate::syscall::thread_destroy(my_token);

    loop {
        let _ = crate::syscall::yield_cpu();
    }
}

#[cfg(all(test, feature = "host-test", feature = "posix"))]
mod tests {
    use super::*;
    use core::sync::atomic::AtomicBool;

    static DTOR_RAN_0: AtomicBool = AtomicBool::new(false);
    static DTOR_RAN_1: AtomicBool = AtomicBool::new(false);
    static DTOR_RAN_2: AtomicBool = AtomicBool::new(false);
    static DTOR_RAN_3: AtomicBool = AtomicBool::new(false);
    static DTOR_RAN_4: AtomicBool = AtomicBool::new(false);

    extern "C" fn dtor0(_: *mut c_void) { DTOR_RAN_0.store(true, Ordering::SeqCst); }
    extern "C" fn dtor1(_: *mut c_void) { DTOR_RAN_1.store(true, Ordering::SeqCst); }
    extern "C" fn dtor2(_: *mut c_void) { DTOR_RAN_2.store(true, Ordering::SeqCst); }
    extern "C" fn dtor3(_: *mut c_void) { DTOR_RAN_3.store(true, Ordering::SeqCst); }
    extern "C" fn dtor4(_: *mut c_void) { DTOR_RAN_4.store(true, Ordering::SeqCst); }

    unsafe fn set_fs_base(addr: usize) {
        let ret: isize;
        core::arch::asm!(
            "syscall",
            inlateout("rax") 158isize => ret,
            inlateout("rdi") 0x1002isize => _,
            inlateout("rsi") addr => _,
            lateout("rdx") _,
            lateout("r10") _,
            lateout("r8") _,
            lateout("r9") _,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
        let _ = ret;
    }

    unsafe fn get_fs_base() -> usize {
        let mut addr: usize = 0;
        let ret: isize;
        core::arch::asm!(
            "syscall",
            inlateout("rax") 158isize => ret,
            inlateout("rdi") 0x1003isize => _,
            inlateout("rsi") &mut addr as *mut usize => _,
            lateout("rdx") _,
            lateout("r10") _,
            lateout("r8") _,
            lateout("r9") _,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
        let _ = ret;
        addr
    }

    /// M4 verification: create 5 pthread keys with destructors, set non-NULL
    /// values, call `run_key_destructors` (the same function called on
    /// `pthread_entry` and `pthread_exit`), and assert all 5 destructors ran.
    #[test]
    fn pthread_key_destructors_all_run_on_exit() {
        for f in [&DTOR_RAN_0, &DTOR_RAN_1, &DTOR_RAN_2, &DTOR_RAN_3, &DTOR_RAN_4] {
            f.store(false, Ordering::SeqCst);
        }

        // Force lazy_static initialization BEFORE changing FS base.
        // lazy_static allocates on the heap via the global allocator, which
        // on the host uses glibc malloc — a TLS-dependent code path.
        let _ = KEY_DESTRUCTORS.lock();

        let mut tls_block = [0u8; TCB_SIZE];
        let block_addr = tls_block.as_mut_ptr() as usize;

        let saved_fs = unsafe { get_fs_base() };
        unsafe { set_fs_base(block_addr); }

        let mut keys = [0u32; 5];
        let r0 = pthread_key_create(&mut keys[0], Some(dtor0));
        let r1 = pthread_key_create(&mut keys[1], Some(dtor1));
        let r2 = pthread_key_create(&mut keys[2], Some(dtor2));
        let r3 = pthread_key_create(&mut keys[3], Some(dtor3));
        let r4 = pthread_key_create(&mut keys[4], Some(dtor4));

        let sentinel: usize = 0xBEEF;
        let s0 = pthread_setspecific(keys[0], sentinel as *const c_void);
        let s1 = pthread_setspecific(keys[1], sentinel as *const c_void);
        let s2 = pthread_setspecific(keys[2], sentinel as *const c_void);
        let s3 = pthread_setspecific(keys[3], sentinel as *const c_void);
        let s4 = pthread_setspecific(keys[4], sentinel as *const c_void);

        super::run_key_destructors();

        unsafe { set_fs_base(saved_fs); }

        assert_eq!(r0, 0, "key_create 0");
        assert_eq!(r1, 0, "key_create 1");
        assert_eq!(r2, 0, "key_create 2");
        assert_eq!(r3, 0, "key_create 3");
        assert_eq!(r4, 0, "key_create 4");
        assert_eq!(s0, 0, "setspecific 0");
        assert_eq!(s1, 0, "setspecific 1");
        assert_eq!(s2, 0, "setspecific 2");
        assert_eq!(s3, 0, "setspecific 3");
        assert_eq!(s4, 0, "setspecific 4");

        assert!(DTOR_RAN_0.load(Ordering::SeqCst), "dtor0 did not run");
        assert!(DTOR_RAN_1.load(Ordering::SeqCst), "dtor1 did not run");
        assert!(DTOR_RAN_2.load(Ordering::SeqCst), "dtor2 did not run");
        assert!(DTOR_RAN_3.load(Ordering::SeqCst), "dtor3 did not run");
        assert!(DTOR_RAN_4.load(Ordering::SeqCst), "dtor4 did not run");

        for &k in &keys {
            assert_eq!(pthread_key_delete(k), 0, "key_delete");
        }
    }

    #[test]
    fn compute_remaining_ms_deadline_passed() {
        let now = super::super::time::Timespec { tv_sec: 100, tv_nsec: 0 };
        let abs = super::super::time::Timespec { tv_sec: 99, tv_nsec: 500_000_000 };
        assert_eq!(super::compute_remaining_ms(&now, &abs), 0);
    }

    #[test]
    fn compute_remaining_ms_exact_now() {
        let now = super::super::time::Timespec { tv_sec: 100, tv_nsec: 0 };
        let abs = super::super::time::Timespec { tv_sec: 100, tv_nsec: 0 };
        assert_eq!(super::compute_remaining_ms(&now, &abs), 0);
    }

    #[test]
    fn compute_remaining_ms_50ms() {
        let now = super::super::time::Timespec { tv_sec: 100, tv_nsec: 0 };
        let abs = super::super::time::Timespec { tv_sec: 100, tv_nsec: 50_000_000 };
        assert_eq!(super::compute_remaining_ms(&now, &abs), 50);
    }

    #[test]
    fn compute_remaining_ms_1s_plus_partial() {
        let now = super::super::time::Timespec { tv_sec: 100, tv_nsec: 100_000_000 };
        let abs = super::super::time::Timespec { tv_sec: 101, tv_nsec: 150_000_000 };
        assert_eq!(super::compute_remaining_ms(&now, &abs), 1050);
    }

    #[test]
    fn compute_remaining_ms_nsec_wrap() {
        let now = super::super::time::Timespec { tv_sec: 100, tv_nsec: 900_000_000 };
        let abs = super::super::time::Timespec { tv_sec: 101, tv_nsec: 100_000_000 };
        assert_eq!(super::compute_remaining_ms(&now, &abs), 200);
    }

    #[test]
    fn cond_timedwait_null_cond_returns_einval() {
        let mutex = AtomicU32::new(0);
        let abs = super::super::time::Timespec { tv_sec: 0, tv_nsec: 0 };
        let r = super::pthread_cond_timedwait(
            core::ptr::null_mut(),
            &mutex as *const AtomicU32 as *mut AtomicU32,
            &abs,
        );
        assert_eq!(r, crate::errno::EINVAL);
    }

    #[test]
    fn cond_timedwait_null_mutex_returns_einval() {
        let cond = AtomicU32::new(0);
        let abs = super::super::time::Timespec { tv_sec: 0, tv_nsec: 0 };
        let r = super::pthread_cond_timedwait(
            &cond as *const AtomicU32 as *mut AtomicU32,
            core::ptr::null_mut(),
            &abs,
        );
        assert_eq!(r, crate::errno::EINVAL);
    }

    #[test]
    fn cond_timedwait_null_abstime_returns_einval() {
        let cond = AtomicU32::new(0);
        let mutex = AtomicU32::new(0);
        let r = super::pthread_cond_timedwait(
            &cond as *const AtomicU32 as *mut AtomicU32,
            &mutex as *const AtomicU32 as *mut AtomicU32,
            core::ptr::null(),
        );
        assert_eq!(r, crate::errno::EINVAL);
    }

    #[test]
    fn cond_timedwait_invalid_nsec_returns_einval() {
        let cond = AtomicU32::new(0);
        let mutex = AtomicU32::new(0);
        let abs = super::super::time::Timespec { tv_sec: 100, tv_nsec: 2_000_000_000 };
        let r = super::pthread_cond_timedwait(
            &cond as *const AtomicU32 as *mut AtomicU32,
            &mutex as *const AtomicU32 as *mut AtomicU32,
            &abs,
        );
        assert_eq!(r, crate::errno::EINVAL);
    }

    #[test]
    fn cond_timedwait_negative_nsec_returns_einval() {
        let cond = AtomicU32::new(0);
        let mutex = AtomicU32::new(0);
        let abs = super::super::time::Timespec { tv_sec: 100, tv_nsec: -1 };
        let r = super::pthread_cond_timedwait(
            &cond as *const AtomicU32 as *mut AtomicU32,
            &mutex as *const AtomicU32 as *mut AtomicU32,
            &abs,
        );
        assert_eq!(r, crate::errno::EINVAL);
    }

    #[test]
    fn attr_setdetachstate_joinable_returns_zero() {
        let mut attr: pthread_attr_t = 0;
        let r = pthread_attr_setdetachstate(&mut attr, PTHREAD_CREATE_JOINABLE);
        assert_eq!(r, 0);
    }

    #[test]
    fn attr_setdetachstate_detached_returns_zero() {
        let mut attr: pthread_attr_t = 0;
        let r = pthread_attr_setdetachstate(&mut attr, PTHREAD_CREATE_DETACHED);
        assert_eq!(r, 0);
    }

    #[test]
    fn attr_setdetachstate_invalid_returns_einval() {
        let mut attr: pthread_attr_t = 0;
        let r = pthread_attr_setdetachstate(&mut attr, 999);
        assert_eq!(r, crate::errno::EINVAL);
    }

    #[test]
    fn attr_setdetachstate_null_returns_einval() {
        let r = pthread_attr_setdetachstate(core::ptr::null_mut(), PTHREAD_CREATE_JOINABLE);
        assert_eq!(r, crate::errno::EINVAL);
    }

    #[test]
    fn cond_init_destroy_100_cycles_no_panic() {
        let mut cond = AtomicU32::new(0);
        for _ in 0..100 {
            assert_eq!(pthread_cond_init(&mut cond, core::ptr::null()), 0);
            assert_eq!(pthread_cond_destroy(&mut cond), 0);
        }
    }

    #[test]
    fn mutex_init_destroy_100_cycles_no_panic() {
        let mut mutex = AtomicU32::new(0);
        for _ in 0..100 {
            assert_eq!(pthread_mutex_init(&mut mutex, core::ptr::null()), 0);
            assert_eq!(pthread_mutex_destroy(&mut mutex), 0);
        }
    }

    #[test]
    fn mutex_trylock_unlock_cycle() {
        let mut mutex = AtomicU32::new(0);
        pthread_mutex_init(&mut mutex, core::ptr::null());
        assert_eq!(pthread_mutex_trylock(&mut mutex), 0);
        assert_eq!(pthread_mutex_trylock(&mut mutex), crate::errno::EBUSY);
        assert_eq!(pthread_mutex_unlock(&mut mutex), 0);
        assert_eq!(pthread_mutex_trylock(&mut mutex), 0);
        assert_eq!(pthread_mutex_unlock(&mut mutex), 0);
        pthread_mutex_destroy(&mut mutex);
    }

    #[test]
    fn cond_init_returns_einval_for_null() {
        assert_eq!(pthread_cond_init(core::ptr::null_mut(), core::ptr::null()), crate::errno::EINVAL);
    }

    #[test]
    fn mutex_init_returns_einval_for_null() {
        assert_eq!(pthread_mutex_init(core::ptr::null_mut(), core::ptr::null()), crate::errno::EINVAL);
    }
}
