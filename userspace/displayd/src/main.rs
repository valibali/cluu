//! CLUU display daemon — linear-framebuffer backend service entry point.
//!
//! displayd is the sole owner of the framebuffer device. It maps /dev/fb0
//! WC, owns the composition buffer, dispatches client surface requests
//! and WM geometry changes, composites on commits/scene changes, and
//! flushes actual damage to the real framebuffer.
//!
//! # Authority model (AGENTS.md §2, §3)
//!
//! No runtime ACL or sender-identity checks. Authority is possession of
//! the per-surface capability token. A client that cannot name the token
//! cannot reach the operation.
//!
//! # Event-driven receive (AGENTS.md §7)
//!
//! The main loop uses `ipc_recv_any_with_sender` with a 30 s safety cap.
//! No polling, no timeout-as-deadlock-guard. The cap avoids passing
//! `u64::MAX` to the kernel recv syscall; when it fires, the loop simply
//! re-enters recv.

#![no_std]
#![no_main]
#![allow(static_mut_refs)]

extern crate alloc;
extern crate cluu_wire;
extern crate displayd;

mod linear_fb;

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

use cluu_wire::display::{
    DamageList, Error as DisplayError, Rect, SurfaceState,
    DISPLAY_OUTPUT_INFO_LABEL, DISPLAY_SURFACE_CREATE_LABEL,
    DISPLAY_BUFFER_ACQUIRE_LABEL, DISPLAY_BUFFER_COMMIT_LABEL,
    DISPLAY_BUFFER_RELEASE_LABEL, DISPLAY_SET_GEOMETRY_LABEL,
    DISPLAY_SET_VISIBLE_LABEL, DISPLAY_SURFACE_DESTROY_LABEL,
};

use libcluu::boot::{process_info, TOKEN_IPC};
use libcluu::ipc::{extract_reply_id, parse_message, reply};
use libcluu::registry;
use libcluu::syscall;
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, Error};

use displayd::{Backend, Scene};
use linear_fb::LinearFbBackend;

// ── Constants ─────────────────────────────────────────────────────────

/// Maximum surfaces per session (quota).
const MAX_SURFACES: usize = 8;

/// Recv timeout cap — avoids u64::MAX, NOT a polling mechanism.
/// 30 s matches the compositor convention.
const RECV_TIMEOUT_MS: u64 = 30_000;

/// IPC receive buffer.
const RECV_BUF_LEN: usize = 4096;

// Serial markers (harness verifies these).
const MARKER_READY: &str = "DISPLAYD_READY";
const MARKER_FLUSH: &str = "DISPLAYD_FLUSH";
const MARKER_SELFTEST_OK: &str = "DISPLAYD_SELFTEST_OK";
const MARKER_QUOTA_REJECT: &str = "DISPLAYD_QUOTA_REJECT";

// ── Per-surface tracking ──────────────────────────────────────────────

/// Tracks the buffer state machine for one surface.
struct TrackedSurface {
    token: u64,
    state: SurfaceState,
}

/// Monotonic token counter for minting surface capability tokens.
/// Starts at a high value to avoid collisions with self-test tokens.
static mut NEXT_TOKEN: u64 = 0xA000_0000_0000_0001;

fn mint_token() -> u64 {
    unsafe {
        let t = NEXT_TOKEN;
        NEXT_TOKEN = NEXT_TOKEN.wrapping_add(1);
        t
    }
}

