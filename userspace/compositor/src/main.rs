#![no_std]
#![no_main]

extern crate alloc;

mod state;
mod shm;
mod protocol;

use libcluu::{debug_print, syscall, Error};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = debug_print("compositor: init");
    let mut _comp = match state::Compositor::init() {
        Ok(c) => c,
        Err(_) => {
            let _ = debug_print("compositor: init failed");
            return -1;
        }
    };
    let _ = debug_print("compositor: ready");

    // Endpoint registration lands in T9. For now, allocate a placeholder
    // 4-element token list and drive recv with all zeros — recv_any will
    // return InvalidArgument and we fall through to yield. This proves the
    // event loop compiles and doesn't crash; real wiring follows.
    let tokens = [0usize; 4];
    let mut buf = [0u8; 1024];

    loop {
        match syscall::ipc_recv_any(&tokens, &mut buf, 1000) {
            Ok((idx, len)) => {
                if let Some((msg, payload)) = libcluu::ipc::parse_message(&buf[..len]) {
                    let kind = protocol::parse(&msg);
                    let _ = debug_print("compositor: msg");
                    let _ = (idx, payload, kind);
                }
            }
            Err(Error::Timeout) | Err(Error::WouldBlock) => {
                // Tick path lives here once we wire status bar + clock.
            }
            Err(_) => {
                // Quiet — at this point in the plan the tokens are zero,
                // so every recv will fail. Don't spam the log.
                let _ = syscall::yield_cpu();
            }
        }
    }
}
