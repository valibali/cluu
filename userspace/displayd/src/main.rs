//! CLUU display daemon — virtio-gpu / linear-framebuffer backend service.
//!
//! displayd is the sole owner of the display output. At startup it tries
//! the virtio-gpu backend (via IPC to gpudev:main); if the driver is
//! absent or not listening, it falls back to the linear-framebuffer
//! backend (maps /dev/fb0 WC). The composition core, scene, and protocol
//! modules are backend-agnostic — selection is runtime, not build-time.
//!
//! Once a backend is selected, displayd owns the composition buffer,
//! dispatches client surface requests and WM geometry changes, composites
//! on commits/scene changes, and flushes actual damage to the backend.
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
mod virtio_gpu_backend;

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use cluu_wire::display::{
    DamageList, Error as DisplayError, OutputInfo, Rect, SurfaceState,
    DISPLAY_OUTPUT_INFO_LABEL, DISPLAY_SURFACE_CREATE_LABEL,
    DISPLAY_BUFFER_ACQUIRE_LABEL, DISPLAY_BUFFER_COMMIT_LABEL,
    DISPLAY_BUFFER_RELEASE_LABEL, DISPLAY_SET_GEOMETRY_LABEL,
    DISPLAY_SET_VISIBLE_LABEL, DISPLAY_SURFACE_DESTROY_LABEL,
    DISPLAY_LEASE_REGISTER_LABEL, DISPLAY_LEASE_ACQUIRE_LABEL,
    DISPLAY_LEASE_RELEASE_LABEL, DISPLAY_LEASE_RELEASE_ACK_LABEL,
    LeaseAcquire, LeaseGranted, LeaseHandle, LeaseOwner,
};

use libcluu::boot::{process_info, space_token, TOKEN_IPC};
use libcluu::ipc::{extract_reply_id, parse_message, reply};
use libcluu::ipc::{VTMGR_DIRECT_ABORT_LABEL, VTMGR_DIRECT_COMMIT_LABEL,
    VTMGR_DIRECT_PREPARE_LABEL, VTMGR_DIRECT_RETURN_COMMIT_LABEL,
    VTMGR_DIRECT_RETURN_PREPARE_LABEL};
use libcluu::registry::{self, RegistryEvent};
use libcluu::syscall::{self, MAP_FRAME_TOKEN};
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, Error};

use displayd::{Backend, Scene, Surface};
use displayd::direct_damage::{fullscreen_damage, parse_damage_payload};
use displayd::lease::{
    bind_compositor, CompositorRegistration, LeaseCoordinator, LeaseIo,
};
use linear_fb::LinearFbBackend;
use virtio_gpu_backend::VirtioGpuBackend;

// ── Constants ─────────────────────────────────────────────────────────

/// Maximum surfaces per session (quota).
const MAX_SURFACES: usize = 8;

/// Recv timeout — displayd polls for display events (resize) on timeout.
/// 1000ms reduces idle wakeups while keeping resize detection responsive.
const RECV_TIMEOUT_MS: u64 = 1000;

/// IPC receive buffer.
const RECV_BUF_LEN: usize = 4096;

// Serial markers (harness verifies these).
const MARKER_READY: &str = "DISPLAYD_READY";
const MARKER_FLUSH: &str = "DISPLAYD_FLUSH";
const MARKER_SELFTEST_OK: &str = "DISPLAYD_SELFTEST_OK";
const MARKER_QUOTA_REJECT: &str = "DISPLAYD_QUOTA_REJECT";
const MARKER_BACKEND: &str = "DISPLAYD_BACKEND";
static DIRECT_FLUSH_DIAG_DONE: AtomicBool = AtomicBool::new(false);
static DIRECT_FLUSH_FAILURE_DIAG_DONE: AtomicBool = AtomicBool::new(false);

// ── Backend selection ─────────────────────────────────────────────────

