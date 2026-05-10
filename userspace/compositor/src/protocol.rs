//! Compositor IPC protocol parser.
//!
//! Maps `libcluu::types::Message::tag.label` onto `Incoming` variants
//! that the main event loop dispatches. Payload bytes (e.g. window
//! titles) are NOT extracted here — the caller has the raw payload
//! slice and decodes per-variant.

use libcluu::ipc::{
    COMP_KBD_EVENT_LABEL, COMP_SHUTDOWN_LABEL, COMP_VT_ACTIVATE_LABEL,
    COMP_VT_DEACTIVATE_LABEL, COMP_WIN_DAMAGE_LABEL, COMP_WIN_DESTROY_LABEL,
    COMP_WIN_REGISTER_LABEL, COMP_WIN_SET_TITLE_LABEL,
};
use libcluu::types::Message;

#[derive(Debug)]
pub enum Incoming {
    WinRegister { req_w: u32, req_h: u32, title_len: u32 },
    WinDamage { window_id: u64, x: u32, y: u32, w: u32, h: u32 },
    WinDestroy { window_id: u64 },
    WinSetTitle { window_id: u64, title_len: u32 },
    KbdEvent { keycode: u32, modifiers: u32, codepoint: u32, kind: u32 },
    VtActivate,
    VtDeactivate,
    Shutdown,
    Other(u32),
}

pub fn parse(msg: &Message) -> Incoming {
    match msg.tag.label {
        COMP_WIN_REGISTER_LABEL => Incoming::WinRegister {
            req_w: msg.words[0] as u32,
            req_h: msg.words[1] as u32,
            title_len: msg.words[2] as u32,
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
        COMP_KBD_EVENT_LABEL => Incoming::KbdEvent {
            keycode: msg.words[0] as u32,
            modifiers: msg.words[1] as u32,
            codepoint: msg.words[2] as u32,
            kind: msg.words[3] as u32,
        },
        COMP_VT_ACTIVATE_LABEL => Incoming::VtActivate,
        COMP_VT_DEACTIVATE_LABEL => Incoming::VtDeactivate,
        COMP_SHUTDOWN_LABEL => Incoming::Shutdown,
        other => Incoming::Other(other),
    }
}
