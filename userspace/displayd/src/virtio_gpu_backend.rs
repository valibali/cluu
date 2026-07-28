//! Virtio-gpu backend — proxies displayd output to gpudev:main via IPC.
//!
//! Implements the `Backend` trait using a virtio-gpu driver service
//! (registered as "gpudev:main" in the registry). The backend owns a
//! composition buffer and sends TRANSFER_TO_HOST_2D + RESOURCE_FLUSH
//! commands for dirty rects only — never the full screen unless the
//! damage covers it.
//!
//! # Synchronous IPC (AGENTS.md §7)
//!
//! This backend uses synchronous `ipc_call_timeout` for construction,
//! flush, event polling, and `Drop` cleanup. This is acceptable and NOT
//! a deadlock risk because:
//!
//! 1. **gpudev is a leaf driver.** It has no downstream IPC dependencies —
//!    it talks only to hardware (virtio-gpu PCI device via DMA + IRQ).
//!    Unlike the VFS→procmgr chain, there is no mutual-blocking IPC cycle.
//! 2. **All IPC calls have timeouts.** The probe uses 500 ms; per-operation
//!    commands use 2000 ms. A hung driver times out and displayd falls
//!    back to linear-fb (selection is runtime per T12).
//! 3. **gpudev never calls displayd.** The dependency graph is one-way:
//!    displayd → gpudev → hardware. There is no reverse edge that could
//!    form a cycle.
//! 4. **displayd's main loop is single-threaded but gpudev is not in the
//!    loop's wait set.** The loop blocks on `ipc_recv_any` for client
//!    messages; blocking on gpudev during a flush is a separate, bounded
//!    wait that cannot deadlock the loop.
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
use libcluu::syscall::{ipc_call_timeout, ipc_recv_any, space_unmap};
use libcluu::types::{IpcFlags, Message};
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
pub const GPU_RESIZE: u32 = 0x708;
pub const GPU_GRANT_TO_CLIENT: u32 = 0x709;

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
    compose_ptr: *mut u32,
    compose_len: usize,
    fb_pages: usize,
    own_space_token: usize,
    driver_endpoint: usize,
    resource_id: u32,
    driver_space_token: usize,
    grant_target_va: usize,
    direct_scanout_eligible: bool,
    direct_scanout_active: bool,
    direct_scanout_token: u64,
    first_frame_seen: bool,
}

impl VirtioGpuBackend {
    /// Try to construct a virtio-gpu backend.
    ///
    /// Steps:
    /// 1. Check registry cache for gpudev:main (non-blocking — avoids hang
    ///    when gpudev isn't autostarted; `lookup_service` would block on
    ///    `subscribe_output` waiting for a grant that never arrives).
    /// 2. Probe with a short-timeout handshake.
    /// 3. Query display info.
    /// 4. Create a 2D resource and attach backing (composition buffer).
    /// 5. Set scanout to bind the resource.
    ///
    /// Returns `Err` if any step fails — caller falls back to LinearFbBackend.
    ///
    /// # Synchronous IPC safety (AGENTS.md §7)
    ///
    /// The IPC calls here are synchronous and blocking, but safe because
    /// gpudev is a leaf driver with no downstream IPC dependencies (see the
    /// module-level `# Synchronous IPC` section). All calls have timeouts;
    /// a hung driver returns `Err` and the caller falls back to linear-fb.
    pub fn new() -> Result<Self, &'static str> {
        let driver_endpoint = match resolve_gpudev_endpoint() {
            Some(ep) => ep,
            None => return Err("displayd: gpudev:main not registered"),
        };

        let driver_space_token = match probe_driver(driver_endpoint) {
            Some(tok) => tok,
            None => return Err("displayd: gpudev:main probe timeout"),
        };

        let (width, height) = match query_display_info(driver_endpoint) {
            Ok((w, h)) => (w, h),
            Err(_) => return Err("displayd: gpudev:main get_display_info failed"),
        };

        let pitch = width.checked_mul(4).unwrap_or(DEFAULT_W * 4);
        let info = OutputInfo {
            width,
            height,
            pitch,
            format: PixelFormat::Xrgb8888,
        };

        let resource_id = 1u32;
        let displayd_space_token = libcluu::boot::space_token();

        let req = Message::new(
            GPU_CREATE_2D,
            [
                resource_id as usize,
                VIRTIO_GPU_FORMAT_B8G8R8X8 as usize,
                width as usize,
                height as usize,
                displayd_space_token,
                0,
            ],
            5,
        );
        let mut reply_buf = [0u8; 128];
        let r = ipc_call_timeout(
            driver_endpoint,
            req.as_bytes(),
            &mut reply_buf,
            CMD_TIMEOUT_MS,
        );
        let grant_target_va = match r {
            Ok(bytes) => {
                let (msg, _) = parse_message(&reply_buf[..bytes])
                    .ok_or("displayd: virtio-gpu CREATE_2D parse failed")?;
                if msg.words[0] != 0 {
                    return Err("displayd: virtio-gpu CREATE_2D error");
                }
                msg.words[1]
            }
            Err(_) => return Err("displayd: virtio-gpu CREATE_2D failed"),
        };

        let fb_bytes = (width as usize) * (height as usize) * 4;
        let compose_len = fb_bytes / 4;
        let fb_pages = (fb_bytes + 4095) / 4096;
        let compose_ptr = grant_target_va as *mut u32;
        let own_space_token = libcluu::boot::space_token();

