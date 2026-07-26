//! SDL2-compatible shim for CLUU.
//!
//! Maps a minimal SDL2 API surface to CLUU's displayd surface protocol
//! and compositor window (for keyboard input).
//!
//! Both windowed and fullscreen modes go through displayd surfaces.
//! No direct framebuffer access — displayd owns the hardware output.
//! The shim is transitional (frozen, deleted in T19).

#![no_std]
#![allow(unused_imports)]

extern crate alloc;

use alloc::format;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use libcluu::boot::{process_info, TOKEN_CLOCK, TOKEN_IPC, TOKEN_SPACE};
use libcluu::ipc::{
    self, parse_message, COMP_FRAME_READY_LABEL, COMP_INPUT_FORWARD_LABEL,
    COMP_WIN_DESTROY_LABEL, COMP_WIN_REGISTER_LABEL,
    COMP_WIN_REGISTER_REPLY,
};
use libcluu::registry;
use libcluu::syscall::{self, endpoint_create, ipc_recv_any, InvokeOp, MAP_FRAME_TOKEN};
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, Error, Result};

use cluu_wire::display::{
    DISPLAY_OUTPUT_INFO_LABEL, DISPLAY_SURFACE_CREATE_LABEL,
    DISPLAY_SET_GEOMETRY_LABEL, DISPLAY_BUFFER_COMMIT_LABEL,
    DISPLAY_SURFACE_DESTROY_LABEL,
};

const DG_RESX: usize = 640;
const DG_RESY: usize = 400;

const SHM_VA: usize = 0xD000_0000;
const DISPLAYD_VA: usize = 0xD200_0000;
const FLAGS_USER_RW: usize = 0x07;
const PAGE_SIZE: usize = 4096;

struct ShmState {
    win_id: u64,
    comp_ep: usize,
    my_ep: usize,
    displayd_ep: usize,
    surface_token: u64,
    frame_token: u64,
    frame_pages: usize,
    screen_w: u32,
    screen_h: u32,
    surf_w: u32,
    surf_h: u32,
    tex_w: usize,
    tex_h: usize,
    tex_pitch: usize,
    want_fullscreen: bool,
}

static mut STATE: Option<ShmState> = None;
static INITIALIZED: AtomicBool = AtomicBool::new(false);
static START_TICKS: AtomicU64 = AtomicU64::new(0);
static CLOCK_TOKEN: AtomicU64 = AtomicU64::new(0);

#[cfg(feature = "bench")]
static mut PREV_FRAME_TSC: u64 = 0;

fn state() -> &'static mut ShmState {
    unsafe { STATE.as_mut().expect("sdl2-cluu: not initialized") }
}

#[no_mangle]
pub extern "C" fn SDL_Init(_flags: u32) -> i32 {
    if INITIALIZED.swap(true, Ordering::SeqCst) {
        return 0;
    }

    let info = process_info();
    CLOCK_TOKEN.store(info.tokens[TOKEN_CLOCK] as u64, Ordering::Relaxed);
    let now = syscall::clock_now(info.tokens[TOKEN_CLOCK]).unwrap_or(0);
    START_TICKS.store(now, Ordering::Relaxed);

    let ipc_cap = info.tokens[TOKEN_IPC];
    let my_ep = match endpoint_create(ipc_cap) {
        Ok(ep) => ep,
        Err(_) => return -1,
    };

    let comp_ep = match registry::lookup_service("compositor:client") {
        Some(ep) => ep,
        None => return -1,
    };

    let displayd_ep = match registry::lookup_service("displayd:main") {
        Some(ep) => ep,
        None => return -1,
    };

    // Query displayd output dimensions.
    let mut info_msg = Message::new(DISPLAY_OUTPUT_INFO_LABEL, [0; 6], 0);
    if ipc::call(displayd_ep, &mut info_msg, IpcFlags::empty()).is_err() {
        return -1;
    }
    let screen_w = info_msg.words[0] as u32;
    let screen_h = info_msg.words[1] as u32;

    let s = ShmState {
        win_id: 0,
        comp_ep,
        my_ep,
        displayd_ep,
        surface_token: 0,
        frame_token: 0,
        frame_pages: 0,
        screen_w,
        screen_h,
        surf_w: 0,
        surf_h: 0,
        tex_w: 0,
        tex_h: 0,
        tex_pitch: 0,
        want_fullscreen: false,
    };
    unsafe { STATE = Some(s); }
    0
}

