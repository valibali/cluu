//! SDL2-compatible shim for CLUU.
//!
//! Maps a minimal SDL2 API surface to CLUU's compositor/PixelRegion,
//! direct framebuffer, keyboard input, and timer APIs.
//!
//! Supports both fullscreen (direct FB) and windowed (compositor PixelRegion).

#![no_std]
#![allow(unused_imports)]

extern crate alloc;

use alloc::format;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use libcluu::boot::{process_info, TOKEN_CLOCK, TOKEN_IPC, TOKEN_SPACE};
use libcluu::ipc::{
    self, parse_message, COMP_FRAME_READY_LABEL, COMP_INPUT_FORWARD_LABEL,
    COMP_VT_ACTIVATE_LABEL, COMP_VT_DEACTIVATE_LABEL,
    COMP_WIN_DAMAGE_LABEL, COMP_WIN_DESTROY_LABEL, COMP_WIN_REGISTER_LABEL,
    COMP_WIN_REGISTER_REPLY, COMP_WIN_SET_PIXEL_REGION_LABEL,
};
use libcluu::pixel_region::{PixelRegion, GLYPH_H, GLYPH_W};
use libcluu::posix::framebuffer::{framebuffer_acquire, FramebufferInfo};
use libcluu::registry;
use libcluu::syscall::{self, endpoint_create, ipc_recv_any, MAP_FRAME_TOKEN};
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, Error, Result};

const DG_RESX: usize = 640;
const DG_RESY: usize = 400;

const SHM_VA: usize = 0xD000_0000;
const FLAGS_USER_RW: usize = 0x07;

struct ShmState {
    win_id: u64,
    comp_ep: usize,
    my_ep: usize,
    region: Option<PixelRegion>,
    fb: Option<FramebufferInfo>,
    tex_w: usize,
    tex_h: usize,
    tex_pitch: usize,
    fullscreen: bool,
    want_fullscreen: bool,
    fb_acquired: bool,
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

    let s = ShmState {
        win_id: 0,
        comp_ep,
        my_ep,
        region: None,
        fb: None,
        tex_w: 0,
        tex_h: 0,
        tex_pitch: 0,
        fullscreen: false,
        want_fullscreen: false,
        fb_acquired: false,
    };
    unsafe { STATE = Some(s); }
    0
}

