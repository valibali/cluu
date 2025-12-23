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
    crate::utils::debug::irq_log::irq_log_simple("sys_write");

    // 1. Validate user buffer
    if let Err(e) = validate_user_ptr(buf, count) {
        log::debug!("sys_write: pointer validation failed: {}", e);
        return e;
    }

    // 2. Get device from FD table (with interrupts disabled)
    // CRITICAL: We must NOT call device.write() inside with_current_process()
    // because device.write() may block, and blocking requires interrupts enabled!
    let device = scheduler::with_current_process(|process| {
        log::debug!("sys_write: got process, looking up fd {}", fd);

        match process.fd_table.get(fd) {
            Ok(dev) => Ok(dev.clone()),  // Clone the Arc<dyn Device>
            Err(e) => {
                log::debug!("sys_write: fd_table.get({}) failed: {:?}", fd, e);
                Err(errno_to_code(e))
            }
        }
    });

    let device = match device {
        Some(Ok(dev)) => dev,
        Some(Err(e)) => return e,
        None => {
            log::debug!("sys_write: with_current_process returned None");
            return -EBADF;
        }
    };

    // 3. Call device.write() OUTSIDE with_current_process() so interrupts are enabled
    let data = unsafe { slice::from_raw_parts(buf, count) };
    log::debug!("sys_write: writing {} bytes: {:?}", count,
        core::str::from_utf8(data).unwrap_or("<invalid utf8>"));

    let ret = match device.write(data) {
        Ok(written) => {
            log::debug!("sys_write: wrote {} bytes", written);
            written as isize
        }
        Err(e) => {
            log::debug!("sys_write: device.write() failed: {:?}", e);
            errno_to_code(e)
        }
    };

    log::debug!("sys_write: returning {}", ret);
    ret
}

/// sys_open - Open file
///
/// Arguments: (path: *const u8, flags: i32, mode: i32)
/// Returns: file descriptor on success, or negative error code
pub fn sys_open(path: *const u8, flags: i32, _mode: i32) -> isize {
    // 1. Validate path pointer
    if let Err(e) = validate_user_ptr(path, 1) {
        return e;
    }

    // 2. Copy path string from userspace (max 256 bytes)
    let mut path_buf = [0u8; 256];
    let mut path_len = 0;

    unsafe {
        for i in 0..256 {
            let ch = *path.add(i);
            if ch == 0 {
                break;
            }
            path_buf[i] = ch;
            path_len += 1;
        }
    }

    if path_len == 0 {
        return -EINVAL;
    }

    let path_str = match core::str::from_utf8(&path_buf[..path_len]) {
        Ok(s) => s,
        Err(_) => return -EINVAL,
    };

    log::debug!("sys_open: path='{}', flags=0x{:x}", path_str, flags);

    // 3. Call VFS to open file
    let file_info = match crate::vfs::vfs_open(path_str, flags) {
        Ok(info) => info,
        Err(e) => return e,
    };

    // 4. Create VFS file device if fsitem is mapped
    if let Some(fsitem_addr) = file_info.fsitem_addr {
        use crate::io::VfsFile;
        use crate::scheduler::shmem::ShmemId;
        use alloc::sync::Arc;

        let vfs_file = Arc::new(VfsFile::new(
            file_info.vfs_fd,
            ShmemId(file_info.shmem_id as usize),
            fsitem_addr,
        ));

        // 5. Insert into current process FD table
        match scheduler::with_current_process_mut(|process| {
            let kernel_fd = process.fd_table.alloc(vfs_file);
            kernel_fd
        }) {
            Some(kernel_fd) => {
                log::info!("sys_open: VFS FD {} mapped to kernel FD {}", file_info.vfs_fd, kernel_fd);
                return kernel_fd as isize;
            }
            None => {
                log::error!("sys_open: No current process");
                return -EBADF;
            }
        }
    }

    // Fallback: No fsitem mapping, return VFS FD directly (IPC-based reads)
    log::warn!("sys_open: No fsitem mapping, returning VFS FD directly (IPC reads)");
    file_info.vfs_fd as isize
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

    // 2. Get device from FD table (with interrupts disabled)
    // CRITICAL: We must NOT call device.read() inside with_current_process()
    // because device.read() may block (e.g., waiting for keyboard input),
    // and blocking requires interrupts to be enabled!
    let device = scheduler::with_current_process(|process| {
        match process.fd_table.get(fd) {
            Ok(dev) => Ok(dev.clone()),  // Clone the Arc<dyn Device>
            Err(e) => Err(errno_to_code(e))
        }
    });

    let device = match device {
        Some(Ok(dev)) => dev,
        Some(Err(e)) => return e,
        None => return -EBADF,
    };

    // 3. Call device.read() OUTSIDE with_current_process() so interrupts are enabled
    // This allows blocking operations to work correctly
    let buffer = unsafe { slice::from_raw_parts_mut(buf, count) };
    match device.read(buffer) {
        Ok(read) => read as isize,
        Err(e) => errno_to_code(e)
    }
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
    scheduler::exit_thread(status);
}

