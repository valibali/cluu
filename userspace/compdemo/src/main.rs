#![no_std]
#![no_main]

extern crate alloc;

use libcluu::ipc::{
    COMP_FRAME_READY_LABEL, COMP_INPUT_FORWARD_LABEL,
    COMP_WIN_DAMAGE_LABEL, COMP_WIN_REGISTER_LABEL, COMP_WIN_REGISTER_REPLY,
};
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, registry, syscall};
use libcluu::boot::{process_info, space_token, TOKEN_IPC};
use libcluu::syscall::MAP_FRAME_TOKEN;
use libcluu::window_shm::WindowShm;

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

    // Allocate a long-lived endpoint for FRAME_READY + INPUT_FORWARD signals.
    let info = process_info();
    let ipc_cap = info.tokens[TOKEN_IPC];
    let my_ep = match syscall::endpoint_create(ipc_cap) {
        Ok(ep) => ep,
        Err(_) => {
            let _ = debug_print("compdemo: endpoint_create failed");
            return 6;
        }
    };

    // Look up compositor's client endpoint via registry.
    let comp_ep = match registry::lookup_service("compositor:client") {
        Some(ep) => ep,
        None => {
            let _ = debug_print("compdemo: no compositor:client in registry");
            return 1;
        }
    };

    // Build the WIN_REGISTER request. Payload is the title bytes.
    // Protocol: words[0]=payload_len, words[1]=req_w, words[2]=req_h,
    //           words[3]=app_input_endpoint (FRAME_READY + INPUT_FORWARD).
    let title = b"demo";
    let req = Message::new(
        COMP_WIN_REGISTER_LABEL,
        [
            title.len(),        // words[0] = payload_len
            REQ_W as usize,     // words[1] = req_w
            REQ_H as usize,     // words[2] = req_h
            my_ep,              // words[3] = app input/frame endpoint
            0,                  // words[4] = reserved
            0,                  // words[5] = reserved
        ],
        4,
    );
    let mut reply = Message::new(0, [0; 6], 0);
    if libcluu::ipc::call_with_payload(comp_ep, &req, title, &mut reply).is_err() {
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

    // Render loop: wait for FRAME_READY (pacing signal from compositor), then
    // render one frame and send DAMAGE. This replaces the busy spin-loop with
    // an event-driven design — compdemo sleeps until the compositor says "go".
    let cells_ptr = (SHM_VA + 32) as *mut u64;
    let mut frame: u32 = 0;
    let mut recv_buf = [0u8; 256];
    let tokens = [my_ep];

    loop {
        // 1. Render cells with a slowly shifting rainbow pattern.
        for iy in 0..gh {
            for ix in 0..gw {
                let bg_idx = 16u64 + ((ix as u64 + iy as u64 + frame as u64) % 216);
                let cp = b' ' as u64 & 0x1F_FFFF;
                let fg = 15u64 << 21;
                let bg = bg_idx << 29;
                let cell = cp | fg | bg | (0u64 << 37);
                unsafe {
                    core::ptr::write_volatile(
                        cells_ptr.add((iy * gw + ix) as usize),
                        cell,
                    );
                }
            }
        }

        // 2. Bump generation (release-store so compositor sees updated cells).
        unsafe {
            let hdr = SHM_VA as *mut WindowShm;
            let g = (*hdr).generation;
            core::ptr::write_volatile(&mut (*hdr).generation as *mut u32, g.wrapping_add(1));
        }

        // 3. Send DAMAGE — words[0]=win_id, words[1]=x, words[2]=y, words[3]=w, words[4]=h
        //    (matches protocol::parse COMP_WIN_DAMAGE_LABEL arm in compositor).
        let dmg = Message::new(
            COMP_WIN_DAMAGE_LABEL,
            [win_id, 0, 0, gw as usize, gh as usize, 0],
            5,
        );
        let _ = libcluu::ipc::send(comp_ep, &dmg, IpcFlags::empty());

        frame = frame.wrapping_add(1);

        // 4. Block waiting for FRAME_READY or INPUT_FORWARD.
        //    timeout_ms=60_000 → block up to 60 s; in practice compositor
        //    sends FRAME_READY right after every flush, so latency is negligible.
        match syscall::ipc_recv_any(&tokens, &mut recv_buf, 60_000) {
            Ok((_idx, len)) => {
                if let Some((msg, _payload)) = libcluu::ipc::parse_message(&recv_buf[..len]) {
                    if msg.tag.label == COMP_INPUT_FORWARD_LABEL {
                        // words[5] = kind: 99 = close-request.
                        let kind = msg.words[5] as u32;
                        if kind == 99 {
                            let _ = debug_print("compdemo: close-request received, exiting");
                            return 0;
                        }
                        // Other input: handled (key presses, etc.) — just loop back.
                    }
                    // FRAME_READY (label == 100) or any other label → proceed to next frame.
                }
            }
            Err(_) => {
                // Timeout or error — yield briefly and retry.
                let _ = syscall::yield_cpu();
            }
        }
    }
}
