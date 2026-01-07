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

/// Additional location where init writes the derived procmgr info.
pub const PROCMGR_INFO_ADDR: usize = BOOT_INFO_ADDR + 0x100;
/// Location where the parent process manager writes per-process exit info.
pub const PARENT_INFO_ADDR: usize = BOOT_INFO_ADDR + 0x200;

/// Procmgr bootstrap payload written by init.
#[repr(C)]
pub struct ProcmgrInfo {
    pub token: usize,
    pub exit_endpoint: usize,
    pub initrd_size: u64,
}

/// Parent-managed exit info payload written by the spawner.
#[repr(C)]
pub struct ParentInfo {
    pub exit_endpoint: usize,
    pub exit_cookie: usize,
}

/// Write the derived procmgr bootstrap info for the child to pick up.
pub fn set_procmgr_info(token: usize, exit_endpoint: usize, initrd_size: u64) {
    unsafe {
        let ptr = PROCMGR_INFO_ADDR as *mut ProcmgrInfo;
        ptr.write(ProcmgrInfo {
            token,
            exit_endpoint,
            initrd_size,
        });
    }
}

/// Read the procmgr bootstrap info stored by init.
pub fn procmgr_info() -> &'static ProcmgrInfo {
    unsafe { &*(PROCMGR_INFO_ADDR as *const ProcmgrInfo) }
}

/// Read the procmgr token handle stored by init.
pub fn procmgr_token_handle() -> usize {
    procmgr_info().token
}

/// Read the exit endpoint token for the process manager.
pub fn procmgr_exit_endpoint() -> usize {
    procmgr_info().exit_endpoint
}

/// Read the parent info stored by the spawner.
pub fn parent_info() -> &'static ParentInfo {
    unsafe { &*(PARENT_INFO_ADDR as *const ParentInfo) }
}

/// Read the exit endpoint token for the current process.
pub fn parent_exit_endpoint() -> usize {
    parent_info().exit_endpoint
}

/// Read the parent-provided exit cookie for the current process.
pub fn parent_exit_cookie() -> usize {
    parent_info().exit_cookie
}
