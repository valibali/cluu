//! x-nes NES emulator — CLUU platform backend.

#![cfg_attr(not(feature = "host-test"), no_std)]
#![cfg_attr(not(feature = "host-test"), no_main)]

extern crate alloc;

use alloc::vec::Vec;

use nes::rom::Rom;
use nes::Emulator;
use libcluu::args;
use libcluu::boot::{process_info, space_token, TOKEN_IPC};
use libcluu::fs::client::VfsClient;
use libcluu::input::ForwardedKey;
use audiod::ring::FrameRing;
use audiod::session::{AUDIOD_STREAM_CLOSE, AUDIOD_STREAM_OPEN};
use libcluu::audio_client::PCM_FMT_S16;
use libcluu::ipc::{
    parse_message, call_with_payload, send,
    COMP_INPUT_FORWARD_LABEL,
    COMP_WIN_DAMAGE_LABEL, COMP_WIN_REGISTER_LABEL, COMP_WIN_REGISTER_REPLY,
    COMP_WIN_SET_PIXEL_REGION_LABEL, COMP_WIN_DESTROY_LABEL,
    COMP_WIN_QUERY_SCREEN_LABEL, COMP_WIN_FLAG_FULLSCREEN, COMP_WIN_FLAG_NO_CHROME,
};
use cluu_wire::display::{
    DISPLAY_OUTPUT_INFO_LABEL, DISPLAY_SURFACE_CREATE_LABEL,
    DISPLAY_BUFFER_COMMIT_LABEL, DISPLAY_SET_GEOMETRY_LABEL,
    DISPLAY_SURFACE_DESTROY_LABEL,
};
use libcluu::pixel_region::PixelRegion;
use libcluu::registry;
use libcluu::syscall::{self};
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, Result};

const NES_W: usize = 256;
const NES_H: usize = 240;

const ROM_CHUNK: usize = 64 * 1024;
const FLAGS_USER_RW: usize = 0x07;

const KEY_UP: u8 = 1;
const KEY_DOWN: u8 = 2;
const KEY_LEFT: u8 = 3;
const KEY_RIGHT: u8 = 4;

const SCAN_Z: u8 = 0x2C;
const SCAN_X: u8 = 0x2D;
const SCAN_ENTER: u8 = 0x1C;
const SCAN_SPACE: u8 = 0x39;
const SCAN_ESC: u8 = 0x01;

const DISPLAYD_VA: usize = 0xD200_0000;
const AUDIO_RING_VA: usize = 0xD300_0000;
const AUDIO_RATE: u32 = 44100;
const AUDIO_CHANNELS: u8 = 1;
const AUDIO_PERIOD_BYTES: usize = 4096;
const PAGE_SIZE: usize = 4096;
const NTSC_FRAME_NS: u64 = 16_639_267;
const INPUT_DRAIN_LIMIT: usize = 32;

static PALETTE: [u32; 64] = [
    0xFF545454, 0xFF001E74, 0xFF080090, 0xFF440088, 0xFF7C005C, 0xFFA4001C, 0xFFA80000, 0xFF880000,
    0xFF5C2800, 0xFF284400, 0xFF005400, 0xFF005030, 0xFF004444, 0xFF000000, 0xFF000000, 0xFF000000,
    0xFFB4B4B4, 0xFF0C54C4, 0xFF303CD8, 0xFF742CC4, 0xFFAC1898, 0xFFD8004C, 0xFFDC0800, 0xFFBC3000,
    0xFF805000, 0xFF486800, 0xFF107800, 0xFF007444, 0xFF00686C, 0xFF000000, 0xFF000000, 0xFF000000,
    0xFFFCFCFC, 0xFF64B0FC, 0xFF9090FC, 0xFFC87CFC, 0xFFFC74FC, 0xFFFC74B8, 0xFFFC7870, 0xFFFC9838,
    0xFFF0B800, 0xFFBCD000, 0xFF84DC48, 0xFF58D878, 0xFF44D0A8, 0xFF000000, 0xFF000000, 0xFF000000,
    0xFFFCFCFC, 0xFFC0E4FC, 0xFFD0D4FC, 0xFFE8CCFC, 0xFFFCC8FC, 0xFFFCC4E0, 0xFFFCC8B8, 0xFFFCD4A0,
    0xFFFCE090, 0xFFE4EC88, 0xFFC8F090, 0xFFA8F0A8, 0xFFB0ECC8, 0xFF000000, 0xFF000000, 0xFF000000,
];

#[inline]
fn nes_color(index: u8) -> u32 {
    PALETTE[(index & 0x3F) as usize]
}