/// Runtime-selected display backend. Tries virtio-gpu first; falls back
/// to linear-fb if the driver is absent or not listening.
enum DisplayBackend {
    Linear(LinearFbBackend),
    VirtioGpu(VirtioGpuBackend),
}

impl Backend for DisplayBackend {
    fn output_info(&self) -> OutputInfo {
        match self {
            DisplayBackend::Linear(b) => b.output_info(),
            DisplayBackend::VirtioGpu(b) => b.output_info(),
        }
    }

    fn scanout_buffer_mut(&mut self) -> &mut [u32] {
        match self {
            DisplayBackend::Linear(b) => b.scanout_buffer_mut(),
            DisplayBackend::VirtioGpu(b) => b.scanout_buffer_mut(),
        }
    }

    fn flush(&mut self, damage: &DamageList) {
        match self {
            DisplayBackend::Linear(b) => b.flush(damage),
            DisplayBackend::VirtioGpu(b) => b.flush(damage),
        }
    }

    fn try_direct_scanout(&mut self, surface: &Surface) -> bool {
        match self {
            DisplayBackend::Linear(b) => b.try_direct_scanout(surface),
            DisplayBackend::VirtioGpu(b) => b.try_direct_scanout(surface),
        }
    }
}

impl DisplayBackend {
    fn flush_direct(&mut self, damage: &DamageList) -> bool {
        match self {
            DisplayBackend::Linear(b) => {
                b.flush(damage);
                true
            }
            DisplayBackend::VirtioGpu(b) => b.flush_direct(damage),
        }
    }
}

struct DisplayLeaseIo<'a> {
    backend: &'a mut DisplayBackend,
    direct_fb_token: &'a mut u64,
    vtmgr_endpoint: Option<usize>,
    compositor: Option<CompositorRegistration>,
}

impl DisplayLeaseIo<'_> {
    fn vtmgr_call(&self, label: u32, words: [usize; 6]) -> Result<(), DisplayError> {
        let endpoint = self.vtmgr_endpoint.ok_or(DisplayError::LeaseIoFailure)?;
        let mut message = Message::new(label, words, 2);
        libcluu::ipc::call(endpoint, &mut message, IpcFlags::empty())
            .map_err(|_| DisplayError::LeaseIoFailure)?;
        if message.words[0] == 0 { Ok(()) } else { Err(DisplayError::LeaseIoFailure) }
    }

}

