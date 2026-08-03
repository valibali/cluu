//! Display surface protocol — wire types and buffer state machine.
//!
//! See `docs/superpowers/specs/2026-07-26-multimedia-architecture-design.md` §3.3
//! for the corrected protocol specification. displayd is the sole hardware owner;
//! surfaces are server-owned double buffers; `present` is a nonblocking commit and
//! `acquire` blocks when no FREE buffer is available.
//!
//! # Authority model
//!
//! No numeric surface ID alone grants authority. Every surface operation requires
//! the per-surface capability token returned by `surface_create`. The token is
//! minted by displayd at creation time and delivered to the client via the
//! session's display endpoint. A client that cannot name the token cannot reach
//! the operation — there is no runtime ACL and no `sender_tid` interrogation
//! (AGENTS.md §2, §3, §5).
//!
//! # Damage coordinate spaces
//!
//! Three distinct coordinate spaces are used for damage. Confusing them is the
//! root cause of the "stale until hovered" bug class
//! (`cluu-modal-damage-clamps-border-out.md`,
//! `cluu-compositor-cursor-clobbered-by-animated-win.md`).
//!
//! - **Content damage** — client-local surface coordinates. Origin (0,0) is the
//!   surface's top-left. Rects are validated against `(surface_w, surface_h)`.
//!   This is what the client commits in `BufferCommit.damage`.
//!
//! - **Scene damage** — compositor output coordinates. Origin (0,0) is the
//!   output's top-left. Content damage is translated by the surface's
//!   `Geometry (x, y)` offset to produce scene damage. displayd clips scene
//!   damage to the output bounds.
//!
//! - **Backend damage** — hardware/scanout coordinates. May differ from scene
//!   damage if the backend performs scaling or scanout placement differs from
//!   the compositor's logical output. The backend is responsible for this
//!   final translation; displayd passes scene damage and the backend narrows
//!   it to scanout bounds.
//!
//! The invariant: any function that translates damage between spaces MUST
//! apply the same offset in both directions. If content damage is translated
//! to scene damage with a `+1, +1` chrome offset, the compose pass MUST read
//! surface content at the same offset — otherwise cells never refresh from
//! SHM and appear "stale until hovered".

// ----- Protocol version -----

/// Wire protocol version for the display surface protocol.
///
/// Increment on breaking changes to any type in this module. displayd rejects
/// messages carrying an unsupported version.
pub const DISPLAY_PROTOCOL_VERSION: u32 = 1;

/// Maximum number of damage rects per commit before bounding-box fallback.
pub const MAX_DAMAGE_RECTS: usize = 8;

// ----- Verb labels (InvokeOp dispatch targets) -----

pub const DISPLAY_OUTPUT_INFO_LABEL: u32 = 300;
pub const DISPLAY_SURFACE_CREATE_LABEL: u32 = 301;
pub const DISPLAY_BUFFER_ACQUIRE_LABEL: u32 = 302;
pub const DISPLAY_BUFFER_COMMIT_LABEL: u32 = 303;
pub const DISPLAY_BUFFER_RELEASE_LABEL: u32 = 304;
pub const DISPLAY_SET_GEOMETRY_LABEL: u32 = 305;
pub const DISPLAY_SET_VISIBLE_LABEL: u32 = 306;
pub const DISPLAY_SURFACE_DESTROY_LABEL: u32 = 307;
pub const DISPLAY_OUTPUT_CHANGED_LABEL: u32 = 308;
pub const DISPLAY_LEASE_REGISTER_LABEL: u32 = 309;
pub const DISPLAY_LEASE_ACQUIRE_LABEL: u32 = 310;
pub const DISPLAY_LEASE_RELEASE_LABEL: u32 = 311;
pub const DISPLAY_LEASE_RELEASE_ACK_LABEL: u32 = 312;

// ----- Pixel format -----

/// Pixel format of a surface buffer. Only `Xrgb8888` is supported in the
/// initial protocol; the enum is exhaustive so future formats are a
/// protocol-version-gated addition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// 32 bits per pixel, 8 bits per channel, X byte = unused (opaque),
    /// R/G/B in low-to-high order. Fast path is `copy_nonoverlapping` per row.
    Xrgb8888,
}

impl PixelFormat {
    /// Bytes per pixel for this format.
    pub const fn bytes_per_pixel(self) -> u32 {
        match self {
            PixelFormat::Xrgb8888 => 4,
        }
    }
}

// ----- Rectangle -----