/// sys_yield - Yield CPU to scheduler
///
/// Arguments: ()
/// Returns: 0 on success
pub fn sys_yield() -> isize {
    scheduler::yield_now();
    0
}

/// sys_getpid - Get current process ID
///
/// Arguments: ()
/// Returns: process ID (always >= 0)
pub fn sys_getpid() -> isize {
    log::debug!("sys_getpid called");
    let result = scheduler::with_current_process(|process| {
        let pid = process.id.as_usize() as isize;
        log::debug!("sys_getpid: returning PID {}", pid);
        pid
    });

    let ret = result.unwrap_or(-EFAULT);
    log::debug!("sys_getpid: final return value {}", ret);
    ret
}

/// sys_getppid - Get parent process ID
///
/// Arguments: ()
/// Returns: parent process ID, or 0 if no parent
pub fn sys_getppid() -> isize {
    let result = scheduler::with_current_process(|process| {
        match process.parent() {
            Some(parent_id) => parent_id.as_usize() as isize,
            None => 0, // No parent (kernel process or orphaned)
        }
    });

    result.unwrap_or(-EFAULT)
}

/// sys_process_ready - Signal that critical process has completed initialization
///
/// Arguments: ()
/// Returns: 0 on success, negative error code on failure
///
/// This syscall is used by critical system services (VFS, memory server, etc.)
/// to signal that they have completed initialization and are ready to serve requests.
///
/// Once all critical processes have signaled ready, the scheduler transitions
/// from Boot mode to Normal mode, allowing user processes to run.
///
/// Only processes marked as Critical can call this successfully.
pub fn sys_process_ready() -> isize {
    log::debug!("sys_process_ready called");

    let pid = match scheduler::current_process_id() {
        Some(pid) => pid,
        None => {
            log::error!("sys_process_ready: no current process");
            return -ESRCH;
        }
    };

    match scheduler::signal_process_ready(pid) {
        Ok(()) => {
            log::info!("sys_process_ready: process {} signaled ready successfully", pid.0);
            0
        }
        Err(e) => {
            log::error!("sys_process_ready: failed for process {}: {}", pid.0, e);
            -EINVAL
        }
    }
}

