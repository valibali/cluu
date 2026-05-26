//! Memory mapping helpers for child processes.
//!
//! These functions are intentionally small and focused so they can be reused
//! by the wiring layer without mixing service policy with mapping logic.

use libcluu::boot::{ProcessInfo, CONSOLE_FB_BASE, INITRD_USER_BASE, PROCESS_INFO_ADDR};
use libcluu::{space_map, space_map_range, Error, Result, MAP_DEVICE, PAGE_SIZE};

/// Map the unified ProcessInfo structure into a child's address space.
///
/// This provides tokens/params in a single page so services can discover
/// their initial handles without extra IPC.
pub fn map_process_info(
    space_token: usize,
    exit_token: usize,
    exit_cookie: usize,
    pid: usize,
    tokens: &[usize; 17],
    params: &[u64; 32],
) -> Result<()> {
    const READ_ONLY: usize = 0x01;
    let page_base = PROCESS_INFO_ADDR & !(PAGE_SIZE - 1);

    let info = ProcessInfo {
        exit_token,
        exit_cookie,
        pid,
        tokens: *tokens,
        params: *params,
    };

    let mut page = [0u8; PAGE_SIZE];
    let bytes = unsafe {
        core::slice::from_raw_parts(
            &info as *const ProcessInfo as *const u8,
            core::mem::size_of::<ProcessInfo>(),
        )
    };
    let offset = PROCESS_INFO_ADDR - page_base;
    let end = offset + bytes.len();
    if end > PAGE_SIZE {
        return Err(Error::InvalidArgument);
    }
    page[offset..end].copy_from_slice(bytes);

    space_map(
        space_token,
        page_base,
        page.as_ptr() as usize,
        READ_ONLY,
        PAGE_SIZE,
    )?;
    Ok(())
}

/// Map framebuffer memory into the console process.
///
/// The console needs direct framebuffer access for rendering, so we map
/// the physical framebuffer into its virtual space as device memory.
pub fn map_framebuffer(space_token: usize, fb_phys: u64, fb_size: u64) -> Result<()> {
    const READ_WRITE: usize = 0x03;
    if fb_phys == 0 || fb_size == 0 {
        return Ok(());
    }
    let num_pages = (fb_size as usize).div_ceil(PAGE_SIZE);
    space_map_range(
        space_token,
        CONSOLE_FB_BASE,
        fb_phys as usize,
        READ_WRITE | MAP_DEVICE,
        num_pages,
        0, // no data copy for device mapping
    )?;
    Ok(())
}

/// Map the initrd into procmgr for service spawning.
///
/// Procmgr spawns services from ELF images stored in initrd, so it must
/// be able to read the archive directly.
pub fn map_initrd(space_token: usize, initrd: &[u8], initrd_size: usize) -> Result<()> {
    const READ_ONLY: usize = 0x01;
    let num_pages = initrd_size.div_ceil(PAGE_SIZE);
    space_map_range(
        space_token,
        INITRD_USER_BASE,
        initrd.as_ptr() as usize,
        READ_ONLY,
        num_pages,
        initrd_size,
    )?;
    Ok(())
}