impl LeaseIo for DisplayLeaseIo<'_> {
    fn clear_for_compositor(&mut self) -> Result<(), DisplayError> {
        if let DisplayBackend::VirtioGpu(backend) = self.backend {
            backend.clear_for_lease();
        }
        Ok(())
    }

    fn prepare_acquire(
        &mut self,
        lease: LeaseGranted,
        request: Option<LeaseAcquire>,
    ) -> Result<(), DisplayError> {
        if lease.owner != LeaseOwner::Fullscreen {
            return Ok(());
        }
        let request = request.ok_or(DisplayError::LeaseIoFailure)?;
        self.vtmgr_call(
            VTMGR_DIRECT_PREPARE_LABEL,
            [request.input_endpoint, lease.handle.generation as usize, 0, 0, 0, 0],
        )?;
        let granted = match self.backend {
            DisplayBackend::VirtioGpu(backend) => {
                backend.clear_for_lease();
                backend.grant_fb_to_client(lease.handle.lease_id, request.client_space_token, request.client_target_va)
            }
            DisplayBackend::Linear(_) => Err("linear framebuffer cannot be directly granted"),
        };
        if granted.is_err() {
            let _ = self.vtmgr_call(
                VTMGR_DIRECT_ABORT_LABEL,
                [lease.handle.generation as usize, 0, 0, 0, 0, 0],
            );
            return Err(DisplayError::LeaseIoFailure);
        }
        let commit = self.vtmgr_call(
            VTMGR_DIRECT_COMMIT_LABEL,
            [lease.handle.generation as usize, 0, 0, 0, 0, 0],
        );
        if commit.is_err() {
            if let DisplayBackend::VirtioGpu(backend) = self.backend {
                let _ = backend.release_direct_grant(lease.handle.lease_id);
            }
            let _ = self.vtmgr_call(
                VTMGR_DIRECT_ABORT_LABEL,
                [lease.handle.generation as usize, 0, 0, 0, 0, 0],
            );
            return Err(DisplayError::LeaseIoFailure);
        }
        Ok(())
    }

    fn prepare_release(&mut self, lease: LeaseGranted) -> Result<(), DisplayError> {
        if lease.owner != LeaseOwner::Fullscreen {
            let compositor = self.compositor.ok_or(DisplayError::LeaseIoFailure)?;
            let mut message = Message::new(
                DISPLAY_LEASE_RELEASE_LABEL,
                [lease.handle.lease_id as usize, lease.handle.generation as usize, 0, 0, 0, 0],
                2,
            );
            libcluu::ipc::call(compositor.endpoint, &mut message, IpcFlags::empty())
                .map_err(|_| DisplayError::LeaseIoFailure)?;
            if message.words[0] != 0 {
                return Err(DisplayError::LeaseIoFailure);
            }
            if let DisplayBackend::VirtioGpu(backend) = self.backend {
                backend
                    .release_direct_grant(compositor.resource_token)
                    .map_err(|_| DisplayError::LeaseIoFailure)?;
            }
            *self.direct_fb_token = 0;
            return Ok(());
        }
        self.vtmgr_call(
            VTMGR_DIRECT_RETURN_PREPARE_LABEL,
            [lease.handle.generation as usize, 0, 0, 0, 0, 0],
        )?;
        Ok(())
    }

    fn complete_release(&mut self, lease: LeaseGranted) -> Result<(), DisplayError> {
        if lease.owner == LeaseOwner::Fullscreen {
            if let DisplayBackend::VirtioGpu(backend) = self.backend {
                backend
                    .release_direct_grant(lease.handle.lease_id)
                    .map_err(|_| DisplayError::LeaseIoFailure)?;
            }
            *self.direct_fb_token = 0;
            self.vtmgr_call(
                VTMGR_DIRECT_RETURN_COMMIT_LABEL,
                [lease.handle.generation as usize, 0, 0, 0, 0, 0],
            )?;
        }
        Ok(())
    }

    fn restore_compositor(
        &mut self,
        lease: LeaseGranted,
        resource_token: u64,
    ) -> Result<(), DisplayError> {
        let compositor = self.compositor.ok_or(DisplayError::LeaseIoFailure)?;
        let output = self.backend.output_info();
        let grant_va = match self.backend {
            DisplayBackend::VirtioGpu(backend) => {
                backend
                    .release_direct_grant(resource_token)
                    .map_err(|_| DisplayError::LeaseIoFailure)?;
                backend
                    .grant_fb_to_client(
                        resource_token,
                        compositor.space_token,
                        compositor.target_va,
                    )
                    .map_err(|_| DisplayError::LeaseIoFailure)?
            }
            DisplayBackend::Linear(_) => return Err(DisplayError::LeaseIoFailure),
        };
        let message = Message::new(
            DISPLAY_LEASE_ACQUIRE_LABEL,
            [
                0,
                lease.handle.lease_id as usize,
                lease.handle.generation as usize,
                output.width as usize,
                output.height as usize,
                output.pitch as usize,
            ],
            6,
        );
        let grant_payload = grant_va.to_le_bytes();
        let mut reply_message = Message::new(0, [0; 6], 0);
        let resume_succeeded = libcluu::ipc::call_with_payload(
            compositor.endpoint,
            &message,
            &grant_payload,
            &mut reply_message,
        )
            .is_ok()
            && reply_message.words[0] == 0;
        if !resume_succeeded {
            if let DisplayBackend::VirtioGpu(backend) = self.backend {
                let _ = backend.release_direct_grant(resource_token);
            }
            *self.direct_fb_token = 0;
            return Err(DisplayError::LeaseIoFailure);
        }
        *self.direct_fb_token = resource_token;
        Ok(())
    }
}

