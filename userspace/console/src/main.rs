#![no_std]
#![no_main]

extern crate alloc;

/// Userspace console service.
///
/// The console owns a text grid and renders it to the framebuffer. It exposes a
/// single input endpoint (`write`) that other services (tty) can subscribe to
/// via the registry. Rendering stays synchronous and deterministic so the
/// console can later be swapped to a different backend (e.g., a GPU driver).
mod backend;
#[cfg(target_arch = "x86_64")]
mod simd;
mod context;
mod protocol;
mod renderer;

use crate::backend::FramebufferBackend;
use crate::context::ConsoleContext;
use crate::protocol::parse_message;
use crate::renderer::Console;
use libcluu::boot::{process_info, PARAM_FB_BASE, PARAM_FB_HEIGHT, PARAM_FB_PITCH, PARAM_FB_WIDTH};
use libcluu::{debug_print, registry, syscall, Error, Result};

/// Cursor blink timeout in milliseconds.
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
    let backend = FramebufferBackend::new(
        fb,
        info.params[PARAM_FB_WIDTH] as usize,
        info.params[PARAM_FB_HEIGHT] as usize,
        info.params[PARAM_FB_PITCH] as usize,
    );
    let mut console = Console::new(backend);

    let context = ConsoleContext::new()?;

    // Yield once so other services can register before we start consuming IPC.
    syscall::yield_cpu()?;

    let mut buf = [0u8; 512];
    loop {
        let tokens = [context.endpoint, context.registry_endpoint];
        match syscall::ipc_recv_any(&tokens, &mut buf, BLINK_TIMEOUT_MS) {
            Ok((index, len)) => {
                if let Some((msg, payload)) = parse_message(&buf[..len]) {
                    handle_incoming(index, &mut console, &msg, payload)?;
                } else {
                    let _ = debug_print("console: parse failed");
                }
            }
            Err(Error::Timeout) | Err(Error::WouldBlock) => {
                console.tick();
            }
            Err(_) => {
                let _ = debug_print("console: recv error");
            }
        }
    }
}

/// Route IPC traffic to the renderer or the registry control handler.
fn handle_incoming(
    index: usize,
    console: &mut Console<FramebufferBackend>,
    msg: &libcluu::types::Message,
    payload: &[u8],
) -> Result<()> {
    if index == 0 {
        console.handle_message(msg, payload)
    } else {
        let _ = registry::handle_incoming_message(msg, payload)?;
        Ok(())
    }
}