fn load_rom(path: &str) -> Result<Vec<u8>> {
    let _ = debug_print(&alloc::format!("xnes: loading ROM {}", path));

    let vfs_ep = registry::subscribe_output("vfs", "main")?;
    let client_id = registry::control_endpoint();
    let vfs = VfsClient::new(vfs_ep, client_id);

    let file = vfs.open(path)?;
    let file_size = file.size;
    let _ = debug_print(&alloc::format!("xnes: ROM size {} bytes", file_size));

    let sp = space_token();
    let scratch_bytes = ROM_CHUNK;
    let scratch_pages = (scratch_bytes + 0xFFF) / 0x1000;
    let frame_token = unsafe {
        syscall::invoke(sp, syscall::InvokeOp::FrameAllocate, scratch_bytes, 0, 0, 0)?
    };
    let scratch_va = syscall::space_map_auto(sp, frame_token, FLAGS_USER_RW, scratch_pages)?;

    let mut rom_data = Vec::new();
    rom_data.try_reserve(file_size).map_err(|_| libcluu::Error::OutOfMemory)?;
    rom_data.resize(file_size, 0u8);

    let mut offset = 0usize;
    while offset < file_size {
        let want = ROM_CHUNK.min(file_size - offset);
        let grant = vfs.read_grant(file, offset, want, sp, scratch_va)?;
        let src = unsafe {
            core::slice::from_raw_parts((scratch_va + grant.offset) as *const u8, grant.len)
        };
        rom_data[offset..offset + grant.len].copy_from_slice(src);
        offset += grant.len;
    }

    vfs.close(file)?;
    let _ = syscall::space_unmap(sp, scratch_va, scratch_pages);

    let _ = debug_print(&alloc::format!("xnes: ROM loaded {} bytes", rom_data.len()));
    Ok(rom_data)
}

struct CompWindow {
    comp_ep: usize,
    win_id: usize,
    my_ep: usize,
    cell_w: u16,
    cell_h: u16,
}

fn query_screen(comp_ep: usize) -> Result<(u16, u16)> {
    let req = Message::new(COMP_WIN_QUERY_SCREEN_LABEL, [0, 0, 0, 0, 0, 0], 0);
    let mut reply_buf = [0u8; 64];
    let bytes = libcluu::syscall::ipc_call(comp_ep, req.as_bytes(), &mut reply_buf)?;
    let (rmsg, _) = parse_message(&reply_buf[..bytes]).ok_or(libcluu::Error::InvalidState)?;
    if rmsg.tag.label != COMP_WIN_QUERY_SCREEN_LABEL {
        return Err(libcluu::Error::InvalidState);
    }
    Ok((rmsg.words[0] as u16, rmsg.words[1] as u16))
}

fn compute_aspect_fit(screen_cols: u16, screen_rows: u16) -> (u16, u16) {
    let avail_c = screen_cols.saturating_sub(2);
    let avail_r = screen_rows.saturating_sub(2);
    let fit_w = avail_c;
    let fit_h_w = (fit_w as u32 * 15 / 32) as u16;
    let fit_h = avail_r;
    let fit_w_h = (fit_h as u32 * 32 / 15) as u16;
    if fit_h_w <= avail_r {
        (fit_w, fit_h_w.max(3))
    } else {
        (fit_w_h.max(3), fit_h)
    }
}

fn create_window(title: &str, cell_w: u32, cell_h: u32, flags: u32) -> Result<CompWindow> {
    let info = process_info();
    let ipc_cap = info.tokens[TOKEN_IPC];
    let my_ep = syscall::endpoint_create(ipc_cap)?;

    let comp_ep = match registry::lookup_service("compositor:client") {
        Some(ep) => ep,
        None => {
            let _ = debug_print("xnes: no compositor:client in registry");
            return Err(libcluu::Error::InvalidState);
        }
    };

    let req = Message::new(
        COMP_WIN_REGISTER_LABEL,
        [title.len(), cell_w as usize, cell_h as usize, my_ep, flags as usize, 0],
        5,
    );
    let mut reply = Message::new(0, [0; 6], 0);
    call_with_payload(comp_ep, &req, title.as_bytes(), &mut reply)?;
    if reply.tag.label != COMP_WIN_REGISTER_REPLY {
        let _ = debug_print("xnes: unexpected register reply label");
        return Err(libcluu::Error::InvalidState);
    }
    let win_id = reply.words[0];
    let _shm_token = reply.words[1];
    let gw = reply.words[2] as u16;
    let gh = reply.words[3] as u16;
    let err = reply.words[4];
    if err != 0 {
        let _ = debug_print("xnes: compositor denied WIN_REGISTER");
        return Err(libcluu::Error::InvalidState);
    }

    let _ = debug_print(&alloc::format!(
        "xnes: window id={} {}x{} cells ({}x{} px)",
        win_id, gw, gh, gw as usize * 8, gh as usize * 16
    ));
    Ok(CompWindow { comp_ep, win_id, my_ep, cell_w: gw, cell_h: gh })
}

