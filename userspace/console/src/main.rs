#![no_std]
#![no_main]

extern crate alloc;

/// Userspace console service.
///
/// The console owns a text grid and renders it to the framebuffer. It exposes
/// per-VT write endpoints (`vt:0`, `vt:1`, ...) and a `control` endpoint via
/// the registry. The receiving endpoint index identifies the VT — no
/// sender-reported VT index is needed, eliminating confused-deputy attacks.
///
/// # Multi-VT Support
///
/// Each console instance maintains its own cell grid and backbuffer.  Only the
/// active console (the one whose PARAM_CONSOLE_ACTIVE == 1 at boot, or the one
/// that last received CONSOLE_ACTIVATE_LABEL from kbd) flushes to the physical
/// framebuffer.  Inactive consoles keep rendering into their backbuffer so VT
/// switches are instant (just a full repaint from the cell grid).
///
/// # Double Buffering
///
/// The console attempts to use a double-buffered backend to reduce tearing.
/// If the heap is too small for the backbuffer (~3MB), it falls back to
/// direct framebuffer writes.
mod backend;
mod context;
mod protocol;
mod renderer;
#[cfg(target_arch = "x86_64")]
mod simd;

use crate::backend::{ConsoleBackend, DoubleBufferBackend, FramebufferBackend};
use crate::context::{ConsoleContext, VT_COUNT};
use crate::protocol::parse_message;
use crate::renderer::Console;
use libcluu::boot::{process_info, PARAM_CONSOLE_ACTIVE};
use libcluu::posix::{
    _close, _open, _read, mmap, c_void, O_RDWR, MAP_SHARED, PROT_READ, PROT_WRITE,
};
use libcluu::ipc::{
    extract_reply_id, reply, CONSOLE_ACTIVATE_LABEL, CONSOLE_CREATE_VT_LABEL,
    CONSOLE_DEACTIVATE_LABEL, CONSOLE_FB_INFO_LABEL, CONSOLE_SCROLL_VT_LABEL,
    CONSOLE_SWITCH_VT_LABEL, CONSOLE_WRITE_LABEL, CONSOLE_WRITE_SYNC_LABEL,
    CONSOLE_WRITE_VT_LABEL, CONSOLE_WRITE_VT_SYNC_LABEL,
};
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, syscall, Error, Result};

/// Cursor blink timeout in milliseconds (used for both modes).
const BLINK_TIMEOUT_MS: u64 = 500;

/// Process entry point expected by the kernel loader.
#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

/// Initialize the console instance and enter the IPC-driven event loop.
fn run() -> Result<()> {
    let info = process_info();
    let start_active = info.params[PARAM_CONSOLE_ACTIVE] != 0;

    // Open /dev/fb0 + read 40-byte geometry header + mmap.
    // libcluu's mmap detects the FB magic and routes to MAP_DEVICE_WC.
    const FB_HEADER_MAGIC: u32 = 0x4642_4630; // "FB0\0"
    let path = b"/dev/fb0\0";
    let fd = unsafe { _open(path.as_ptr() as *const i8, O_RDWR, 0) };
    if fd < 0 {
        let _ = debug_print("console: open /dev/fb0 failed");
        return Err(Error::NotFound);
    }
    let mut hdr = [0u8; 40];
    let n = unsafe { _read(fd, hdr.as_mut_ptr() as *mut c_void, 40) };
    if n != 40 {
        unsafe { _close(fd); }
        let _ = debug_print("console: short read /dev/fb0");
        return Err(Error::InvalidArgument);
    }
    let magic = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
    if magic != FB_HEADER_MAGIC {
        unsafe { _close(fd); }
        let _ = debug_print("console: bad fb header magic");
        return Err(Error::InvalidArgument);
    }
    let width  = u32::from_le_bytes([hdr[ 4], hdr[ 5], hdr[ 6], hdr[ 7]]) as usize;
    let height = u32::from_le_bytes([hdr[ 8], hdr[ 9], hdr[10], hdr[11]]) as usize;
    let pitch  = u32::from_le_bytes([hdr[12], hdr[13], hdr[14], hdr[15]]) as usize;
    let fb_size = u64::from_le_bytes([
        hdr[24], hdr[25], hdr[26], hdr[27],
        hdr[28], hdr[29], hdr[30], hdr[31],
    ]);
    let fb_phys = u64::from_le_bytes([
        hdr[32], hdr[33], hdr[34], hdr[35],
        hdr[36], hdr[37], hdr[38], hdr[39],
    ]);
    let mapped = unsafe {
        mmap(
            core::ptr::null_mut::<c_void>(),
            fb_size as usize,
            PROT_READ | PROT_WRITE,
            MAP_SHARED,
            fd,
            0,
        )
    };
    unsafe { _close(fd); }
    if mapped as isize == -1 || mapped.is_null() {
        let _ = debug_print("console: mmap /dev/fb0 failed");
        return Err(Error::InvalidArgument);
    }
    let fb = mapped as *mut u8;
    let _ = debug_print("console: /dev/fb0 mapped");

    // Try double buffering first, fall back to direct framebuffer if heap too small
    if let Some(backend) = DoubleBufferBackend::try_new(fb, width, height, pitch) {
        let _ = debug_print("console: using double buffering");
        run_with_backend(backend, fb_phys, fb_size, start_active)
    } else {
        let _ = debug_print("console: using direct framebuffer (heap too small for backbuffer)");
        let backend = FramebufferBackend::new(fb, width, height, pitch);
        run_with_backend(backend, fb_phys, fb_size, start_active)
    }
}

