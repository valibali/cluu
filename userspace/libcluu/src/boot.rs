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

/// Additional location where init writes the derived procmgr token.
pub const PROCMGR_TOKEN_OFFSET: usize = 0x100;
pub const PROCMGR_TOKEN_ADDR: usize = BOOT_INFO_ADDR + PROCMGR_TOKEN_OFFSET;

/// Write the derived procmgr token handle for the child to pick up.
pub fn set_procmgr_token(token: usize) {
    unsafe {
        let ptr = PROCMGR_TOKEN_ADDR as *mut usize;
        ptr.write(token);
    }
}

/// Read the procmgr token handle stored by init.
pub fn procmgr_token_handle() -> usize {
    unsafe { (PROCMGR_TOKEN_ADDR as *const usize).read() }
}