/// sys_spawn - Spawn new process from ELF binary
///
/// Arguments: (path: *const u8, argv: *const *const u8)
/// Returns: child PID on success, or negative error code
///
/// This syscall loads an ELF binary from initrd, creates a new process
/// with fresh address space, and returns the child PID to the parent.
pub fn sys_spawn(path: *const u8, _argv: *const *const u8) -> isize {
    // TODO: Parse argv array when we implement proper argv passing

    // 1. Validate path pointer
    if let Err(e) = validate_user_ptr(path, 1) {
        return e;
    }

    // 2. Copy path from userspace to kernel buffer
    // Read until NULL terminator (max 256 chars)
    let mut path_buf = [0u8; 256];
    let mut path_len = 0;

    unsafe {
        for i in 0..path_buf.len() {
            let c = *path.add(i);
            if c == 0 {
                break;
            }
            path_buf[i] = c;
            path_len = i + 1;
        }
    }

    if path_len == 0 {
        return -EINVAL; // Empty path
    }

    let path_str = core::str::from_utf8(&path_buf[..path_len])
        .map_err(|_| -EINVAL)
        .unwrap();

    log::debug!("sys_spawn: path = '{}'", path_str);

    // 3. Get parent process ID for setting child's parent
    let parent_id = scheduler::with_current_process(|process| {
        process.id
    });

    let parent_id = match parent_id {
        Some(pid) => pid,
        None => return -EFAULT,
    };

    // 4. Switch to kernel page tables for the rest of sys_spawn
    // IMPORTANT: We need kernel page tables (with identity mapping) for:
    // - Reading from initrd (physical memory access)
    // - Spawning process (page table manipulation, memory allocation)
    // - Setting up child process (data structure access)
    //
    // We'll switch back to userspace page tables at the very end, before returning
    use x86_64::registers::control::Cr3;
    use crate::scheduler::process::ProcessId;

    // Save current (userspace) page table
    let (user_cr3, cr3_flags) = Cr3::read();

    // Get kernel process (PID 0) page table
    let kernel_pt = scheduler::with_process_mut(ProcessId::new(0), |kernel_proc| {
        use x86_64::structures::paging::PhysFrame;
        PhysFrame::containing_address(kernel_proc.address_space.page_table_root)
    });

    let kernel_pt = match kernel_pt {
        Some(pt) => pt,
        None => return -EFAULT,
    };

    // Switch to kernel page table for the rest of sys_spawn
    unsafe {
        Cr3::write(kernel_pt, cr3_flags);
    }

    // Read ELF binary from initrd
    let elf_data = crate::initrd::read_file(path_str);

    let elf_data = match elf_data {
        Ok(data) => data,
        Err(_) => return -super::numbers::ENOENT, // File not found (errno 2)
    };

    log::debug!("sys_spawn: loaded {} bytes from initrd", elf_data.len());

    // 5. Spawn the process using ELF loader (spawned via syscall = user process)
    let result = crate::loaders::elf::spawn_elf_process(
        elf_data,
        path_str,
        &[],
        scheduler::ProcessType::User
    );

    let (child_pid, _child_tid) = match result {
        Ok((pid, tid)) => (pid, tid),
        Err(e) => {
            log::error!("sys_spawn: failed to spawn process: {:?}", e);
            return -ENOMEM; // ELF loading failed
        }
    };

    // 6. Set parent-child relationship
    scheduler::with_process_mut(child_pid, |child_process| {
        child_process.set_parent(parent_id);
    });

    log::info!("sys_spawn: spawned process {} (parent: {})",
               child_pid.as_usize(), parent_id.as_usize());

    // 7. Switch back to userspace page tables before returning
    // This is safe because:
    // - Kernel code/data is mapped in userspace page tables (entries 1-511)
    // - We're done with operations that need identity mapping (entry 0)
    // - syscall_entry will use SYSRET to return to userspace
    unsafe {
        Cr3::write(user_cr3, cr3_flags);
    }

    // 8. Return child PID
    child_pid.as_usize() as isize
}

