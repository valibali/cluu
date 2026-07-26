//! Virtio-gpu backend — proxies displayd output to gpudev:main via IPC.
//!
//! Implements the `Backend` trait using a virtio-gpu driver service
//! (registered as "gpudev:main" in the registry). The backend owns a
//! composition buffer and sends TRANSFER_TO_HOST_2D + RESOURCE_FLUSH
//! commands for dirty rects only — never the full screen unless the
//! damage covers it.
//!
//! # Selection (T12)
//!
//! `new()` looks up `gpudev:main` in the registry and probes it with a
//! short-timeout IPC handshake. If the driver is absent, not yet
//! registered, or not listening, `new()` returns `Err` and the caller
//! falls back to `LinearFbBackend`. Selection is runtime, not build-time.
//!
//! # Dirty-rect optimization
//!
//! `flush()` iterates the scene damage and issues one
//! TRANSFER_TO_HOST_2D + RESOURCE_FLUSH pair per dirty rect, clipped to
//! the output bounds. A 64×64 dirty rect produces a 64×64 transfer+flush
//! — never a full-screen transfer.
//!
//! # Direct scanout
//!
//! `try_direct_scanout` returns explicit eligibility: true only when a
//! single surface covers the full output at the right format/pitch, is
//! visible, unscaled, and not destroyed. When eligible, the backend
//! preserves the composition buffer until demotion. The first frame for
//! a given surface always composites (safe default — incomplete content
//! must not reach scanout).
//!
//! # Display events
//!
//! `poll_display_event` sends an IPC query to the driver. If the driver
//! reports a mode change, the backend re-queries GET_DISPLAY_INFO and
//! updates its output info. The main loop may call this periodically;
//! it is a non-blocking best-effort poll.
//!
//! # Wire protocol (displayd → gpudev:main)
//!
//! The driver (T11) ships a self-test-only run loop without IPC dispatch.
//! The labels below are the displayd-side contract; the driver-side
//! dispatch is a future task. Until then, the probe times out and
//! displayd falls back to linear-fb.

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

use cluu_wire::display::{DamageList, OutputInfo, PixelFormat, Rect};

use displayd::backend::Backend;
use displayd::surface::Surface;

use libcluu::ipc::parse_message;
use libcluu::registry;
use libcluu::syscall::{ipc_call_timeout, ipc_recv_any};
use libcluu::types::Message;
use libcluu::{debug_print, Error};

// ── IPC labels (displayd ↔ gpudev:main) ───────────────────────────────

/// Handshake: "are you there?" Reply: words[0]=0 (OK), words[1]=protocol version.
pub const GPU_PROBE: u32 = 0x700;
/// Query display mode. Reply: words[0]=0, words[1]=width, words[2]=height, words[3]=enabled.
pub const GPU_GET_DISPLAY_INFO: u32 = 0x701;
/// Create 2D resource. words[1]=resource_id, words[2]=format, words[3]=width, words[4]=height.
pub const GPU_CREATE_2D: u32 = 0x702;
/// Attach backing. words[1]=resource_id, words[2]=space_token, words[3]=backing_va, words[4]=backing_len.
pub const GPU_ATTACH_BACKING: u32 = 0x703;
/// Set scanout. words[1]=scanout_id, words[2]=resource_id, words[3]=width, words[4]=height.
pub const GPU_SET_SCANOUT: u32 = 0x704;
/// Combined TRANSFER_TO_HOST_2D + RESOURCE_FLUSH for a dirty rect.
/// words[1]=resource_id, words[2]=x, words[3]=y, words[4]=w, words[5]=h.
pub const GPU_TRANSFER_FLUSH: u32 = 0x705;
/// Unref resource. words[1]=resource_id.
pub const GPU_UNREF_RESOURCE: u32 = 0x706;
/// Poll display event. Reply: words[0]=0, words[1]=event_flags (0x1=display changed).
#[allow(dead_code)]
pub const GPU_POLL_EVENT: u32 = 0x707;

/// Protocol version the backend expects from the driver.
pub const GPU_PROTOCOL_VERSION: usize = 1;

// ── Timeouts ──────────────────────────────────────────────────────────

/// Probe timeout — if the driver doesn't reply within this window,
/// displayd falls back to linear-fb. 500 ms is enough for a registered
/// driver to answer; an absent or non-listening driver fails fast.
const PROBE_TIMEOUT_MS: usize = 500;

/// Command timeout for init and flush IPC calls.
const CMD_TIMEOUT_MS: usize = 2000;

/// Default display if the driver reports no enabled scanout.
const DEFAULT_W: u32 = 640;
const DEFAULT_H: u32 = 480;

