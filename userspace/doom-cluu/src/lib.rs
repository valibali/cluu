//! CLUU platform backend for doomgeneric.
//!
//! Implements the 6 doomgeneric platform functions (DG_Init, DG_DrawFrame,
//! DG_SleepMs, DG_GetTicksMs, DG_GetKey, DG_SetWindowTitle) as `extern "C"`
//! so the doomgeneric C engine links against this staticlib.
//!
//! Display: compositor fullscreen window + PixelRegion (ARGB32 SHM).
//! Input:  COMP_INPUT_FORWARD keyboard events → SPSC key queue.
//! Audio:  AudioSessionClient against virtio-snd (S16LE, best-effort).

#![no_std]
#![allow(unused_imports)]

extern crate alloc;

use alloc::format;
use alloc::string::String;

use libcluu::audio_client::{hz_to_rate, AudioSessionClient, PcmParams, PCM_FMT_S16};
use libcluu::boot::{process_info, TOKEN_CLOCK, TOKEN_IPC, TOKEN_SPACE};
use libcluu::fs::client::VfsClient;
use libcluu::ipc::{
    self, parse_message, COMP_FRAME_READY_LABEL, COMP_INPUT_FORWARD_LABEL,
    COMP_WIN_DAMAGE_LABEL, COMP_WIN_DESTROY_LABEL, COMP_WIN_QUERY_SCREEN_LABEL,
    COMP_WIN_REGISTER_LABEL, COMP_WIN_REGISTER_REPLY, COMP_WIN_SET_PIXEL_REGION_LABEL,
    COMP_WIN_FLAG_FULLSCREEN,
};
use libcluu::pixel_region::{PixelRegion, GLYPH_H, GLYPH_W};
use libcluu::registry;
use libcluu::syscall::{
    self, endpoint_create, ipc_recv_any, MAP_FRAME_TOKEN,
};
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, Error, Result};

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const DG_RESX: usize = 640;
const DG_RESY: usize = 400;

const DOOM_AUDIO_RATE: u32 = 11025;
const AUDIO_SCRATCH_VA: usize = 0x7000_0000;
const AUDIO_PERIOD_BYTES: usize = 4096;
const AUDIO_RING_SLOTS: usize = 8;

const SHM_VA: usize = 0xD000_0000;
const FLAGS_USER_RW: usize = 0x07;

const KEY_QUEUE_SIZE: usize = 64;

const EXT_UP: u8 = 1;
const EXT_DOWN: u8 = 2;
const EXT_LEFT: u8 = 3;
const EXT_RIGHT: u8 = 4;

static INITIALIZED: AtomicBool = AtomicBool::new(false);
static START_TICKS: AtomicU64 = AtomicU64::new(0);
static CLOCK_TOKEN: AtomicU64 = AtomicU64::new(0);

// The C engine defines and mallocs this pointer (doomgeneric.c:7).
// We read from it in DG_DrawFrame.
extern "C" {
    static mut DG_ScreenBuffer: *mut u32;
}

struct DoomState {
    win_id: u64,
    comp_ep: usize,
    my_ep: usize,
    region: Option<PixelRegion>,
    granted_pixel_w: usize,
    granted_pixel_h: usize,
    audio: Option<AudioSessionClient>,
    key_queue: [u16; KEY_QUEUE_SIZE],
    key_read: usize,
    key_write: usize,
    quit_requested: bool,
}

static mut STATE: Option<DoomState> = None;

impl DoomState {
    fn take() -> &'static mut Self {
        unsafe { STATE.as_mut().expect("doom-cluu: not initialized") }
    }
}

// ============================================================================
// doomgeneric platform API
// ============================================================================

#[no_mangle]
pub extern "C" fn DG_Init() {
    if INITIALIZED.swap(true, Ordering::SeqCst) {
        return;
    }

    let _ = debug_print("doom-cluu: DG_Init");

    let state = match init_state() {
        Ok(s) => s,
        Err(e) => {
            let _ = debug_print(&format!("doom-cluu: init failed {:?}", e));
            panic!("doom-cluu: init failed");
        }
    };

    unsafe { STATE = Some(state) };
}

