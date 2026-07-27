//! Boot information passed from the kernel to the first userspace process
//!
//! Contains the root token handle as well as initrd metadata that init
//! uses to locate system binaries.

/// Physical address where the kernel stores boot information.
pub const BOOT_INFO_ADDR: usize = 0x7fe0_0000;

/// Boot information structure written by the kernel.
#[repr(C)]
pub struct BootInfo {
    pub root_token: usize,
    pub clock_token: usize,
    pub view_mgr_token: usize,
    pub block_region_token: usize,
    pub device_region_token: usize,
    pub irq_root_token: usize,
    pub initrd_phys: u64,
    pub initrd_size: u64,
    pub fb_phys: u64,
    pub fb_size: u64,
    pub fb_width: u32,
    pub fb_height: u32,
    pub fb_pitch: u32,
    pub acpi_ptr: u64,
}

/// Virtual address where the initrd is mapped in userspace.
pub const INITRD_USER_BASE: usize = 0x7000_0000;

/// Return a reference to the boot info structure.
pub fn boot_info() -> &'static BootInfo {
    unsafe { &*(BOOT_INFO_ADDR as *const BootInfo) }
}

/// Root token handle supplied by the kernel.
pub fn root_token_handle() -> usize {
    boot_info().root_token
}

/// Clock token handle supplied by the kernel.
pub fn clock_token_handle() -> usize {
    boot_info().clock_token
}

/// IRQ root token handle supplied by the kernel.
pub fn irq_root_token_handle() -> usize {
    boot_info().irq_root_token
}

/// Universal process info address - same for all processes.
pub const PROCESS_INFO_ADDR: usize = BOOT_INFO_ADDR + 0x100;

/// Virtual address where the console maps the framebuffer.
pub const CONSOLE_FB_BASE: usize = 0x9000_0000;

/// Universal process info structure.
///
/// All processes receive this structure at PROCESS_INFO_ADDR.
/// The spawner fills in the appropriate fields.
#[repr(C)]
pub struct ProcessInfo {
    /// Token to send exit notification to parent (0 if root process)
    pub exit_token: usize,
    /// Cookie to identify this process to parent
    pub exit_cookie: usize,
    /// Process ID (set by procmgr at spawn time)
    pub pid: usize,

    /// Generic token slots (indexed by convention)
    pub tokens: [usize; 17],

    /// Generic parameters (service-specific data).
    /// Slots 0-9: existing (see PARAM_* constants below).
    /// Slots 10-11: cwd offset / length (Shell-A).
    /// Slots 12-13: redir offset / length (file redirection).
    /// Slots 14-15: VFS fd trailer offset / length (FdInherit VFS-backed fd handoff).
    /// Slot  16:    compositor session-mode discriminator (PARAM_SESSION_MODE).
    /// Slots 17-31: reserved for future use.
    pub params: [u64; 32],
}

// ═══════════════════════════════════════════════════════════════════════════
// Token Slot Layout
// ═══════════════════════════════════════════════════════════════════════════
//
// ProcessInfo.tokens[16] layout:
//
// ┌─────────────────────────────────────────────────────────────────────────┐
// │  UNIVERSAL (0-8) - Same meaning for every process                       │
// ├─────────────────────────────────────────────────────────────────────────┤
// │  0-3:  Standard I/O endpoints                                           │
// │  4-7:  Core capabilities (rights vary per process)                      │
// │  8:    System services                                                  │
// ├─────────────────────────────────────────────────────────────────────────┤
// │  CONTEXTUAL (9-15) - Parent-defined, varies by process type             │
// │  9-15: Device caps (IRQs, PCI, DMA), or empty for regular programs      │
// └─────────────────────────────────────────────────────────────────────────┘
//
// Token types:
//   • Endpoints: IPC destinations (stdin, stdout, registry, etc.)
//   • Capabilities: Authority for kernel operations (self, space, ipc, clock)
//
// ═══════════════════════════════════════════════════════════════════════════

// ─── Standard I/O Endpoints (0-3) ───────────────────────────────────────────
/// Input stream endpoint (slot 0)
pub const TOKEN_STDIN: usize = 0;
/// Output stream endpoint (slot 1)
pub const TOKEN_STDOUT: usize = 1;
/// Error stream endpoint (slot 2)
pub const TOKEN_STDERR: usize = 2;
/// Logging stream endpoint (slot 3)
pub const TOKEN_STDLOG: usize = 3;

// ─── Core Capabilities (4-7) ────────────────────────────────────────────────
/// Thread control capability (slot 4)
/// Operations: create threads, set priority, suspend/resume
/// Rights vary: normal processes get basic, procmgr gets elevated
pub const TOKEN_SELF: usize = 4;