/// Axis-aligned pixel rectangle in a named coordinate space. The space is
/// determined by context: content, scene, or backend (see module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    /// Zero rect — sentinel for empty damage list slots. `w == 0 && h == 0`
    /// makes it a malformed rect that validation will reject if it ever
    /// reaches a commit.
    pub const ZERO: Rect = Rect { x: 0, y: 0, w: 0, h: 0 };

    /// Overflow-checked construction. Returns `None` if `w` or `h` is zero,
    /// or if `x + w` / `y + h` overflow `u32`.
    pub const fn new(x: u32, y: u32, w: u32, h: u32) -> Option<Rect> {
        if w == 0 || h == 0 {
            return None;
        }
        if x.checked_add(w).is_none() || y.checked_add(h).is_none() {
            return None;
        }
        Some(Rect { x, y, w, h })
    }

    /// Right edge: `x + w`, saturating on overflow.
    pub const fn right(self) -> u32 {
        self.x.saturating_add(self.w)
    }

    /// Bottom edge: `y + h`, saturating on overflow.
    pub const fn bottom(self) -> u32 {
        self.y.saturating_add(self.h)
    }

    /// Clip `self` to `bounds`. Returns `None` if the intersection is empty.
    pub fn clip_to(self, bounds: Rect) -> Option<Rect> {
        let x = self.x.max(bounds.x);
        let y = self.y.max(bounds.y);
        let right = self.right().min(bounds.right());
        let bottom = self.bottom().min(bounds.bottom());
        if right <= x || bottom <= y {
            return None;
        }
        Some(Rect { x, y, w: right - x, h: bottom - y })
    }

    /// Smallest rect containing both `self` and `other`. Returns `self` if
    /// `other` is empty (w==0 or h==0), and vice versa.
    pub fn extend(self, other: Rect) -> Rect {
        if other.w == 0 || other.h == 0 {
            return self;
        }
        if self.w == 0 || self.h == 0 {
            return other;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Rect { x, y, w: right - x, h: bottom - y }
    }

    /// Bounding box of a slice of rects. Returns `None` if the slice is empty
    /// or all rects are empty.
    pub fn bounding_box(rects: &[Rect]) -> Option<Rect> {
        let mut acc: Option<Rect> = None;
        for r in rects {
            if r.w == 0 || r.h == 0 {
                continue;
            }
            acc = Some(match acc {
                None => *r,
                Some(a) => a.extend(*r),
            });
        }
        acc
    }
}

// ----- Output info -----

/// Output dimensions and format reported by displayd at connection time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputInfo {
    pub width: u32,
    pub height: u32,
    /// Bytes per scanline. Must be >= `width * bytes_per_pixel(format)`.
    pub pitch: u32,
    pub format: PixelFormat,
}

// ----- Surface creation -----

/// Create a new surface. `surface_cap_token` is the per-surface capability
/// token that displayd mints and the client must present for all subsequent
/// operations on this surface. No numeric surface ID grants authority — the
/// token IS the authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceCreate {
    pub surface_cap_token: u64,
    pub width: u32,
    pub height: u32,
    /// Bytes per scanline for the backing buffers. Must be >=
    /// `width * format.bytes_per_pixel()`. displayd allocates `pitch * height`
    /// bytes per backing buffer.
    pub pitch: u32,
}

// ----- Buffer acquire / acquired -----

/// Request acquisition of a free buffer for writing. The client blocks until
/// a FREE buffer is available (backpressure — see spec §3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferAcquire {
    pub surface_cap_token: u64,
}

/// Reply to `BufferAcquire`. The client now owns `buffer_index` for writing
/// and must commit it with the matching `seq`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferAcquired {
    /// 0 or 1 for double-buffered surfaces.
    pub buffer_index: u8,
    /// Monotonic sequence number assigned by displayd. Must be echoed in the
    /// subsequent `BufferCommit` and `Release`. A mismatch indicates a stale
    /// or foreign operation.
    pub seq: u64,
    /// Offset into the shared backing region, or frame token, depending on
    /// the backend. The client maps this to write pixels.
    pub ptr_or_offset: u64,
    pub pitch: u32,
}

// ----- Damage list -----

/// Fixed-capacity damage rect list with bounding-box fallback.
///
/// Contains at most `MAX_DAMAGE_RECTS` (8) rects. If a client presents more
/// than 8 rects, `from_rects` collapses them to a single bounding box and sets
/// `bounding_fallback = true`. displayd composites the union of damage across
/// all surfaces for a frame.
///
/// Coordinate space: **content damage** — client-local surface coordinates
/// with origin (0, 0) at the surface's top-left. displayd translates to scene
/// damage using the surface's `Geometry (x, y)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DamageList {
    pub rects: [Rect; MAX_DAMAGE_RECTS],
    /// Number of valid entries in `rects`. If `bounding_fallback` is true,
    /// `count` is 1 and `rects[0]` is the bounding box.
    pub count: u8,
    /// True if the original rect list exceeded `MAX_DAMAGE_RECTS` and was
    /// collapsed to a single bounding-box rect.
    pub bounding_fallback: bool,
}

impl DamageList {
    /// Empty damage list (no rects). `count == 0`, `bounding_fallback == false`.
    pub const fn empty() -> Self {
        DamageList {
            rects: [Rect::ZERO; MAX_DAMAGE_RECTS],
            count: 0,
            bounding_fallback: false,
        }
    }

    /// Build a damage list from a slice of rects. If `rects.len()` exceeds
    /// `MAX_DAMAGE_RECTS`, collapses to the bounding box of all rects and
    /// sets `bounding_fallback = true`.
    pub fn from_rects(rects: &[Rect]) -> Self {
        if rects.len() <= MAX_DAMAGE_RECTS {
            let mut r = [Rect::ZERO; MAX_DAMAGE_RECTS];
            let mut i = 0;
            while i < rects.len() {
                r[i] = rects[i];
                i += 1;
            }
            DamageList { rects: r, count: rects.len() as u8, bounding_fallback: false }
        } else {
            let bb = Rect::bounding_box(rects).unwrap_or(Rect::ZERO);
            let mut r = [Rect::ZERO; MAX_DAMAGE_RECTS];
            r[0] = bb;
            DamageList { rects: r, count: 1, bounding_fallback: true }
        }
    }