/// sys_waitpid - Wait for process to change state
///
/// Arguments: (pid: i32, status: *mut i32, options: i32)
/// Returns: PID of child that changed state, or negative error code
///
/// If child has exited, writes exit status to *status and reaps zombie.
/// If child still running, this simplified version returns -EINVAL
/// (blocking support to be added later).
pub fn sys_waitpid(pid: i32, status: *mut i32, _options: i32) -> isize {
    use crate::scheduler::process::ProcessId;

    // 1. Validate status pointer if not NULL
    if !status.is_null() {
        if let Err(e) = validate_user_ptr(status, 1) {
            return e;
        }
    }

    // 2. Get current process ID
    let parent_id = scheduler::with_current_process(|process| {
        process.id
    });

    let parent_id = match parent_id {
        Some(pid) => pid,
        None => return -EFAULT,
    };

    let child_pid = ProcessId::new(pid as usize);

    // 3. Poll until child becomes zombie (simple blocking implementation)
    // TODO: Replace with proper wait queues and scheduler blocking
    loop {
        let is_child_and_zombie = scheduler::with_process_mut(child_pid, |child_process| {
            // Verify parent-child relationship
            if child_process.parent() != Some(parent_id) {
                return Err(-ECHILD); // Not a child of current process
            }

            // Check if process is zombie
            if child_process.is_zombie() {
                let exit_code = child_process.exit_code.unwrap_or(0);

                // Write exit status to userspace if pointer provided
                if !status.is_null() {
                    unsafe {
                        *status = exit_code;
                    }
                }

                Ok(exit_code)
            } else {
                // Process still running - yield and try again
                Err(-1) // Sentinel value to indicate "retry"
            }
        });

        match is_child_and_zombie {
            Some(Ok(_exit_code)) => {
                // Child is zombie, break out of loop
                break;
            }
            Some(Err(e)) if e == -ECHILD => {
                // Not a child - return error immediately
                return -ECHILD;
            }
            Some(Err(_)) => {
                // Child still running - yield CPU and try again
                scheduler::yield_now();
                continue;
            }
            None => {
                return -ECHILD; // Process doesn't exist
            }
        }
    }

    let is_child_and_zombie = scheduler::with_process_mut(child_pid, |child_process| {
        if child_process.is_zombie() {
            let exit_code = child_process.exit_code.unwrap_or(0);
            Ok(exit_code)
        } else {
            Err(-EINVAL)
        }
    });

    match is_child_and_zombie {
        Some(Ok(_exit_code)) => {
            // Process was zombie, we read its exit code
            // Now reap it (remove from process table and free resources)
            if scheduler::reap_process(child_pid) {
                log::info!("sys_waitpid: reaped zombie process {}", child_pid.as_usize());
            }
            child_pid.as_usize() as isize
        }
        Some(Err(e)) => e, // Error (ECHILD or EINVAL)
        None => -ECHILD,   // Process doesn't exist
    }
}

// ============================================================================
// Group D: IPC (Inter-Process Communication) Syscalls
// ============================================================================

/// sys_port_create - Create a new IPC port
///
/// Arguments: ()
/// Returns: port ID on success, or negative error code
pub fn sys_port_create() -> isize {
    match scheduler::ipc::port_create() {
        Ok(port_id) => {
            log::debug!("sys_port_create: created port {}", port_id.0);
            port_id.0 as isize
        }
        Err(e) => {
            log::error!("sys_port_create: failed: {:?}", e);
            -ENOMEM // Port creation failed (likely out of memory)
        }
    }
}

/// sys_port_destroy - Destroy an IPC port
///
/// Arguments: (port_id: usize)
/// Returns: 0 on success, or negative error code
pub fn sys_port_destroy(port_id: usize) -> isize {
    let port_id = scheduler::ipc::PortId(port_id);

    match scheduler::ipc::port_destroy(port_id) {
        Ok(()) => {
            log::debug!("sys_port_destroy: destroyed port {}", port_id.0);
            0
        }
        Err(e) => {
            log::error!("sys_port_destroy: failed for port {}: {:?}", port_id.0, e);
            match e {
                scheduler::ipc::IpcError::PortNotFound => -EBADF,
                scheduler::ipc::IpcError::NotOwner => -EINVAL,
                _ => -EINVAL,
            }
        }
    }
}

