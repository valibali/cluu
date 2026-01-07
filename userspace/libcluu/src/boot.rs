//! Boot information passed from the kernel to the first userspace process
//!
//! Contains the root token handle as well as initrd metadata that init
//! uses to locate system binaries.

/// Physical address where the kernel stores boot information.
pub const BOOT_INFO_ADDR: usize = 0x7fe0_0000;
/// Virtual address where per-thread self info is mapped.
pub const THREAD_SELF_ADDR: usize = 0x7fe0_1000;

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
pub const PROCMGR_TOKEN_ADDR: usize = BOOT_INFO_ADDR + 0x100;

/// Procmgr bootstrap payload written by init.
#[repr(C)]
pub struct ProcmgrInfo {
    pub token: usize,
    pub initrd_size: u64,
}

/// Per-thread self info payload written by the spawner.
#[repr(C)]
pub struct ThreadSelfInfo {
    pub thread_token: usize,
}

/// Write the derived procmgr bootstrap info for the child to pick up.
pub fn set_procmgr_info(token: usize, initrd_size: u64) {
    unsafe {
        let ptr = PROCMGR_TOKEN_ADDR as *mut ProcmgrInfo;
        ptr.write(ProcmgrInfo { token, initrd_size });
    }
}

/// Read the procmgr bootstrap info stored by init.
pub fn procmgr_info() -> &'static ProcmgrInfo {
    unsafe { &*(PROCMGR_TOKEN_ADDR as *const ProcmgrInfo) }
}

/// Read the procmgr token handle stored by init.
pub fn procmgr_token_handle() -> usize {
    procmgr_info().token
}

/// Read the self info stored by the spawner.
pub fn thread_self_info() -> &'static ThreadSelfInfo {
    unsafe { &*(THREAD_SELF_ADDR as *const ThreadSelfInfo) }
}

/// Read the thread token handle for the current thread.
pub fn thread_self_token_handle() -> usize {
    thread_self_info().thread_token
}