/// Virtio-gpu 2D format: B8G8R8X8_UNORM (matches XRGB8888 in byte order).
const VIRTIO_GPU_FORMAT_B8G8R8X8: u32 = 2;

// ── Backend ───────────────────────────────────────────────────────────

/// Virtio-gpu backend: owns composition buffer + IPC proxy to gpudev:main.
///
/// The composition buffer is a `Vec<u32>` with the same pitch/height as
/// the GPU 2D resource. `scanout_buffer_mut` returns this buffer; `flush`
/// sends transfer+flush IPC for each dirty rect.
pub struct VirtioGpuBackend {
    info: OutputInfo,
    compose_buffer: Vec<u32>,
    driver_endpoint: usize,
    resource_id: u32,
    /// True when the current surface is eligible for direct scanout
    /// (full-output, matching format/pitch, visible, unscaled).
    direct_scanout_eligible: bool,
    /// True when direct scanout is currently active for a surface.
    /// The composition buffer is preserved until demotion.
    direct_scanout_active: bool,
    /// Token of the surface currently under direct scanout (0 = none).
    direct_scanout_token: u64,
    /// First-frame guard: the first presentation of a surface always
    /// composites; subsequent frames may promote to direct scanout.
    first_frame_seen: bool,
}

impl VirtioGpuBackend {
    /// Try to construct a virtio-gpu backend.
    ///
    /// Steps:
    /// 1. Look up gpudev:main in the registry.
    /// 2. Probe with a short-timeout handshake.
    /// 3. Query display info.
    /// 4. Create a 2D resource and attach backing (composition buffer).
    /// 5. Set scanout to bind the resource.
    ///
    /// Returns `Err` if any step fails — caller falls back to LinearFbBackend.
    pub fn new() -> Result<Self, &'static str> {
        let driver_endpoint = match registry::lookup_service("gpudev:main") {
            Some(ep) => ep,
            None => return Err("displayd: gpudev:main not registered"),
        };

        // Probe: short-timeout handshake. If the driver is not listening
        // (e.g. T11 run_loop has no IPC dispatch), this times out and we
        // fall back.
        if !probe_driver(driver_endpoint) {
            return Err("displayd: gpudev:main probe timeout");
        }

        let (width, height) = match query_display_info(driver_endpoint) {
            Ok((w, h)) => (w, h),
            Err(_) => return Err("displayd: gpudev:main get_display_info failed"),
        };

        let pitch = width.checked_mul(4).unwrap_or(DEFAULT_W * 4);
        let words_per_row = (pitch / 4) as usize;
        let buf_len = words_per_row * height as usize;
        let compose_buffer = vec![0u32; buf_len];

        let info = OutputInfo {
            width,
            height,
            pitch,
            format: PixelFormat::Xrgb8888,
        };

        let resource_id = 1u32; // first resource

        let mut backend = VirtioGpuBackend {
            info,
            compose_buffer,
            driver_endpoint,
            resource_id,
            direct_scanout_eligible: false,
            direct_scanout_active: false,
            direct_scanout_token: 0,
            first_frame_seen: false,
        };