/// sys_port_send - Send a message to an IPC port (non-blocking)
///
/// Arguments: (port_id: usize, message: *const u8, len: usize)
/// Returns: 0 on success, or negative error code
///
/// Message must be exactly 256 bytes.
pub fn sys_port_send(port_id: usize, message: *const u8, len: usize) -> isize {
    // 1. Validate message pointer and length
    if len != 256 {
        log::error!("sys_port_send: invalid message length {} (must be 256)", len);
        return -EINVAL;
    }

    if let Err(e) = validate_user_ptr(message, len) {
        return e;
    }

    // 2. Copy message from userspace
    let user_data = unsafe { slice::from_raw_parts(message, 256) };
    let mut ipc_msg = scheduler::ipc::Message::new();
    ipc_msg.as_bytes_mut().copy_from_slice(user_data);

    // 3. Send to port
    let port_id = scheduler::ipc::PortId(port_id);

    match scheduler::ipc::port_send(port_id, ipc_msg) {
        Ok(()) => {
            log::debug!("sys_port_send: sent message to port {}", port_id.0);
            0
        }
        Err(e) => {
            log::error!("sys_port_send: failed for port {}: {:?}", port_id.0, e);
            match e {
                scheduler::ipc::IpcError::PortNotFound => -EBADF,
                scheduler::ipc::IpcError::QueueFull => -ENOMEM,
                _ => -EINVAL,
            }
        }
    }
}

/// sys_port_recv - Receive a message from an IPC port (blocking)
///
/// Arguments: (port_id: usize, message: *mut u8, len: usize)
/// Returns: 0 on success, or negative error code
///
/// Message buffer must be at least 256 bytes.
pub fn sys_port_recv(port_id: usize, message: *mut u8, len: usize) -> isize {
    // 1. Validate message pointer and length
    if len < 256 {
        log::error!("sys_port_recv: buffer too small {} (must be >= 256)", len);
        return -EINVAL;
    }

    if let Err(e) = validate_user_ptr(message, 256) {
        return e;
    }

    // 2. Receive from port (blocks)
    let port_id = scheduler::ipc::PortId(port_id);

    let ipc_msg = match scheduler::ipc::port_recv(port_id) {
        Ok(msg) => msg,
        Err(e) => {
            log::error!("sys_port_recv: failed for port {}: {:?}", port_id.0, e);
            return match e {
                scheduler::ipc::IpcError::PortNotFound => -EBADF,
                scheduler::ipc::IpcError::NotOwner => -EINVAL,
                _ => -EINVAL,
            };
        }
    };

    // 3. Copy message to userspace
    let user_buffer = unsafe { slice::from_raw_parts_mut(message, 256) };
    user_buffer.copy_from_slice(ipc_msg.as_bytes());

    log::debug!("sys_port_recv: received message from port {}", port_id.0);
    0
}

/// sys_port_try_recv - Try to receive a message from an IPC port (non-blocking)
///
/// Arguments: (port_id: usize, message: *mut u8, len: usize)
/// Returns: 1 if message received, 0 if no message, or negative error code
pub fn sys_port_try_recv(port_id: usize, message: *mut u8, len: usize) -> isize {
    // 1. Validate message pointer and length
    if len < 256 {
        log::error!("sys_port_try_recv: buffer too small {} (must be >= 256)", len);
        return -EINVAL;
    }

    if let Err(e) = validate_user_ptr(message, 256) {
        return e;
    }

    // 2. Try to receive from port (non-blocking)
    let port_id = scheduler::ipc::PortId(port_id);

    match scheduler::ipc::port_try_recv(port_id) {
        Ok(Some(ipc_msg)) => {
            // 3. Copy message to userspace
            let user_buffer = unsafe { slice::from_raw_parts_mut(message, 256) };
            user_buffer.copy_from_slice(ipc_msg.as_bytes());

            log::debug!("sys_port_try_recv: received message from port {}", port_id.0);
            1 // Message received
        }
        Ok(None) => {
            log::debug!("sys_port_try_recv: no message available on port {}", port_id.0);
            0 // No message available
        }
        Err(e) => {
            log::error!("sys_port_try_recv: failed for port {}: {:?}", port_id.0, e);
            match e {
                scheduler::ipc::IpcError::PortNotFound => -EBADF,
                scheduler::ipc::IpcError::NotOwner => -EINVAL,
                _ => -EINVAL,
            }
        }
    }
}