fn handle_key(emu: &mut Emulator, key: &ForwardedKey) -> bool {
    match key {
        ForwardedKey::Press { scancode, extended, .. } => {
            match *extended {
                KEY_UP => { emu.bus.pad1.set_up(true); return true; }
                KEY_DOWN => { emu.bus.pad1.set_down(true); return true; }
                KEY_LEFT => { emu.bus.pad1.set_left(true); return true; }
                KEY_RIGHT => { emu.bus.pad1.set_right(true); return true; }
                _ => {}
            }
            match *scancode {
                SCAN_Z => { emu.bus.pad1.set_b(true); return true; }
                SCAN_X => { emu.bus.pad1.set_a(true); return true; }
                SCAN_ENTER => { emu.bus.pad1.set_start(true); return true; }
                SCAN_SPACE => { emu.bus.pad1.set_select(true); return true; }
                _ => {}
            }
            false
        }
        ForwardedKey::Release { scancode, extended } => {
            let scan = scancode & 0x7F;
            match *extended {
                KEY_UP => { emu.bus.pad1.set_up(false); return true; }
                KEY_DOWN => { emu.bus.pad1.set_down(false); return true; }
                KEY_LEFT => { emu.bus.pad1.set_left(false); return true; }
                KEY_RIGHT => { emu.bus.pad1.set_right(false); return true; }
                _ => {}
            }
            match scan {
                SCAN_Z => { emu.bus.pad1.set_b(false); return true; }
                SCAN_X => { emu.bus.pad1.set_a(false); return true; }
                SCAN_ENTER => { emu.bus.pad1.set_start(false); return true; }
                SCAN_SPACE => { emu.bus.pad1.set_select(false); return true; }
                _ => {}
            }
            false
        }
        ForwardedKey::Close => true,
    }
}

fn render_frame(
    pr: &mut PixelRegion,
    nes_frame: &[u8; 61440],
    dst_w: usize,
    dst_h: usize,
    off_x: usize,
    off_y: usize,
    x_lut: &[u16],
) {
    let ptr = pr.as_ptr();
    for dy in 0..dst_h {
        let sy = dy * NES_H / dst_h;
        let src_row = sy * NES_W;
        let dst_row = (dy + off_y) * pr.pixel_w + off_x;
        for dx in 0..dst_w {
            let argb = nes_color(nes_frame[src_row + x_lut[dx] as usize]);
            unsafe { core::ptr::write_volatile(ptr.add(dst_row + dx), argb); }
        }
    }
}

fn render_frame_to_buf(
    buf: *mut u32,
    buf_w: usize,
    buf_h: usize,
    nes_frame: &[u8; 61440],
    fit_w: usize,
    fit_h: usize,
    x_lut: &[u16],
) {
    let off_x = (buf_w - fit_w) / 2;
    let off_y = (buf_h - fit_h) / 2;
    for dy in 0..buf_h {
        let dst_row = dy * buf_w;
        if dy < off_y || dy >= off_y + fit_h {
            for dx in 0..buf_w {
                unsafe { core::ptr::write_volatile(buf.add(dst_row + dx), 0xFF000000); }
            }
            continue;
        }
        let sy = (dy - off_y) * NES_H / fit_h;
        let src_row = sy * NES_W;
        for dx in 0..buf_w {
            let argb = if dx < off_x || dx >= off_x + fit_w {
                0xFF000000
            } else {
                nes_color(nes_frame[src_row + x_lut[dx - off_x] as usize])
            };
            unsafe { core::ptr::write_volatile(buf.add(dst_row + dx), argb); }
        }
    }
}

fn query_displayd_output(displayd_ep: usize) -> Result<(u32, u32, u32)> {
    let req = Message::new(DISPLAY_OUTPUT_INFO_LABEL, [0; 6], 0);
    let mut reply_buf = [0u8; 64];
    let bytes = libcluu::syscall::ipc_call(displayd_ep, req.as_bytes(), &mut reply_buf)?;
    let (rmsg, _) = parse_message(&reply_buf[..bytes]).ok_or(libcluu::Error::InvalidState)?;
    if rmsg.tag.label != DISPLAY_OUTPUT_INFO_LABEL {
        return Err(libcluu::Error::InvalidState);
    }
    Ok((rmsg.words[0] as u32, rmsg.words[1] as u32, rmsg.words[2] as u32))
}