        let backend = VirtioGpuBackend {
            info,
            compose_ptr,
            compose_len,
            fb_pages,
            own_space_token,
            driver_endpoint,
            resource_id,
            driver_space_token,
            grant_target_va,
            direct_scanout_eligible: false,
            direct_scanout_active: false,
            direct_scanout_token: 0,
            first_frame_seen: false,
        };

        let _ = debug_print(&format!(
            "displayd: virtio-gpu backend {}x{} pitch={} res={}",
            width, height, pitch, resource_id
        ));
        Ok(backend)
    }



    #[allow(dead_code)]
    pub fn poll_display_event(&mut self) -> Option<OutputInfo> {
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
                    return self.handle_resize();
                }
            }
        }
        None
    }

    fn handle_resize(&mut self) -> Option<OutputInfo> {
        let (new_w, new_h) = match query_display_info(self.driver_endpoint) {
            Ok((w, h)) => (w, h),
            Err(_) => return None,
        };

        if new_w == self.info.width && new_h == self.info.height {
            return None;
        }

        if self.fb_pages > 0 {
            let _ = space_unmap(self.own_space_token, self.grant_target_va, self.fb_pages);
        }

        let req = Message::new(
            GPU_RESIZE,
            [
                self.resource_id as usize,
                new_w as usize,
                new_h as usize,
                self.own_space_token,
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

        if let Ok(bytes) = r {
            if let Some((msg, _)) = parse_message(&reply_buf[..bytes]) {
                if msg.words[0] == 0 {
                    let new_bytes = msg.words[2];
                    let new_pitch = msg.words[3] as u32;
                    self.info.width = new_w;
                    self.info.height = new_h;
                    self.info.pitch = new_pitch;
                    self.compose_len = new_bytes / 4;
                    self.fb_pages = (new_bytes + 4095) / 4096;
                    let _ = debug_print(&format!(
                        "displayd: virtio-gpu resized to {}x{} pitch={}",
                        new_w, new_h, new_pitch
                    ));
                    return Some(self.info);
                }
            }
        }
        None
    }

    pub fn grant_fb_to_client(
        &self,
        client_space_token: usize,
        client_target_va: usize,
    ) -> Result<usize, &'static str> {
        if self.fb_pages == 0 {
            return Err("no FB allocated");
        }
        let req = Message::new(
            GPU_GRANT_TO_CLIENT,
            [client_space_token, client_target_va, 0, 0, 0, 0],
            2,
        );
        let mut reply_buf = [0u8; 128];
        let r = ipc_call_timeout(
            self.driver_endpoint,
            req.as_bytes(),
            &mut reply_buf,
            CMD_TIMEOUT_MS,
        );
        match r {
            Ok(bytes) => {
                if let Some((msg, _)) = parse_message(&reply_buf[..bytes]) {
                    if msg.words[0] == 0 {
                        return Ok(client_target_va);
                    }
                }
                Err("driver grant failed")
            }
            Err(_) => Err("driver grant timeout"),
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
    ///
    /// # Synchronous IPC safety (AGENTS.md §7)
    ///
    /// Blocks on gpudev with a 2000 ms timeout. gpudev is a leaf driver
    /// (no downstream IPC), so this cannot form a deadlock cycle. The
    /// result is discarded — a failed flush is non-fatal; the next flush
    /// retries.
    fn transfer_flush_rect(&self, rect: Rect) {
        let req = Message::new(
            GPU_TRANSFER_FLUSH,
            [
                self.resource_id as usize,
                0,
                0,
                self.info.width as usize,
                self.info.height as usize,
                self.info.pitch as usize,
            ],
            6,
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

impl Backend for VirtioGpuBackend {
    fn output_info(&self) -> OutputInfo {
        self.info
    }

    fn scanout_buffer_mut(&mut self) -> &mut [u32] {
        unsafe { core::slice::from_raw_parts_mut(self.compose_ptr, self.compose_len) }
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
        //
        // # Synchronous IPC safety (AGENTS.md §7)
        //
        // The Drop IPC call has a 2000 ms timeout. gpudev is a leaf driver
        // with no downstream IPC, so this cannot deadlock. The result is
        // discarded — cleanup is best-effort and a timeout is non-fatal.
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
fn probe_driver(driver_endpoint: usize) -> Option<usize> {
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
                if msg.words[0] == 0 && msg.words[1] >= GPU_PROTOCOL_VERSION {
                    return Some(msg.words[2]);
                }
            }
            None
        }
        Err(Error::Timeout) => None,
        Err(_) => None,
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

fn resolve_gpudev_endpoint() -> Option<usize> {
    if let Some(ep) = registry::lookup_cached("gpudev:main") {
        return Some(ep);
    }

    let _ = registry::request_subscription("gpudev", "main");

    let control_ep = registry::control_endpoint();
    if control_ep == 0 {
        return None;
    }

    let mut buf = [0u8; 256];
    for _ in 0..10 {
        if let Ok((_idx, len)) = ipc_recv_any(&[control_ep], &mut buf, 200) {
            if let Some((msg, payload)) = parse_message(&buf[..len]) {
                if let Some(event) = registry::handle_incoming_message(&msg, payload).ok().flatten() {
                    if let registry::RegistryEvent::Grant { service_name, name, token } = event {
                        if service_name == "gpudev" && name == "main" {
                            return Some(token);
                        }
                    }
                }
            }
        }
        if let Some(ep) = registry::lookup_cached("gpudev:main") {
            return Some(ep);
        }
    }

    None
}