/// sys_register_port_name - Register a well-known name for an IPC port
///
/// Arguments: (name: *const u8, port_id: usize)
/// Returns: 0 on success, or negative error code
pub fn sys_register_port_name(name: *const u8, port_id: usize) -> isize {
    // 1. Validate name pointer
    if let Err(e) = validate_user_ptr(name, 1) {
        return e;
    }

    // 2. Copy name string from userspace (max 64 bytes)
    let mut name_buf = [0u8; 64];
    let mut name_len = 0;

    unsafe {
        for i in 0..64 {
            let ch = *name.add(i);
            if ch == 0 {
                break;
            }
            name_buf[i] = ch;
            name_len += 1;
        }
    }

    if name_len == 0 {
        return -EINVAL;
    }

    let name_str = core::str::from_utf8(&name_buf[..name_len])
        .map_err(|_| -EINVAL)
        .unwrap();

    // 3. Convert to static str (this is a limitation - we can only register
    // compile-time string constants. For dynamic strings, we'd need a different approach)
    // For now, we only support "vfs" as a well-known name
    let static_name: &'static str = match name_str {
        "vfs" => "vfs",
        _ => {
            log::error!("sys_register_port_name: unsupported port name '{}'", name_str);
            return -EINVAL;
        }
    };

    // 4. Register port name
    let port_id = scheduler::ipc::PortId(port_id);

    // Special handling for "vfs" - also register with VFS module
    let result = if static_name == "vfs" {
        crate::vfs::register_vfs_server(port_id)
    } else {
        crate::vfs::register_port_name(static_name, port_id)
    };

    match result {
        Ok(()) => {
            log::info!("sys_register_port_name: registered '{}' -> port {}", static_name, port_id.0);
            0
        }
        Err(e) => {
            log::error!("sys_register_port_name: failed: {}", e);
            -EINVAL
        }
    }
}

/// sys_lookup_port_name - Look up an IPC port by well-known name
///
/// Arguments: (name: *const u8)
/// Returns: port ID on success, or negative error code
pub fn sys_lookup_port_name(name: *const u8) -> isize {
    // 1. Validate name pointer
    if let Err(e) = validate_user_ptr(name, 1) {
        return e;
    }

    // 2. Copy name string from userspace (max 64 bytes)
    let mut name_buf = [0u8; 64];
    let mut name_len = 0;

    unsafe {
        for i in 0..64 {
            let ch = *name.add(i);
            if ch == 0 {
                break;
            }
            name_buf[i] = ch;
            name_len += 1;
        }
    }

    if name_len == 0 {
        return -EINVAL;
    }

    let name_str = match core::str::from_utf8(&name_buf[..name_len]) {
        Ok(s) => s,
        Err(_) => return -EINVAL,
    };

    // 3. Look up port name
    match crate::vfs::lookup_port_name(name_str) {
        Some(port_id) => {
            log::debug!("sys_lookup_port_name: found '{}' -> port {}", name_str, port_id.0);
            port_id.0 as isize
        }
        None => {
            log::debug!("sys_lookup_port_name: port name '{}' not found", name_str);
            -ENOENT
        }
    }
}

// ============================================================================
// Group E: Shared Memory Syscalls
// ============================================================================

/// sys_shmem_create - Create a new shared memory region
///
/// Arguments: (size: usize, permissions: u32)
/// Returns: shared memory ID on success, or negative error code
pub fn sys_shmem_create(size: usize, permissions: u32) -> isize {
    // Get current process ID
    let process_id = match scheduler::current_process_id() {
        Some(pid) => pid,
        None => {
            log::error!("sys_shmem_create: no current process");
            return -EINVAL;
        }
    };

    // Convert permissions flags
    let perms = scheduler::shmem::ShmemPermissions::from_flags(permissions);

    // Create shared memory region
    match scheduler::shmem::shmem_create(size, process_id, perms) {
        Ok(shmem_id) => {
            log::debug!(
                "sys_shmem_create: created shmem {} ({} bytes) for process {}",
                shmem_id.0,
                size,
                process_id.0
            );
            shmem_id.0 as isize
        }
        Err(e) => {
            log::error!("sys_shmem_create: failed: {:?}", e);
            match e {
                scheduler::shmem::ShmemError::OutOfMemory => -ENOMEM,
                scheduler::shmem::ShmemError::InvalidSize => -EINVAL,
                _ => -EINVAL,
            }
        }
    }
}