fn compute_pixel_aspect_fit(screen_w: u32, screen_h: u32) -> (u32, u32) {
    let nw = NES_W as u32;
    let nh = NES_H as u32;
    let scale_w = screen_w / nw;
    let scale_h = screen_h / nh;
    let scale = scale_w.min(scale_h).max(1);
    (nw * scale, nh * scale)
}

struct DisplaydSurface {
    ep: usize,
    token: u64,
    frame_va: usize,
    frame_pages: usize,
    frame_token: u64,
    width: u32,
    height: u32,
}

fn displayd_create_surface(
    displayd_ep: usize,
    width: u32,
    height: u32,
) -> Result<DisplaydSurface> {
    let pitch = width * 4;
    let create_msg = Message::new(
        DISPLAY_SURFACE_CREATE_LABEL,
        [0, 0, width as usize, height as usize, pitch as usize, 0],
        5,
    );
    let mut reply_buf = [0u8; 128];
    let bytes = libcluu::syscall::ipc_call(displayd_ep, create_msg.as_bytes(), &mut reply_buf)?;
    let (rmsg, _) = parse_message(&reply_buf[..bytes]).ok_or(libcluu::Error::InvalidState)?;
    let token = rmsg.words[0] as u64;
    if token == 0 {
        let _ = debug_print(&alloc::format!(
            "xnes: displayd SURFACE_CREATE failed err={}", rmsg.words[4]
        ));
        return Err(libcluu::Error::InvalidState);
    }

    let sp = space_token();
    let frame_bytes = (width as usize) * (height as usize) * 4;
    let frame_pages = (frame_bytes + 0xFFF) / 0x1000;
    let frame_token = unsafe {
        syscall::invoke(sp, syscall::InvokeOp::FrameAllocate, frame_bytes, 0, 0, 0)? as u64
    };
    syscall::space_map_range(
        sp,
        DISPLAYD_VA,
        frame_token as usize,
        FLAGS_USER_RW | syscall::MAP_FRAME_TOKEN,
        frame_pages,
        0,
    )?;
    unsafe {
        core::ptr::write_bytes(DISPLAYD_VA as *mut u8, 0, frame_bytes);
    }

    let geo_msg = Message::new(
        DISPLAY_SET_GEOMETRY_LABEL,
        [0, token as usize, 0, 0, 0, 0],
        5,
    );
    let mut geo_payload = [0u8; 5];
    geo_payload[0..4].copy_from_slice(&100i32.to_le_bytes());
    geo_payload[4] = 1;
    let _ = libcluu::ipc::send_msg_with_payload(displayd_ep, &geo_msg, &geo_payload);

    let _ = debug_print(&alloc::format!(
        "xnes: displayd surface {}x{} token={}", width, height, token
    ));

    Ok(DisplaydSurface {
        ep: displayd_ep,
        token,
        frame_va: DISPLAYD_VA,
        frame_pages,
        frame_token,
        width,
        height,
    })
}

fn displayd_commit_frame(surf: &DisplaydSurface) -> Result<()> {
    let mut damage_bytes = [0u8; 16];
    damage_bytes[0..4].copy_from_slice(&0u32.to_le_bytes());
    damage_bytes[4..8].copy_from_slice(&0u32.to_le_bytes());
    damage_bytes[8..12].copy_from_slice(&surf.width.to_le_bytes());
    damage_bytes[12..16].copy_from_slice(&surf.height.to_le_bytes());
    let commit_msg = Message::new(
        DISPLAY_BUFFER_COMMIT_LABEL,
        [damage_bytes.len(), surf.token as usize, 0, 0, surf.frame_token as usize, 0],
        5,
    );
    let mut reply = Message::new(0, [0; 6], 0);
    call_with_payload(surf.ep, &commit_msg, &damage_bytes, &mut reply)?;
    if reply.words[0] != 0 {
        return Err(libcluu::Error::InvalidState);
    }
    Ok(())
}

fn displayd_destroy(surf: &DisplaydSurface) {
    let destroy_msg = Message::new(
        DISPLAY_SURFACE_DESTROY_LABEL,
        [0, surf.token as usize, 0, 0, 0, 0],
        2,
    );
    let _ = send(surf.ep, &destroy_msg, IpcFlags::empty());
    let sp = space_token();
    let _ = syscall::space_unmap(sp, surf.frame_va, surf.frame_pages);
    if surf.frame_token != 0 {
        unsafe {
            let _ = syscall::invoke(
                surf.frame_token as usize,
                syscall::InvokeOp::FrameFree,
                0, 0, 0, 0,
            );
        }
    }
}

