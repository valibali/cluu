#![no_std]
#![no_main]

extern crate alloc;

mod config;
mod state;
mod shm;
mod protocol;
mod compose;
mod hotkeys;
mod status;
mod window_mgr;
mod render;

use libcluu::boot::{process_info, TOKEN_IPC};
use libcluu::ipc::{
    extract_reply_id, reply, send_msg_with_payload,
    COMP_WIN_REGISTER_REPLY, COMP_FRAME_READY_LABEL, VTMGR_PIN_VT_LABEL,
};
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, registry, syscall, Error};

/// Send COMP_FRAME_READY_LABEL to windows that have pending damage since the
/// last broadcast.  A window is eligible if:
///   (a) it sent a WIN_DAMAGE event since the last broadcast (`pending_frame_ready`), OR
///   (b) its SHM `generation` counter advanced past the snapshot in `last_gen`
///       (catches apps that write to SHM without a WIN_DAMAGE message).
/// This gates the 60 Hz ticker so windows that haven't rendered a new frame
/// don't accumulate FRAME_READY messages in their endpoint queue.
fn broadcast_frame_ready(comp: &mut state::Compositor) {
    for win in comp.windows.iter_mut() {
        if win.input_endpoint == 0 { continue; }
        // Check SHM generation as a secondary damage signal.
        let current_gen = win.mapping.header().generation;
        let gen_advanced = current_gen != win.last_gen;
        if !win.pending_frame_ready && !gen_advanced { continue; }
        let msg = libcluu::types::Message::new(
            COMP_FRAME_READY_LABEL,
            [win.id as usize, 0, 0, 0, 0, 0],
            1,
        );
        // Only clear damage flag + advance gen snapshot on successful send,
        // so a transient send error doesn't strand the window's pending
        // damage forever.
        if libcluu::ipc::send(win.input_endpoint, &msg, libcluu::types::IpcFlags::empty()).is_ok() {
            win.pending_frame_ready = false;
            win.last_gen = current_gen;
        }
    }
}

/// Return the current monotonic time in milliseconds.
/// Reuses the already-cached timeserver endpoint to avoid a registry lookup.
/// Returns 0 if the endpoint is not yet resolved.
fn clock_now_ms(cached_ep: &mut usize) -> u64 {
    if *cached_ep == 0 {
        if let Some(ep) = registry::lookup_service("timeserver:main") {
            *cached_ep = ep;
        } else {
            return 0;
        }
    }
    match libcluu::time::query_endpoint(*cached_ep, libcluu::time::TIME_GETCLOCK) {
        Ok((s, ns)) => s * 1_000 + ns / 1_000_000,
        Err(_) => {
            *cached_ep = 0;
            0
        }
    }
}


