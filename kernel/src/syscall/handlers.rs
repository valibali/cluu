/*
 * System Call Handlers
 *
 * This module implements the actual syscall handler functions that are
 * dispatched from the syscall entry point.
 *
 * Each handler:
 * - Validates arguments from userspace (pointers, file descriptors, etc.)
 * - Performs the requested operation
 * - Returns result or error code (negative for errors)
 *
 * Security considerations:
 * - All userspace pointers MUST be validated before dereferencing
 * - File descriptors must be checked for validity
 * - Integer overflows must be prevented
 * - Resources must be properly cleaned up on error paths
 */

use super::numbers::*;
use crate::io::device::Errno;
use crate::scheduler;
use core::slice;

/// Validate a user pointer
///
/// Checks that a pointer from userspace is:
/// - Not NULL
/// - Within userspace address range (< 0x0000_8000_0000_0000)
/// - Does not overflow when adding count
///
/// Returns Ok(()) if valid, Err(error_code) otherwise.
fn validate_user_ptr<T>(ptr: *const T, count: usize) -> Result<(), isize> {
    let addr = ptr as usize;

    // Check for NULL pointer
    if addr == 0 {
        return Err(-EFAULT);
    }

    // Check if address is in kernel space (high half)
    if addr >= 0x0000_8000_0000_0000 {
        return Err(-EFAULT);
    }

    // Check for overflow when computing end address
    if addr.checked_add(count * core::mem::size_of::<T>()).is_none() {
        return Err(-EFAULT);
    }

    Ok(())
}

/// Helper: Convert Errno to negative error code
fn errno_to_code(errno: Errno) -> isize {
    -(errno as isize)
}

/// sys_write - Write to file descriptor
///
/// Arguments: (fd: i32, buf: *const u8, count: usize)
/// Returns: number of bytes written, or negative error code
pub fn sys_write(fd: i32, buf: *const u8, count: usize) -> isize {
    log::debug!("sys_write: fd={}, buf={:p}, count={}", fd, buf, count);

    // 1. Validate user buffer
    if let Err(e) = validate_user_ptr(buf, count) {
        log::debug!("sys_write: pointer validation failed: {}", e);
        return e;
    }

    // 2. Get current process's FD table
    let result = scheduler::with_current_process(|process| {
        log::debug!("sys_write: got process, looking up fd {}", fd);

        // 3. Get device from FD table
        let device = match process.fd_table.get(fd) {
            Ok(dev) => dev,
            Err(e) => {
                log::debug!("sys_write: fd_table.get({}) failed: {:?}", fd, e);
                return errno_to_code(e);
            }
        };

        // 4. Create safe slice and call device.write()
        let data = unsafe { slice::from_raw_parts(buf, count) };
        log::debug!("sys_write: writing {} bytes: {:?}", count,
            core::str::from_utf8(data).unwrap_or("<invalid utf8>"));

        match device.write(data) {
            Ok(written) => {
                log::debug!("sys_write: wrote {} bytes", written);
                written as isize
            }
            Err(e) => {
                log::debug!("sys_write: device.write() failed: {:?}", e);
                errno_to_code(e)
            }
        }
    });

    let ret = result.unwrap_or_else(|| {
        log::debug!("sys_write: with_current_process returned None");
        -EBADF
    });

    log::debug!("sys_write: returning {}", ret);
    ret
}

/// sys_read - Read from file descriptor
///
/// Arguments: (fd: i32, buf: *mut u8, count: usize)
/// Returns: number of bytes read, or negative error code
pub fn sys_read(fd: i32, buf: *mut u8, count: usize) -> isize {
    // 1. Validate user buffer
    if let Err(e) = validate_user_ptr(buf, count) {
        return e;
    }

    // 2. Get current process's FD table
    let result = scheduler::with_current_process(|process| {
        // 3. Get device from FD table
        let device = match process.fd_table.get(fd) {
            Ok(dev) => dev,
            Err(e) => return errno_to_code(e),
        };

        // 4. Create safe mutable slice and call device.read()
        let buffer = unsafe { slice::from_raw_parts_mut(buf, count) };
        match device.read(buffer) {
            Ok(read) => read as isize,
            Err(e) => errno_to_code(e),
        }
    });

    result.unwrap_or(-EBADF)
}