#[no_mangle]
pub extern "C" fn DG_DrawFrame() {
    let state = DoomState::take();

    drain_keyboard(state);

    if let Some(ref mut region) = state.region {
        let src = unsafe { DG_ScreenBuffer };
        if src.is_null() {
            return;
        }
        scale_nearest_to_region(
            src,
            DG_RESX,
            DG_RESY,
            region,
            state.granted_pixel_w,
            state.granted_pixel_h,
        );
    }

    let dmg = Message::new(
        COMP_WIN_DAMAGE_LABEL,
        [state.win_id as usize, 0, 0, 0xFFFF, 0xFFFF, 0],
        5,
    );
    let _ = ipc::send(state.comp_ep, &dmg, IpcFlags::empty());
}

#[no_mangle]
pub extern "C" fn DG_SleepMs(ms: u32) {
    let _ = libcluu::posix::usleep(ms * 1000);
}

#[no_mangle]
pub extern "C" fn DG_GetTicksMs() -> u32 {
    let clock_tok = CLOCK_TOKEN.load(Ordering::Relaxed) as usize;
    let now = libcluu::syscall::clock_now(clock_tok).unwrap_or(0);
    let start = START_TICKS.load(Ordering::Relaxed);
    ((now - start) / 1_000_000) as u32
}

#[no_mangle]
pub extern "C" fn DG_GetKey(pressed: *mut i32, key: *mut u8) -> i32 {
    let state = DoomState::take();

    drain_keyboard(state);

    if state.key_read == state.key_write {
        return 0;
    }

    let entry = state.key_queue[state.key_read];
    state.key_read = (state.key_read + 1) % KEY_QUEUE_SIZE;

    unsafe {
        *pressed = ((entry >> 8) & 1) as i32;
        *key = (entry & 0xFF) as u8;
    }
    1
}

#[no_mangle]
pub extern "C" fn DG_SetWindowTitle(title: *const i8) {
    let _ = title;
}

// ============================================================================
// Init
// ============================================================================

fn init_state() -> Result<DoomState> {
    let info = process_info();
    let clock_tok = info.tokens[TOKEN_CLOCK];
    CLOCK_TOKEN.store(clock_tok as u64, Ordering::Relaxed);

    let now = libcluu::syscall::clock_now(clock_tok).unwrap_or(0);
    START_TICKS.store(now, Ordering::Relaxed);

    let _ = registry::init("doom");
    let _ = registry::register_default_outputs();

    let ipc_cap = info.tokens[TOKEN_IPC];
    let my_ep = endpoint_create(ipc_cap)?;

    let comp_ep = registry::lookup_service("compositor:client")
        .ok_or(Error::NotFound)?;

    let content_w: u16 = 160;
    let content_h: u16 = 50;
    let req_w: u16 = content_w + 4;
    let req_h: u16 = content_h + 2;

    let _ = debug_print(&format!(
        "doom-cluu: requesting {}x{} cells", req_w, req_h
    ));

    let title = b"DOOM";
    let req = Message::new(
        COMP_WIN_REGISTER_LABEL,
        [title.len(), req_w as usize, req_h as usize, my_ep, 0, 0],
        4,
    );
    let mut reply = Message::new(0, [0; 6], 0);
    ipc::call_with_payload(comp_ep, &req, title, &mut reply)?;

    if reply.tag.label != COMP_WIN_REGISTER_REPLY {
        return Err(Error::InvalidArgument);
    }
    let win_id = reply.words[0] as u64;
    let shm_token = reply.words[1];
    let gw = reply.words[2] as u16;
    let gh = reply.words[3] as u16;
    let err = reply.words[4];
    if err != 0 {
        return Err(Error::PermissionDenied);
    }

    let _ = debug_print(&format!(
        "doom-cluu: window {} granted {}x{} cells",
        win_id, gw, gh
    ));

    let cells_bytes = gw as usize * gh as usize * 8;
    let total = (32 + cells_bytes + 0xFFF) & !0xFFF;
    let num_pages = total / 0x1000;
    let space = info.tokens[TOKEN_SPACE];
    let _ = syscall::space_map_range(
        space,
        SHM_VA,
        shm_token,
        FLAGS_USER_RW | MAP_FRAME_TOKEN,
        num_pages,
        0,
    );

    let fr = Message::new(COMP_FRAME_READY_LABEL, [win_id as usize, 0, 0, 0, 0, 0], 1);
    let _ = ipc::send(my_ep, &fr, IpcFlags::empty());

    let region = PixelRegion::alloc(content_w, content_h)?;
    let _ = debug_print(&format!(
        "doom-cluu: pixel region {}x{} px",
        region.pixel_w, region.pixel_h
    ));

    let pr_msg = Message::new(
        COMP_WIN_SET_PIXEL_REGION_LABEL,
        [
            win_id as usize,
            2,
            1,
            content_w as usize,
            content_h as usize,
            region.frame_token() as usize,
        ],
        6,
    );
    let _ = ipc::send(comp_ep, &pr_msg, IpcFlags::empty());

    let audio = open_audio(space).ok();
    if audio.is_some() {
        let _ = debug_print("doom-cluu: audio session opened");
    } else {
        let _ = debug_print("doom-cluu: no audio (running silent)");
    }

    Ok(DoomState {
        win_id,
        comp_ep,
        my_ep,
        region: Some(region),
        granted_pixel_w: content_w as usize * GLYPH_W,
        granted_pixel_h: content_h as usize * GLYPH_H,
        audio,
        key_queue: [0; KEY_QUEUE_SIZE],
        key_read: 0,
        key_write: 0,
        quit_requested: false,
    })
}

