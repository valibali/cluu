use libcluu::ipc::MOUSE_EVENT_LABEL;
use libcluu::types::Message;

pub use libcluu::ipc::parse_message;

pub fn build_mouse_event(dx: i32, dy: i32, buttons: u8) -> Message {
    Message::new(
        MOUSE_EVENT_LABEL,
        [
            0,
            dx as usize,
            dy as usize,
            buttons as usize,
            0,
            0,
        ],
        4,
    )
}
