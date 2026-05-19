#![no_std]
#![no_main]

extern crate alloc;
extern crate cluu_proto;

mod config;
mod state;
mod shm;
mod protocol;
mod compose;
mod hotkeys;
mod status;
mod window_mgr;
mod render;

use libcluu::boot::{process_info, PARAM_NOTIFY_READY_EP, TOKEN_IPC};
use libcluu::ipc::{
    extract_reply_id, reply, reply_with_payload, send_msg_with_payload,
    COMP_WIN_REGISTER_REPLY, COMP_FRAME_READY_LABEL, VTMGR_PIN_VT_LABEL,
};
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, registry, syscall, Error};
use registry::RegistryEvent;

use cluu_proto::session::{
    SESSION_ENDED_LABEL, COMPOSITOR_SESSION_HANDOFF_LABEL,
    CompositorSessionHandoffRequest, CompositorSessionHandoffReply,
    SessionEndedEvent, SessionErr,
};
use cluu_proto::spawn::{SpawnEnvelope, ViewSource};
use cluu_proto::ABI_VERSION;

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
    // session_mode removed — compositor runs in single persistent mode
    // (Task 9, Plan 3: session lifecycle refactor)
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

    // READY notify is *deferred* until the main dispatch loop has actually
    // started running (see one-shot send at the top of `loop` below).
    // Sending it here, before vtmgr lookup + `recv_any` loop entry, races
    // against any grant request the parent's downstream consumer (cluuterm)
    // may send the instant it receives our READY — the request would land
    // on `control_endpoint` while we are blocked inside
    // `lookup_service("vtmgr:control")` (which only polls one endpoint and
    // can exit on its own grant before our queued REGISTRY_GRANT_REQUEST is
    // drained). Holding the READY until the polyendpoint recv is actually
    // armed closes that window.
