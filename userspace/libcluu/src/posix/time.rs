//! Time-related syscall stubs.

use super::{c_int, c_void, clock_t, time_t};
use crate::errno::{set_errno, EINVAL};

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

    // TODO: Query timeserver or use TSC
    // For now, return 0 (epoch) - programs will see time as 1970-01-01
    unsafe {
        (*tv).tv_sec = 0;
        (*tv).tv_usec = 0;
    }

    0
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

    // Return 0 ticks elapsed
    0
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
        CLOCK_REALTIME => {
            let mut tv = Timeval {
                tv_sec: 0,
                tv_usec: 0,
            };
            _gettimeofday(&mut tv, core::ptr::null_mut());
            unsafe {
                (*tp).tv_sec = tv.tv_sec;
                (*tp).tv_nsec = tv.tv_usec * 1000;
            }
            0
        }
        CLOCK_MONOTONIC => {
            // TODO: Use TSC or scheduler ticks for monotonic time
            unsafe {
                (*tp).tv_sec = 0;
                (*tp).tv_nsec = 0;
            }
            0
        }
        _ => {
            set_errno(EINVAL);
            -1
        }
    }
}

/// Sleep for specified time.
///
/// # Arguments
/// - `seconds`: Number of seconds to sleep
///
/// # Returns
/// 0 on success, remaining seconds if interrupted.
#[no_mangle]
pub extern "C" fn sleep(seconds: u32) -> u32 {
    // Simple implementation: yield in a loop
    // Each scheduler tick is ~4ms, so 250 ticks ≈ 1 second
    let ticks = seconds as u64 * 250;
    for _ in 0..ticks {
        let _ = crate::syscall::yield_cpu();
    }
    0
}

/// Sleep for specified microseconds.
///
/// # Arguments
/// - `usec`: Microseconds to sleep
///
/// # Returns
/// 0 on success, -1 on error.
#[no_mangle]
pub extern "C" fn usleep(usec: u32) -> c_int {
    // Each tick ≈ 4000 microseconds (4ms)
    let ticks = (usec as u64 + 3999) / 4000;
    for _ in 0..ticks.max(1) {
        let _ = crate::syscall::yield_cpu();
    }
    0
}