    /// View of the valid rects.
    pub fn rects(&self) -> &[Rect] {
        let end = self.count as usize;
        &self.rects[..end]
    }
}

// ----- Buffer commit -----

/// Commit a buffer for display. Nonblocking — returns immediately and
/// displayd schedules the composite/scanout at the next frame boundary.
///
/// `seq` must match the `seq` returned by the corresponding `BufferAcquired`.
/// A mismatch indicates a stale or replayed commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferCommit {
    pub surface_cap_token: u64,
    pub buffer_index: u8,
    pub seq: u64,
    pub damage: DamageList,
}

// ----- Release -----

/// Release a displayed buffer back to the free pool. The client must not
/// write to the buffer after release. `seq` must match the commit's `seq`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Release {
    pub surface_cap_token: u64,
    pub buffer_index: u8,
    pub seq: u64,
}

// ----- Geometry / visibility -----

/// Surface geometry and stacking. Set by the compositor (WM capability
/// required). `x, y` is the surface's position in output (scene) coordinates.
/// `z_order` determines painter's-algorithm order (lower = farther back).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub surface_cap_token: u64,
    pub x: i32,
    pub y: i32,
    pub z_order: i32,
    pub visible: bool,
}

/// Destroy a surface. All inflight buffers are released to the free pool and
/// the surface capability token is invalidated. Further operations on this
/// token return `Error::InvalidCapability`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Destroy {
    pub surface_cap_token: u64,
}

// ----- Framebuffer lease -----

/// Exclusive framebuffer owner. Exactly one owner may be active at a time.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseOwner {
    Compositor = 0,
    Fullscreen = 1,
}

/// Lifecycle handle for one framebuffer lease generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseHandle {
    pub lease_id: u64,
    pub generation: u64,
}

/// Register the default compositor framebuffer owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseRegister {
    pub owner: LeaseOwner,
}

/// Request fullscreen framebuffer ownership after compositor release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseAcquire {
    /// Client address-space capability receiving the framebuffer mapping.
    pub client_space_token: usize,
    /// Page-aligned client virtual address for the framebuffer mapping.
    pub client_target_va: usize,
    /// Endpoint that receives input while fullscreen owns the display.
    pub input_endpoint: usize,
}

/// Begin release of the lease identified by `handle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseRelease {
    pub handle: LeaseHandle,
}

/// Acknowledge that owner-side unmap/release work for `handle` completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseReleaseAck {
    pub handle: LeaseHandle,
}

/// Logical lease grant returned by displayd.
///
/// Wire reply words are `[lease_id, generation, width, height, pitch, status]`.
/// `status == 0` means grant success; geometry fields describe direct
/// framebuffer mapping for fullscreen clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseGranted {
    pub handle: LeaseHandle,
    pub owner: LeaseOwner,
}

// ----- Error enum -----

/// Protocol error. Exhaustive — every displayd failure maps to exactly one
/// variant. No `Internal(u32)` catch-all: protocol-level errors are
/// structural and the client must be able to distinguish them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// All buffers are currently Queued or Displayed; `acquire` would block.
    /// The client should retry after a release.
    NoFreeBuffer,
    /// A buffer was committed twice without an intervening release cycle
    /// (buffer is already Queued or Displayed).
    DoubleCommit,
    /// The `seq` in the commit/release does not match the `seq` assigned at
    /// acquire time. The client is replaying or reusing a stale sequence.
    StaleSequence,
    /// The capability token does not match this surface. The client is
    /// attempting to operate on a surface it does not own.
    ForeignSurface,
    /// The buffer was not acquired (state is Free) or was already released.
    UnacquiredBuffer,
    /// A damage rect has zero width/height, or extends beyond surface bounds.
    InvalidRect,
    /// `pitch * height` overflows `u32` — the backing buffer cannot be
    /// allocated.
    PitchOverflow,
    /// `buffer_index` is out of range for the surface's buffer count.
    BufferOverflow,
    /// The surface capability token is invalid (destroyed, revoked, or never
    /// existed). Also returned for any operation on a destroyed surface.
    InvalidCapability,
    /// The compositor currently owns the exclusive framebuffer.
    FramebufferBusy,
    /// A framebuffer lease is waiting for owner-side release acknowledgement.
    LeaseTransitioning,
    /// A release acknowledgement is required before ownership can change.
    ReleaseRequired,
    /// The lease handle does not identify the current lifecycle generation.
    StaleLease,
    /// A terminal lease release was repeated; no side effect was performed.
    AlreadyReleased,
    /// Lease-side preparation or completion failed; coordinator stays closed.
    LeaseIoFailure,
    /// Lease generation counter cannot advance safely.
    LeaseGenerationExhausted,
}

// ----- Buffer state machine -----