#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = debug_print("compositor: init");
    let mut comp = match state::Compositor::init() {
        Ok(c) => c,
        Err(_) => {
            let _ = debug_print("compositor: init failed");
            return -1;
        }
    };

    let info = process_info();
    let ipc_cap = info.tokens[TOKEN_IPC];
    comp.instance_id = 0;
    // Register as "compositor" (not "compositor:0") so lookup_service("compositor:client")
    // resolves correctly: it splits on ':' to get service="compositor", output="client".
    if registry::init("compositor").is_err() {
        let _ = debug_print("compositor: registry init failed");
        return -1;
    }
    if registry::register_default_outputs().is_err() {
        let _ = debug_print("compositor: register_default_outputs failed");
    }

    comp.client_endpoint = match syscall::endpoint_create(ipc_cap) {
        Ok(ep) => ep,
        Err(_) => { let _ = debug_print("compositor: client endpoint failed"); return -1; }
    };
    comp.input_endpoint_global = match syscall::endpoint_create(ipc_cap) {
        Ok(ep) => ep,
        Err(_) => { let _ = debug_print("compositor: input endpoint failed"); return -1; }
    };
    comp.control_endpoint = match syscall::endpoint_create(ipc_cap) {
        Ok(ep) => ep,
        Err(_) => { let _ = debug_print("compositor: control endpoint failed"); return -1; }
    };
    let _ = registry::register_output("client", comp.client_endpoint);
    let _ = registry::register_output("input", comp.input_endpoint_global);
    let _ = registry::register_output("control", comp.control_endpoint);
    comp.registry_endpoint = registry::control_endpoint();

    let _ = debug_print("compositor: endpoints registered");

    // Pin ourselves to VT4 in vtmgr so the slot is explicit and stable
    // regardless of service-launch order.  This is a best-effort fire-and-forget:
    // if vtmgr isn't up yet the message is dropped and vtmgr falls back to the
    // DEFAULT_COMPOSITOR_VT constant (also 4).
    const COMPOSITOR_VT: usize = 4;
    const SERVICE_NAME: &[u8] = b"compositor";
    if let Some(vtmgr_ep) = registry::lookup_service("vtmgr:control") {
        let pin_msg = Message::new(
            VTMGR_PIN_VT_LABEL,
            [COMPOSITOR_VT, 0, 0, 0, 0, 0],
            1,
        );
        let _ = send_msg_with_payload(vtmgr_ep, &pin_msg, SERVICE_NAME);
        let _ = debug_print("compositor: pinned to VT4");
    } else {
        let _ = debug_print("compositor: vtmgr not yet available, relying on default VT4 pin");
    }

    let _ = debug_print("compositor: ready");

    // vtmgr owns fb arbitration: compositor starts inactive and waits
    // for COMP_VT_ACTIVATE_LABEL from vtmgr (Ctrl+Alt+F5).

    let tokens = [
        comp.client_endpoint,
        comp.input_endpoint_global,
        comp.control_endpoint,
        comp.registry_endpoint,
    ];
    let mut buf = [0u8; 1024];
    // Cached timeserver endpoint — 0 means not yet resolved.
    let mut time_ep: usize = 0;

    // Index of the registry endpoint in the tokens array.
    const REGISTRY_TOKEN_IDX: usize = 3;

    loop {
        let now_ms = clock_now_ms(&mut time_ep);
        let now_secs = now_ms / 1000;

        let timeout_ms = comp.deadlines.next_timeout_ms(now_ms);

        match syscall::ipc_recv_any_with_sender(&tokens, &mut buf, timeout_ms) {
            Ok((idx, len, sender_tid)) => {
                if let Some((msg, payload)) = libcluu::ipc::parse_message(&buf[..len]) {
                    // Registry control messages (grant requests from subscribers) must
                    // be forwarded to the registry client so it can mint tokens.
                    if idx == REGISTRY_TOKEN_IDX {
                        let _ = registry::handle_incoming_message(&msg, payload);
                        continue;
                    }
                    let kind = protocol::parse(&msg);
                    match kind {
                        protocol::Incoming::WinRegister { req_w, req_h, title_len, input_endpoint } => {
                            let title_len_usize = (title_len as usize).min(payload.len());
                            let title_bytes = &payload[..title_len_usize];
                            let title = core::str::from_utf8(title_bytes).unwrap_or("");
                            // Sender info: extract reply_id for the WIN_REGISTER_REPLY.
                            // owner_pid is a tid-as-pid surrogate: CLUU does not yet expose
                            // a pid-from-tid lookup; one-thread apps have tid == pid in practice.
                            // Proper pid resolution deferred (spec §10).
                            let reply_token = extract_reply_id(&msg).unwrap_or(0);
                            let owner_pid = sender_tid as u32;
                            match comp.handle_win_register(
                                owner_pid,
                                req_w,
                                req_h,
                                title,
                                input_endpoint,
                            ) {
                                Ok((id, token, gw, gh)) => {
                                    let reply_msg = Message::new(
                                        COMP_WIN_REGISTER_REPLY,
                                        [id as usize, token as usize, gw as usize, gh as usize, 0, 0],
                                        6,
                                    );
                                    let _ = reply(
                                        reply_token,
                                        &reply_msg,
                                        IpcFlags::empty(),
                                    );
                                    // Send a synthetic FRAME_READY so the app can
                                    // render its first frame without deadlocking
                                    // (it blocks on FRAME_READY before its first DAMAGE).
                                    if input_endpoint != 0 {
                                        let fr_msg = Message::new(
                                            COMP_FRAME_READY_LABEL,
                                            [id as usize, 0, 0, 0, 0, 0],
                                            1,
                                        );
                                        let _ = libcluu::ipc::send(
                                            input_endpoint,
                                            &fr_msg,
                                            IpcFlags::empty(),
                                        );
                                    }
                                    let _ = debug_print("compositor: window registered");
                                }
                                Err(_) => {
                                    let reply_msg = Message::new(
                                        COMP_WIN_REGISTER_REPLY,
                                        [0, 0, 0, 0, 1 /* error code: 1 = denied */, 0],
                                        6,
                                    );
                                    let _ = reply(
                                        reply_token,
                                        &reply_msg,
                                        IpcFlags::empty(),
                                    );
                                    let _ = debug_print("compositor: WIN_REGISTER denied");
                                }
                            }
                        }
                        protocol::Incoming::WinDestroy { window_id } => {
                            comp.handle_win_destroy(window_id);
                            let _ = debug_print("compositor: window destroyed");
                        }
                        protocol::Incoming::WinDamage { window_id, x, y, w, h } => {
                            comp.handle_win_damage(window_id, x, y, w, h);
                        }
                        protocol::Incoming::WinSetTitle { window_id, title_len } => {
                            let n = (title_len as usize).min(payload.len());
                            if let Ok(s) = core::str::from_utf8(&payload[..n]) {
                                comp.handle_win_set_title(window_id, s);
                            }
                        }
                        protocol::Incoming::KbdEvent { ascii, modifiers, scancode, extended } => {
                            if let Some(hk) = hotkeys::match_hotkey(modifiers, scancode, extended) {
                                match hk {
                                    hotkeys::Hotkey::FocusNext  => comp.focus_next(),
                                    hotkeys::Hotkey::FocusPrev  => comp.focus_prev(),
                                    hotkeys::Hotkey::MoveLeft   => comp.move_focused(-1, 0),
                                    hotkeys::Hotkey::MoveRight  => comp.move_focused( 1, 0),
                                    hotkeys::Hotkey::MoveUp     => comp.move_focused( 0,-1),
                                    hotkeys::Hotkey::MoveDown   => comp.move_focused( 0, 1),
                                    hotkeys::Hotkey::ResizeLeft  => comp.resize_focused(-1, 0),
                                    hotkeys::Hotkey::ResizeRight => comp.resize_focused( 1, 0),
                                    hotkeys::Hotkey::ResizeUp    => comp.resize_focused( 0,-1),
                                    hotkeys::Hotkey::ResizeDown  => comp.resize_focused( 0, 1),
                                    hotkeys::Hotkey::CloseRequest => {
                                        comp.forward_close_request();
                                    }
                                    hotkeys::Hotkey::SpawnDemo => {
                                        comp.spawn_demo();
                                    }
                                }
                            } else {
                                comp.forward_input_event(ascii, modifiers, scancode, extended);
                            }
                        }
                        protocol::Incoming::VtActivate => {
                            comp.handle_vt_activate();
                            let _ = debug_print("compositor: VT activate");
                        }
                        protocol::Incoming::VtDeactivate => {
                            comp.handle_vt_deactivate();
                            let _ = debug_print("compositor: VT deactivate");
                        }
                        protocol::Incoming::Shutdown => {
                            let _ = debug_print("compositor: shutdown");
                            return 0;
                        }
                        protocol::Incoming::Other(_label) => {
                            // Unknown message — ignore.
                        }
                    }
                    // Any state-changing message may have dirtied cells; arm
                    // the frame deadline so tick_frame fires promptly.
                    if !comp.cell_dirty.is_empty() {
                        comp.schedule_frame(now_ms);
                    }
                }
            }
            Err(Error::Timeout) | Err(Error::WouldBlock) => {
                // Fall through to deadline handling below.
            }
            Err(_) => {
                let _ = syscall::yield_cpu();
                continue;
            }
        }

        // Fire whichever deadlines expired this iteration.
        comp.tick_clock(now_ms, now_secs);
        compose::recompute_dirty(&mut comp);
        compose::render_status_row(&mut comp);

        if comp.tick_frame(now_ms) {
            broadcast_frame_ready(&mut comp);
        }
    }
}
