//! Time-related syscall stubs.

use super::{c_int, c_void, clock_t, time_t};
use crate::errno::{set_errno, EINVAL, ENOENT, ENOSYS};
use crate::ipc;
use crate::time::{TIME_GETCLOCK, TIME_GETTIMEOFDAY};
use crate::types::Message;

/// Time value structure (seconds + microseconds).
#[repr(C)]
pub struct Timeval {
    pub tv_sec: time_t,
    pub tv_usec: i64,
}

/// Timezone structure (deprecated, usually ignored).
#[repr(C)]
pub struct Timezone {
    pub tz_minuteswest: c_int,
    pub tz_dsttime: c_int,
}

/// Process times structure.
#[repr(C)]
pub struct Tms {
    pub tms_utime: clock_t,  // User CPU time
    pub tms_stime: clock_t,  // System CPU time
    pub tms_cutime: clock_t, // User CPU time of children
    pub tms_cstime: clock_t, // System CPU time of children
}

/// Timespec structure (seconds + nanoseconds).
#[repr(C)]
pub struct Timespec {
    pub tv_sec: time_t,
    pub tv_nsec: i64,
}

// Clock IDs
pub const CLOCK_REALTIME: c_int = 0;
pub const CLOCK_MONOTONIC: c_int = 1;
pub const CLOCK_PROCESS_CPUTIME_ID: c_int = 2;
pub const CLOCK_THREAD_CPUTIME_ID: c_int = 3;

/// Get current time of day.
///
/// # Arguments
/// - `tv`: Pointer to timeval structure to fill
/// - `tz`: Pointer to timezone (ignored, can be NULL)
///
/// # Returns
/// 0 on success, -1 on error.
///
/// # Notes
///
/// Currently returns a rough approximation based on scheduler ticks.
/// For accurate time, a proper timeserver should be implemented.
#[no_mangle]
pub extern "C" fn _gettimeofday(tv: *mut Timeval, _tz: *mut c_void) -> c_int {
    if tv.is_null() {
        set_errno(EINVAL);
        return -1;
    }

    match query_time(TIME_GETTIMEOFDAY) {
        Ok((sec, nsec)) => {
            unsafe {
                (*tv).tv_sec = sec as time_t;
                (*tv).tv_usec = (nsec / 1000) as i64;
            }
            0
        }
        Err(_) => {
            set_errno(ENOENT);
            -1
        }
    }
}

/// Get process times.
///
/// # Arguments
/// - `buf`: Pointer to tms structure to fill
///
/// # Returns
/// Elapsed real time in clock ticks, or -1 on error.
///
/// # Notes
///
/// Currently returns stub values. Would need scheduler tick tracking per-process.
#[no_mangle]
pub extern "C" fn _times(buf: *mut Tms) -> clock_t {
    if !buf.is_null() {
        unsafe {
            (*buf).tms_utime = 0;
            (*buf).tms_stime = 0;
            (*buf).tms_cutime = 0;
            (*buf).tms_cstime = 0;
        }
    }

    // Report elapsed time in clock ticks (100 ticks/sec)
    const TICKS_PER_SEC: i64 = 100;
    match query_time(TIME_GETCLOCK) {
        Ok((sec, nsec)) => {
            let ticks =
                (sec as i64 * TICKS_PER_SEC) + (nsec as i64 / (1_000_000_000 / TICKS_PER_SEC));
            ticks as clock_t
        }
        Err(_) => 0,
    }
}

/// Get time (seconds since epoch).
///
/// # Arguments
/// - `tloc`: Pointer to store time (can be NULL)
///
/// # Returns
/// Current time in seconds since epoch.
#[no_mangle]
pub extern "C" fn time(tloc: *mut time_t) -> time_t {
    let mut tv = Timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    _gettimeofday(&mut tv, core::ptr::null_mut());

    if !tloc.is_null() {
        unsafe {
            *tloc = tv.tv_sec;
        }
    }

    tv.tv_sec
}

/// Get clock time.
///
/// # Arguments
/// - `clock_id`: Clock to query (CLOCK_REALTIME, CLOCK_MONOTONIC, etc.)
/// - `tp`: Pointer to timespec structure to fill
///
/// # Returns
/// 0 on success, -1 on error.
#[no_mangle]
pub extern "C" fn clock_gettime(clock_id: c_int, tp: *mut Timespec) -> c_int {
    if tp.is_null() {
        set_errno(EINVAL);
        return -1;
    }

    match clock_id {
        CLOCK_REALTIME => match query_time(TIME_GETTIMEOFDAY) {
            Ok((sec, nsec)) => {
                unsafe {
                    (*tp).tv_sec = sec as time_t;
                    (*tp).tv_nsec = nsec as i64;
                }
                0
            }
            Err(_) => {
                set_errno(EINVAL);
                -1
            }
        },
        CLOCK_MONOTONIC => match query_time(TIME_GETCLOCK) {
            Ok((sec, nsec)) => {
                unsafe {
                    (*tp).tv_sec = sec as time_t;
                    (*tp).tv_nsec = nsec as i64;
                }
                0
            }
            Err(_) => {
                set_errno(EINVAL);
                -1
            }
        },
        _ => {
            set_errno(EINVAL);
            -1
        }
    }
}