/// sys_isatty - Check if file descriptor is a TTY
///
/// Arguments: (fd: i32)
/// Returns: 1 if TTY, 0 if not, or negative error code
pub fn sys_isatty(fd: i32) -> isize {
    let result = scheduler::with_current_process(|process| {
        let device = match process.fd_table.get(fd) {
            Ok(dev) => dev,
            Err(e) => return errno_to_code(e),
        };

        if device.is_tty() { 1 } else { 0 }
    });

    result.unwrap_or(-EBADF)
}

/// sys_fstat - Get file status
///
/// Arguments: (fd: i32, statbuf: *mut Stat)
/// Returns: 0 on success, or negative error code
pub fn sys_fstat(fd: i32, statbuf: *mut u8) -> isize {
    // Note: statbuf is *mut u8 because we don't have Stat struct exposed yet
    // For now, just validate and return ENOSYS
    if let Err(e) = validate_user_ptr(statbuf, 128) {
        return e;
    }

    let result = scheduler::with_current_process(|process| {
        let device = match process.fd_table.get(fd) {
            Ok(dev) => dev,
            Err(e) => return errno_to_code(e),
        };

        // Get stat from device
        let stat = device.stat();

        // For now, just write a simple structure
        // In a full implementation, we'd properly serialize Stat
        unsafe {
            // Write st_mode at offset 0 (first field typically)
            *(statbuf as *mut u32) = stat.st_mode;
        }

        0
    });

    result.unwrap_or(-EBADF)
}

/// sys_close - Close file descriptor
///
/// Arguments: (fd: i32)
/// Returns: 0 on success, or negative error code
pub fn sys_close(fd: i32) -> isize {
    let result = scheduler::with_current_process_mut(|process| {
        match process.fd_table.close(fd) {
            Ok(()) => 0,
            Err(e) => errno_to_code(e),
        }
    });

    result.unwrap_or(-EBADF)
}

/// sys_lseek - Seek to position in file
///
/// Arguments: (fd: i32, offset: i64, whence: i32)
/// Returns: new file position, or negative error code
pub fn sys_lseek(fd: i32, offset: i64, whence: i32) -> isize {
    let result = scheduler::with_current_process(|process| {
        let device = match process.fd_table.get(fd) {
            Ok(dev) => dev,
            Err(e) => return errno_to_code(e),
        };

        match device.seek(offset, whence) {
            Ok(pos) => pos as isize,
            Err(e) => errno_to_code(e),
        }
    });

    result.unwrap_or(-EBADF)
}

/// sys_brk - Set program break (heap boundary)
///
/// Arguments: (addr: *mut u8)
/// Returns: new break on success, or negative error code
///
/// This implements the Unix _sbrk syscall with lazy allocation:
/// - Updates the heap boundary (current_brk)
/// - Does NOT allocate physical pages immediately
/// - Physical pages are allocated on first access via page fault handler
pub fn sys_brk(addr: *mut u8) -> isize {
    let new_brk = addr as usize;

    let result = scheduler::with_current_process_mut(|process| {
        let heap = &mut process.address_space.heap;

        // If addr is 0, return current brk (query mode)
        if new_brk == 0 {
            return heap.current_brk.as_u64() as isize;
        }

        // Validate: must be within heap region bounds
        if new_brk < heap.start.as_u64() as usize {
            return -EINVAL; // Below heap start
        }
        if new_brk > heap.max.as_u64() as usize {
            return -ENOMEM; // Would exceed heap limit
        }

        // Update brk (lazy allocation - pages allocated on page fault)
        heap.current_brk = x86_64::VirtAddr::new(new_brk as u64);

        log::debug!("sys_brk: set brk to {:#x} (heap size: {} bytes)",
                    new_brk, heap.size());

        new_brk as isize
    });

    result.unwrap_or(-EFAULT)
}

/// sys_exit - Exit current thread/process
///
/// Arguments: (status: i32)
/// Does not return
pub fn sys_exit(status: i32) -> ! {
    log::info!("Thread exiting with status {}", status);
    scheduler::exit_thread();
}

/// sys_yield - Yield CPU to scheduler
///
/// Arguments: ()
/// Returns: 0 on success
pub fn sys_yield() -> isize {
    scheduler::yield_now();
    0
}