let notify_ep = info.params[PARAM_NOTIFY_READY_EP] as usize;

    // Pin ourselves to VT4 in vtmgr so the slot is explicit and stable
    // regardless of service-launch order.  This is a best-effort fire-and-forget:
    // if vtmgr isn't up yet the message is dropped and vtmgr falls back to the
    // DEFAULT_COMPOSITOR_VT constant (also 4).
    const COMPOSITOR_VT: usize = 4;
    const SERVICE_NAME: &[u8] = b"compositor";
    if let Some(vtmgr_ep) = registry::lookup_service("vtmgr:control") {
        // words[0] is overwritten with payload_len by send_msg_with_payload, so
        // vt_index lives in words[1] to survive the transport.
        let pin_msg = Message::new(
            VTMGR_PIN_VT_LABEL,
            [SERVICE_NAME.len(), COMPOSITOR_VT, 0, 0, 0, 0],
            2,
        );
        let _ = send_msg_with_payload(vtmgr_ep, &pin_msg, SERVICE_NAME);
        let _ = debug_print("compositor: pinned to VT4");
    } else {
        let _ = debug_print("compositor: vtmgr not yet available, relying on default VT4 pin");
    }

    let _ = debug_print("compositor: ready");

    spawn_login_window(&mut comp);

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
    // Whether we have already sent a subscription request for timeserver:main.
    let mut requested_timeserver = false;

    // Subscribe to timeserver:main up-front so we get a Grant when it registers.
    if registry::request_subscription("timeserver", "main").is_ok() {
        requested_timeserver = true;
        let _ = debug_print("compositor: timeserver subscription requested");
    }
    // Armed once we successfully send TIME_SUBSCRIBE_PERIODIC to timeserver.
    let mut pushmode_armed = false;

    // Index of the registry endpoint in the tokens array.
    const REGISTRY_TOKEN_IDX: usize = 3;

    let _ = notify_ep;

    loop {
        let now_ms = comp.last_clock_now_ms;

        // Pure event-driven: block for at most 30 s then loop on Timeout.
        // This avoids passing near-u64::MAX to the kernel recv syscall
        // (violates the "no timeouts as deadlock guards" rule). When a frame
        // deadline is pending, the cap is tightened to that deadline.
        const RECV_MAX_MS: u64 = 30_000;
        let timeout_ms = comp.deadlines.next_timeout_ms(now_ms, RECV_MAX_MS);

        match syscall::ipc_recv_any_with_sender(&tokens, &mut buf, timeout_ms) {
            Ok((idx, len, sender_tid)) => {
                if let Some((msg, payload)) = libcluu::ipc::parse_message(&buf[..len]) {
                    // TIME_TICK from timeserver push-mode subscription.
                    // Arrives on input_endpoint_global (idx=1). Update the
                    // cached clock and fire tick_clock (which marks row 0
                    // dirty in cell_dirty), then FALL THROUGH so the
                    // post-recv block runs recompute_dirty + render_status_row
                    // to actually rewrite the clock string into cell_grid.
                    // Without falling through, cells stay dirty but the grid
                    // is never updated → status bar shows stale "--:--:--".
                    if msg.tag.label == libcluu::time::TIME_TICK_LABEL && idx != REGISTRY_TOKEN_IDX {
                        let now_ms_from_tick = msg.words[1] as u64;
                        comp.last_clock_now_ms = now_ms_from_tick;
                        comp.tick_clock(now_ms_from_tick, now_ms_from_tick / 1000);
                        // Do NOT continue — fall through to post-recv block.
                    }

                    // Session handoff from login service. Handled with raw label match
                    // because the payload is postcard-encoded (not word-based).
                    if msg.tag.label == COMPOSITOR_SESSION_HANDOFF_LABEL {
                        handle_session_handoff(&mut comp, &msg, payload, sender_tid);
                        continue;
                    }
                    if msg.tag.label == SESSION_ENDED_LABEL {
                        handle_session_ended(&mut comp, &msg, payload);
                        continue;
                    }

                    // Registry control messages (grant requests from subscribers) must
                    // be forwarded to the registry client so it can mint tokens.
                    if idx == REGISTRY_TOKEN_IDX {
                        let _ = debug_print(&alloc::format!(
                            "compositor: registry msg label=0x{:x}", msg.tag.label
                        ));
                        let result = registry::handle_incoming_message(&msg, payload);
                        if let Err(ref e) = result {
                            let _ = debug_print(&alloc::format!(
                                "compositor: handle_incoming_message err={:?}", e
                            ));
                        }
                        if let Ok(Some(event)) = result {
                            match event {
                                RegistryEvent::Grant { service_name, name, token } => {
                                    if service_name == "timeserver" && name == "main" {
                                        time_ep = token;
                                        let _ = debug_print("compositor: timeserver subscribed");
                                        // Arm push-mode: subscribe for 1 Hz ticks on the input
                                        // endpoint. The status-bar clock has 1 s granularity
                                        // so 1000 ms suffices.
                                        if !pushmode_armed && time_ep != 0 {
                                            let notify_ep = comp.input_endpoint_global;
                                            let mut sub = libcluu::types::Message::new(
                                                libcluu::time::TIME_SUBSCRIBE_PERIODIC_LABEL,
                                                [1000, notify_ep, 0, 0, 0, 0],
                                                3,
                                            );
                                            if libcluu::ipc::call(time_ep, &mut sub, IpcFlags::empty()).is_ok()
                                                && sub.words[0] == 0
                                            {
                                                pushmode_armed = true;
                                                let _ = debug_print("compositor: subscribed to timeserver pushmode 1000ms");
                                            }
                                        }
                                    }
                                }
                                RegistryEvent::SubscribeStatus { code } => {
                                    if code != 0 {
                                        // Subscription failed; retry next iteration.
                                        requested_timeserver = false;
                                    }
                                }
                            }
                        }
                        continue;
                    }
                    let kind = protocol::parse(&msg);
                    match kind {
                        protocol::Incoming::WinRegister { req_w, req_h, title_len, input_endpoint, flags } => {
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
                                flags,
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
                let _ = debug_print("compositor: recv_any -> WouldBlock/Timeout");
                // Fall through to deadline handling below.
            }
            Err(_) => {
                let _ = debug_print(&alloc::format!("compositor: recv_any -> Err other -> yield+continue"));
                let _ = syscall::yield_cpu();
                continue;
            }
        }

        // Retry timeserver subscription if not yet requested (e.g. registry
        // was unavailable at startup).
        if !requested_timeserver && time_ep == 0 {
            if registry::request_subscription("timeserver", "main").is_ok() {
                requested_timeserver = true;
            }
        }

        // Recompute dirty cells (tick_clock is now fired by TIME_TICK push arm).
        compose::recompute_dirty(&mut comp);
        compose::render_status_row(&mut comp);
        // Arm the frame deadline if the clock tick or status render dirtied
        // the cell grid.  (The message-receive arm above only covers
        // protocol-message-driven dirt; clock-tick dirt arrives here.)
        if comp.prev_cell_grid != comp.cell_grid {
            comp.schedule_frame(now_ms);
        }

        if comp.tick_frame(now_ms) {
            broadcast_frame_ready(&mut comp);
        }
    }
}

// ── Session lifecycle handlers ─────────────────────────────────────

fn handle_session_handoff(
    comp: &mut state::Compositor,
    msg: &Message,
    payload: &[u8],
    _sender_tid: usize,
) {
    let req: CompositorSessionHandoffRequest = match postcard::from_bytes(payload) {
        Ok(r) => r,
        Err(_) => {
            reply_handoff_err(comp, msg, SessionErr::Internal(0xE4u32));
            return;
        }
    };

    let our_event_send = event_endpoint_send_cap(comp);

    match libcluu::session::subscribe(req.token_sub, our_event_send) {
        Ok(()) => {
            comp.tracked_sessions.insert(req.session_id);
            let reply: CompositorSessionHandoffReply = Ok(());
            reply_postcard(comp, msg, COMPOSITOR_SESSION_HANDOFF_LABEL, &reply);
        }
        Err(e) => {
            reply_handoff_err(comp, msg, e);
        }
    }
}

fn handle_session_ended(comp: &mut state::Compositor, _msg: &Message, payload: &[u8]) {
    let event: SessionEndedEvent = match postcard::from_bytes(payload) {
        Ok(e) => e,
        Err(_) => return,
    };

    let to_close: alloc::vec::Vec<u64> = comp
        .windows
        .iter()
        .filter(|w| w.session_id == Some(event.session_id))
        .map(|w| w.id)
        .collect();
    for window_id in to_close {
        close_window(comp, window_id);
    }
    comp.tracked_sessions.remove(&event.session_id);
    spawn_login_window(comp);
}

// ── Helpers ─────────────────────────────────────────────────────────

fn reply_postcard<R: serde::Serialize>(
    comp: &state::Compositor,
    msg: &Message,
    label: u32,
    value: &R,
) {
    let bytes = postcard::to_allocvec(value).expect("ser");
    let reply_msg = Message::new(label, [bytes.len(), ABI_VERSION as usize, 0, 0, 0, 0], 2);
    let reply_token = extract_reply_id(msg).unwrap_or(0);
    let _ = reply_with_payload(reply_token, &reply_msg, &bytes);
}

fn reply_handoff_err(comp: &state::Compositor, msg: &Message, err: SessionErr) {
    let reply: CompositorSessionHandoffReply = Err(err);
    reply_postcard(comp, msg, COMPOSITOR_SESSION_HANDOFF_LABEL, &reply);
}

/// Return the token handle the compositor uses to send SESSION_ENDED
/// subscription events to procmgr. Stub — returns 0 until the event
/// endpoint is properly created and wired.
fn event_endpoint_send_cap(_comp: &state::Compositor) -> cluu_proto::TokenHandle {
    0
}

/// Return the view token that login and session windows derive their
/// VFS view from. Stub — returns 0 until the real view token plumbing
/// is in place.
fn compositor_view_token(_comp: &state::Compositor) -> cluu_proto::TokenHandle {
    0
}

fn close_window(_comp: &mut state::Compositor, _window_id: u64) {
    // Stub — real close_window will call handle_win_destroy + SHM free.
}

fn spawn_login_window(comp: &mut state::Compositor) {
    let envelope = SpawnEnvelope {
        image: alloc::string::String::from("login"),
        args: alloc::vec::Vec::new(),
        env: alloc::vec::Vec::new(),
        view: ViewSource::Derive(compositor_view_token(comp)),
        fd_inherit: alloc::vec::Vec::new(),
        session: None,
        notify: None,
    };
    if let Err(e) = libcluu::spawn::spawn(envelope) {
        let _ = libcluu::debug_print(&alloc::format!(
            "compositor: login spawn failed {:?}\n",
            e
        ));
    }
}