struct AudioStream {
    ep: usize,
    stream_id: u32,
    session_id: u32,
    ring_va: usize,
    ring_bytes: usize,
    ring_pages: usize,
}

fn open_audio_stream(audiod_ep: usize) -> Result<AudioStream> {
    let req = Message::new(
        AUDIOD_STREAM_OPEN,
        [0, AUDIO_RATE as usize, AUDIO_CHANNELS as usize, AUDIO_PERIOD_BYTES, PCM_FMT_S16 as usize, 0],
        5,
    );
    let mut reply_buf = [0u8; 128];
    let bytes = libcluu::syscall::ipc_call(audiod_ep, req.as_bytes(), &mut reply_buf)?;
    let (rmsg, _) = parse_message(&reply_buf[..bytes]).ok_or(libcluu::Error::InvalidState)?;
    if rmsg.words[0] != 0 {
        let _ = debug_print(&alloc::format!("xnes: audiod stream open failed err={}", rmsg.words[0]));
        return Err(libcluu::Error::InvalidState);
    }
    let stream_id = rmsg.words[1] as u32;
    let session_id = rmsg.words[2] as u32;
    let frame_token = rmsg.words[3] as u64;
    let ring_bytes = rmsg.words[4];

    let sp = space_token();
    let ring_pages = (ring_bytes + PAGE_SIZE - 1) / PAGE_SIZE;
    let audio = AudioStream {
        ep: audiod_ep,
        stream_id,
        session_id,
        ring_va: AUDIO_RING_VA,
        ring_bytes,
        ring_pages,
    };
    if let Err(err) = syscall::space_map_range(
        sp,
        audio.ring_va,
        frame_token as usize,
        FLAGS_USER_RW | syscall::MAP_FRAME_TOKEN,
        audio.ring_pages,
        0,
    ) {
        close_audio(&audio);
        return Err(err);
    }

    let _ = debug_print(&alloc::format!(
        "xnes: audio stream {} sid={} ring={}B", stream_id, session_id, ring_bytes
    ));

    Ok(audio)
}

fn push_audio(audio: &AudioStream, samples: &[i16]) -> usize {
    if samples.is_empty() {
        return 0;
    }
    let backing = unsafe {
        core::slice::from_raw_parts_mut(audio.ring_va as *mut u8, audio.ring_bytes)
    };
    if let Some(mut ring) = FrameRing::attach(backing) {
        ring.push_mono(samples)
    } else {
        0
    }
}

fn close_audio(audio: &AudioStream) {
    let msg = Message::new(
        AUDIOD_STREAM_CLOSE,
        [audio.session_id as usize, audio.stream_id as usize, 0, 0, 0, 0],
        2,
    );
    let _ = send(audio.ep, &msg, IpcFlags::empty());
    let _ = syscall::space_unmap(space_token(), audio.ring_va, audio.ring_pages);
}

struct FramePacer {
    clock_token: usize,
    clock_hz: u64,
    next_deadline: u64,
    frame_ticks_numerator: u128,
    tick_remainder: u128,
}

impl FramePacer {
    fn new(clock_token: usize, clock_hz: u64) -> Option<Self> {
        if clock_hz == 0 {
            return None;
        }
        let now = syscall::clock_now(clock_token).ok()?;
        Some(Self {
            clock_token,
            clock_hz,
            next_deadline: now,
            frame_ticks_numerator: (clock_hz as u128) * (NTSC_FRAME_NS as u128),
            tick_remainder: 0,
        })
    }

    fn timeout_ms(&self) -> u64 {
        let now = syscall::clock_now(self.clock_token).unwrap_or(self.next_deadline);
        if now >= self.next_deadline {
            return 0;
        }
        let remaining = (self.next_deadline - now) as u128;
        let millis = (remaining * 1_000 + self.clock_hz as u128 - 1)
            / self.clock_hz as u128;
        millis.max(1).min(u64::MAX as u128) as u64
    }