/// Try virtio-gpu first; fall back to linear-fb. Returns the selected
/// backend and a backend name string for the READY marker.
fn select_backend() -> Result<(DisplayBackend, &'static str), &'static str> {
    match VirtioGpuBackend::new() {
        Ok(b) => Ok((DisplayBackend::VirtioGpu(b), "virtio_gpu")),
        Err(e) => {
            let _ = debug_print(e);
            let fb = linear_fb::map_framebuffer()?;
            Ok((DisplayBackend::Linear(LinearFbBackend::new(fb)), "linear_fb"))
        }
    }
}

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
    // SAFETY: `NEXT_TOKEN` is a `static mut` accessed only from displayd's
    // single main thread. displayd is single-threaded (no `spawn`, no extra
    // threads — the main loop is `ipc_recv_any` → `handle_message`), so
    // there is no concurrent access. `wrapping_add` prevents overflow panic.
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

    let _ = registry::init("displayd");

    let (mut backend, backend_name) = match select_backend() {
        Ok((b, name)) => (b, name),
        Err(e) => {
            let _ = debug_print(e);
            return -1;
        }
    };

    let output = backend.output_info();
    let mut scene = Scene::new(output);

    let _ = debug_print(&format!(
        "displayd: {} {} {} {}",
        backend_name, output.width, output.height, output.pitch
    ));

    let info = process_info();
    let ipc_cap = info.tokens[TOKEN_IPC];
    let endpoint = match syscall::endpoint_create(ipc_cap) {
        Ok(ep) => ep,
        Err(_) => {
            let _ = debug_print("displayd: endpoint create failed");
            return -1;
        }
    };

    if registry::register_output("main", endpoint).is_err() {
        let _ = debug_print("displayd: register_output failed");
        return -1;
    }

    let mut vtmgr_endpoint = registry::lookup_cached("vtmgr:input");
    if vtmgr_endpoint.is_none() {
        let _ = registry::request_subscription("vtmgr", "input");
    }
    let mut leases = LeaseCoordinator::new();
    let mut direct_fb_token: u64 = 0;
    {
        let mut io = DisplayLeaseIo {
            backend: &mut backend,
            direct_fb_token: &mut direct_fb_token,
            vtmgr_endpoint,
            compositor: None,
        };
        if leases.register_compositor(&mut io).is_err() {
            let _ = debug_print("displayd: compositor lease registration failed");
            return -1;
        }
    }

    // 4. READY marker — emitted only after dispatch endpoint can receive.
    let _ = debug_print(&format!(
        "{} {} {} {} {}",
        MARKER_READY, output.width, output.height, output.pitch, backend_name
    ));
    let _ = debug_print(&format!("{} {}", MARKER_BACKEND, backend_name));

    // 5. Self-test: checkerboard with partial damage + quota check.
    let surfaces: Vec<TrackedSurface> = Vec::new();
    run_self_test(&mut scene, &mut backend);

    // 6. Event-driven main loop.
    // Include the registry control endpoint so displayd can process grant
    // requests from clients resolving "displayd:main" via the registry.
    let registry_ep = registry::control_endpoint();
    let tokens = if registry_ep != 0 {
        [endpoint, registry_ep]
    } else {
        [endpoint, endpoint]
    };
    let mut buf = [0u8; RECV_BUF_LEN];
    let mut surfaces = surfaces;
    let mut compositor: Option<CompositorRegistration> = None;

    loop {
        match syscall::ipc_recv_any_with_sender(&tokens, &mut buf, RECV_TIMEOUT_MS) {
            Ok((idx, len, sender_tid)) => {
                if let Some((msg, payload)) = parse_message(&buf[..len]) {
                    if msg.tag.label == DISPLAY_SURFACE_CREATE_LABEL {
                        let _ = debug_print(&format!(
                            "displayd: RECV SURFACE_CREATE idx={} len={}", idx, len
                        ));
                    }
                    if registry_ep != 0 && idx == 1 {
                        if let Ok(Some(RegistryEvent::Grant { service_name, name, token })) =
                            registry::handle_incoming_message(&msg, payload)
                        {
                            if service_name == "vtmgr" && name == "input" {
                                vtmgr_endpoint = Some(token);
                            }
                        }
                        continue;
                    }
                    handle_message(
                        &msg,
                        payload,
                        sender_tid,
                        &mut scene,
                        &mut backend,
                        &mut surfaces,
                         &mut direct_fb_token,
                         &mut leases,
                         vtmgr_endpoint,
                         &mut compositor,
                         endpoint,
                    );
                }
            }
            Err(Error::Timeout) | Err(Error::WouldBlock) => {
                if let DisplayBackend::VirtioGpu(ref mut b) = backend {
                    if let Some(new_output) = b.poll_display_event() {
                        scene.set_output(new_output);
                        scene.full_damage();
                        let frame_damage = scene.composite_frame(&mut backend);
                        emit_flush_marker(&frame_damage);
                    }
                }
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
fn run_self_test(scene: &mut Scene, backend: &mut DisplayBackend) {
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
/// Gated behind CLUU_BENCH to avoid serial flooding in production —
/// debug_print blocks the vCPU on the serial line.
#[cfg(feature = "bench")]
fn emit_flush_marker(damage: &DamageList) {
    for r in damage.rects() {
        let _ = debug_print(&format!("{} {} {}", MARKER_FLUSH, r.w, r.h));
    }
}

#[cfg(not(feature = "bench"))]
fn emit_flush_marker(_damage: &DamageList) {}

/// Check if creating one more surface would exceed the quota.
fn surfaces_exceed_quota(existing: &[u64], max: usize) -> bool {
    existing.len() >= max
}

fn reply_lease(
    reply_token: usize,
    label: u32,
    lease: LeaseGranted,
    output: OutputInfo,
    error: Option<DisplayError>,
) {
    let status = error.map_or(0, |value| value as usize);
    let message = Message::new(
        label,
        [
            lease.handle.lease_id as usize,
            lease.handle.generation as usize,
            output.width as usize,
            output.height as usize,
            output.pitch as usize,
            status,
        ],
        6,
    );
    let _ = reply(reply_token, &message, IpcFlags::empty());
}

fn reply_lease_status(reply_token: usize, label: u32, status: usize) {
    let words = match label {
        DISPLAY_LEASE_REGISTER_LABEL | DISPLAY_LEASE_ACQUIRE_LABEL => [0, 0, 0, 0, 0, status],
        _ => [status, 0, 0, 0, 0, 0],
    };
    let message = Message::new(label, words, 6);
    let _ = reply(reply_token, &message, IpcFlags::empty());
}

fn reply_lease_error(reply_token: usize, label: u32, error: DisplayError) {
    reply_lease_status(reply_token, label, error as usize);
}

// ── IPC dispatch ──────────────────────────────────────────────────────

/// Dispatch an incoming IPC message to the appropriate handler.
fn handle_message(
    msg: &Message,
    payload: &[u8],
    _sender_tid: usize,
    scene: &mut Scene,
    backend: &mut DisplayBackend,
    surfaces: &mut Vec<TrackedSurface>,
    direct_fb_token: &mut u64,
    leases: &mut LeaseCoordinator,
    vtmgr_endpoint: Option<usize>,
    compositor: &mut Option<CompositorRegistration>,
    _endpoint: usize,
) {
    let label = msg.tag.label;
    let reply_token = extract_reply_id(msg).unwrap_or(0);

    if label == DISPLAY_SURFACE_CREATE_LABEL {
        let _ = debug_print("displayd: HANDLE_MSG SURFACE_CREATE");
    }

    match label {
        DISPLAY_LEASE_REGISTER_LABEL => {
            let output = backend.output_info();
            let requested = CompositorRegistration::new(
                msg.words[0],
                msg.words[1],
                msg.words[2],
                msg.words[3] as u64,
            );
            let registration = match bind_compositor(compositor, requested) {
                Ok(registration) => registration,
                Err(error) => {
                    reply_lease_error(reply_token, DISPLAY_LEASE_REGISTER_LABEL, error);
                    return;
                }
            };
            let mut io = DisplayLeaseIo {
                backend,
                direct_fb_token,
                vtmgr_endpoint,
                compositor: Some(registration),
            };
            match leases.active_lease() {
                Some(granted) if granted.owner == LeaseOwner::Compositor => {
                    reply_lease(reply_token, DISPLAY_LEASE_REGISTER_LABEL, granted, output, None)
                }
                Some(_) => reply_lease_error(reply_token, DISPLAY_LEASE_REGISTER_LABEL, DisplayError::FramebufferBusy),
                None => match leases.register_compositor(&mut io) {
                    Ok(granted) => {
                        reply_lease(reply_token, DISPLAY_LEASE_REGISTER_LABEL, granted, output, None)
                    }
                    Err(error) => reply_lease_error(reply_token, DISPLAY_LEASE_REGISTER_LABEL, error),
                },
            }
        }

        DISPLAY_LEASE_ACQUIRE_LABEL => {
            let output = backend.output_info();
            let request = LeaseAcquire {
                client_space_token: msg.words[0],
                client_target_va: msg.words[1],
                input_endpoint: msg.words[2],
            };
            let mut io = DisplayLeaseIo {
                backend,
                direct_fb_token,
                vtmgr_endpoint,
                compositor: *compositor,
            };
            match leases.acquire_fullscreen(&mut io, request) {
                Ok(granted) => reply_lease(reply_token, DISPLAY_LEASE_ACQUIRE_LABEL, granted, output, None),
                Err(error) => reply_lease_error(reply_token, DISPLAY_LEASE_ACQUIRE_LABEL, error),
            }
        }

        DISPLAY_LEASE_RELEASE_LABEL => {
            let handle = LeaseHandle { lease_id: msg.words[0] as u64, generation: msg.words[1] as u64 };
            let mut io = DisplayLeaseIo {
                backend,
                direct_fb_token,
                vtmgr_endpoint,
                compositor: *compositor,
            };
            match leases.release(&mut io, handle) {
                Ok(_) => reply_lease_status(reply_token, DISPLAY_LEASE_RELEASE_LABEL, 0),
                Err(error) => reply_lease_error(reply_token, DISPLAY_LEASE_RELEASE_LABEL, error),
            }
        }

        DISPLAY_LEASE_RELEASE_ACK_LABEL => {
            let handle = LeaseHandle { lease_id: msg.words[0] as u64, generation: msg.words[1] as u64 };
            let mut io = DisplayLeaseIo {
                backend,
                direct_fb_token,
                vtmgr_endpoint,
                compositor: *compositor,
            };
            let resource_token = match *compositor {
                Some(registration) => registration.resource_token,
                None => 0,
            };
            match leases.acknowledge_release_and_restore(&mut io, handle, resource_token) {
                Ok(_) => reply_lease_status(reply_token, DISPLAY_LEASE_RELEASE_ACK_LABEL, 0),
                Err(error) => reply_lease_error(reply_token, DISPLAY_LEASE_RELEASE_ACK_LABEL, error),
            }
        }

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
            let client_space_token = msg.words[0];
            let client_grant_va = msg.words[1];
            let width = msg.words[2] as u32;
            let height = msg.words[3] as u32;
            let pitch = msg.words[4] as u32;
            let _ = debug_print("displayd: SURFACE_CREATE recv");

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

                    let grant_va = if client_space_token != 0 && client_grant_va != 0 {
                        match backend {
                            DisplayBackend::VirtioGpu(ref mut b) => {
                                match b.grant_fb_to_client(
                                    token,
                                    client_space_token,
                                    client_grant_va,
                                ) {
                                    Ok(va) => {
                                        *direct_fb_token = token;
                                        va
                                    }
                                    Err(e) => {
                                        let _ = debug_print(&format!(
                                            "displayd: grant_fb_to_client failed: {}", e
                                        ));
                                        0
                                    }
                                }
                            }
                            DisplayBackend::Linear(_) => 0,
                        }
                    } else {
                        0
                    };

                    let reply_msg = Message::new(
                        DISPLAY_SURFACE_CREATE_LABEL,
                        [token as usize, grant_va, 0, 0, 0, 0],
                        2,
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
            // words[4] = client_frame_token: T8 extension for pixel transfer
            // (T7 gap — BufferAcquired.ptr_or_offset was never wired).
            let token = msg.words[1] as u64;
            let buffer_index = msg.words[2] as u8;
            let seq = msg.words[3] as u64;
            let client_frame_token = msg.words[4] as u64;

            let rects = parse_damage_payload(payload);
            let damage = DamageList::from_rects(&rects);

            // Direct-FB path: client wrote directly to the granted FB.
            // No copy, no composite — just TRANSFER the damage to QEMU.
            let lease_damage = leases.active_lease().is_some_and(|lease| {
                lease.owner == LeaseOwner::Fullscreen
                    && lease.handle
                        == LeaseHandle {
                            lease_id: token,
                            generation: seq,
                        }
            });
            let direct_damage = if lease_damage {
                fullscreen_damage(payload, &rects, backend.output_info())
            } else {
                damage
            };
            if client_frame_token == 0 && lease_damage && leases.mark_fullscreen_commit_diag(
                LeaseHandle { lease_id: token, generation: seq },
            ) {
                let first = rects.first().copied();
                let bounds = backend.output_info();
                let clipped_rects = rects
                    .iter()
                    .filter(|rect| {
                        rect.clip_to(Rect {
                            x: 0,
                            y: 0,
                            w: bounds.width,
                            h: bounds.height,
                        })
                        .is_some()
                    })
                    .count();
                let _ = debug_print(&format!(
                    "displayd: fullscreen commit diag payload_len={} parsed_rects={} first_rect={:?} clipped_rects={}",
                    payload.len(), rects.len(), first, clipped_rects
                ));
            }
            if client_frame_token == 0 && lease_damage {
                let flush_ok = backend.flush_direct(&direct_damage);
                let first_flush = !DIRECT_FLUSH_DIAG_DONE.swap(true, Ordering::Relaxed);
                let first_failure = !flush_ok
                    && !DIRECT_FLUSH_FAILURE_DIAG_DONE.swap(true, Ordering::Relaxed);
                if first_flush || first_failure {
                    let _ = debug_print(&format!(
                        "displayd: direct commit flush_result={} lease_match=true id={} generation={}",
                        flush_ok,
                        token,
                        seq
                    ));
                }
                emit_flush_marker(&direct_damage);
                let reply_msg = Message::new(
                    DISPLAY_BUFFER_COMMIT_LABEL,
                    [0, 0, 0, 0, 0, 0],
                    1,
                );
                let _ = reply(reply_token, &reply_msg, IpcFlags::empty());
                return;
            }

            if client_frame_token == 0 && token == *direct_fb_token && *direct_fb_token != 0 {
                backend.flush(&damage);
                emit_flush_marker(&damage);
                let reply_msg = Message::new(
                    DISPLAY_BUFFER_COMMIT_LABEL,
                    [0, 0, 0, 0, 0, 0],
                    1,
                );
                let _ = reply(reply_token, &reply_msg, IpcFlags::empty());
                return;
            }

            // Frame-token path: compositor client provides pixels via frame token.
            if client_frame_token != 0 {
                let surface_idx = scene.find_surface_idx(token);
                let copied = if let Some(idx) = surface_idx {
                    let (sw, sh, spitch) = {
                        let s = scene.surface(idx).unwrap();
                        (s.width, s.height, s.pitch)
                    };
                    let pitch_words = spitch as usize / 4;
                    let dr = damage.rects().first().copied().unwrap_or(Rect {
                        x: 0, y: 0, w: sw, h: sh,
                    });
                    let total_bytes = (dr.w as usize) * (dr.h as usize) * 4;
                    let pages = (total_bytes + 0xFFF) / 0x1000;
                    let scratch_va: usize = 0xD000_0000;
                    let flags = 0x07 | MAP_FRAME_TOKEN;
                    let mapped = syscall::space_map_range(
                        space_token(),
                        scratch_va,
                        client_frame_token as usize,
                        flags,
                        pages,
                        0,
                    ).is_ok();
                    if mapped {
                        let src_len = (dr.w as usize) * (dr.h as usize);
                        // SAFETY: `scratch_va` was just mapped by
                        // `space_map_range` with `pages` pages (each 4 KiB),
                        // and `src_len = dr.w * dr.h` u32s = `src_len * 4`
                        // bytes. The bounds check below ensures
                        // `src_off + copy_len <= src.len()` before any read.
                        // The mapping is unmap'd after the copy, but `src`
                        // is only used within this block. Alignment: the
                        // kernel maps pages page-aligned (4 KiB), which
                        // satisfies u32's 4-byte alignment.
                        let src = unsafe {
                            core::slice::from_raw_parts(
                                scratch_va as *const u32,
                                src_len,
                            )
                        };
                        if let Some(surf) = scene.surface_mut(idx) {
                            let buf = &mut surf.buffer_data[buffer_index as usize];
                            for row in 0..dr.h as usize {
                                let dst_off = (dr.y as usize + row) * pitch_words + dr.x as usize;
                                let src_off = row * dr.w as usize;
                                let copy_len = dr.w as usize;
                                if dst_off + copy_len <= buf.len() && src_off + copy_len <= src.len() {
                                    buf[dst_off..dst_off + copy_len]
                                        .copy_from_slice(&src[src_off..src_off + copy_len]);
                                }
                            }
                        }
                        let _ = syscall::space_unmap(
                            space_token(),
                            scratch_va,
                            pages,
                        );
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };

                if copied {
                    let _ = scene.present_surface(token, buffer_index, damage);
                    let frame_damage = scene.composite_frame(backend);
                    emit_flush_marker(&frame_damage);
                }

                let error_code = if copied { 0 } else { DisplayError::InvalidCapability as usize };
                let reply_msg = Message::new(
                    DISPLAY_BUFFER_COMMIT_LABEL,
                    [error_code, 0, 0, 0, 0, 0],
                    1,
                );
                let _ = reply(reply_token, &reply_msg, IpcFlags::empty());
                return;
            }

            // State-machine path (original T7).
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

            if token != *direct_fb_token {
                let frame_damage = scene.composite_frame(backend);
                emit_flush_marker(&frame_damage);
            }

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
                    if token != *direct_fb_token {
                        let frame_damage = scene.composite_frame(backend);
                        emit_flush_marker(&frame_damage);
                    }
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

            let error_code = if token == *direct_fb_token {
                match backend {
                    DisplayBackend::VirtioGpu(b) => {
                        if b.release_direct_grant(token).is_err() {
                            DisplayError::LeaseIoFailure as usize
                        } else {
                            *direct_fb_token = 0;
                            0
                        }
                    }
                    DisplayBackend::Linear(_) => 0,
                }
            } else {
                0
            };
            let error_code = if error_code != 0 {
                error_code
            } else {
                match scene.destroy_surface(token) {
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
                }
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