/// Address space capability (slot 5)
/// Operations: map, unmap, grant memory regions
/// Rights vary: normal processes get SPACE_MAP|SPACE_GRANT, vfs/procmgr get more
pub const TOKEN_SPACE: usize = 5;

/// IPC capability (slot 6)
/// Operations: create endpoints, send/receive messages
/// All processes get: CREATE, IPC_SEND, IPC_RECV, IPC_CALL, GRANT
pub const TOKEN_IPC: usize = 6;

/// Clock/time capability (slot 7)
/// Operations: read TSC, query time
/// All processes get: READ
pub const TOKEN_CLOCK: usize = 7;

// ─── System Services (8) ────────────────────────────────────────────────────
/// Registry endpoint for service discovery (slot 8)
pub const TOKEN_REGISTRY: usize = 8;

// ─── Contextual / Parent-Defined (9-15) ─────────────────────────────────────
// These slots have meaning defined by the parent process.
// For drivers: device capabilities (IRQ, PCI, DMA)
// For regular programs: typically empty (0)

/// Extra capability slot 0 (slot 9) - e.g., IRQ token for drivers
pub const TOKEN_EXTRA_0: usize = 9;
/// Extra capability slot 1 (slot 10) - e.g., PCI/device capability
pub const TOKEN_EXTRA_1: usize = 10;
/// Extra capability slot 2 (slot 11) - e.g., DMA capability
pub const TOKEN_EXTRA_2: usize = 11;
/// Extra capability slot 4 (slot 13)
pub const TOKEN_EXTRA_4: usize = 13;
/// Extra capability slot 5 (slot 14)
pub const TOKEN_EXTRA_5: usize = 14;
/// Extra capability slot 6 (slot 15)
pub const TOKEN_EXTRA_6: usize = 15;

/// VFS view-manager capability (slot 16)
pub const TOKEN_VFS_VIEW_MGR: usize = 16;

/// Thread-typed capability for the process's main thread. Only populated
/// for procmgr — the thread_token is produced by `thread_create()` *after*
/// the tokens array is built, so init does an explicit post-create patch
/// (via THREAD_CREATE_START_SUSPENDED + ProcessInfo re-map + thread_resume)
/// for procmgr alone. Zero in all other processes.
pub const TOKEN_SELF_THREAD: usize = 12;

// ─── Slot Ranges ────────────────────────────────────────────────────────────
/// First standard I/O slot
pub const TOKEN_STDIO_START: usize = 0;
/// Last standard I/O slot
pub const TOKEN_STDIO_END: usize = 3;
/// First core capability slot
pub const TOKEN_CAPS_START: usize = 4;
/// Last core capability slot
pub const TOKEN_CAPS_END: usize = 7;
/// First contextual/extra slot
pub const TOKEN_EXTRA_START: usize = 9;
/// Last contextual/extra slot
pub const TOKEN_EXTRA_END: usize = 15;

// Well-known param indices for fd pipe mask (regular processes)
// Shares slot 0 with PARAM_FB_BASE (console-only, which never calls init_stdio pipe path)
pub const PARAM_FD_PIPE_MASK: usize = 0;

// Well-known param indices for console service
pub const PARAM_FB_BASE: usize = 0;
pub const PARAM_FB_SIZE: usize = 1;
pub const PARAM_FB_WIDTH: usize = 2;
pub const PARAM_FB_HEIGHT: usize = 3;
pub const PARAM_FB_PITCH: usize = 4;
pub const PARAM_CONSOLE_INSTANCE: usize = 5;
pub const PARAM_FB_PHYS: usize = 6;
/// 1 if this console instance starts as the active (visible) VT, 0 otherwise.
pub const PARAM_CONSOLE_ACTIVE: usize = 7;

/// Fixed virtual address where apps map the framebuffer for direct access.
pub const APP_FB_BASE: usize = 0xA000_0000;

// Well-known param indices for procmgr
pub const PARAM_INITRD_SIZE: usize = 0;

// Well-known param indices for tty
pub const PARAM_TTY_INSTANCE: usize = 0;

// Well-known param indices for VFS (FB info passed from init via ProcessInfo)
pub const PARAM_VFS_FB_PHYS: usize = 1;
pub const PARAM_VFS_FB_SIZE: usize = 2;
pub const PARAM_VFS_FB_WIDTH: usize = 3;
pub const PARAM_VFS_FB_HEIGHT: usize = 4;
pub const PARAM_VFS_FB_PITCH: usize = 6; // skip 5 = PARAM_CAP_PROFILE

// Well-known param indices for argv (shared by procmgr + crt0)
pub const PARAM_ARGC: usize = 6;
pub const PARAM_ARGV_OFFSET: usize = 7;

// Well-known param indices for environment (shared by procmgr + libcluu env init)
pub const PARAM_ENVC: usize = 8;
pub const PARAM_ENV_OFFSET: usize = 9;

