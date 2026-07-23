//! Rust-native threading abstractions over the C ABI pthread layer.
//!
//! Provides:
//! - `Mutex<T>` — RAII futex-based mutex with `Deref`/`DerefMut` guard
//! - `Shared<T>` — heap-allocated `Mutex<T>` safe to share across threads
//! - `spawn()` — idiomatic closure-based thread creation
//! - `join()` / `sleep_ms()` — no `extern "C"` needed in consumer code

extern crate alloc;

use alloc::boxed::Box;
use core::ffi::c_void;
use core::marker::PhantomData;
use core::sync::atomic::{AtomicU32, Ordering};

pub use crate::posix::pthread::{
    pthread_create, pthread_join, pthread_mutex_init, pthread_mutex_lock, pthread_mutex_unlock,
};

// ─── Mutex ─────────────────────────────────────────────────────────────

pub struct Mutex<T> {
    inner: AtomicU32,
    data: core::cell::UnsafeCell<T>,
}

unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Send> Sync for Mutex<T> {}

pub struct MutexGuard<'a, T> {
    mutex: &'a Mutex<T>,
    _marker: PhantomData<*mut ()>,
}

impl<T> core::ops::Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.mutex.data.get() }
    }
}

impl<T> core::ops::DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<T> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        unsafe {
            pthread_mutex_unlock(&self.mutex.inner as *const _ as *mut AtomicU32);
        }
    }
}

impl<T> Mutex<T> {
    pub fn new(value: T) -> Self {
        let m = Mutex {
            inner: AtomicU32::new(0),
            data: core::cell::UnsafeCell::new(value),
        };
        unsafe {
            pthread_mutex_init(&m.inner as *const _ as *mut AtomicU32, core::ptr::null());
        }
        m
    }

    pub fn lock(&self) -> MutexGuard<'_, T> {
        unsafe {
            pthread_mutex_lock(&self.inner as *const _ as *mut AtomicU32);
        }
        MutexGuard {
            mutex: self,
            _marker: PhantomData,
        }
    }

    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        if self.inner.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_ok() {
            Some(MutexGuard { mutex: self, _marker: PhantomData })
        } else {
            None
        }
    }
}

// ─── Shared<T> ─────────────────────────────────────────────────────────

/// Heap-allocated `Mutex<T>` that is safe to share across threads by
/// cloning a handle. The underlying allocation lives until `into_inner()`
/// is called. No `Arc` / refcount — the caller is responsible for keeping
/// at least one `Shared<T>` alive.
///
/// ```
/// use libcluu::thread::Shared;
///
/// let shared = Shared::new(42);
/// let shared2 = shared.clone();
/// {
///     let mut g = shared2.lock();
///     *g += 1;
/// }
/// assert_eq!(*shared.lock(), 43);
/// ```
pub struct Shared<T> {
    ptr: *mut Mutex<T>,
}

unsafe impl<T: Send> Send for Shared<T> {}
unsafe impl<T: Send> Sync for Shared<T> {}

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Shared { ptr: self.ptr }
    }
}

impl<T> Shared<T> {
    pub fn new(value: T) -> Self {
        Shared {
            ptr: Box::into_raw(Box::new(Mutex::new(value))),
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, T> {
        unsafe { (*self.ptr).lock() }
    }

    /// Reclaim the allocation and return the inner value.
    /// Caller must ensure no other `Shared<T>` clones exist and no
    /// guards are held.
    pub fn into_inner(self) -> T {
        let boxed = unsafe { Box::from_raw(self.ptr) };
        // Extract T from Mutex. We can't move out of Mutex directly,
        // so we read the UnsafeCell after ensuring no contention.
        unsafe { core::ptr::read((*self.ptr).data.get()) }
    }
}

// ─── Thread spawn ──────────────────────────────────────────────────────

pub fn spawn<F: FnOnce() + Send + 'static>(f: F) -> usize {
    let boxed: Box<Box<dyn FnOnce()>> = Box::new(Box::new(f));
    let raw = Box::into_raw(boxed) as *mut c_void;

    let mut tid: usize = 0;
    let ret = unsafe { pthread_create(&mut tid, core::ptr::null(), thread_trampoline, raw) };
    if ret != 0 {
        unsafe { drop(Box::from_raw(raw as *mut Box<dyn FnOnce()>)); }
        return 0;
    }
    tid
}

pub fn spawn_raw(start: extern "C" fn(*mut c_void) -> *mut c_void, arg: *mut c_void) -> usize {
    let mut tid: usize = 0;
    let ret = unsafe { pthread_create(&mut tid, core::ptr::null(), start, arg) };
    if ret != 0 {
        return 0;
    }
    tid
}

pub fn join(tid: usize) -> *mut c_void {
    let mut retval: *mut c_void = core::ptr::null_mut();
    unsafe { pthread_join(tid, &mut retval) };
    retval
}

extern "C" fn thread_trampoline(arg: *mut c_void) -> *mut c_void {
    let boxed: Box<Box<dyn FnOnce()>> = unsafe { Box::from_raw(arg as *mut Box<dyn FnOnce()>) };
    boxed();
    core::ptr::null_mut()
}

// ─── Sleep ─────────────────────────────────────────────────────────────

#[repr(C)]
struct Timespec {
    tv_sec: i64,
    tv_nsec: i64,
}

extern "C" {
    fn nanosleep(req: *const Timespec, rem: *mut Timespec) -> i32;
}

/// Sleep for `ms` milliseconds. Blocks the calling thread.
pub fn sleep_ms(ms: u64) {
    let req = Timespec {
        tv_sec: (ms / 1000) as i64,
        tv_nsec: ((ms % 1000) * 1_000_000) as i64,
    };
    unsafe { nanosleep(&req, core::ptr::null_mut()) };
}