fn open_audio(space_token: usize) -> Result<AudioSessionClient> {
    let snd_ep = registry::subscribe_output("snddev", "main")?;
    let params = PcmParams {
        format: PCM_FMT_S16,
        rate: hz_to_rate(DOOM_AUDIO_RATE),
        channels: 1,
    };
    let audio = AudioSessionClient::open(snd_ep, params)?;

    syscall::space_map_range(
        space_token,
        AUDIO_SCRATCH_VA,
        0,
        0x03,
        AUDIO_RING_SLOTS,
        0,
    )?;

    for i in 0..AUDIO_RING_SLOTS {
        syscall::space_grant(
            space_token,
            audio.driver_space_token,
            AUDIO_SCRATCH_VA + i * AUDIO_PERIOD_BYTES,
            audio.grant_target_va + i * AUDIO_PERIOD_BYTES,
            0,
        )?;
    }

    Ok(audio)
}

// ============================================================================
// Keyboard: drain COMP_INPUT_FORWARD → key queue
// ============================================================================

fn drain_keyboard(state: &mut DoomState) {
    let tokens = [state.my_ep];
    let mut buf = [0u8; 256];

    loop {
        match ipc_recv_any(&tokens, &mut buf, 0) {
            Ok((_idx, len)) => {
                if let Some((msg, _)) = parse_message(&buf[..len]) {
                    if msg.tag.label == COMP_INPUT_FORWARD_LABEL {
                        handle_key_event(state, &msg);
                    }
                }
            }
            Err(_) => break,
        }
    }
}

fn handle_key_event(state: &mut DoomState, msg: &Message) {
    let ascii = msg.words[1] as u8;
    let scancode = msg.words[3] as u8;
    let extended = msg.words[4] as u8;
    let kind = msg.words[5] as u32;

    if kind == 99 {
        let destroy = Message::new(
            COMP_WIN_DESTROY_LABEL,
            [state.win_id as usize, 0, 0, 0, 0, 0],
            1,
        );
        let _ = ipc::send(state.comp_ep, &destroy, IpcFlags::empty());
        state.quit_requested = true;
        return;
    }

    let pressed = kind == 0;
    let doom_key = map_to_doom_key(ascii, scancode, extended);
    if doom_key == 0 {
        return;
    }

    let next_write = (state.key_write + 1) % KEY_QUEUE_SIZE;
    if next_write == state.key_read {
        return;
    }
    let pressed_bit = if pressed { 1u16 } else { 0u16 };
    state.key_queue[state.key_write] = (pressed_bit << 8) | doom_key as u16;
    state.key_write = next_write;
}

fn map_to_doom_key(ascii: u8, scancode: u8, extended: u8) -> u8 {
    if extended != 0 {
        return match extended {
            EXT_UP => 0xAD,
            EXT_DOWN => 0xAF,
            EXT_LEFT => 0xAC,
            EXT_RIGHT => 0xAE,
            _ => 0,
        };
    }

    let sc = scancode & 0x7F;
    match sc {
        0x01 => return 27,
        0x1C => return 13,
        0x39 => return 0x20,
        0x0E => return 0x7F,
        0x0F => return 9,
        0x1D => return 0x80 | 0x1D,
        0x2A | 0x36 => return 0x80 | 0x36,
        0x38 => return 0x80 | 0x38,
        _ => {}
    }

    if ascii != 0 {
        return ascii.to_ascii_uppercase();
    }

    0
}

