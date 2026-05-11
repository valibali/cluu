//! Shared on-wire layout for compositor SHM windows.
//!
//! Compositor allocates the frame, writes this header at offset 0,
//! shares the token with the client. Both sides must agree on the
//! header layout — keeping it here prevents drift.

#[repr(C)]
pub struct WindowShm {
    pub magic: u32,
    pub version: u32,
    pub width: u32,
    pub height: u32,
    pub cursor_x: u32,
    pub cursor_y: u32,
    pub cursor_visible: u32,
    pub generation: u32,
}

pub const WIN_SHM_MAGIC: u32 = 0x57494e44; // "WIND"
pub const WIN_SHM_VERSION: u32 = 1;