// Well-known param index for capability profile (shared by procmgr + all processes)
// Overlaps with PARAM_CONSOLE_INSTANCE for console service — console's profile is
// implied (SERVICE) and it uses this slot for its VT instance index instead.
pub const PARAM_CAP_PROFILE: usize = 5;

/// Current working directory inherited across posix_spawn (Shell-A).
/// Byte offset within the 4 KB ProcessInfo page where the cwd bytes live.
pub const PARAM_CWD_OFFSET: usize = 10;
/// Length of the cwd bytes. 0 means "no inherited cwd; use /".
pub const PARAM_CWD_LEN: usize = 11;
/// Maximum cwd byte length carried across spawn.
pub const CWD_MAX: usize = 1024;

/// Redirection trailer offset within the ProcessInfo page.
pub const PARAM_REDIR_OFFSET: usize = 12;
/// Redirection trailer length (excludes the magic+len_marker; just the entries).
pub const PARAM_REDIR_LEN: usize = 13;

/// VFS fd trailer offset within the ProcessInfo page.
///
/// Points to a 64-byte block (4 × 16 bytes) that carries per-fd VFS metadata
/// for fds 0..3.  Each entry is `(u64 vfs_client_id, u64 vfs_remote_fd)`.
/// `vfs_client_id == 0` means the fd is NOT VFS-backed (legacy tty/pipe path).
///
/// Written by procmgr's FdInherit handler when the parent used
/// `posix_spawn_file_actions_adddup2` on a VFS-backed fd (e.g. `/dev/pts/*`).
/// Read by `libcluu::fd_table::init_stdio` to build `FdEntry::file(...)`.
pub const PARAM_FD_VFS_OFFSET: usize = 14;
/// VFS fd trailer length.  64 when present (4 fds × 16 bytes), 0 when absent.
pub const PARAM_FD_VFS_LEN: usize = 15;

/// Compositor session-mode discriminator.
///   0 = system (autostarted at boot, hosts login modal only)
///   1 = user   (spawned at SESSION_LOGIN under user envelope, full desktop)
/// Default 0 when absent (procmgr leaves the slot zero-initialised).
pub const PARAM_SESSION_MODE: usize = 16;

/// Endpoint to which a spawned process should send a "ready" notification
/// once it has finished startup (registered its public endpoints, etc.).
/// 0 = no notification expected. Used by procmgr at SESSION_LOGIN kind=1
/// to gate cluuterm-spawn on user-compositor readiness.
pub const PARAM_NOTIFY_READY_EP: usize = 17;
pub const PARAM_SESSION_VFS_EP: usize = 18;

/// Read the process info structure.
pub fn process_info() -> &'static ProcessInfo {
    unsafe { &*(PROCESS_INFO_ADDR as *const ProcessInfo) }
}

/// Convenience: get exit token
pub fn exit_token() -> usize {
    process_info().exit_token
}

/// Convenience: get exit cookie
pub fn exit_cookie() -> usize {
    process_info().exit_cookie
}

/// Convenience: get a token by index
pub fn token(index: usize) -> usize {
    process_info().tokens[index]
}

/// Convenience: get a param by index
pub fn param(index: usize) -> u64 {
    process_info().params[index]
}

/// Convenience: get stdin token
pub fn stdin() -> usize {
    token(TOKEN_STDIN)
}

/// Convenience: get stdout token
pub fn stdout() -> usize {
    token(TOKEN_STDOUT)
}

/// Convenience: get stderr token
pub fn stderr() -> usize {
    token(TOKEN_STDERR)
}

/// Convenience: get self/thread control token.
pub fn token_self() -> usize {
    token(TOKEN_SELF)
}

/// Convenience: get process space token (SPACE_MAP right).
pub fn space_token() -> usize {
    token(TOKEN_SPACE)
}

/// Convenience: get IPC capability token.
pub fn token_ipc() -> usize {
    token(TOKEN_IPC)
}

/// Convenience: get clock token.
pub fn token_clock() -> usize {
    token(TOKEN_CLOCK)
}

/// Convenience: get registry endpoint.
pub fn token_registry() -> usize {
    token(TOKEN_REGISTRY)
}

/// Convenience: get extra token by index (0-6 → slots 9-15).
pub fn token_extra(index: usize) -> usize {
    assert!(index <= 6, "token_extra index must be 0-6");
    token(TOKEN_EXTRA_START + index)
}

/// Convenience: get process ID.
pub fn pid() -> usize {
    process_info().pid
}

const _: () = {
    let size = core::mem::size_of::<ProcessInfo>();
    // 3 * usize + 16 * usize + 32 * u64 on x86_64 = 24 + 128 + 256 = 408 bytes.
    // Page is 4096; plenty of room for argv/env/cwd/redir/fd-vfs payloads after the header.
    assert!(size <= 512, "ProcessInfo grew unexpectedly large");
};
