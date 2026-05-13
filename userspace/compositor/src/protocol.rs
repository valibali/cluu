//! Compositor IPC protocol parser.
//!
//! Maps `libcluu::types::Message::tag.label` onto `Incoming` variants
//! that the main event loop dispatches. Payload bytes (e.g. window
//! titles) are NOT extracted here — the caller has the raw payload
//! slice and decodes per-variant.
//!
//! Keyboard events: we use the kbd service's native KBD_EVENT_LABEL
//! directly (label = 1). Word layout per kbd/src/protocol.rs:
//!   words[1] = ASCII (0 if none)
//!   words[2] = modifier bitmask (MOD_SHIFT=1<<0, MOD_CTRL=1<<1, MOD_ALT=1<<2, ...)
//!   words[3] = raw scancode (press/release bit stripped)
//!   words[4] = extended key code (0 for normal keys)
//! COMP_KBD_EVENT_LABEL (95) is left reserved; kbd routes directly to the
//! compositor's input endpoint, reusing the same message format (T25).

use libcluu::ipc::{
    KBD_EVENT_LABEL, COMP_SHUTDOWN_LABEL, COMP_VT_ACTIVATE_LABEL,
    COMP_VT_DEACTIVATE_LABEL, COMP_WIN_DAMAGE_LABEL, COMP_WIN_DESTROY_LABEL,
    COMP_WIN_REGISTER_LABEL, COMP_WIN_SET_TITLE_LABEL,
};
use libcluu::types::Message;

#[derive(Debug)]
pub enum Incoming {
    WinRegister { req_w: u32, req_h: u32, title_len: u32, input_endpoint: usize, flags: u32 },
    WinDamage { window_id: u64, x: u32, y: u32, w: u32, h: u32 },
    WinDestroy { window_id: u64 },
    WinSetTitle { window_id: u64, title_len: u32 },
    KbdEvent { ascii: u8, modifiers: u8, scancode: u8, extended: u8 },
    VtActivate,
    VtDeactivate,
    Shutdown,
    Other(u32),
}

pub fn parse(msg: &Message) -> Incoming {
    match msg.tag.label {
        COMP_WIN_REGISTER_LABEL => Incoming::WinRegister {
            // words[0] = payload_len (title byte count, per parse_message convention)
            // words[1] = req_w, words[2] = req_h, words[3] = app_input_endpoint
            // words[4] = flags (COMP_WIN_FLAG_*)
            req_w: msg.words[1] as u32,
            req_h: msg.words[2] as u32,
            title_len: msg.words[0] as u32,
            input_endpoint: msg.words[3],
            flags: msg.words[4] as u32,
        },
        COMP_WIN_DAMAGE_LABEL => Incoming::WinDamage {
            window_id: msg.words[0] as u64,
            x: msg.words[1] as u32,
            y: msg.words[2] as u32,
            w: msg.words[3] as u32,
            h: msg.words[4] as u32,
        },
        COMP_WIN_DESTROY_LABEL => Incoming::WinDestroy {
            window_id: msg.words[0] as u64,
        },
        COMP_WIN_SET_TITLE_LABEL => Incoming::WinSetTitle {
            window_id: msg.words[0] as u64,
            title_len: msg.words[1] as u32,
        },
        KBD_EVENT_LABEL => Incoming::KbdEvent {
            ascii:     msg.words[1] as u8,
            modifiers: msg.words[2] as u8,
            scancode:  msg.words[3] as u8,
            extended:  msg.words[4] as u8,
        },
        COMP_VT_ACTIVATE_LABEL => Incoming::VtActivate,
        COMP_VT_DEACTIVATE_LABEL => Incoming::VtDeactivate,
        COMP_SHUTDOWN_LABEL => Incoming::Shutdown,
        other => Incoming::Other(other),
    }
}