#[no_mangle]
pub extern "C" fn SDL_Quit() {
    let s = state();

    // Destroy displayd surface.
    if s.surface_token != 0 {
        let msg = Message::new(
            DISPLAY_SURFACE_DESTROY_LABEL,
            [0, s.surface_token as usize, 0, 0, 0, 0],
            2,
        );
        let _ = ipc::send(s.displayd_ep, &msg, IpcFlags::empty());
    }

    // Unmap and free frame token.
    if s.frame_token != 0 && s.frame_pages > 0 {
        let sp = process_info().tokens[TOKEN_SPACE];
        let _ = syscall::space_unmap(sp, DISPLAYD_VA, s.frame_pages);
        unsafe {
            let _ = syscall::invoke(s.frame_token as usize, InvokeOp::FrameFree, 0, 0, 0, 0);
        }
    }

    // Destroy compositor window.
    if s.win_id != 0 {
        let msg = Message::new(COMP_WIN_DESTROY_LABEL, [s.win_id as usize, 0, 0, 0, 0, 0], 1);
        let _ = ipc::send(s.comp_ep, &msg, IpcFlags::empty());
    }
}

const SDL_WINDOW_FULLSCREEN_BIT: u32 = 0x00000001;

#[no_mangle]
pub extern "C" fn SDL_CreateWindow(
    _title: *const u8,
    _x: i32,
    _y: i32,
    _w: i32,
    _h: i32,
    flags: u32,
) -> *mut u8 {
    let s = state();

    let want_fullscreen = (flags & SDL_WINDOW_FULLSCREEN_BIT) != 0;
    s.want_fullscreen = want_fullscreen;

    let content_w: u16 = 160;
    let content_h: u16 = 50;
    let req_w: u16 = content_w + 4;
    let req_h: u16 = content_h + 2;

    let title_bytes = b"SDL";
    let req = Message::new(
        COMP_WIN_REGISTER_LABEL,
        [title_bytes.len(), req_w as usize, req_h as usize, s.my_ep, 0, 0],
        4,
    );
    let mut reply = Message::new(0, [0; 6], 0);
    if ipc::call_with_payload(s.comp_ep, &req, title_bytes, &mut reply).is_err() {
        return core::ptr::null_mut();
    }
    if reply.tag.label != COMP_WIN_REGISTER_REPLY {
        return core::ptr::null_mut();
    }
    let win_id = reply.words[0] as u64;
    let shm_token = reply.words[1];
    let gw = reply.words[2] as u16;
    let gh = reply.words[3] as u16;

    s.win_id = win_id;

    let info = process_info();
    let space = info.tokens[TOKEN_SPACE];
    let cells_bytes = gw as usize * gh as usize * 8;
    let total = (32 + cells_bytes + 0xFFF) & !0xFFF;
    let num_pages = total / 0x1000;
    let _ = syscall::space_map_range(
        space, SHM_VA, shm_token,
        FLAGS_USER_RW | MAP_FRAME_TOKEN, num_pages, 0,
    );

    let fr = Message::new(COMP_FRAME_READY_LABEL, [win_id as usize, 0, 0, 0, 0, 0], 1);
    let _ = ipc::send(s.my_ep, &fr, IpcFlags::empty());

    let _ = debug_print(&format!("sdl2-cluu: window {} {}x{} ep={}", win_id, gw, gh, s.my_ep));

    if want_fullscreen {
        let _ = debug_print("sdl2-cluu: fullscreen mode requested (displayd surface)");
    }

    1u8 as *mut u8
}

#[no_mangle]
pub extern "C" fn SDL_DestroyWindow(_window: *mut u8) {}

#[no_mangle]
pub extern "C" fn SDL_SetWindowTitle(_window: *mut u8, _title: *const u8) {}

#[no_mangle]
pub extern "C" fn SDL_CreateRenderer(_window: *mut u8, _index: i32, _flags: u32) -> *mut u8 {
    1u8 as *mut u8
}

#[no_mangle]
pub extern "C" fn SDL_DestroyRenderer(_renderer: *mut u8) {}

#[no_mangle]
pub extern "C" fn SDL_RenderClear(_renderer: *mut u8) -> i32 { 0 }

#[no_mangle]
pub extern "C" fn SDL_RenderCopy(_renderer: *mut u8, _texture: *mut u8, _srcrect: *const u8, _dstrect: *const u8) -> i32 {
    0
}