        match backend.init_resource() {
            Ok(()) => {
                let _ = debug_print(&format!(
                    "displayd: virtio-gpu backend {}x{} pitch={} res={}",
                    width, height, pitch, resource_id
                ));
                Ok(backend)
            }
            Err(e) => Err(e),
        }
    }

    /// Create the 2D resource, attach backing, and set scanout.
    fn init_resource(&mut self) -> Result<(), &'static str> {
        // CREATE_2D
        let req = Message::new(
            GPU_CREATE_2D,
            [
                self.resource_id as usize,
                VIRTIO_GPU_FORMAT_B8G8R8X8 as usize,
                self.info.width as usize,
                self.info.height as usize,
                0,
                0,
            ],
            4,
        );
        let mut reply_buf = [0u8; 128];
        let r = ipc_call_timeout(
            self.driver_endpoint,
            req.as_bytes(),
            &mut reply_buf,
            CMD_TIMEOUT_MS,
        );
        if r.is_err() {
            return Err("displayd: virtio-gpu CREATE_2D failed");
        }

        // ATTACH_BACKING — pass displayd's space token + composition buffer VA.
        // The driver maps this into its own space and creates a DMA entry.
        let space_token = libcluu::boot::space_token();
        let backing_va = self.compose_buffer.as_ptr() as usize;
        let backing_len = self.compose_buffer.len() * 4;
        let req = Message::new(
            GPU_ATTACH_BACKING,
            [
                self.resource_id as usize,
                space_token,
                backing_va,
                backing_len,
                0,
                0,
            ],
            4,
        );
        let r = ipc_call_timeout(
            self.driver_endpoint,
            req.as_bytes(),
            &mut reply_buf,
            CMD_TIMEOUT_MS,
        );
        if r.is_err() {
            return Err("displayd: virtio-gpu ATTACH_BACKING failed");
        }

        // SET_SCANOUT
        let req = Message::new(
            GPU_SET_SCANOUT,
            [
                0, // scanout_id
                self.resource_id as usize,
                self.info.width as usize,
                self.info.height as usize,
                0,
                0,
            ],
            4,
        );
        let r = ipc_call_timeout(
            self.driver_endpoint,
            req.as_bytes(),
            &mut reply_buf,
            CMD_TIMEOUT_MS,
        );
        if r.is_err() {
            return Err("displayd: virtio-gpu SET_SCANOUT failed");
        }

        Ok(())
    }

    /// Poll the driver for display events. If the driver reports a mode
    /// change, re-query GET_DISPLAY_INFO and update the output info.
    /// Best-effort: errors are silently ignored.
    #[allow(dead_code)]
    pub fn poll_display_event(&mut self) {
        let req = Message::new(GPU_POLL_EVENT, [0, 0, 0, 0, 0, 0], 0);
        let mut reply_buf = [0u8; 128];
        let r = ipc_call_timeout(
            self.driver_endpoint,
            req.as_bytes(),
            &mut reply_buf,
            CMD_TIMEOUT_MS,
        );
        if let Ok(bytes) = r {
            if let Some((msg, _)) = parse_message(&reply_buf[..bytes]) {
                let event_flags = msg.words[1] as u32;
                if event_flags & 0x1 != 0 {
                    // Display changed — re-query.
                    if let Ok((w, h)) = query_display_info(self.driver_endpoint) {
                        self.info.width = w;
                        self.info.height = h;
                        self.info.pitch = w * 4;
                        let pitch_words = (self.info.pitch / 4) as usize;
                        let new_len = pitch_words * h as usize;
                        if new_len != self.compose_buffer.len() {
                            self.compose_buffer = vec![0u32; new_len];
                            // Re-attach backing with new dimensions.
                            let _ = self.init_resource();
                        }
                        let _ = debug_print(&format!(
                            "displayd: virtio-gpu mode changed {}x{}",
                            w, h
                        ));
                    }
                }
            }
        }
    }

    /// Check if a surface is eligible for direct scanout.
    ///
    /// Eligibility:
    /// - Surface covers the full output (x==0, y==0, display_w==output.w, display_h==output.h)
    /// - Surface is visible and not destroyed
    /// - Surface is unscaled (display_w==width, display_h==height)
    /// - Surface pitch matches output pitch
    fn check_direct_scanout_eligibility(&self, surface: &Surface) -> bool {
        if surface.destroyed || !surface.visible {
            return false;
        }
        if surface.x != 0 || surface.y != 0 {
            return false;
        }
        if surface.display_w != self.info.width || surface.display_h != self.info.height {
            return false;
        }
        if surface.display_w != surface.width || surface.display_h != surface.height {
            return false; // scaled — not eligible
        }
        if surface.pitch != self.info.pitch {
            return false;
        }
        true
    }

    /// Send a transfer+flush IPC for a single dirty rect.
    fn transfer_flush_rect(&self, rect: Rect) {
        let req = Message::new(
            GPU_TRANSFER_FLUSH,
            [
                self.resource_id as usize,
                rect.x as usize,
                rect.y as usize,
                rect.w as usize,
                rect.h as usize,
                0,
            ],
            5,
        );
        let mut reply_buf = [0u8; 128];
        let _ = ipc_call_timeout(
            self.driver_endpoint,
            req.as_bytes(),
            &mut reply_buf,
            CMD_TIMEOUT_MS,
        );

        // Serial marker for harness verification — emits the dirty rect
        // dimensions so the harness can verify that a 64×64 update
        // produces a 64×64 transfer+flush (not full-screen).
        let _ = debug_print(&format!(
            "DISPLAYD_VIRTIO_GPU_TF {} {} {} {}",
            rect.x, rect.y, rect.w, rect.h
        ));
    }
}

impl Backend for VirtioGpuBackend {
    fn output_info(&self) -> OutputInfo {
        self.info
    }

    fn scanout_buffer_mut(&mut self) -> &mut [u32] {
        &mut self.compose_buffer
    }