/// Run the console event loop with the given backend.
fn run_with_backend<B: ConsoleBackend>(
    backend: B,
    fb_phys: u64,
    fb_size: u64,
    start_active: bool,
) -> Result<()> {
    let mut console = Console::new(backend, fb_phys, fb_size);
    console.set_active(start_active);

    let mut context = ConsoleContext::new()?;

    // Yield once so other services can register before we start consuming IPC.
    syscall::yield_cpu()?;

    // Initial flush to show the cleared screen (no-op if inactive)
    console.flush();

    let mut buf = [0u8; 512];

    loop {
        context.request_subscriptions();
        // Token layout: [vt:0, vt:1, ..., vt:N-1, control, registry]
        let mut tokens = [0usize; VT_COUNT + 2];
        for i in 0..VT_COUNT {
            tokens[i] = context.vt_endpoints[i];
        }
        tokens[VT_COUNT] = context.control_endpoint;
        tokens[VT_COUNT + 1] = context.registry_endpoint;
        match syscall::ipc_recv_any(&tokens, &mut buf, BLINK_TIMEOUT_MS) {
            Ok((index, len)) => {
                if let Some((msg, payload)) = parse_message(&buf[..len]) {
                    handle_incoming(index, &mut console, &mut context, &msg, payload)?;
                    // Flush after IPC for responsive input (no-op if inactive)
                    console.flush();
                } else {
                    let _ = debug_print("console: parse failed");
                }
            }
            Err(Error::Timeout) | Err(Error::WouldBlock) => {
                console.tick();
                // Flush on timeout for cursor blink (no-op if inactive)
                console.flush();
            }
            Err(_) => {
                let _ = debug_print("console: recv error");
            }
        }
    }
}

/// Route IPC traffic by endpoint index.
///
/// Token layout: [vt:0 .. vt:N-1, control, registry].
/// VT endpoints use the index as the VT number — no sender-reported VT index
/// needed, eliminating confused-deputy attacks.
fn handle_incoming<B: ConsoleBackend>(
    index: usize,
    console: &mut Console<B>,
    context: &mut ConsoleContext,
    msg: &libcluu::types::Message,
    payload: &[u8],
) -> Result<()> {
    if index < VT_COUNT {
        // Per-VT write endpoint — VT index is the endpoint index.
        let vt_index = index;
        match msg.tag.label {
            CONSOLE_WRITE_LABEL | CONSOLE_WRITE_VT_LABEL => {
                console.write_to_vt(vt_index, payload);
                context.record_rendered_bytes(payload.len());
            }
            CONSOLE_WRITE_SYNC_LABEL | CONSOLE_WRITE_VT_SYNC_LABEL => {
                console.write_to_vt(vt_index, payload);
                context.record_rendered_bytes(payload.len());
                if let Some(reply_token) = extract_reply_id(msg) {
                    let reply_msg = Message::new(msg.tag.label, [0; 6], 0);
                    let _ = reply(reply_token, &reply_msg, IpcFlags::empty());
                }
            }
            CONSOLE_FB_INFO_LABEL => {
                // Passive framebuffer metadata query — safe on the write
                // endpoint so clients that only hold a write grant (e.g.
                // framebuffer_acquire in libcluu) can discover the FB layout.
                let _ = console.handle_message(msg, payload);
            }
            // Forward presentation-layer commands (clear, cursor move, blink
            // toggle) so per-session shells can affect their own VT without
            // needing the privileged control endpoint. The renderer uses
            // `active_vt` internally, so the operation only takes visible
            // effect when this VT is on screen — matching user expectation.
            _ => {
                let _ = console.handle_message(msg, payload);
            }
        }
        Ok(())
    } else if index == VT_COUNT {
        // Control endpoint — VT lifecycle commands from vtmgr.
        match msg.tag.label {
            CONSOLE_SWITCH_VT_LABEL => {
                // Atomic VT switch: words[0] = old_vt, words[1] = new_vt.
                let new_vt = if msg.tag.words >= 2 { msg.words[1] } else { 0 };
                console.switch_vt(new_vt);
            }
            CONSOLE_ACTIVATE_LABEL => {
                // Backward compat: legacy two-message protocol.
                let vt_index = if msg.tag.words >= 1 { msg.words[0] } else { 0 };
                console.switch_vt(vt_index);
            }
            CONSOLE_DEACTIVATE_LABEL => {
                // Backward compat: legacy two-message protocol.
                let vt_index = if msg.tag.words >= 1 { msg.words[0] } else { 0 };
                console.deactivate_vt(vt_index);
            }
            CONSOLE_CREATE_VT_LABEL => {
                if msg.tag.words >= 1 {
                    console.create_vt(msg.words[0]);
                }
            }
            CONSOLE_SCROLL_VT_LABEL => {
                let vt_index = if msg.tag.words >= 1 { msg.words[0] } else { 0 };
                let direction = if msg.tag.words >= 2 { msg.words[1] } else { 0 };
                console.scroll_vt(vt_index, direction);
            }
            _ => {
                // Forward other management labels (CLEAR, CURSOR, BLINK, FB_INFO).
                let _ = console.handle_message(msg, payload);
            }
        }
        Ok(())
    } else {
        // Registry control endpoint.
        context.handle_registry_event(msg, payload);
        Ok(())
    }
}