#[no_mangle]
pub extern "C" fn SDL_Quit() {
    let s = state();
    if s.fullscreen {
        if let Some(ref fb) = s.fb {
            let _ = libcluu::posix::framebuffer::framebuffer_release(fb);
        }
        let act = Message::new(COMP_VT_ACTIVATE_LABEL, [0; 6], 0);
        let _ = ipc::send(s.comp_ep, &act, IpcFlags::empty());
    }
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
        let _ = debug_print("sdl2-cluu: fullscreen mode, FB acquire deferred to first frame");
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
    if !s.fullscreen {
        let dmg = Message::new(
            COMP_WIN_DAMAGE_LABEL,
            [s.win_id as usize, 0, 0, 0xFFFF, 0xFFFF, 0],
            5,
        );
        let _ = ipc::send(s.comp_ep, &dmg, IpcFlags::empty());
    }

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

    if !s.fullscreen {
        let content_w: u16 = 160;
        let content_h: u16 = 50;
        match PixelRegion::alloc(content_w, content_h) {
            Ok(region) => {
                let _ = debug_print(&format!(
                    "sdl2-cluu: pixel region {}x{}", region.pixel_w, region.pixel_h
                ));
                let pr_msg = Message::new(
                    COMP_WIN_SET_PIXEL_REGION_LABEL,
                    [
                        s.win_id as usize, 2, 1,
                        content_w as usize, content_h as usize,
                        region.frame_token() as usize,
                    ],
                    6,
                );
                let _ = ipc::send(s.comp_ep, &pr_msg, IpcFlags::empty());
                s.region = Some(region);
            }
            Err(_) => return core::ptr::null_mut(),
        }
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

    if s.want_fullscreen && !s.fb_acquired {
        let deact = Message::new(COMP_VT_DEACTIVATE_LABEL, [0; 6], 0);
        let _ = ipc::send(s.comp_ep, &deact, IpcFlags::empty());
        let _ = debug_print("sdl2-cluu: VT_DEACTIVATE sent (first frame)");

        let mut fb = FramebufferInfo {
            base: core::ptr::null_mut(),
            phys: 0, size: 0, width: 0, height: 0, pitch: 0, bpp: 0,
        };
        if framebuffer_acquire(&mut fb) == 0 {
            let _ = debug_print(&format!(
                "sdl2-cluu: direct FB {}x{} pitch={}",
                fb.width, fb.height, fb.pitch
            ));

            let fb_w = fb.width as usize;
            let fb_h = fb.height as usize;
            let fb_pitch = fb.pitch as usize / 4;
            let fb_ptr = fb.base as *mut u32;
            for y in 0..fb_h {
                unsafe {
                    core::ptr::write_bytes(
                        fb_ptr.add(y * fb_pitch),
                        0,
                        fb_w,
                    );
                }
            }

            s.fb = Some(fb);
            s.fullscreen = true;
        } else {
            let _ = debug_print("sdl2-cluu: FB acquire failed, staying compositor");
        }
        s.fb_acquired = true;
    }

    if let Some(ref fb) = s.fb {
        let fb_w = fb.width as usize;
        let fb_h = fb.height as usize;
        let fb_pitch = fb.pitch as usize / 4;
        let fb_ptr = fb.base as *mut u32;

        let scale_x = fb_w / DG_RESX;
        let scale_y = fb_h / DG_RESY;
        let dst_w = DG_RESX * scale_x;
        let dst_h = DG_RESY * scale_y;
        let offset_x = (fb_w - dst_w) / 2;
        let offset_y = (fb_h - dst_h) / 2;

        for sy in 0..DG_RESY {
            let src_row = unsafe { core::slice::from_raw_parts(src.add(sy * src_pitch), DG_RESX) };
            let dst_y0 = offset_y + sy * scale_y;
            let dst_row0 = unsafe { fb_ptr.add(dst_y0 * fb_pitch + offset_x) };
            let mut dx = 0;
            for &px in src_row {
                let val = px | 0xFF00_0000;
                for _ in 0..scale_x {
                    unsafe { core::ptr::write_volatile(dst_row0.add(dx), val); }
                    dx += 1;
                }
            }
            for dy in 1..scale_y {
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        dst_row0,
                        fb_ptr.add((dst_y0 + dy) * fb_pitch + offset_x),
                        dst_w,
                    );
                }
            }
        }
    } else if let Some(ref region) = s.region {
        let dst = region.as_ptr();
        let dst_w = region.pixel_w;
        let dst_h = region.pixel_h;

        let scale_x = dst_w / DG_RESX;
        let scale_y = dst_h / DG_RESY;

        if scale_x >= 1 && scale_y >= 1
            && dst_w == DG_RESX * scale_x
            && dst_h == DG_RESY * scale_y
        {
            for sy in 0..DG_RESY {
                let src_row = unsafe { core::slice::from_raw_parts(src.add(sy * src_pitch), DG_RESX) };
                let dst_y0 = sy * scale_y;
                let dst_row0 = unsafe { dst.add(dst_y0 * dst_w) };
                let mut dx = 0;
                for &px in src_row {
                    let val = px | 0xFF00_0000;
                    for _ in 0..scale_x {
                        unsafe { core::ptr::write_volatile(dst_row0.add(dx), val); }
                        dx += 1;
                    }
                }
                for dy in 1..scale_y {
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            dst_row0,
                            dst.add((dst_y0 + dy) * dst_w),
                            dst_w,
                        );
                    }
                }
            }
        } else {
            for dy in 0..dst_h {
                let sy = (dy * DG_RESY / dst_h).min(DG_RESY - 1);
                let src_row = unsafe { src.add(sy * src_pitch) };
                let dst_row = unsafe { dst.add(dy * dst_w) };
                for dx in 0..dst_w {
                    let sx = (dx * DG_RESX / dst_w).min(DG_RESX - 1);
                    let px = unsafe { *src_row.add(sx) };
                    unsafe { core::ptr::write_volatile(dst_row.add(dx), px | 0xFF00_0000); }
                }
            }
        }
    }

    #[cfg(feature = "bench")]
    {
        let elapsed = read_tsc().saturating_sub(_bench_start);
        let bytes = DG_RESX * DG_RESY * 4;
        let mode = if s.fullscreen { "fullscreen" } else { "windowed" };
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
