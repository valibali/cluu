#![no_std]
#![no_main]

extern crate alloc;

use libcluu::ipc::{
    call_with_payload, COMP_WIN_DAMAGE_LABEL, COMP_WIN_REGISTER_LABEL,
    COMP_WIN_REGISTER_REPLY,
};
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, registry, syscall};
use libcluu::boot::space_token;
use libcluu::syscall::MAP_FRAME_TOKEN;

#[repr(C)]
struct WindowShm {
    magic: u32,
    version: u32,
    width: u32,
    height: u32,
    cursor_x: u32,
    cursor_y: u32,
    cursor_visible: u32,
    generation: u32,
}

#[allow(dead_code)]
const WIN_SHM_MAGIC: u32 = 0x57494e44;
#[allow(dead_code)]
const WIN_SHM_VERSION: u32 = 1;

const FLAGS_USER_RW: usize = 0x07;
const SHM_VA: usize = 0xD000_0000;

const REQ_W: u32 = 40;
const REQ_H: u32 = 12;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = debug_print("compdemo: start");

    // Registry init is required before lookup_service.
    if registry::init("compdemo").is_err() {
        let _ = debug_print("compdemo: registry init failed");
        return 1;
    }

    // Look up compositor's client endpoint via registry.
    let comp_ep = match registry::lookup_service("compositor:client") {
        Some(ep) => ep,
        None => {
            let _ = debug_print("compdemo: no compositor:client in registry");
            return 1;
        }
    };

    // Build the WIN_REGISTER request. Payload is the title bytes.
    let title = b"demo";
    let req = Message::new(
        COMP_WIN_REGISTER_LABEL,
        [REQ_W as usize, REQ_H as usize, title.len(), 0, 0, 0],
        title.len() as u8,
    );
    let mut reply = Message::new(0, [0; 6], 0);
    if call_with_payload(comp_ep, &req, title, &mut reply).is_err() {
        let _ = debug_print("compdemo: WIN_REGISTER call failed");
        return 2;
    }
    if reply.tag.label != COMP_WIN_REGISTER_REPLY {
        let _ = debug_print("compdemo: unexpected register reply label");
        return 3;
    }
    let win_id = reply.words[0];
    let token = reply.words[1];
    let gw = reply.words[2] as u32;
    let gh = reply.words[3] as u32;
    let err = reply.words[4];
    if err != 0 {
        let _ = debug_print("compdemo: compositor denied WIN_REGISTER");
        return 4;
    }

    // Map the SHM token into our space at SHM_VA.
    let cells_bytes = gw as usize * gh as usize * 8;
    let total = (32 + cells_bytes + 0xFFF) & !0xFFF;
    let num_pages = total / 0x1000;
    let space = space_token();
    if syscall::space_map_range(
        space, SHM_VA, token, FLAGS_USER_RW | MAP_FRAME_TOKEN, num_pages, 0,
    ).is_err() {
        let _ = debug_print("compdemo: SHM map failed");
        return 5;
    }
    let _ = debug_print("compdemo: window registered + SHM mapped");

    // Keep alive for T22 — replaced in T22 with rainbow loop.
    let _ = (win_id, gw, gh, comp_ep);
    loop {
        let _ = syscall::yield_cpu();
    }
}