/// Lifecycle state of a single backing buffer.
///
/// ```text
///   Free ──acquire──▶ Drawing ──commit──▶ Queued ──flip──▶ Displayed ──release──▶ Free
/// ```
///
/// - `Free → Drawing`: client acquires a buffer for writing.
/// - `Drawing → Queued`: client commits; nonblocking, displayd schedules display.
/// - `Queued → Displayed`: displayd flips this buffer to the screen.
/// - `Displayed → Free`: client releases (or displayd reclaims on destroy).
///
/// One outstanding queued commit per buffer: committing a Queued or Displayed
/// buffer returns `Error::DoubleCommit`. Committing a Free buffer returns
/// `Error::UnacquiredBuffer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferState {
    /// Available for acquisition.
    Free,
    /// Client is writing pixels; not yet committed.
    Drawing,
    /// Client committed; waiting for displayd to flip.
    Queued,
    /// displayd is scaning out this buffer; client must not write.
    Displayed,
}

/// Per-buffer slot in the surface's double-buffer pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferSlot {
    pub state: BufferState,
    /// Sequence number assigned at the most recent acquire. The commit and
    /// release must echo this exact value. Zero means never acquired.
    pub assigned_seq: u64,
}

impl BufferSlot {
    pub const fn free() -> Self {
        BufferSlot { state: BufferState::Free, assigned_seq: 0 }
    }
}

/// Number of backing buffers per surface (double-buffered).
pub const NUM_BUFFERS: usize = 2;

/// Server-owned surface state. displayd allocates and maps the backing memory
/// for every buffer and retains lifecycle ownership (spec §3.3). Clients
/// receive buffer tokens (frame capabilities) they can map for writing, but
/// the server controls layout, lifetime, and reclamation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceState {
    pub surface_cap_token: u64,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub buffers: [BufferSlot; NUM_BUFFERS],
    /// Monotonic counter; incremented on each successful acquire.
    pub next_seq: u64,
    /// True after `destroy()` — all further operations return
    /// `Error::InvalidCapability`.
    pub destroyed: bool,
}

impl SurfaceState {
    /// Create a new surface state. Validates that `pitch * height` does not
    /// overflow `u32` ( PitchOverflow) and that dimensions are non-zero.
    pub fn new(surface_cap_token: u64, width: u32, height: u32, pitch: u32) -> Result<Self, Error> {
        if width == 0 || height == 0 || pitch == 0 {
            return Err(Error::InvalidRect);
        }
        let _bytes = pitch.checked_mul(height).ok_or(Error::PitchOverflow)?;
        Ok(SurfaceState {
            surface_cap_token,
            width,
            height,
            pitch,
            buffers: [BufferSlot::free(), BufferSlot::free()],
            next_seq: 0,
            destroyed: false,
        })
    }

    /// Acquire a free buffer for writing. Returns the buffer index and a
    /// fresh sequence number. Blocks in the client until a Free buffer is
    /// available; the state machine returns `Error::NoFreeBuffer` if both
    /// buffers are Queued or Displayed.
    pub fn acquire(&mut self, surface_cap_token: u64) -> Result<BufferAcquired, Error> {
        if self.destroyed {
            return Err(Error::InvalidCapability);
        }
        if surface_cap_token != self.surface_cap_token {
            return Err(Error::ForeignSurface);
        }
        let idx = self
            .buffers
            .iter()
            .position(|b| b.state == BufferState::Free);
        let idx = match idx {
            Some(i) => i,
            None => return Err(Error::NoFreeBuffer),
        };
        self.next_seq = self.next_seq.checked_add(1).ok_or(Error::StaleSequence)?;
        let seq = self.next_seq;
        self.buffers[idx] = BufferSlot {
            state: BufferState::Drawing,
            assigned_seq: seq,
        };
        Ok(BufferAcquired {
            buffer_index: idx as u8,
            seq,
            ptr_or_offset: 0,
            pitch: self.pitch,
        })
    }

    /// Commit a buffer for display (nonblocking). Validates the damage rects
    /// against surface bounds before transitioning to Queued.
    pub fn commit(
        &mut self,
        surface_cap_token: u64,
        buffer_index: u8,
        seq: u64,
        damage: &DamageList,
    ) -> Result<(), Error> {
        if self.destroyed {
            return Err(Error::InvalidCapability);
        }
        if surface_cap_token != self.surface_cap_token {
            return Err(Error::ForeignSurface);
        }
        let idx = buffer_index as usize;
        if idx >= NUM_BUFFERS {
            return Err(Error::BufferOverflow);
        }
        // Validate damage rects before mutating state.
        for r in damage.rects() {
            if r.w == 0 || r.h == 0 {
                return Err(Error::InvalidRect);
            }
            if r.x.checked_add(r.w).map(|e| e > self.width).unwrap_or(true) {
                return Err(Error::InvalidRect);
            }
            if r.y.checked_add(r.h).map(|e| e > self.height).unwrap_or(true) {
                return Err(Error::InvalidRect);
            }
        }
        let slot = &mut self.buffers[idx];
        match slot.state {
            BufferState::Free => Err(Error::UnacquiredBuffer),
            BufferState::Drawing => {
                if seq != slot.assigned_seq {
                    return Err(Error::StaleSequence);
                }
                slot.state = BufferState::Queued;
                Ok(())
            }
            BufferState::Queued | BufferState::Displayed => Err(Error::DoubleCommit),
        }
    }