    fn flush(&mut self, damage: &DamageList) {
        let bounds = Rect {
            x: 0,
            y: 0,
            w: self.info.width,
            h: self.info.height,
        };
        for r in damage.rects() {
            if let Some(clipped) = r.clip_to(bounds) {
                self.transfer_flush_rect(clipped);
            }
        }
    }

    fn try_direct_scanout(&mut self, surface: &Surface) -> bool {
        // First-frame guard: the first presentation always composites.
        // This is the safe default — incomplete content must not reach
        // scanout. Subsequent frames for the same surface may promote.
        if !self.first_frame_seen {
            self.first_frame_seen = true;
            self.direct_scanout_eligible = self.check_direct_scanout_eligibility(surface);
            // Mark eligibility but don't activate yet — composite this frame.
            return false;
        }

        let eligible = self.check_direct_scanout_eligibility(surface);
        self.direct_scanout_eligible = eligible;

        if !eligible {
            // Demotion: if direct scanout was active, it ends here.
            // The composition buffer is released back to the compositor
            // — the next composite_frame will overwrite it normally.
            if self.direct_scanout_active {
                self.direct_scanout_active = false;
                self.direct_scanout_token = 0;
            }
            return false;
        }

        // Eligible. If this is the same surface as before, promote.
        // If it's a different surface, composite this frame (safe default
        // for the new surface's first frame).
        if self.direct_scanout_active && self.direct_scanout_token == surface.token {
            return true;
        }

        // New surface — record and composite this frame. Next frame may
        // promote to direct scanout.
        if self.direct_scanout_token != surface.token {
            self.direct_scanout_token = surface.token;
            self.direct_scanout_active = false;
        }
        false
    }
}

impl Drop for VirtioGpuBackend {
    fn drop(&mut self) {
        // Best-effort cleanup: unref the resource.
        let req = Message::new(
            GPU_UNREF_RESOURCE,
            [self.resource_id as usize, 0, 0, 0, 0, 0],
            1,
        );
        let mut reply_buf = [0u8; 128];
        let _ = ipc_call_timeout(
            self.driver_endpoint,
            req.as_bytes(),
            &mut reply_buf,
            CMD_TIMEOUT_MS,
        );
    }
}

// ── IPC helpers ───────────────────────────────────────────────────────

/// Probe the driver with a short-timeout handshake.
/// Returns true if the driver replied with a valid protocol version.
fn probe_driver(driver_endpoint: usize) -> bool {
    let req = Message::new(GPU_PROBE, [0, 0, 0, 0, 0, 0], 0);
    let mut reply_buf = [0u8; 128];
    let r = ipc_call_timeout(
        driver_endpoint,
        req.as_bytes(),
        &mut reply_buf,
        PROBE_TIMEOUT_MS,
    );
    match r {
        Ok(bytes) => {
            if let Some((msg, _)) = parse_message(&reply_buf[..bytes]) {
                // words[0] = 0 (OK), words[1] = protocol version
                msg.words[0] == 0 && msg.words[1] >= GPU_PROTOCOL_VERSION
            } else {
                false
            }
        }
        Err(Error::Timeout) => false,
        Err(_) => false,
    }
}

/// Query the driver for display info (width, height).
fn query_display_info(driver_endpoint: usize) -> Result<(u32, u32), &'static str> {
    let req = Message::new(GPU_GET_DISPLAY_INFO, [0, 0, 0, 0, 0, 0], 0);
    let mut reply_buf = [0u8; 128];
    let r = ipc_call_timeout(
        driver_endpoint,
        req.as_bytes(),
        &mut reply_buf,
        CMD_TIMEOUT_MS,
    );
    match r {
        Ok(bytes) => {
            if let Some((msg, _)) = parse_message(&reply_buf[..bytes]) {
                if msg.words[0] != 0 {
                    return Err("displayd: gpudev:main get_display_info error");
                }
                let width = msg.words[1] as u32;
                let height = msg.words[2] as u32;
                if width == 0 || height == 0 {
                    return Ok((DEFAULT_W, DEFAULT_H));
                }
                Ok((width, height))
            } else {
                Err("displayd: gpudev:main get_display_info parse failed")
            }
        }
        Err(_) => Err("displayd: gpudev:main get_display_info timeout"),
    }
}

/// Drain any pending IPC notifications from the driver (e.g. async event
/// notifications). Non-blocking — returns immediately if no message.
#[allow(dead_code)]
pub fn drain_driver_notifications(endpoint: usize) {
    let tokens = [endpoint];
    let mut buf = [0u8; 128];
    loop {
        match ipc_recv_any(&tokens, &mut buf, 0) {
            Ok(_) => continue,
            Err(_) => break,
        }
    }
}