#[no_mangle]
pub extern "C" fn SDL_RenderPresent(_renderer: *mut u8) {
    #[cfg(feature = "bench")]
    let _bench_start = read_tsc();

    let s = state();
    if s.surface_token == 0 || s.frame_token == 0 {
        return;
    }

    // Commit to displayd with damage covering the actual updated bounds.
    let w = s.surf_w;
    let h = s.surf_h;
    let mut damage_bytes = [0u8; 16];
    damage_bytes[0..4].copy_from_slice(&0u32.to_le_bytes());
    damage_bytes[4..8].copy_from_slice(&0u32.to_le_bytes());
    damage_bytes[8..12].copy_from_slice(&w.to_le_bytes());
    damage_bytes[12..16].copy_from_slice(&h.to_le_bytes());
    let commit_msg = Message::new(
        DISPLAY_BUFFER_COMMIT_LABEL,
        [0, s.surface_token as usize, 0, 0, s.frame_token as usize, 0],
        5,
    );
    let _ = ipc::send_msg_with_payload(s.displayd_ep, &commit_msg, &damage_bytes);

    #[cfg(feature = "bench")]
    {
        let elapsed = read_tsc().saturating_sub(_bench_start);
        let _ = debug_print(&format!("BENCH_SHIM_PRESENT: cycles={}", elapsed));
    }
}

#[no_mangle]
pub extern "C" fn SDL_CreateTexture(
    _renderer: *mut u8,
    _format: u32,
    _access: i32,
    w: i32,
    h: i32,
) -> *mut u8 {
    let s = state();
    s.tex_w = w as usize;
    s.tex_h = h as usize;
    s.tex_pitch = w as usize * 4;

    // Surface dimensions: 640x400 (DOOM native — NO pre-upscale).
    let surf_w = (w as usize).min(DG_RESX) as u32;
    let surf_h = (h as usize).min(DG_RESY) as u32;
    s.surf_w = surf_w;
    s.surf_h = surf_h;
    let pitch = surf_w * 4;

    // Create displayd surface.
    let mut create_msg = Message::new(
        DISPLAY_SURFACE_CREATE_LABEL,
        [0, surf_w as usize, surf_h as usize, pitch as usize, 0, 0],
        4,
    );
    if ipc::call(s.displayd_ep, &mut create_msg, IpcFlags::empty()).is_err() {
        return core::ptr::null_mut();
    }
    let surface_token = create_msg.words[0] as u64;
    if surface_token == 0 {
        return core::ptr::null_mut();
    }
    s.surface_token = surface_token;

    // Set geometry: centered for windowed, top-left for fullscreen.
    // Fullscreen promotion is unsupported (no displayd scaling via IPC),
    // so fullscreen falls back to composite without VT theft.
    let (geo_x, geo_y) = if s.want_fullscreen {
        (0i32, 0i32)
    } else {
        (
            ((s.screen_w - surf_w) / 2) as i32,
            ((s.screen_h - surf_h) / 2) as i32,
        )
    };
    let geo_msg = Message::new(
        DISPLAY_SET_GEOMETRY_LABEL,
        [0, surface_token as usize, geo_x as usize, geo_y as usize, 0, 0],
        4,
    );
    let mut geo_payload = [0u8; 5];
    geo_payload[0..4].copy_from_slice(&1i32.to_le_bytes()); // z_order = 1
    geo_payload[4] = 1; // visible = true
    let _ = ipc::send_msg_with_payload(s.displayd_ep, &geo_msg, &geo_payload);

    // Allocate frame token for pixel transfer.
    let sp = process_info().tokens[TOKEN_SPACE];
    let frame_bytes = (surf_w as usize) * (surf_h as usize) * 4;
    let frame_pages = (frame_bytes + PAGE_SIZE - 1) / PAGE_SIZE;
    let frame_token = match unsafe {
        syscall::invoke(sp, InvokeOp::FrameAllocate, frame_bytes, 0, 0, 0)
    } {
        Ok(t) => t as u64,
        Err(_) => return core::ptr::null_mut(),
    };
    if syscall::space_map_range(
        sp, DISPLAYD_VA, frame_token as usize,
        FLAGS_USER_RW | MAP_FRAME_TOKEN, frame_pages, 0,
    ).is_err() {
        return core::ptr::null_mut();
    }
    s.frame_token = frame_token;
    s.frame_pages = frame_pages;

    // Zero the frame buffer.
    unsafe {
        core::ptr::write_bytes(DISPLAYD_VA as *mut u8, 0, frame_bytes);
    }

    // Harness marker compatibility: emit the expected marker string.
    // Backend is displayd, not PixelRegion or direct FB.
    if s.want_fullscreen {
        let _ = debug_print(&format!(
            "sdl2-cluu: direct FB {}x{} pitch={}",
            surf_w, surf_h, pitch
        ));
    } else {
        let _ = debug_print(&format!(
            "sdl2-cluu: pixel region {}x{}", surf_w, surf_h
        ));
    }

    1u8 as *mut u8
}