    /// displayd flips a Queued buffer to Displayed. Called by the display
    /// server's frame loop, not by clients.
    pub fn flip(&mut self, buffer_index: u8) -> Result<(), Error> {
        let idx = buffer_index as usize;
        if idx >= NUM_BUFFERS {
            return Err(Error::BufferOverflow);
        }
        let slot = &mut self.buffers[idx];
        match slot.state {
            BufferState::Queued => {
                slot.state = BufferState::Displayed;
                Ok(())
            }
            _ => Err(Error::InvalidCapability),
        }
    }

    /// Release a Displayed buffer back to the free pool. The client must
    /// not write to the buffer after release.
    pub fn release(
        &mut self,
        surface_cap_token: u64,
        buffer_index: u8,
        seq: u64,
    ) -> Result<(), Error> {
        if self.destroyed {
            return Err(Error::InvalidCapability);
        }
        if surface_cap_token != self.surface_cap_token {
            return Err(Error::ForeignSurface);
        }
        let idx = buffer_index as usize;
        if idx >= NUM_BUFFERS {
            return Err(Error::BufferOverflow);
        }
        let slot = &mut self.buffers[idx];
        match slot.state {
            BufferState::Displayed => {
                if seq != slot.assigned_seq {
                    return Err(Error::StaleSequence);
                }
                slot.state = BufferState::Free;
                Ok(())
            }
            _ => Err(Error::UnacquiredBuffer),
        }
    }

