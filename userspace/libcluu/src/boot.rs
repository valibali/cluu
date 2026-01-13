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
    pub initrd_phys: u64,
    pub initrd_size: u64,
    pub fb_phys: u64,
    pub fb_size: u64,
    pub fb_width: u32,
    pub fb_height: u32,
    pub fb_pitch: u32,
}

/// Virtual address where the initrd is mapped in userspace.
pub const INITRD_USER_BASE: usize = 0x8000_0000;

/// Return a reference to the boot info structure.
pub fn boot_info() -> &'static BootInfo {
    unsafe { &*(BOOT_INFO_ADDR as *const BootInfo) }
}

/// Root token handle supplied by the kernel.
pub fn root_token_handle() -> usize {
    boot_info().root_token
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

    /// Generic token slots (indexed by convention)
    pub tokens: [usize; 16],

    /// Generic parameters (service-specific data)
    pub params: [u64; 8],
}

// Well-known token indices (convention, not enforced)
pub const TOKEN_STDIN: usize = 0;
pub const TOKEN_STDOUT: usize = 1;
pub const TOKEN_STDERR: usize = 2;
pub const TOKEN_STDLOG: usize = 3;
// Services use indices 4+ for their specific needs

// Well-known param indices for console service
pub const PARAM_FB_BASE: usize = 0;
pub const PARAM_FB_SIZE: usize = 1;
pub const PARAM_FB_WIDTH: usize = 2;
pub const PARAM_FB_HEIGHT: usize = 3;
pub const PARAM_FB_PITCH: usize = 4;

// Well-known param indices for procmgr
pub const PARAM_INITRD_SIZE: usize = 0;

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