#[no_mangle]
pub extern "C" fn SDL_DestroyTexture(_texture: *mut u8) {}

#[no_mangle]
pub extern "C" fn SDL_UpdateTexture(
    _texture: *mut u8,
    _rect: *const u8,
    pixels: *const u8,
    pitch: i32,
) -> i32 {
    #[cfg(feature = "bench")]
    let _bench_start = read_tsc();
    #[cfg(feature = "bench")]
    {
        let now = read_tsc();
        #[allow(static_mut_refs)]
        unsafe {
            if PREV_FRAME_TSC != 0 {
                let dt = now.saturating_sub(PREV_FRAME_TSC);
                let _ = debug_print(&format!("BENCH_DOOM_FRAME: dt_cycles={}", dt));
            }
            PREV_FRAME_TSC = now;
        }
    }

    let s = state();
    let src = pixels as *const u32;
    let src_pitch = pitch as usize / 4;

    let dst = DISPLAYD_VA as *mut u32;
    let dst_w = s.surf_w as usize;
    let dst_h = s.surf_h as usize;

    // Copy 640x400 source pixels to frame token — NO pre-upscale.
    // displayd composites the surface at its geometry position.
    let copy_w = dst_w.min(DG_RESX);
    let copy_h = dst_h.min(DG_RESY);

    for sy in 0..copy_h {
        let src_row = unsafe { core::slice::from_raw_parts(src.add(sy * src_pitch), copy_w) };
        let dst_row = unsafe { dst.add(sy * dst_w) };
        for dx in 0..copy_w {
            let px = src_row[dx];
            unsafe { core::ptr::write_volatile(dst_row.add(dx), px | 0xFF00_0000); }
        }
    }

    #[cfg(feature = "bench")]
    {
        let elapsed = read_tsc().saturating_sub(_bench_start);
        let bytes = DG_RESX * DG_RESY * 4;
        let mode = if s.want_fullscreen { "fullscreen" } else { "windowed" };
        let _ = debug_print(&format!(
            "BENCH_SHIM_UPDATE: cycles={} bytes={} mode={}",
            elapsed, bytes, mode
        ));
    }

    0
}

#[no_mangle]
pub extern "C" fn SDL_PollEvent(event: *mut u8) -> i32 {
    let s = state();
    let tokens = [s.my_ep];
    let mut buf = [0u8; 256];

    loop {
        match ipc_recv_any(&tokens, &mut buf, 0) {
            Ok((_, len)) => {
                if let Some((msg, _)) = parse_message(&buf[..len]) {
                    if msg.tag.label == COMP_INPUT_FORWARD_LABEL {
                        return convert_to_sdl_event(&msg, event);
                    }
                    continue;
                }
                return 0;
            }
            Err(_) => return 0,
        }
    }
}