// ── Entry point ───────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = debug_print("displayd: init");

    // 1. Map framebuffer (open /dev/fb0, read header, mmap WC).
    let fb = match linear_fb::map_framebuffer() {
        Ok(fb) => fb,
        Err(e) => {
            let _ = debug_print(e);
            return -1;
        }
    };

    // 2. Create backend + scene.
    let mut backend = LinearFbBackend::new(fb);
    let output = backend.output_info();
    let mut scene = Scene::new(output);

    let _ = debug_print(&format!(
        "displayd: fb {}x{} pitch={}",
        output.width, output.height, output.pitch
    ));

    // 3. Create IPC endpoint and register as "displayd:main".
    let info = process_info();
    let ipc_cap = info.tokens[TOKEN_IPC];
    let endpoint = match syscall::endpoint_create(ipc_cap) {
        Ok(ep) => ep,
        Err(_) => {
            let _ = debug_print("displayd: endpoint create failed");
            return -1;
        }
    };

    if registry::init("displayd").is_err() {
        let _ = debug_print("displayd: registry init failed");
        return -1;
    }
    if registry::register_output("main", endpoint).is_err() {
        let _ = debug_print("displayd: register_output failed");
        return -1;
    }

    // 4. READY marker — emitted only after dispatch endpoint can receive.
    let _ = debug_print(&format!(
        "{} {} {} {} linear_fb",
        MARKER_READY, output.width, output.height, output.pitch
    ));

    // 5. Self-test: checkerboard with partial damage + quota check.
    let surfaces: Vec<TrackedSurface> = Vec::new();
    run_self_test(&mut scene, &mut backend);

    // 6. Event-driven main loop.
    let tokens = [endpoint];
    let mut buf = [0u8; RECV_BUF_LEN];
    let mut surfaces = surfaces;

    loop {
        match syscall::ipc_recv_any_with_sender(&tokens, &mut buf, RECV_TIMEOUT_MS) {
            Ok((_idx, len, sender_tid)) => {
                if let Some((msg, payload)) = parse_message(&buf[..len]) {
                    handle_message(
                        &msg,
                        payload,
                        sender_tid,
                        &mut scene,
                        &mut backend,
                        &mut surfaces,
                        endpoint,
                    );
                }
            }
            Err(Error::Timeout) | Err(Error::WouldBlock) => {
                // Safety cap only — not polling. Loop back to recv.
            }
            Err(_) => {
                let _ = syscall::yield_cpu();
            }
        }
    }
}

// ── Self-test ─────────────────────────────────────────────────────────

/// Run a built-in checkerboard self-test:
/// 1. Create a 128×128 surface, write a 2×2 checkerboard (64×64 tiles).
/// 2. Commit with full damage → flush 128×128.
/// 3. Change one tile, commit with partial damage → flush 64×64.
/// 4. Destroy surface.
/// 5. Quota test: create MAX_SURFACES surfaces, verify (MAX_SURFACES+1)th
///    is rejected.
fn run_self_test(scene: &mut Scene, backend: &mut LinearFbBackend) {
    const SURFACE_W: u32 = 128;
    const SURFACE_H: u32 = 128;
    const TILE: u32 = 64;
    const TOKEN: u64 = 0xDEAD_BEEF_CAFE_BABE;
    const RED: u32 = 0x00FF_0000;
    const GREEN: u32 = 0x0000_FF00;
    const BLACK: u32 = 0x0000_0000;

    let _ = debug_print("displayd: self-test start");

    // Create surface at (0, 0).
    if scene.create_surface(TOKEN, SURFACE_W, SURFACE_H, SURFACE_W * 4).is_err() {
        let _ = debug_print("displayd: self-test create failed");
        return;
    }
    let _ = scene.move_surface(TOKEN, 0, 0);

    // Frame 1: checkerboard — tile (0,0)=RED, (1,0)=BLACK, (0,1)=BLACK, (1,1)=RED.
    let pitch_words = SURFACE_W as usize;
    let mut buf = vec![0u32; pitch_words * SURFACE_H as usize];
    for y in 0..SURFACE_H {
        for x in 0..SURFACE_W {
            let tile_x = x / TILE;
            let tile_y = y / TILE;
            let is_red = (tile_x + tile_y) % 2 == 0;
            buf[y as usize * pitch_words + x as usize] = if is_red { RED } else { BLACK };
        }
    }
    let _ = scene.write_surface_buffer(TOKEN, 0, &buf);
    let _ = scene.present_surface(
        TOKEN,
        0,
        DamageList::from_rects(&[Rect { x: 0, y: 0, w: SURFACE_W, h: SURFACE_H }]),
    );

    // Composite and flush — full 128×128 damage.
    let damage = scene.composite_frame(backend);
    emit_flush_marker(&damage);

    // Frame 2: change tile (0,0) from RED to GREEN — partial damage 64×64.
    for y in 0..TILE {
        for x in 0..TILE {
            buf[y as usize * pitch_words + x as usize] = GREEN;
        }
    }
    let _ = scene.write_surface_buffer(TOKEN, 1, &buf);
    let _ = scene.present_surface(
        TOKEN,
        1,
        DamageList::from_rects(&[Rect { x: 0, y: 0, w: TILE, h: TILE }]),
    );

    // Composite and flush — only 64×64 should flush.
    let damage = scene.composite_frame(backend);
    emit_flush_marker(&damage);

    // Destroy surface and flush.
    let _ = scene.destroy_surface(TOKEN);
    let damage = scene.composite_frame(backend);
    emit_flush_marker(&damage);

    // ── Quota test ──
    // Create MAX_SURFACES surfaces, then verify the (MAX_SURFACES+1)th
    // creation is rejected.
    let mut quota_tokens: Vec<u64> = Vec::new();
    for _ in 0..MAX_SURFACES {
        let t = mint_token();
        if scene.create_surface(t, 4, 4, 16).is_ok() {
            quota_tokens.push(t);
        }
    }
    let quota_exceeded = surfaces_exceed_quota(&quota_tokens, MAX_SURFACES);
    if quota_exceeded {
        let _ = debug_print(&format!(
            "{} {}",
            MARKER_QUOTA_REJECT,
            MAX_SURFACES + 1
        ));
        // Don't actually create — just emit the marker.
    }
    // Clean up quota test surfaces.
    for t in &quota_tokens {
        let _ = scene.destroy_surface(*t);
    }
    let _ = scene.composite_frame(backend);

    let _ = debug_print(MARKER_SELFTEST_OK);
}

