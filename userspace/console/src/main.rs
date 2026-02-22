#![no_std]
#![no_main]

extern crate alloc;

/// Userspace console service.
///
/// The console owns a text grid and renders it to the framebuffer. It exposes a
/// single input endpoint (`write`) that other services (tty) can subscribe to
/// via the registry. Rendering stays synchronous and deterministic so the
/// console can later be swapped to a different backend (e.g., a GPU driver).
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
use crate::context::ConsoleContext;
use crate::protocol::parse_message;
use crate::renderer::Console;
use libcluu::boot::{
    process_info, PARAM_CONSOLE_ACTIVE, PARAM_FB_BASE, PARAM_FB_HEIGHT, PARAM_FB_PHYS,
    PARAM_FB_PITCH, PARAM_FB_SIZE, PARAM_FB_WIDTH,
};
use libcluu::ipc::{
    extract_reply_id, reply, CONSOLE_ACTIVATE_LABEL, CONSOLE_CREATE_VT_LABEL,
    CONSOLE_DEACTIVATE_LABEL, CONSOLE_WRITE_VT_LABEL, CONSOLE_WRITE_VT_SYNC_LABEL,
};
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, registry, syscall, Error, Result};

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
    let fb = info.params[PARAM_FB_BASE] as *mut u8;
    let width = info.params[PARAM_FB_WIDTH] as usize;
    let height = info.params[PARAM_FB_HEIGHT] as usize;
    let pitch = info.params[PARAM_FB_PITCH] as usize;
    let fb_phys = info.params[PARAM_FB_PHYS];
    let fb_size = info.params[PARAM_FB_SIZE];
    let start_active = info.params[PARAM_CONSOLE_ACTIVE] != 0;

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

    let context = ConsoleContext::new()?;

    // Yield once so other services can register before we start consuming IPC.
    syscall::yield_cpu()?;

    // Initial flush to show the cleared screen (no-op if inactive)
    console.flush();

    let mut buf = [0u8; 512];

    loop {
        let tokens = [context.endpoint, context.registry_endpoint];
        match syscall::ipc_recv_any(&tokens, &mut buf, BLINK_TIMEOUT_MS) {
            Ok((index, len)) => {
                if let Some((msg, payload)) = parse_message(&buf[..len]) {
                    handle_incoming(index, &mut console, &msg, payload)?;
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

/// Route IPC traffic to the renderer or the registry control handler.
fn handle_incoming<B: ConsoleBackend>(
    index: usize,
    console: &mut Console<B>,
    msg: &libcluu::types::Message,
    payload: &[u8],
) -> Result<()> {
    if index == 0 {
        // VT lifecycle and per-VT write messages from vtmgr/tty.
        match msg.tag.label {
            CONSOLE_ACTIVATE_LABEL => {
                let vt_index = if msg.tag.words >= 1 { msg.words[0] } else { 0 };
                console.switch_vt(vt_index);
                return Ok(());
            }
            CONSOLE_DEACTIVATE_LABEL => {
                let vt_index = if msg.tag.words >= 1 { msg.words[0] } else { 0 };
                console.deactivate_vt(vt_index);
                return Ok(());
            }
            CONSOLE_CREATE_VT_LABEL => {
                if msg.tag.words >= 1 {
                    console.create_vt(msg.words[0]);
                }
                return Ok(());
            }
            CONSOLE_WRITE_VT_LABEL => {
                if msg.tag.words >= 2 {
                    let vt_index = msg.words[1];
                    console.write_to_vt(vt_index, payload);
                }
                return Ok(());
            }
            CONSOLE_WRITE_VT_SYNC_LABEL => {
                if msg.tag.words >= 2 {
                    let vt_index = msg.words[1];
                    console.write_to_vt(vt_index, payload);
                }
                if let Some(reply_token) = extract_reply_id(msg) {
                    let reply_msg = Message::new(CONSOLE_WRITE_VT_SYNC_LABEL, [0; 6], 0);
                    let _ = reply(reply_token, &reply_msg, IpcFlags::empty());
                }
                return Ok(());
            }
            _ => {}
        }
        console.handle_message(msg, payload)
    } else {
        let _ = registry::handle_incoming_message(msg, payload)?;
        Ok(())
    }
}
