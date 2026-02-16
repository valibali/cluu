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
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use lazy_static::lazy_static;
use spin::Mutex;

// ═══════════════════════════════════════════════════════════════════════════
// Types
// ═══════════════════════════════════════════════════════════════════════════

/// pthread_t is the thread token handle returned by ThreadCreate.
pub type pthread_t = usize;

/// pthread_attr_t — ignored for now (all threads use defaults).
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

extern "C" {
    static __tdata_start: u8;
    static __tdata_end: u8;
    static __tbss_start: u8;
    static __tbss_end: u8;
}

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
pub fn alloc_tls_block() -> Option<(usize, usize)> {
    let tmpl = get_tls_template();

    // Align TLS data size to 16 (matches linker's ALIGN(16) for @tpoff relocations).
    let tls_aligned = (tmpl.total_size + 15) & !15;

    // Block: TLS data (aligned) + TCB (self-ptr + thread token + key values).
    let block_size = tls_aligned + TCB_SIZE;

    let layout = core::alloc::Layout::from_size_align(block_size, 16).ok()?;
    let block = unsafe { alloc::alloc::alloc_zeroed(layout) };
    if block.is_null() {
        return None;
    }

    let block_addr = block as usize;

    // Copy .tdata template at start of block.
    // In x86_64 TLS variant II, the compiler accesses TLS variables at
    // negative offsets from the TCB (FS base). The linker uses the aligned
    // TLS segment size for @tpoff, so variables are at tcb - tls_aligned + offset.
    // Since tcb = block + tls_aligned, this maps to block + offset — exactly
    // where we place the .tdata copy.
    if tmpl.tdata_size > 0 {
        unsafe {
            core::ptr::copy_nonoverlapping(
                tmpl.tdata_src as *const u8,
                block,
                tmpl.tdata_size,
            );
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
fn alloc_thread_stack(num_pages: usize) -> Option<(usize, usize)> {
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
/// - `attr`: Thread attributes (ignored, pass NULL)
/// - `start_routine`: Function the new thread will execute
/// - `arg`: Argument passed to start_routine
///
/// # Returns
/// 0 on success, positive errno on error.
#[no_mangle]
pub extern "C" fn pthread_create(
    thread: *mut pthread_t,
    _attr: *const pthread_attr_t,
    start_routine: extern "C" fn(*mut c_void) -> *mut c_void,
    arg: *mut c_void,
) -> c_int {
    if thread.is_null() {
        return crate::errno::EINVAL;
    }

    let space = crate::boot::space_token();

    // 1. Allocate stack with guard page.
    let (stack_base, stack_top) = match alloc_thread_stack(DEFAULT_STACK_PAGES) {
        Some(s) => s,
        None => return crate::errno::EAGAIN,
    };

    // 2. Allocate TLS block.
    let (tls_block, tls_tcb_addr) = match alloc_tls_block() {
        Some(t) => t,
        None => {
            let _ = crate::syscall::space_unmap(space, stack_base, DEFAULT_STACK_PAGES);
            return crate::errno::EAGAIN;
        }
    };

    // 3. Allocate PthreadInternal (long-lived).
    let internal = Box::new(PthreadInternal {
        token: 0, // set after thread_create
        stack_base,
        stack_size: DEFAULT_STACK_SIZE,
        tls_block,
        tls_block_size: tls_block_alloc_size(),
        exit_value: AtomicUsize::new(0),
        exited: AtomicU32::new(0),
        detached: AtomicU32::new(0),
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
    ) {
        Ok(t) => t,
        Err(_) => {
            unsafe { drop(Box::from_raw(internal_ptr)); }
            let layout = core::alloc::Layout::from_size_align(tls_block_alloc_size(), 16).unwrap();
            unsafe { alloc::alloc::dealloc(tls_block as *mut u8, layout); }
            let _ = crate::syscall::space_unmap(space, stack_base, DEFAULT_STACK_PAGES);
            return crate::errno::EAGAIN;
        }
    };

    // 6. Store child token in PthreadInternal.
    unsafe { (*internal_ptr).token = child_token; }

    // 7. Write child token to TLS block at FS:8 (for pthread_self).
    unsafe {
        core::ptr::write((tls_tcb_addr + TLS_THREAD_TOKEN_OFFSET) as *mut usize, child_token);
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
    unsafe { *thread = child_token; }

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
    unsafe { drop(Box::from_raw(internal_ptr)); }

    // Destroy thread (kernel-side).
    let _ = crate::syscall::thread_destroy(token);

    // Free stack pages.
    let _ = crate::syscall::space_unmap(space, stack_base, stack_pages);

    // Free TLS block.
    if let Ok(layout) = core::alloc::Layout::from_size_align(tls_size, 16) {
        unsafe { alloc::alloc::dealloc(tls_block as *mut u8, layout); }
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
        let token = internal.token;
        {
            let mut threads = THREADS.lock();
            threads.remove(&thread);
        }

        let _ = crate::syscall::thread_destroy(token);
        let tls_block = internal.tls_block;
        let tls_size = internal.tls_block_size;
        if let Ok(layout) = core::alloc::Layout::from_size_align(tls_size, 16) {
            unsafe { alloc::alloc::dealloc(tls_block as *mut u8, layout); }
        }
        // Note: stack leak for detached threads (known limitation).

        unsafe { drop(Box::from_raw(internal_raw as *mut PthreadInternal)); }
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
    if val != 0 { val } else { crate::boot::token_self() }
}

/// pthread_equal — compare two thread IDs.
#[no_mangle]
pub extern "C" fn pthread_equal(t1: pthread_t, t2: pthread_t) -> c_int {
    if t1 == t2 { 1 } else { 0 }
}

/// pthread_attr_init — initialize thread attributes (stub).
#[no_mangle]
pub extern "C" fn pthread_attr_init(_attr: *mut pthread_attr_t) -> c_int {
    if !_attr.is_null() {
        unsafe { *_attr = 0; }
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
    unsafe { *attr = stacksize; }
    0
}

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
    unsafe { *stacksize = if sz == 0 { DEFAULT_STACK_SIZE } else { sz }; }
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
pub extern "C" fn pthread_mutex_init(
    mutex: *mut AtomicU32,
    _attr: *const c_void,
) -> c_int {
    if mutex.is_null() {
        return crate::errno::EINVAL;
    }
    unsafe { (*mutex).store(MUTEX_UNLOCKED, Ordering::Release); }
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
    if m.compare_exchange(MUTEX_UNLOCKED, MUTEX_LOCKED, Ordering::Acquire, Ordering::Relaxed)
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
        let _ = crate::syscall::futex_wait(
            space,
            mutex as usize,
            MUTEX_CONTENDED,
            0,
        );
    }
}

/// Try to lock a mutex without blocking.
#[no_mangle]
pub extern "C" fn pthread_mutex_trylock(mutex: *mut AtomicU32) -> c_int {
    if mutex.is_null() {
        return crate::errno::EINVAL;
    }
    let m = unsafe { &*mutex };
    if m.compare_exchange(MUTEX_UNLOCKED, MUTEX_LOCKED, Ordering::Acquire, Ordering::Relaxed)
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
pub extern "C" fn pthread_cond_init(
    cond: *mut AtomicU32,
    _attr: *const c_void,
) -> c_int {
    if cond.is_null() {
        return crate::errno::EINVAL;
    }
    unsafe { (*cond).store(0, Ordering::Release); }
    0
}

/// Destroy a condition variable (no-op).
#[no_mangle]
pub extern "C" fn pthread_cond_destroy(_cond: *mut AtomicU32) -> c_int {
    0
}

/// Wait on a condition variable. Releases mutex, waits, re-acquires mutex.
#[no_mangle]
pub extern "C" fn pthread_cond_wait(
    cond: *mut AtomicU32,
    mutex: *mut AtomicU32,
) -> c_int {
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
pub extern "C" fn pthread_once(
    once: *mut AtomicU32,
    init_routine: extern "C" fn(),
) -> c_int {
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
            unsafe { *key = i as u32; }
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

        for i in 0..PTHREAD_KEYS_MAX {
            if alloc_bits & (1 << i) == 0 {
                continue;
            }
            if let Some(dtor) = dtors[i] {
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
        internal.exit_value.store(retval as usize, Ordering::Release);
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