/// sys_shmem_map - Map shared memory into current process
///
/// Arguments: (shmem_id: usize, hint_addr: usize, permissions: u32)
/// Returns: virtual address on success, or negative error code
pub fn sys_shmem_map(shmem_id: usize, hint_addr: usize, permissions: u32) -> isize {
    log::info!("sys_shmem_map: called with shmem_id={}, hint=0x{:x}", shmem_id, hint_addr);

    // Get current process ID
    let process_id = match scheduler::current_process_id() {
        Some(pid) => pid,
        None => {
            log::error!("sys_shmem_map: no current process");
            return -EINVAL;
        }
    };

    // Convert types
    let shmem_id = scheduler::shmem::ShmemId(shmem_id);
    let perms = scheduler::shmem::ShmemPermissions::from_flags(permissions);

    // Map shared memory
    match scheduler::shmem::shmem_map(shmem_id, process_id, hint_addr as u64, perms) {
        Ok(virt_addr) => {
            log::info!(
                "sys_shmem_map: SUCCESS mapped shmem {} to 0x{:x} for process {}",
                shmem_id.0,
                virt_addr,
                process_id.0
            );
            virt_addr as isize
        }
        Err(e) => {
            log::error!("sys_shmem_map: FAILED: {:?}", e);
            match e {
                scheduler::shmem::ShmemError::OutOfMemory => -ENOMEM,
                scheduler::shmem::ShmemError::InvalidId => -EBADF,
                scheduler::shmem::ShmemError::InvalidPermissions => -EINVAL,
                _ => -EINVAL,
            }
        }
    }
}

/// sys_shmem_unmap - Unmap shared memory from current process
///
/// Arguments: (addr: usize)
/// Returns: 0 on success, or negative error code
pub fn sys_shmem_unmap(addr: usize) -> isize {
    // Get current process ID
    let process_id = match scheduler::current_process_id() {
        Some(pid) => pid,
        None => {
            log::error!("sys_shmem_unmap: no current process");
            return -EINVAL;
        }
    };

    // Unmap shared memory
    match scheduler::shmem::shmem_unmap(addr as u64, process_id) {
        Ok(()) => {
            log::debug!(
                "sys_shmem_unmap: unmapped shmem at 0x{:x} for process {}",
                addr,
                process_id.0
            );
            0
        }
        Err(e) => {
            log::error!("sys_shmem_unmap: failed: {:?}", e);
            match e {
                scheduler::shmem::ShmemError::NotMapped => -EINVAL,
                scheduler::shmem::ShmemError::InvalidId => -EBADF,
                _ => -EINVAL,
            }
        }
    }
}

/// sys_shmem_destroy - Destroy shared memory region (mark for deletion)
///
/// Arguments: (shmem_id: usize)
/// Returns: 0 on success, or negative error code
pub fn sys_shmem_destroy(shmem_id: usize) -> isize {
    // Get current process ID
    let process_id = match scheduler::current_process_id() {
        Some(pid) => pid,
        None => {
            log::error!("sys_shmem_destroy: no current process");
            return -EINVAL;
        }
    };

    // Convert type
    let shmem_id = scheduler::shmem::ShmemId(shmem_id);

    // Destroy shared memory
    match scheduler::shmem::shmem_destroy(shmem_id, process_id) {
        Ok(()) => {
            log::debug!(
                "sys_shmem_destroy: marked shmem {} for deletion by process {}",
                shmem_id.0,
                process_id.0
            );
            0
        }
        Err(e) => {
            log::error!("sys_shmem_destroy: failed: {:?}", e);
            match e {
                scheduler::shmem::ShmemError::InvalidId => -EBADF,
                scheduler::shmem::ShmemError::NotOwner => -EINVAL,
                _ => -EINVAL,
            }
        }
    }
}