/// Sleep for specified time using kernel-level timed blocking.
///
/// Uses `ipc_recv_any` with a timeout on a dummy endpoint so the kernel
/// blocks the thread without burning CPU.
///
/// # Arguments
/// - `seconds`: Number of seconds to sleep
///
/// # Returns
/// 0 on success, remaining seconds if interrupted.
#[no_mangle]
pub extern "C" fn sleep(seconds: u32) -> u32 {
    if timed_sleep_ms(seconds as u64 * 1000) {
        0
    } else {
        seconds
    }
}

/// Sleep for specified microseconds using kernel-level timed blocking.
///
/// # Arguments
/// - `usec`: Microseconds to sleep
///
/// # Returns
/// 0 on success, -1 on error.
#[no_mangle]
pub extern "C" fn usleep(usec: u32) -> c_int {
    if timed_sleep_ms(((usec as u64) + 999) / 1000) {
        0
    } else {
        set_errno(ENOSYS);
        -1
    }
}

/// POSIX nanosleep - sleep with nanosecond granularity.
///
/// Sleeps for the time specified in `req`. The actual resolution is
/// milliseconds (kernel timer granularity), so nanosecond precision
/// is rounded up to the nearest millisecond.
///
/// # Arguments
/// - `req`: Requested sleep duration
/// - `rem`: If non-NULL, stores remaining time if interrupted (always zeroed)
///
/// # Returns
/// 0 on success, -1 on error (errno set).
#[no_mangle]
pub extern "C" fn nanosleep(req: *const Timespec, rem: *mut Timespec) -> c_int {
    if req.is_null() {
        set_errno(EINVAL);
        return -1;
    }

    let ts = unsafe { &*req };
    if ts.tv_sec < 0 || ts.tv_nsec < 0 || ts.tv_nsec >= 1_000_000_000 {
        set_errno(EINVAL);
        return -1;
    }

    // Convert to milliseconds (round up so we never sleep less than requested)
    let ms = (ts.tv_sec as u64) * 1000 + ((ts.tv_nsec as u64) + 999_999) / 1_000_000;
    if !timed_sleep_ms(ms) {
        set_errno(ENOSYS);
        return -1;
    }

    // No interruption support — remaining is always zero
    if !rem.is_null() {
        unsafe {
            (*rem).tv_sec = 0;
            (*rem).tv_nsec = 0;
        }
    }

    0
}

// ═══════════════════════════════════════════════════════════════════════════
// Timed sleep via kernel blocking
// ═══════════════════════════════════════════════════════════════════════════

use core::sync::atomic::{AtomicUsize, Ordering};

/// Cached dummy endpoint for sleep operations.
/// Created once, recv on it always times out (nobody sends to it).
static SLEEP_ENDPOINT: AtomicUsize = AtomicUsize::new(0);

fn get_sleep_endpoint() -> usize {
    let cached = SLEEP_ENDPOINT.load(Ordering::Relaxed);
    if cached != 0 {
        return cached;
    }
    let info = crate::boot::process_info();
    let ipc_cap = info.tokens[crate::boot::TOKEN_IPC];
    if ipc_cap == 0 {
        return 0;
    }
    match crate::syscall::endpoint_create(ipc_cap) {
        Ok(ep) => {
            SLEEP_ENDPOINT.store(ep, Ordering::Relaxed);
            ep
        }
        Err(_) => 0,
    }
}

/// Sleep for `ms` milliseconds using kernel timed IPC recv.
///
/// Creates a dummy endpoint (lazily, once) and does a timed recv on it.
/// The kernel blocks the thread and wakes it after the timeout expires.
/// Returns false if timed blocking is unavailable.
fn timed_sleep_ms(ms: u64) -> bool {
    if ms == 0 {
        return true;
    }

    let ep = get_sleep_endpoint();
    if ep != 0 {
        let tokens = [ep];
        let mut buf = [0u8; 64];
        // This blocks until timeout — the Timeout error is expected
        let _ = crate::syscall::ipc_recv_any(&tokens, &mut buf, ms);
        return true;
    }

    false
}

fn query_time(label: u32) -> core::result::Result<(u64, u64), crate::Error> {
    let endpoint = match crate::registry::lookup_service("timeserver:main") {
        Some(ep) => ep,
        None => return Err(crate::Error::NotFound),
    };
    let mut msg = Message::new(label, [0; 6], 1);
    msg.words[0] = 0;
    ipc::call(endpoint, &mut msg, crate::IpcFlags::empty())?;
    let status = msg.words[0] as isize;
    if status < 0 {
        return Err(crate::Error::from_errno(status));
    }
    Ok((msg.words[1] as u64, msg.words[2] as u64))
}
