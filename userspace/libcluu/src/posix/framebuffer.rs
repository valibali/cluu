//! Framebuffer access API for userspace applications.
//!
//! Queries the console service for framebuffer parameters, then maps the
//! physical framebuffer into the caller's address space at APP_FB_BASE.

use super::c_int;
use crate::boot::{space_token, APP_FB_BASE};
use crate::ipc::CONSOLE_FB_INFO_LABEL;
use crate::syscall::{self, MAP_DEVICE};
use crate::types::Message;

/// Framebuffer info returned to C callers.
#[repr(C)]
pub struct FramebufferInfo {
    pub base: *mut u8,
    pub phys: u64,
    pub size: u32,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub bpp: u32,
}

/// Acquire a direct-mapped framebuffer.
///
/// 1. Subscribes to the console service via registry.
/// 2. Queries the console for framebuffer physical address and dimensions.
/// 3. Maps the physical framebuffer at APP_FB_BASE (0xA000_0000).
///
/// Returns 0 on success, -1 on failure.
#[no_mangle]
pub extern "C" fn framebuffer_acquire(info: *mut FramebufferInfo) -> c_int {
    if info.is_null() {
        return -1;
    }

    // Step 1: Get console endpoint via registry subscription.
    // Console registers as "console:<instance_id>", default instance is 0.
    let _ = crate::syscall::debug_print("fb: subscribing to console");
    let console_ep = match crate::registry::subscribe_output("console:0", "write") {
        Ok(ep) => ep,
        Err(_) => {
            let _ = crate::syscall::debug_print("fb: subscribe failed");
            return -1;
        }
    };
    let _ = crate::syscall::debug_print("fb: subscribe ok, calling console");

    // Step 2: Query framebuffer info via IPC call.
    let msg = Message::new(CONSOLE_FB_INFO_LABEL, [0; 6], 0);
    let mut reply_buf = [0u8; 512];
    let bytes = match syscall::ipc_call(console_ep, msg.as_bytes(), &mut reply_buf) {
        Ok(b) => b,
        Err(_) => {
            let _ = crate::syscall::debug_print("fb: ipc_call failed");
            return -1;
        }
    };
    let _ = crate::syscall::debug_print("fb: got reply from console");

    if bytes < core::mem::size_of::<Message>() {
        return -1;
    }

    let reply = unsafe { (reply_buf.as_ptr() as *const Message).read_unaligned() };
    let fb_phys = reply.words[0];
    let fb_size = reply.words[1];
    let fb_width = reply.words[2];
    let fb_height = reply.words[3];
    let fb_pitch = reply.words[4];
    let fb_bpp = reply.words[5];

    if fb_phys == 0 || fb_size == 0 {
        return -1;
    }

    // Step 3: Map the physical framebuffer into our address space.
    let num_pages = fb_size.div_ceil(0x1000);
    let flags = 0x03 | MAP_DEVICE; // read + write + device (uncacheable)
    match syscall::space_map_range(space_token(), APP_FB_BASE, fb_phys, flags, num_pages, 0) {
        Ok(_) => {}
        Err(_) => return -1,
    }

    // Step 4: Fill in the info struct.
    unsafe {
        (*info).base = APP_FB_BASE as *mut u8;
        (*info).phys = fb_phys as u64;
        (*info).size = fb_size as u32;
        (*info).width = fb_width as u32;
        (*info).height = fb_height as u32;
        (*info).pitch = fb_pitch as u32;
        (*info).bpp = fb_bpp as u32;
    }

    0
}

/// Release a previously acquired framebuffer mapping.
///
/// Unmaps the framebuffer pages from the caller's address space.
/// Must be called before process exit to avoid kernel teardown issues
/// with device-mapped memory.
///
/// Returns 0 on success, -1 on failure.
#[no_mangle]
pub extern "C" fn framebuffer_release(info: *const FramebufferInfo) -> c_int {
    if info.is_null() {
        return -1;
    }

    let size = unsafe { (*info).size } as usize;
    if size == 0 {
        return 0;
    }

    let num_pages = size.div_ceil(0x1000);
    let _ = syscall::space_unmap(space_token(), APP_FB_BASE, num_pages);

    0
}