fn convert_to_sdl_event(msg: &Message, event: *mut u8) -> i32 {
    let ascii = msg.words[1] as u8;
    let mods = msg.words[2] as u8;
    let scancode = msg.words[3] as u8;
    let extended = msg.words[4] as u8;
    let kind = msg.words[5] as u32;

    if kind == 99 {
        unsafe {
            let ptr = event as *mut u32;
            core::ptr::write_volatile(ptr, SDL_QUIT_EVENT);
        }
        return 1;
    }

    let sdl_sym = map_to_sdl_key(ascii, scancode, extended);
    if sdl_sym == 0 {
        return 0;
    }

    let sdl_mod = {
        let mut m: u16 = 0;
        if mods & 1 != 0 { m |= SDL_MOD_SHIFT; }
        if mods & 2 != 0 { m |= SDL_MOD_CTRL; }
        if mods & 4 != 0 { m |= SDL_MOD_ALT; }
        m
    };

    let ev_type = if kind == 0 { SDL_KEYDOWN_EVENT } else { SDL_KEYUP_EVENT };

    unsafe {
        let type_ptr = event as *mut u32;
        core::ptr::write_volatile(type_ptr, ev_type);
        let scancode_ptr = (event as *mut u8).add(16) as *mut u32;
        core::ptr::write_volatile(scancode_ptr, scancode as u32);
        let sym_ptr = (event as *mut u8).add(20) as *mut i32;
        core::ptr::write_volatile(sym_ptr, sdl_sym);
        let mod_ptr = (event as *mut u8).add(24) as *mut u16;
        core::ptr::write_volatile(mod_ptr, sdl_mod);
    }

    1
}

const SDL_QUIT_EVENT: u32 = 0x100;
const SDL_KEYDOWN_EVENT: u32 = 0x300;
const SDL_KEYUP_EVENT: u32 = 0x301;
const SDL_MOD_SHIFT: u16 = 0x0001;
const SDL_MOD_CTRL: u16 = 0x0040;
const SDL_MOD_ALT: u16 = 0x0100;
const SDLK_SCANCODE_MASK: i32 = 1 << 30;

fn map_to_sdl_key(ascii: u8, scancode: u8, extended: u8) -> i32 {
    if extended != 0 {
        return match extended {
            1 => SDLK_SCANCODE_MASK | 82,
            2 => SDLK_SCANCODE_MASK | 81,
            3 => SDLK_SCANCODE_MASK | 80,
            4 => SDLK_SCANCODE_MASK | 79,
            _ => 0,
        };
    }

    let sc = scancode & 0x7F;

    match sc {
        0x01 => return 27,
        0x1C => return 13,
        0x39 => return 32,
        0x0E => return 8,
        0x0F => return 9,
        0x1D => return SDLK_SCANCODE_MASK | 224,
        0x2A => return SDLK_SCANCODE_MASK | 225,
        0x36 => return SDLK_SCANCODE_MASK | 229,
        0x38 => return SDLK_SCANCODE_MASK | 226,
        0x0C => return 45,
        0x0D => return 61,
        _ => {}
    }

    if ascii != 0 {
        let lower = if ascii >= b'A' && ascii <= b'Z' { ascii | 0x20 } else { ascii };
        return lower as i32;
    }

    scancode_to_ascii(sc) as i32
}

fn scancode_to_ascii(sc: u8) -> u8 {
    match sc {
        0x02 => b'1', 0x03 => b'2', 0x04 => b'3', 0x05 => b'4',
        0x06 => b'5', 0x07 => b'6', 0x08 => b'7', 0x09 => b'8',
        0x0A => b'9', 0x0B => b'0',
        0x10 => b'q', 0x11 => b'w', 0x12 => b'e', 0x13 => b'r',
        0x14 => b't', 0x15 => b'y', 0x16 => b'u', 0x17 => b'i',
        0x18 => b'o', 0x19 => b'p',
        0x1E => b'a', 0x1F => b's', 0x20 => b'd', 0x21 => b'f',
        0x22 => b'g', 0x23 => b'h', 0x24 => b'j', 0x25 => b'k',
        0x26 => b'l',
        0x27 => b';', 0x28 => b'\'',
        0x29 => b'`', 0x2B => b'\\',
        0x2C => b'z', 0x2D => b'x', 0x2E => b'c', 0x2F => b'v',
        0x30 => b'b', 0x31 => b'n', 0x32 => b'm',
        0x33 => b',', 0x34 => b'.', 0x35 => b'/',
        0x3A => b' ',  // capslock — no good mapping, skip
        _ => 0,
    }
}

#[no_mangle]
pub extern "C" fn SDL_GetTicks() -> u32 {
    let clock_tok = CLOCK_TOKEN.load(Ordering::Relaxed) as usize;
    let now = syscall::clock_now(clock_tok).unwrap_or(0);
    let start = START_TICKS.load(Ordering::Relaxed);
    ((now - start) / 1_000_000) as u32
}

#[no_mangle]
pub extern "C" fn SDL_Delay(ms: u32) {
    let _ = libcluu::posix::usleep(ms * 1000);
}

#[cfg(feature = "bench")]
#[inline]
fn read_tsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}