    /// Destroy the surface. All buffers are released to Free and the
    /// capability token is invalidated. Further operations return
    /// `Error::InvalidCapability`.
    pub fn destroy(&mut self) {
        self.destroyed = true;
        for b in &mut self.buffers {
            b.state = BufferState::Free;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests — host-testable via `rustc --edition 2021 --test display.rs`
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SURFACE_TOKEN_A: u64 = 0xA000_0000_0000_0001;
    const SURFACE_TOKEN_B: u64 = 0xA000_0000_0000_0002;
    const SURFACE_W: u32 = 320;
    const SURFACE_H: u32 = 200;
    const SURFACE_PITCH: u32 = 320 * 4;

    fn fresh_surface() -> SurfaceState {
        SurfaceState::new(SURFACE_TOKEN_A, SURFACE_W, SURFACE_H, SURFACE_PITCH)
            .expect("surface creation should succeed")
    }

    fn full_surface_damage() -> DamageList {
        DamageList::from_rects(&[Rect { x: 0, y: 0, w: SURFACE_W, h: SURFACE_H }])
    }

    // Test 12a: protocol version is explicit and defined.
    #[test]
    fn protocol_version_is_defined() {
        assert!(DISPLAY_PROTOCOL_VERSION >= 1);
    }

    // Test 12b: error enum is exhaustive — every variant is constructible
    // and distinguishable. This test must be updated when a variant is added.
    #[test]
    fn error_enum_is_exhaustive() {
        let errors = [
            Error::NoFreeBuffer,
            Error::DoubleCommit,
            Error::StaleSequence,
            Error::ForeignSurface,
            Error::UnacquiredBuffer,
            Error::InvalidRect,
            Error::PitchOverflow,
            Error::BufferOverflow,
            Error::InvalidCapability,
            Error::FramebufferBusy,
            Error::LeaseTransitioning,
            Error::ReleaseRequired,
            Error::StaleLease,
            Error::AlreadyReleased,
            Error::LeaseIoFailure,
            Error::LeaseGenerationExhausted,
        ];
        // Every variant is distinct.
        for i in 0..errors.len() {
            for j in (i + 1)..errors.len() {
                assert_ne!(errors[i], errors[j], "duplicate error variant at {}", i);
            }
        }
        // Exactly 16 variants — update this test if the enum grows.
        assert_eq!(errors.len(), 16);
    }

    // Test 1: valid lifecycle — 2 buffers alternate 1000 times, no premature reuse.
    #[test]
    fn valid_lifecycle_1000_alternating_commits() {
        let mut s = fresh_surface();
        for _ in 0..1000 {
            // Acquire buf A.
            let a = s.acquire(SURFACE_TOKEN_A).expect("acquire A");
            s.commit(SURFACE_TOKEN_A, a.buffer_index, a.seq, &full_surface_damage())
                .expect("commit A");
            s.flip(a.buffer_index).expect("flip A");

            // Acquire buf B (the other one).
            let b = s.acquire(SURFACE_TOKEN_A).expect("acquire B");
            assert_ne!(b.buffer_index, a.buffer_index, "premature reuse of same buffer");
            s.commit(SURFACE_TOKEN_A, b.buffer_index, b.seq, &full_surface_damage())
                .expect("commit B");
            s.flip(b.buffer_index).expect("flip B");

            // Both displayed now — no free buffer available.
            assert_eq!(
                s.acquire(SURFACE_TOKEN_A).err(),
                Some(Error::NoFreeBuffer),
                "should have no free buffer while both displayed"
            );

            // Release A, then re-acquire A.
            s.release(SURFACE_TOKEN_A, a.buffer_index, a.seq).expect("release A");
            let a2 = s.acquire(SURFACE_TOKEN_A).expect("re-acquire A");
            assert_eq!(a2.buffer_index, a.buffer_index, "should reuse released buffer");
            s.commit(SURFACE_TOKEN_A, a2.buffer_index, a2.seq, &full_surface_damage())
                .expect("commit A2");
            s.flip(a2.buffer_index).expect("flip A2");

            // Release B, then release A2.
            s.release(SURFACE_TOKEN_A, b.buffer_index, b.seq).expect("release B");
            s.release(SURFACE_TOKEN_A, a2.buffer_index, a2.seq).expect("release A2");
        }
    }

    // Test 2: double commit of same buffer → DoubleCommit.
    #[test]
    fn double_commit_returns_double_commit_error() {
        let mut s = fresh_surface();
        let a = s.acquire(SURFACE_TOKEN_A).expect("acquire");
        s.commit(SURFACE_TOKEN_A, a.buffer_index, a.seq, &full_surface_damage())
            .expect("first commit");
        let err = s
            .commit(SURFACE_TOKEN_A, a.buffer_index, a.seq, &full_surface_damage())
            .unwrap_err();
        assert_eq!(err, Error::DoubleCommit);
    }

    // Test 3: commit of foreign-surface capability → ForeignSurface.
    #[test]
    fn commit_with_foreign_token_returns_foreign_surface() {
        let mut s = fresh_surface();
        let a = s.acquire(SURFACE_TOKEN_A).expect("acquire");
        let err = s
            .commit(SURFACE_TOKEN_B, a.buffer_index, a.seq, &full_surface_damage())
            .unwrap_err();
        assert_eq!(err, Error::ForeignSurface);
    }

    // Test 3b: acquire with foreign token → ForeignSurface.
    #[test]
    fn acquire_with_foreign_token_returns_foreign_surface() {
        let mut s = fresh_surface();
        let err = s.acquire(SURFACE_TOKEN_B).unwrap_err();
        assert_eq!(err, Error::ForeignSurface);
    }

    // Test 4: commit of unacquired buffer → UnacquiredBuffer.
    #[test]
    fn commit_of_unacquired_buffer_returns_error() {
        let mut s = fresh_surface();
        let err = s
            .commit(SURFACE_TOKEN_A, 0, 1, &full_surface_damage())
            .unwrap_err();
        assert_eq!(err, Error::UnacquiredBuffer);
    }

    // Test 5: stale-sequence commit → StaleSequence.
    #[test]
    fn stale_sequence_commit_returns_error() {
        let mut s = fresh_surface();
        let a = s.acquire(SURFACE_TOKEN_A).expect("acquire");
        // Use a wrong seq (off by one).
        let err = s
            .commit(SURFACE_TOKEN_A, a.buffer_index, a.seq + 999, &full_surface_damage())
            .unwrap_err();
        assert_eq!(err, Error::StaleSequence);
    }

    // Test 5b: stale-sequence release → StaleSequence.
    #[test]
    fn stale_sequence_release_returns_error() {
        let mut s = fresh_surface();
        let a = s.acquire(SURFACE_TOKEN_A).expect("acquire");
        s.commit(SURFACE_TOKEN_A, a.buffer_index, a.seq, &full_surface_damage())
            .expect("commit");
        s.flip(a.buffer_index).expect("flip");
        let err = s
            .release(SURFACE_TOKEN_A, a.buffer_index, a.seq + 1)
            .unwrap_err();
        assert_eq!(err, Error::StaleSequence);
    }

    // Test 6: destroy-inflight → buffers released, further ops error.
    #[test]
    fn destroy_inflight_releases_buffers_and_errors_further_ops() {
        let mut s = fresh_surface();
        let a = s.acquire(SURFACE_TOKEN_A).expect("acquire");
        s.commit(SURFACE_TOKEN_A, a.buffer_index, a.seq, &full_surface_damage())
            .expect("commit");
        s.flip(a.buffer_index).expect("flip");

        s.destroy();

        // All buffers should be Free now.
        for b in &s.buffers {
            assert_eq!(b.state, BufferState::Free, "buffer should be Free after destroy");
        }
        // Further acquire errors.
        assert_eq!(s.acquire(SURFACE_TOKEN_A).err(), Some(Error::InvalidCapability));
        // Further commit errors.
        assert_eq!(
            s.commit(SURFACE_TOKEN_A, 0, 1, &full_surface_damage()).err(),
            Some(Error::InvalidCapability)
        );
        // Further release errors.
        assert_eq!(
            s.release(SURFACE_TOKEN_A, 0, 1).err(),
            Some(Error::InvalidCapability)
        );
    }

    // Test 7: malformed rectangle (w=0 or h=0) → InvalidRect.
    #[test]
    fn malformed_rectangle_returns_invalid_rect() {
        let mut s = fresh_surface();
        let a = s.acquire(SURFACE_TOKEN_A).expect("acquire");
        let bad = DamageList::from_rects(&[Rect { x: 0, y: 0, w: 0, h: 100 }]);
        let err = s.commit(SURFACE_TOKEN_A, a.buffer_index, a.seq, &bad).unwrap_err();
        assert_eq!(err, Error::InvalidRect);

        // Re-acquire (commit failed, buffer still Drawing).
        let bad2 = DamageList::from_rects(&[Rect { x: 0, y: 0, w: 100, h: 0 }]);
        let err = s.commit(SURFACE_TOKEN_A, a.buffer_index, a.seq, &bad2).unwrap_err();
        assert_eq!(err, Error::InvalidRect);
    }

    // Test 8: overflow rectangle (x+w > surface_w) → InvalidRect.
    #[test]
    fn overflow_rectangle_returns_invalid_rect() {
        let mut s = fresh_surface();
        let a = s.acquire(SURFACE_TOKEN_A).expect("acquire");
        let overflow = DamageList::from_rects(&[
            Rect { x: 310, y: 0, w: 20, h: 10 }, // x+w = 330 > 320
        ]);
        let err = s.commit(SURFACE_TOKEN_A, a.buffer_index, a.seq, &overflow).unwrap_err();
        assert_eq!(err, Error::InvalidRect);
    }

    // Test 8b: clipping utility — Rect::clip_to narrows to bounds.
    #[test]
    fn rect_clip_to_narrows_to_bounds() {
        let bounds = Rect { x: 0, y: 0, w: 100, h: 100 };
        let r = Rect { x: 90, y: 90, w: 50, h: 50 };
        let clipped = r.clip_to(bounds).expect("non-empty intersection");
        assert_eq!(clipped, Rect { x: 90, y: 90, w: 10, h: 10 });

        // No intersection.
        let outside = Rect { x: 200, y: 200, w: 10, h: 10 };
        assert_eq!(outside.clip_to(bounds), None);
    }

    // Test 8c: Rect::new rejects zero-dimension and overflow.
    #[test]
    fn rect_new_rejects_zero_and_overflow() {
        assert_eq!(Rect::new(0, 0, 0, 10), None);
        assert_eq!(Rect::new(0, 0, 10, 0), None);
        assert_eq!(Rect::new(0, 0, 10, 10), Some(Rect { x: 0, y: 0, w: 10, h: 10 }));
        // u32 overflow on x + w.
        assert_eq!(Rect::new(u32::MAX - 5, 0, 10, 10), None);
        // u32 overflow on y + h.
        assert_eq!(Rect::new(0, u32::MAX - 5, 10, 10), None);
    }

    // Test 9: pitch overflow (pitch * height overflows u32) → PitchOverflow.
    #[test]
    fn pitch_overflow_returns_error() {
        let pitch = u32::MAX;
        let height = 2u32;
        let err = SurfaceState::new(SURFACE_TOKEN_A, 100, height, pitch).unwrap_err();
        assert_eq!(err, Error::PitchOverflow);
    }

    // Test 9b: zero dimensions → InvalidRect.
    #[test]
    fn zero_dimensions_return_error() {
        assert_eq!(
            SurfaceState::new(SURFACE_TOKEN_A, 0, 100, 400).err(),
            Some(Error::InvalidRect)
        );
        assert_eq!(
            SurfaceState::new(SURFACE_TOKEN_A, 100, 0, 400).err(),
            Some(Error::InvalidRect)
        );
        assert_eq!(
            SurfaceState::new(SURFACE_TOKEN_A, 100, 100, 0).err(),
            Some(Error::InvalidRect)
        );
    }

    // Test 10: damage fallback — >8 rects → bounding box, bounding_fallback=true.
    #[test]
    fn damage_list_falls_back_to_bounding_box() {
        let rects: [Rect; 10] = [
            Rect { x: 0, y: 0, w: 10, h: 10 },
            Rect { x: 5, y: 5, w: 10, h: 10 },
            Rect { x: 10, y: 10, w: 10, h: 10 },
            Rect { x: 15, y: 15, w: 10, h: 10 },
            Rect { x: 20, y: 20, w: 10, h: 10 },
            Rect { x: 25, y: 25, w: 10, h: 10 },
            Rect { x: 30, y: 30, w: 10, h: 10 },
            Rect { x: 35, y: 35, w: 10, h: 10 },
            Rect { x: 40, y: 40, w: 10, h: 10 },
            Rect { x: 45, y: 45, w: 10, h: 10 },
        ];
        let dl = DamageList::from_rects(&rects);
        assert!(dl.bounding_fallback);
        assert_eq!(dl.count, 1);
        // Bounding box: x=0, y=0, right=55, bottom=55.
        assert_eq!(dl.rects[0], Rect { x: 0, y: 0, w: 55, h: 55 });
    }

    // Test 10b: damage list with <=8 rects — no fallback.
    #[test]
    fn damage_list_under_limit_no_fallback() {
        let rects = [
            Rect { x: 0, y: 0, w: 10, h: 10 },
            Rect { x: 20, y: 20, w: 10, h: 10 },
        ];
        let dl = DamageList::from_rects(&rects);
        assert!(!dl.bounding_fallback);
        assert_eq!(dl.count, 2);
        assert_eq!(dl.rects().len(), 2);
    }

    // Test 10c: empty damage list.
    #[test]
    fn empty_damage_list() {
        let dl = DamageList::empty();
        assert_eq!(dl.count, 0);
        assert!(!dl.bounding_fallback);
        assert!(dl.rects().is_empty());
    }

    // Test 11: release ordering — release of non-displayed buffer → error.
    #[test]
    fn release_of_non_displayed_buffer_returns_error() {
        let mut s = fresh_surface();
        let a = s.acquire(SURFACE_TOKEN_A).expect("acquire");

        // Release while Drawing (not yet committed) → error.
        let err = s.release(SURFACE_TOKEN_A, a.buffer_index, a.seq).unwrap_err();
        assert_eq!(err, Error::UnacquiredBuffer);

        // Commit but don't flip — release while Queued → error.
        s.commit(SURFACE_TOKEN_A, a.buffer_index, a.seq, &full_surface_damage())
            .expect("commit");
        let err = s.release(SURFACE_TOKEN_A, a.buffer_index, a.seq).unwrap_err();
        assert_eq!(err, Error::UnacquiredBuffer);

        // Flip to Displayed — release now succeeds.
        s.flip(a.buffer_index).expect("flip");
        s.release(SURFACE_TOKEN_A, a.buffer_index, a.seq).expect("release after flip");
    }

    // Test 11b: buffer_index out of range → BufferOverflow.
    #[test]
    fn buffer_index_out_of_range_returns_buffer_overflow() {
        let mut s = fresh_surface();
        let err = s
            .commit(SURFACE_TOKEN_A, 5, 1, &full_surface_damage())
            .unwrap_err();
        assert_eq!(err, Error::BufferOverflow);

        let err = s.release(SURFACE_TOKEN_A, 5, 1).unwrap_err();
        assert_eq!(err, Error::BufferOverflow);
    }

    // Test 11c: flip of non-queued buffer → InvalidCapability.
    #[test]
    fn flip_of_non_queued_buffer_returns_error() {
        let mut s = fresh_surface();
        // Flip a Free buffer.
        let err = s.flip(0).unwrap_err();
        assert_eq!(err, Error::InvalidCapability);

        let a = s.acquire(SURFACE_TOKEN_A).expect("acquire");
        // Flip a Drawing buffer.
        let err = s.flip(a.buffer_index).unwrap_err();
        assert_eq!(err, Error::InvalidCapability);
    }

    // Test 11d: full lifecycle state transitions are exact.
    #[test]
    fn state_transitions_are_exact() {
        let mut s = fresh_surface();
        assert_eq!(s.buffers[0].state, BufferState::Free);
        assert_eq!(s.buffers[1].state, BufferState::Free);

        let a = s.acquire(SURFACE_TOKEN_A).expect("acquire");
        assert_eq!(s.buffers[a.buffer_index as usize].state, BufferState::Drawing);

        s.commit(SURFACE_TOKEN_A, a.buffer_index, a.seq, &full_surface_damage())
            .expect("commit");
        assert_eq!(s.buffers[a.buffer_index as usize].state, BufferState::Queued);

        s.flip(a.buffer_index).expect("flip");
        assert_eq!(s.buffers[a.buffer_index as usize].state, BufferState::Displayed);

        s.release(SURFACE_TOKEN_A, a.buffer_index, a.seq).expect("release");
        assert_eq!(s.buffers[a.buffer_index as usize].state, BufferState::Free);
    }

    // Test 11e: seq monotonicity — each acquire gets a strictly higher seq.
    #[test]
    fn acquire_seq_is_monotonic() {
        let mut s = fresh_surface();
        let a = s.acquire(SURFACE_TOKEN_A).expect("acquire A");
        let b = s.acquire(SURFACE_TOKEN_A).expect("acquire B");
        assert!(b.seq > a.seq, "seq must be monotonic");
    }

    // Test 12: PixelFormat bytes_per_pixel.
    #[test]
    fn pixel_format_bytes_per_pixel() {
        assert_eq!(PixelFormat::Xrgb8888.bytes_per_pixel(), 4);
    }

    // Test 12b: Rect::extend / bounding_box utilities.
    #[test]
    fn rect_extend_and_bounding_box() {
        let r1 = Rect { x: 0, y: 0, w: 10, h: 10 };
        let r2 = Rect { x: 5, y: 5, w: 10, h: 10 };
        assert_eq!(r1.extend(r2), Rect { x: 0, y: 0, w: 15, h: 15 });

        let bb = Rect::bounding_box(&[r1, r2]).expect("non-empty");
        assert_eq!(bb, Rect { x: 0, y: 0, w: 15, h: 15 });

        assert_eq!(Rect::bounding_box(&[]), None);
    }

    // Test 12c: Geometry and Destroy wire types are constructible.
    #[test]
    fn geometry_and_destroy_constructible() {
        let g = Geometry {
            surface_cap_token: SURFACE_TOKEN_A,
            x: 100,
            y: 50,
            z_order: 0,
            visible: true,
        };
        assert!(g.visible);
        assert_eq!(g.z_order, 0);

        let d = Destroy { surface_cap_token: SURFACE_TOKEN_A };
        assert_eq!(d.surface_cap_token, SURFACE_TOKEN_A);
    }
}