    fn advance(&mut self) {
        let now = syscall::clock_now(self.clock_token).unwrap_or(self.next_deadline);
        let total = self.tick_remainder + self.frame_ticks_numerator;
        let step = total / 1_000_000_000;
        self.tick_remainder = total % 1_000_000_000;
        self.next_deadline = self.next_deadline.saturating_add(step as u64);

        let late_limit = step.saturating_mul(4) as u64;
        if now > self.next_deadline.saturating_add(late_limit) {
            self.next_deadline = now;
            self.tick_remainder = 0;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InputExitPolicy {
    CloseOnly,
    CloseOrEscape,
}

impl InputExitPolicy {
    fn should_exit(self, key: &ForwardedKey) -> bool {
        matches!(key, ForwardedKey::Close)
            || matches!(
                (self, key),
                (Self::CloseOrEscape, ForwardedKey::Press { scancode: SCAN_ESC, .. })
            )
    }
}

fn process_input_message(emu: &mut Emulator, exit_policy: InputExitPolicy, msg: &Message) -> bool {
    if msg.tag.label != COMP_INPUT_FORWARD_LABEL {
        return true;
    }
    let Some(key) = ForwardedKey::from_message(&msg.words) else {
        return true;
    };
    let _ = debug_print(&alloc::format!("xnes: key {:?}", key));
    if exit_policy.should_exit(&key) {
        false
    } else {
        let handled = handle_key(emu, &key);
        let _ = debug_print(&alloc::format!("xnes: key handled={}", handled));
        true
    }
}

fn wait_for_inputs(
    emu: &mut Emulator,
    exit_policy: InputExitPolicy,
    tokens: &[usize],
    recv_buf: &mut [u8],
    first_timeout_ms: u64,
) -> bool {
    for index in 0..INPUT_DRAIN_LIMIT {
        let timeout_ms = if index == 0 { first_timeout_ms } else { 0 };
        let Ok((_idx, len)) = syscall::ipc_recv_any(tokens, recv_buf, timeout_ms) else {
            break;
        };
        let Some((msg, _)) = parse_message(&recv_buf[..len]) else {
            continue;
        };
        if index == 0 {
            let _ = debug_print(&alloc::format!(
                "xnes: input recv label={} words={}", msg.tag.label, msg.tag.words
            ));
        }
        if !process_input_message(emu, exit_policy, &msg) {
            return false;
        }
    }
    true
}

fn wait_for_next_frame(
    emu: &mut Emulator,
    exit_policy: InputExitPolicy,
    tokens: &[usize],
    recv_buf: &mut [u8],
    pacer: &mut FramePacer,
) -> bool {
    let keep_running = wait_for_inputs(
        emu,
        exit_policy,
        tokens,
        recv_buf,
        pacer.timeout_ms(),
    );
    pacer.advance();
    keep_running
}

fn retain_unpushed_samples(samples: &mut [i16], pushed: usize) -> usize {
    let pushed = pushed.min(samples.len());
    samples.copy_within(pushed.., 0);
    samples.len() - pushed
}

fn pump_audio(emu: &mut Emulator, audio: Option<&AudioStream>, frame_count: u64) {
    let Some(audio) = audio else {
        return;
    };
    let sample_count = emu.bus.apu.sample_count;
    if sample_count == 0 {
        return;
    }
    let peak = emu.bus.apu.audio_samples[..sample_count]
        .iter()
        .map(|sample| i32::from(*sample).abs())
        .max()
        .unwrap_or(0);
    let pushed = push_audio(audio, &emu.bus.apu.audio_samples[..sample_count]);
    emu.bus.apu.sample_count =
        retain_unpushed_samples(&mut emu.bus.apu.audio_samples[..sample_count], pushed);
    if frame_count % 60 == 0 {
        let (available, written, read, xruns) = unsafe {
            let backing = core::slice::from_raw_parts_mut(audio.ring_va as *mut u8, audio.ring_bytes);
            match FrameRing::attach(backing) {
                Some(ring) => (ring.available_read(), ring.total_written(), ring.total_read(), ring.xrun_count()),
                None => (0, 0, 0, 0),
            }
        };
        let _ = debug_print(&alloc::format!(
            "xnes: audio frame={} samples={} pushed={} remain={} peak={} avail={} written={} read={} xruns={}",
            frame_count,
            sample_count,
            pushed,
            emu.bus.apu.sample_count,
            peak,
            available,
            written,
            read,
            xruns,
        ));
    }
}

fn run_emulator_loop(
    emu: &mut Emulator,
    exit_policy: InputExitPolicy,
    tokens: &[usize],
    mut presenter: impl FnMut(&[u8; 61440]),
) -> Option<AudioStream> {
    let audio = registry::lookup_service("audiod:main")
        .and_then(|ep| open_audio_stream(ep).ok());
    if audio.is_some() {
        let _ = debug_print("xnes: audio stream opened");
    } else {
        let _ = debug_print("xnes: no audio (audiod unavailable)");
    }

    let mut recv_buf = [0u8; 256];
    let mut running = true;
    let mut frame_count: u64 = 0;
    let mut fps_start: u64 = 0;
    let mut fps_frames: u64 = 0;
    let clk_tok = libcluu::boot::token_clock();
    let clk_hz = syscall::clock_frequency(clk_tok).unwrap_or(1000);
    let mut pacer = FramePacer::new(clk_tok, clk_hz);

    while running {
        while !emu.bus.ppu.frame_complete {
            let _ = emu.tick();
        }
        emu.bus.ppu.frame_complete = false;

        pump_audio(emu, audio.as_ref(), frame_count + 1);
        presenter(&emu.bus.ppu.frame);

        frame_count = frame_count.wrapping_add(1);
        fps_frames += 1;

        if fps_frames == 1 {
            fps_start = syscall::clock_now(clk_tok).unwrap_or(0);
        }
        if frame_count % 60 == 0 {
            let now = syscall::clock_now(clk_tok).unwrap_or(0);
            let elapsed = now.saturating_sub(fps_start);
            if elapsed > 0 {
                let fps = fps_frames * clk_hz / elapsed;
                let _ = debug_print(&alloc::format!("xnes: {} fps (frame {})", fps, frame_count));
            }
            fps_frames = 0;
            fps_start = now;
        }

        running = match pacer.as_mut() {
            Some(pacer) => wait_for_next_frame(
                emu,
                exit_policy,
                tokens,
                &mut recv_buf,
                pacer,
            ),
            None => wait_for_inputs(emu, exit_policy, tokens, &mut recv_buf, 16),
        };
    }

    audio
}

#[cfg(not(feature = "host-test"))]
#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = debug_print("xnes: start");

    let argv = args::args();
    let mut rom_path: Option<&str> = None;
    let mut fullscreen = false;
    for arg in &argv[1..] {
        match arg.as_str() {
            "-fullscreen" | "--fullscreen" => fullscreen = true,
            _ => {
                if rom_path.is_none() {
                    rom_path = Some(arg.as_str());
                }
            }
        }
    }
    let rom_path = match rom_path {
        Some(p) => p,
        None => {
            let _ = debug_print("xnes: usage: xnes <rom-path> [-fullscreen]");
            return 1;
        }
    };
    if fullscreen {
        let _ = debug_print("xnes: fullscreen mode");
    }

    if registry::init("xnes").is_err() {
        let _ = debug_print("xnes: registry init failed");
        return 1;
    }

    let rom_data = match load_rom(rom_path) {
        Ok(data) => data,
        Err(e) => {
            let _ = debug_print(&alloc::format!("xnes: ROM load failed {:?}", e));
            return 1;
        }
    };

    let rom = match Rom::new(&rom_data) {
        Ok(r) => r,
        Err(e) => {
            let _ = debug_print(&alloc::format!("xnes: ROM parse failed: {}", e));
            return 1;
        }
    };
    let mapper = rom.create_mapper();
    let mut emu = Emulator::new(mapper);
    let _ = debug_print("xnes: emulator initialized");

    if fullscreen {
        run_windowed(&mut emu, true)
    } else {
        run_windowed(&mut emu, false)
    }
}

fn run_windowed(emu: &mut Emulator, fullscreen: bool) -> i32 {
    let (win_cell_w, win_cell_h, pr_cell_x, pr_cell_y, pr_cell_w, pr_cell_h) = {
        let comp_ep = match registry::lookup_service("compositor:client") {
            Some(ep) => ep,
            None => {
                let _ = debug_print("xnes: no compositor:client in registry");
                return 1;
            }
        };
        match query_screen(comp_ep) {
            Ok((cols, rows)) => {
                let _ = debug_print(&alloc::format!(
                    "xnes: screen {}x{} cells", cols, rows
                ));
                if fullscreen {
                    (cols, rows, 0u16, 0u16, cols, rows)
                } else {
                    let (w, h) = compute_aspect_fit(cols, rows);
                    let _ = debug_print(&alloc::format!("xnes: windowed fit {}x{}", w, h));
                    (w, h, 0u16, 0u16, w, h)
                }
            }
            Err(_) => {
                let _ = debug_print("xnes: screen query failed, using 64x30");
                (64, 30, 0u16, 0u16, 64, 30)
            }
        }
    };

    let flags = if fullscreen {
        COMP_WIN_FLAG_FULLSCREEN | COMP_WIN_FLAG_NO_CHROME
    } else {
        0
    };
    let win = match create_window("xnes", win_cell_w as u32, win_cell_h as u32, flags) {
        Ok(w) => w,
        Err(e) => {
            let _ = debug_print(&alloc::format!("xnes: window creation failed {:?}", e));
            return 1;
        }
    };

    let mut pixel_region = match PixelRegion::alloc(pr_cell_w, pr_cell_h) {
        Ok(pr) => pr,
        Err(e) => {
            let _ = debug_print(&alloc::format!("xnes: PixelRegion alloc failed {:?}", e));
            let destroy_msg = Message::new(
                COMP_WIN_DESTROY_LABEL,
                [win.win_id, 0, 0, 0, 0, 0],
                1,
            );
            let _ = send(win.comp_ep, &destroy_msg, IpcFlags::empty());
            return 1;
        }
    };

    let set_pr = Message::new(
        COMP_WIN_SET_PIXEL_REGION_LABEL,
        [
            win.win_id,
            pr_cell_x as usize,
            pr_cell_y as usize,
            pr_cell_w as usize,
            pr_cell_h as usize,
            pixel_region.frame_token() as usize,
        ],
        6,
    );
    if send(win.comp_ep, &set_pr, IpcFlags::empty()).is_err() {
        let _ = debug_print("xnes: SET_PIXEL_REGION send failed");
        pixel_region.destroy();
        let destroy_msg = Message::new(
            COMP_WIN_DESTROY_LABEL,
            [win.win_id, 0, 0, 0, 0, 0],
            1,
        );
        let _ = send(win.comp_ep, &destroy_msg, IpcFlags::empty());
        return 1;
    }
    let _ = debug_print("xnes: PixelRegion attached to window");

    let (dst_w, dst_h) = if fullscreen {
        let (w, h) = compute_pixel_aspect_fit(
            pixel_region.pixel_w as u32,
            pixel_region.pixel_h as u32,
        );
        (w as usize, h as usize)
    } else {
        (pixel_region.pixel_w, pixel_region.pixel_h)
    };
    let off_x = (pixel_region.pixel_w.saturating_sub(dst_w)) / 2;
    let off_y = (pixel_region.pixel_h.saturating_sub(dst_h)) / 2;
    let _ = debug_print(&alloc::format!("xnes: render target {}x{}", dst_w, dst_h));

    let x_lut: Vec<u16> = (0..dst_w).map(|dx| (dx * NES_W / dst_w) as u16).collect();

    let tokens = [win.my_ep];
    let exit_policy = if fullscreen {
        InputExitPolicy::CloseOrEscape
    } else {
        InputExitPolicy::CloseOnly
    };
    let audio = run_emulator_loop(emu, exit_policy, &tokens, |frame| {
        render_frame(
            &mut pixel_region,
            frame,
            dst_w,
            dst_h,
            off_x,
            off_y,
            &x_lut,
        );
        let dmg = Message::new(
            COMP_WIN_DAMAGE_LABEL,
            [win.win_id, 0, 0, win.cell_w as usize, win.cell_h as usize, 0],
            5,
        );
        let _ = send(win.comp_ep, &dmg, IpcFlags::empty());
    });

    let destroy_msg = Message::new(
        COMP_WIN_DESTROY_LABEL,
        [win.win_id, 0, 0, 0, 0, 0],
        1,
    );
    pixel_region.unmap();
    let _ = send(win.comp_ep, &destroy_msg, IpcFlags::empty());
    if let Some(audio) = audio {
        close_audio(&audio);
    }

    let _ = debug_print("xnes: exit");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_only_policy_ignores_escape() {
        let key = ForwardedKey::Press {
            ascii: 0,
            modifiers: 0,
            scancode: SCAN_ESC,
            extended: 0,
        };
        assert!(!InputExitPolicy::CloseOnly.should_exit(&key));
    }

    #[test]
    fn close_or_escape_policy_exits_on_escape() {
        let key = ForwardedKey::Press {
            ascii: 0,
            modifiers: 0,
            scancode: SCAN_ESC,
            extended: 0,
        };
        assert!(InputExitPolicy::CloseOrEscape.should_exit(&key));
    }

    #[test]
    fn every_policy_exits_on_close() {
        assert!(InputExitPolicy::CloseOnly.should_exit(&ForwardedKey::Close));
    }

    #[test]
    fn retain_unpushed_samples_moves_backlog_to_front() {
        let mut samples = [1, 2, 3, 4];
        let remaining = retain_unpushed_samples(&mut samples, 2);
        assert_eq!((remaining, samples), (2, [3, 4, 3, 4]));
    }

}