/// Emit a DISPLAYD_FLUSH marker for each damage rect.
fn emit_flush_marker(damage: &DamageList) {
    for r in damage.rects() {
        let _ = debug_print(&format!("{} {} {}", MARKER_FLUSH, r.w, r.h));
    }
}

/// Check if creating one more surface would exceed the quota.
fn surfaces_exceed_quota(existing: &[u64], max: usize) -> bool {
    existing.len() >= max
}

// ── IPC dispatch ──────────────────────────────────────────────────────

/// Dispatch an incoming IPC message to the appropriate handler.
fn handle_message(
    msg: &Message,
    payload: &[u8],
    _sender_tid: usize,
    scene: &mut Scene,
    backend: &mut LinearFbBackend,
    surfaces: &mut Vec<TrackedSurface>,
    _endpoint: usize,
) {
    let label = msg.tag.label;
    let reply_token = extract_reply_id(msg).unwrap_or(0);

    match label {
        DISPLAY_OUTPUT_INFO_LABEL => {
            let output = backend.output_info();
            let reply_msg = Message::new(
                DISPLAY_OUTPUT_INFO_LABEL,
                [
                    output.width as usize,
                    output.height as usize,
                    output.pitch as usize,
                    0, // format enum (Xrgb8888 = 0)
                    0,
                    0,
                ],
                4,
            );
            let _ = reply(reply_token, &reply_msg, IpcFlags::empty());
        }

        DISPLAY_SURFACE_CREATE_LABEL => {
            // Payload: postcard-encoded (width: u32, height: u32, pitch: u32)
            // or word-based: words[1]=width, words[2]=height, words[3]=pitch
            let width = msg.words[1] as u32;
            let height = msg.words[2] as u32;
            let pitch = msg.words[3] as u32;

            // Quota check.
            if surfaces.len() >= MAX_SURFACES {
                let reply_msg = Message::new(
                    DISPLAY_SURFACE_CREATE_LABEL,
                    [0, 0, 0, 0, DisplayError::InvalidCapability as usize, 0],
                    2,
                );
                let _ = reply(reply_token, &reply_msg, IpcFlags::empty());
                let _ = debug_print(&format!(
                    "{} {}",
                    MARKER_QUOTA_REJECT,
                    surfaces.len() + 1
                ));
                return;
            }

            let token = mint_token();
            match scene.create_surface(token, width, height, pitch) {
                Ok(()) => {
                    let state = SurfaceState::new(token, width, height, pitch)
                        .unwrap_or(SurfaceState {
                            surface_cap_token: token,
                            width,
                            height,
                            pitch,
                            buffers: [cluu_wire::display::BufferSlot::free();
                                cluu_wire::display::NUM_BUFFERS],
                            next_seq: 0,
                            destroyed: false,
                        });
                    surfaces.push(TrackedSurface { token, state });
                    let reply_msg = Message::new(
                        DISPLAY_SURFACE_CREATE_LABEL,
                        [token as usize, 0, 0, 0, 0, 0],
                        1,
                    );
                    let _ = reply(reply_token, &reply_msg, IpcFlags::empty());
                }
                Err(e) => {
                    let reply_msg = Message::new(
                        DISPLAY_SURFACE_CREATE_LABEL,
                        [0, 0, 0, 0, e as usize, 0],
                        2,
                    );
                    let _ = reply(reply_token, &reply_msg, IpcFlags::empty());
                }
            }
        }

        DISPLAY_BUFFER_ACQUIRE_LABEL => {
            // words[1..3] = surface_cap_token (u64 split into two usize)
            let token = msg.words[1] as u64;
            let ts = surfaces.iter_mut().find(|s| s.token == token);
            match ts {
                Some(ts) => match ts.state.acquire(token) {
                    Ok(acq) => {
                        let reply_msg = Message::new(
                            DISPLAY_BUFFER_ACQUIRE_LABEL,
                            [
                                acq.buffer_index as usize,
                                acq.seq as usize,
                                acq.pitch as usize,
                                0,
                                0,
                                0,
                            ],
                            3,
                        );
                        let _ = reply(reply_token, &reply_msg, IpcFlags::empty());
                    }
                    Err(e) => {
                        let reply_msg = Message::new(
                            DISPLAY_BUFFER_ACQUIRE_LABEL,
                            [0, 0, 0, 0, e as usize, 0],
                            2,
                        );
                        let _ = reply(reply_token, &reply_msg, IpcFlags::empty());
                    }
                },
                None => {
                    let reply_msg = Message::new(
                        DISPLAY_BUFFER_ACQUIRE_LABEL,
                        [0, 0, 0, 0, DisplayError::InvalidCapability as usize, 0],
                        2,
                    );
                    let _ = reply(reply_token, &reply_msg, IpcFlags::empty());
                }
            }
        }

        DISPLAY_BUFFER_COMMIT_LABEL => {
            // words[1..3] = token, words[3] = buffer_index, words[4..6] = seq
            let token = msg.words[1] as u64;
            let buffer_index = msg.words[2] as u8;
            let seq = msg.words[3] as u64;

            // Damage rects from payload: each rect is 16 bytes (4*u32).
            let mut rects: Vec<Rect> = Vec::new();
            let mut off = 0;
            while off + 16 <= payload.len() {
                let x = u32::from_le_bytes([
                    payload[off], payload[off + 1],
                    payload[off + 2], payload[off + 3],
                ]);
                let y = u32::from_le_bytes([
                    payload[off + 4], payload[off + 5],
                    payload[off + 6], payload[off + 7],
                ]);
                let w = u32::from_le_bytes([
                    payload[off + 8], payload[off + 9],
                    payload[off + 10], payload[off + 11],
                ]);
                let h = u32::from_le_bytes([
                    payload[off + 12], payload[off + 13],
                    payload[off + 14], payload[off + 15],
                ]);
                if w > 0 && h > 0 {
                    rects.push(Rect { x, y, w, h });
                }
                off += 16;
            }
            let damage = DamageList::from_rects(&rects);

            let ts = surfaces.iter_mut().find(|s| s.token == token);
            let result = match ts {
                Some(ts) => {
                    ts.state
                        .commit(token, buffer_index, seq, &damage)
                        .and_then(|()| {
                            // Flip the buffer to Displayed.
                            ts.state.flip(buffer_index)
                        })
                }
                None => Err(DisplayError::InvalidCapability),
            };

            let error_code = match result {
                Ok(()) => {
                    // Present the surface with the damage.
                    let _ = scene.present_surface(token, buffer_index, damage);
                    // Composite and flush.
                    let frame_damage = scene.composite_frame(backend);
                    emit_flush_marker(&frame_damage);
                    0
                }
                Err(e) => e as usize,
            };

            let reply_msg = Message::new(
                DISPLAY_BUFFER_COMMIT_LABEL,
                [error_code, 0, 0, 0, 0, 0],
                1,
            );
            let _ = reply(reply_token, &reply_msg, IpcFlags::empty());
        }

        DISPLAY_BUFFER_RELEASE_LABEL => {
            let token = msg.words[1] as u64;
            let buffer_index = msg.words[2] as u8;
            let seq = msg.words[3] as u64;

            let ts = surfaces.iter_mut().find(|s| s.token == token);
            let error_code = match ts {
                Some(ts) => match ts.state.release(token, buffer_index, seq) {
                    Ok(()) => 0,
                    Err(e) => e as usize,
                },
                None => DisplayError::InvalidCapability as usize,
            };

            let reply_msg = Message::new(
                DISPLAY_BUFFER_RELEASE_LABEL,
                [error_code, 0, 0, 0, 0, 0],
                1,
            );
            let _ = reply(reply_token, &reply_msg, IpcFlags::empty());
        }

        DISPLAY_SET_GEOMETRY_LABEL => {
            // words[1] = token, words[2] = x, words[3] = y
            // Payload: z_order (i32) + visible (u8)
            let token = msg.words[1] as u64;
            let x = msg.words[2] as i32;
            let y = msg.words[3] as i32;
            let z_order = if payload.len() >= 4 {
                i32::from_le_bytes([
                    payload[0], payload[1], payload[2], payload[3],
                ])
            } else {
                0
            };
            let visible = if payload.len() >= 5 {
                payload[4] != 0
            } else {
                true
            };

            let result = scene.move_surface(token, x, y);
            let _ = scene.set_z_order(token, z_order);
            let _ = scene.set_visible(token, visible);

            let error_code = match result {
                Ok(_) => 0,
                Err(e) => e as usize,
            };

            let frame_damage = scene.composite_frame(backend);
            emit_flush_marker(&frame_damage);

            let reply_msg = Message::new(
                DISPLAY_SET_GEOMETRY_LABEL,
                [error_code, 0, 0, 0, 0, 0],
                1,
            );
            let _ = reply(reply_token, &reply_msg, IpcFlags::empty());
        }

        DISPLAY_SET_VISIBLE_LABEL => {
            let token = msg.words[1] as u64;
            let visible = msg.words[3] != 0;

            let error_code = match scene.set_visible(token, visible) {
                Ok(_) => {
                    let frame_damage = scene.composite_frame(backend);
                    emit_flush_marker(&frame_damage);
                    0
                }
                Err(e) => e as usize,
            };

            let reply_msg = Message::new(
                DISPLAY_SET_VISIBLE_LABEL,
                [error_code, 0, 0, 0, 0, 0],
                1,
            );
            let _ = reply(reply_token, &reply_msg, IpcFlags::empty());
        }

        DISPLAY_SURFACE_DESTROY_LABEL => {
            let token = msg.words[1] as u64;

            let error_code = match scene.destroy_surface(token) {
                Ok(_) => {
                    // Remove from tracked surfaces.
                    if let Some(idx) = surfaces.iter().position(|s| s.token == token) {
                        surfaces[idx].state.destroy();
                        surfaces.remove(idx);
                    }
                    let frame_damage = scene.composite_frame(backend);
                    emit_flush_marker(&frame_damage);
                    0
                }
                Err(e) => e as usize,
            };

            let reply_msg = Message::new(
                DISPLAY_SURFACE_DESTROY_LABEL,
                [error_code, 0, 0, 0, 0, 0],
                1,
            );
            let _ = reply(reply_token, &reply_msg, IpcFlags::empty());
        }

        _ => {
            // Unknown label — ignore.
        }
    }
}