// ============================================================================
// WAD bulk loader: grant-based zero-copy file read
// ============================================================================

const WAD_VA: usize = 0xE000_0000;
const WAD_SCRATCH: usize = 0xD000_0000;
const WAD_CHUNK: usize = 64 * 1024;

#[no_mangle]
pub extern "C" fn cluu_wad_load(path: *const i8, out_len: *mut u64) -> *mut u8 {
    let path_str = unsafe {
        let mut len = 0;
        while *path.add(len) != 0 {
            len += 1;
        }
        core::str::from_utf8_unchecked(core::slice::from_raw_parts(path as *const u8, len))
    };

    let _ = debug_print(&format!("doom-cluu: bulk-loading WAD {}", path_str));

    let result = wad_load_inner(path_str);
    match result {
        Ok((ptr, len)) => {
            unsafe { *out_len = len as u64; }
            let _ = debug_print(&format!("doom-cluu: WAD loaded {} bytes via grant", len));
            ptr as *mut u8
        }
        Err(e) => {
            let _ = debug_print(&format!("doom-cluu: WAD bulk load failed {:?}", e));
            core::ptr::null_mut()
        }
    }
}

fn wad_load_inner(path: &str) -> Result<(*const u8, usize)> {
    let vfs_ep = registry::subscribe_output("vfs", "main")?;
    let client_id = registry::control_endpoint();
    let vfs = VfsClient::new(vfs_ep, client_id);

    let file = vfs.open(path)?;
    let file_size = file.size;

    let info = process_info();
    let space_token = info.tokens[TOKEN_SPACE];

    let _ = debug_print(&format!("doom-cluu: WAD size {} bytes, reading in 64KB chunks", file_size));

    let num_pages = (file_size + 0xFFF) / 0x1000;
    syscall::space_map_range(space_token, WAD_VA, 0, 0x03, num_pages, 0)?;

    let scratch_pages = (WAD_CHUNK + 0xFFF) / 0x1000;
    syscall::space_map_range(space_token, WAD_SCRATCH, 0, 0x03, scratch_pages, 0)?;

    let mut offset = 0usize;
    while offset < file_size {
        let want = WAD_CHUNK.min(file_size - offset);
        let grant = vfs.read_grant(file, offset, want, space_token, WAD_SCRATCH)?;

        let src = unsafe {
            core::slice::from_raw_parts((WAD_SCRATCH + grant.offset) as *const u8, grant.len)
        };
        unsafe {
            core::ptr::copy_nonoverlapping(
                src.as_ptr(),
                (WAD_VA + offset) as *mut u8,
                grant.len,
            );
        }

        offset += grant.len;
        if (offset / (1024 * 1024)) != ((offset - grant.len) / (1024 * 1024)) {
            let _ = debug_print(&format!("doom-cluu: WAD loaded {}MB / {}MB",
                offset / (1024*1024), file_size / (1024*1024)));
        }
    }

    vfs.close(file)?;

    let ptr = WAD_VA as *const u8;
    Ok((ptr, file_size))
}

// ============================================================================
// Scaling: DG_ScreenBuffer (640x400 XRGB8888) -> PixelRegion
// ============================================================================

fn scale_nearest_to_region(
    src: *mut u32,
    src_w: usize,
    src_h: usize,
    region: &mut PixelRegion,
    dst_w: usize,
    dst_h: usize,
) {
    let dst_ptr = region.as_ptr();

    for dy in 0..dst_h {
        let sy = ((dy * src_h) / dst_h).min(src_h - 1);
        let src_row = unsafe { src.add(sy * src_w) };
        let dst_row = unsafe { dst_ptr.add(dy * dst_w) };
        for dx in 0..dst_w {
            let sx = ((dx * src_w) / dst_w).min(src_w - 1);
            let pixel = unsafe { *src_row.add(sx) };
            unsafe { core::ptr::write_volatile(dst_row.add(dx), pixel); }
        }
    }
}
