/*
 * System Call Numbers
 *
 * This module defines the syscall numbers following the System V AMD64 ABI
 * convention used by Linux and most Unix-like systems.
 *
 * Syscall Mechanism:
 * - RAX register contains syscall number
 * - Arguments in RDI, RSI, RDX, R10, R8, R9 (6 args max)
 * - Return value in RAX (signed: negative = error code)
 *
 * Why these numbers:
 * - Compatibility with newlib C library expectations
 * - Standard Linux syscall numbers where applicable
 * - Custom numbers (>= 1000) for CLUU-specific syscalls
 */

/// Syscall error codes (negative values returned in RAX)
pub const ENOSYS: isize = 38;   // Function not implemented
pub const EBADF: isize = 9;     // Bad file descriptor
pub const EFAULT: isize = 14;   // Bad address (invalid pointer from userspace)
pub const EINVAL: isize = 22;   // Invalid argument
pub const ENOMEM: isize = 12;   // Out of memory
pub const ESPIPE: isize = 29;   // Illegal seek (e.g., seek on TTY)
pub const ENOENT: isize = 2;    // No such file or directory
pub const ECHILD: isize = 10;   // No child processes

/// Group A: Console I/O syscalls (required for basic userspace)
///
/// These syscalls provide the minimum I/O functionality needed for
/// newlib's stdio to work (printf, scanf, etc.)

/// Read from file descriptor
///
/// Arguments: (fd: i32, buf: *mut u8, count: usize) -> isize
/// Returns: number of bytes read, or negative error code
pub const SYS_READ: usize = 0;

/// Write to file descriptor
///
/// Arguments: (fd: i32, buf: *const u8, count: usize) -> isize
/// Returns: number of bytes written, or negative error code
pub const SYS_WRITE: usize = 1;

/// Close file descriptor
///
/// Arguments: (fd: i32) -> isize
/// Returns: 0 on success, or negative error code
pub const SYS_CLOSE: usize = 3;

/// Get file status
///
/// Arguments: (fd: i32, statbuf: *mut Stat) -> isize
/// Returns: 0 on success, or negative error code
pub const SYS_FSTAT: usize = 5;

/// Seek to position in file
///
/// Arguments: (fd: i32, offset: i64, whence: i32) -> isize
/// Returns: new file position, or negative error code
pub const SYS_LSEEK: usize = 8;

/// Check if file descriptor is a TTY
///
/// Arguments: (fd: i32) -> isize
/// Returns: 1 if TTY, 0 if not, or negative error code
pub const SYS_ISATTY: usize = 16;

/// Group B: Heap management syscall (required for malloc/new)

/// Set program break (heap boundary)
///
/// Arguments: (addr: *mut u8) -> isize
/// Returns: new break on success, or negative error code
///
/// Note: Physical pages are allocated lazily on first access (page fault)
pub const SYS_BRK: usize = 12;

/// Process control syscalls

/// Exit current process
///
/// Arguments: (status: i32) -> !
/// Does not return
pub const SYS_EXIT: usize = 60;

/// Yield CPU to scheduler
///
/// Arguments: () -> isize
/// Returns: 0 on success
pub const SYS_YIELD: usize = 158;  // sched_yield in Linux

/// Group C: Process management syscalls

/// Get current process ID
///
/// Arguments: () -> isize
/// Returns: process ID (always >= 0)
pub const SYS_GETPID: usize = 39;

/// Spawn new process from ELF binary
///
/// Arguments: (path: *const u8, argv: *const *const u8) -> isize
/// Returns: child PID on success, or negative error code
///
/// This syscall loads an ELF binary from initrd, creates a new process
/// with fresh address space, and returns the child PID to the parent.
/// Unlike fork/exec, this is a single-step process creation.
pub const SYS_SPAWN: usize = 57;

/// Wait for process to change state
///
/// Arguments: (pid: i32, status: *mut i32, options: i32) -> isize
/// Returns: PID of child that changed state, or negative error code
///
/// If child has exited, writes exit status to *status and reaps zombie.
/// If child still running, blocks until child exits (if options=0).
pub const SYS_WAITPID: usize = 61;

/// Get parent process ID
///
/// Arguments: () -> isize
/// Returns: parent process ID, or 0 if orphaned
pub const SYS_GETPPID: usize = 110;

/// Group D: IPC (Inter-Process Communication) syscalls
///
/// These syscalls provide port-based message passing for microkernel IPC.
/// Used by userspace servers (VFS, device drivers, etc.) to communicate
/// with kernel and other processes.

/// Create a new IPC port
///
/// Arguments: () -> isize
/// Returns: port ID on success, or negative error code
///
/// The calling thread becomes the owner of the port and is the only
/// thread that can receive messages from it. Any thread can send to the port.
pub const SYS_PORT_CREATE: usize = 1000;

/// Destroy an IPC port
///
/// Arguments: (port_id: usize) -> isize
/// Returns: 0 on success, or negative error code
///
/// Only the port owner can destroy a port. All waiting threads are woken.
pub const SYS_PORT_DESTROY: usize = 1001;

/// Send a message to an IPC port (non-blocking)
///
/// Arguments: (port_id: usize, message: *const u8, len: usize) -> isize
/// Returns: 0 on success, or negative error code
///
/// The message is copied into the port's queue. If queue is full, returns error.
/// Message must be exactly 256 bytes.
pub const SYS_PORT_SEND: usize = 1002;

/// Receive a message from an IPC port (blocking)
///
/// Arguments: (port_id: usize, message: *mut u8, len: usize) -> isize
/// Returns: 0 on success, or negative error code
///
/// Only the port owner can receive. Blocks if no messages available.
/// Message buffer must be at least 256 bytes.
pub const SYS_PORT_RECV: usize = 1003;

/// Try to receive a message from an IPC port (non-blocking)
///
/// Arguments: (port_id: usize, message: *mut u8, len: usize) -> isize
/// Returns: 1 if message received, 0 if no message, or negative error code
///
/// Only the port owner can receive. Returns immediately if no messages available.
pub const SYS_PORT_TRY_RECV: usize = 1004;

/// Register a well-known name for an IPC port
///
/// Arguments: (name: *const u8, port_id: usize) -> isize
/// Returns: 0 on success, or negative error code
///
/// Allows services to register their ports with well-known names (e.g., "vfs")
/// so clients can find them by name instead of needing to know the port ID.
pub const SYS_REGISTER_PORT_NAME: usize = 1005;

/// Look up an IPC port by well-known name
///
/// Arguments: (name: *const u8) -> isize
/// Returns: port ID on success, or negative error code
///
/// Looks up a port that was registered with SYS_REGISTER_PORT_NAME.
pub const SYS_LOOKUP_PORT_NAME: usize = 1006;
