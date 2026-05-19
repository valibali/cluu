# Window Protocol Formalization Implementation Plan

> **For agentic workers:** Self-contained for handoff (target: deepseek v4 pro). Each step: exact paths + complete code + verification commands. Steps use checkbox (`- [ ]`).

**Goal:** Formalize the Wayland-shape protocol the compositor already speaks informally. 16 verb labels (210-226). Client-owned typed-frame buffers (via existing MAP_SHARE_PHYS, plus spec 1's frame typing). Per-frame request frame-callback (retires `broadcast_frame_ready`). Explicit `WIN_BUFFER_RELEASED` event. Pre-translated input. Surface-to-session integration consuming `SESSION_ENDED` from plan 3.

**Architecture:** New `cluu_proto::window` module defines all types + labels. Compositor's `state::Window` becomes a formal `Surface` with state machine (`Created → BufferAttached → Mapped → Closing → Destroyed`). Per-client async event endpoint (replaces global `compositor:input`). Compositor reads keymap from `/etc/keymap/<layout>.toml`, emits pre-translated `KeyEvent`s. Surface `session_id` filled from `WIN_CREATE.session_token`; `SESSION_ENDED` event closes matching surfaces.

**Tech Stack:** Rust 2021, postcard 1.x, bitflags 2.4, `cluu_proto` (plan 1).

**Reference spec:** `docs/superpowers/specs/2026-05-18-window-protocol-design.md`.

**Prerequisites:**
- Plan 1 tasks 1-4 (cluu_proto crate); typed-frame discipline (already landed per `frame-typing-redesign-landed-2026-05-18`).
- Plan 3 task 5 (compositor subscribes to SESSION_ENDED). If plan 3 not landed, the surface-to-session integration parts of this plan stub session_id = None.

Plan 4 is the last of the four-spec sequence. Run after plans 1-3 are at least partially landed.

---

## File Structure

### New files

- `userspace/cluu_proto/src/window.rs` — labels 210-226, all request/reply/event types, `Termios`-like layout, `InputEvent` variants.
- `userspace/libcluu/src/window.rs` — client-side wrappers (`create`, `attach_buffer`, `commit`, `request_frame_callback`, etc.) + `SurfaceBufferPool` helper.
- `userspace/compositor/src/surface.rs` — `Surface` typed object, state machine, buffer table.
- `userspace/compositor/src/buffer_table.rs` — per-surface buffer state (Detached/Attached/Pending/Scanout/ReleasedLocked).
- `/etc/keymap/us.toml` — default US keymap (compiled-in if file absent).

### Modified files

- `userspace/cluu_proto/src/lib.rs` — declare `window` module.
- `userspace/libcluu/src/lib.rs` — pub use new module.
- `userspace/compositor/src/main.rs` — dispatch for 9 client-facing verbs + render loop using new state machine.
- `userspace/compositor/src/protocol.rs` — replace legacy `COMP_WIN_*` constants with `cluu_proto::window::*`.
- `userspace/compositor/src/state.rs` — surface table + per-client event endpoints + keymap state + tracked_sessions.
- `userspace/compositor/src/render.rs` — read from Surface.buffers per state.
- `userspace/compositor/src/compose.rs` — damage-rect-aware composition.
- `userspace/cluuterm/src/main.rs` — flips to `libcluu::window` for surface management.
- `userspace/cluuterm/src/render.rs` — uses `SurfaceBufferPool`.
- `userspace/login/src/main.rs` — flips to `libcluu::window` for its single window.

---

## Build / verify cheat sheet

- Build: `cargo xtask build`.
- Lint: `cargo clippy --workspace --all-targets -- -D warnings`.
- Single crate: `cargo build -p <crate>` (`cluu_proto`, `libcluu`, `compositor`, `cluuterm`, `login`).
- Boot smoke: `bash scripts/harness_run.sh` (expect `compositor: ready` + login window).
- Visual smoke: `scripts/fb_dump.sh` (per `reference_fb_dump`) captures the framebuffer.
- Marker: `HARNESS_FORCE_BUILD=1 MARKER_MODE=<m> bash scripts/harness_run.sh; grep "<m>:" serial.log`.

---

## Task 1: `cluu_proto::window` types + labels

**Files:**
- Create: `userspace/cluu_proto/src/window.rs`
- Modify: `userspace/cluu_proto/src/lib.rs`

- [ ] **Step 1: Declare module**

In `userspace/cluu_proto/src/lib.rs`:

```rust
pub mod window;
```

- [ ] **Step 2: Write `userspace/cluu_proto/src/window.rs`**

```rust
//! Window protocol — see spec 4.

use alloc::string::String;
use alloc::vec::Vec;
use bitflags::bitflags;
use serde::{Deserialize, Serialize};

use crate::TokenHandle;

// ----- Verb labels -----

// Client → compositor (compositor:client):
pub const WIN_CREATE_LABEL:                 u32 = 210;
pub const WIN_DESTROY_LABEL:                u32 = 211;
pub const WIN_ATTACH_BUFFER_LABEL:          u32 = 212;
pub const WIN_DETACH_BUFFER_LABEL:          u32 = 213;
pub const WIN_COMMIT_LABEL:                 u32 = 214;
pub const WIN_REQUEST_FRAME_CALLBACK_LABEL: u32 = 215;
pub const WIN_SET_TITLE_LABEL:              u32 = 216;
pub const WIN_SET_GEOMETRY_HINT_LABEL:      u32 = 217;
pub const WIN_REQUEST_FOCUS_LABEL:          u32 = 218;

// Compositor → client (per-client async endpoint):
pub const WIN_FRAME_READY_LABEL:            u32 = 220;
pub const WIN_BUFFER_RELEASED_LABEL:        u32 = 221;
pub const WIN_CONFIGURE_LABEL:              u32 = 222;
pub const WIN_INPUT_LABEL:                  u32 = 223;
pub const WIN_FOCUS_IN_LABEL:               u32 = 224;
pub const WIN_FOCUS_OUT_LABEL:              u32 = 225;
pub const WIN_CLOSED_LABEL:                 u32 = 226;

// ----- Geometry -----

pub type SurfaceId = u32;
pub type BufferId  = u32;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Rect { pub x: i32, pub y: i32, pub w: u32, pub h: u32 }

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Size { pub w: u32, pub h: u32 }

// ----- Pixel format -----

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum PixelFormat {
    Bgra8888,
    Rgba8888,
    Rgb565,
}

impl PixelFormat {
    pub fn bytes_per_pixel(self) -> u32 {
        match self {
            PixelFormat::Bgra8888 | PixelFormat::Rgba8888 => 4,
            PixelFormat::Rgb565 => 2,
        }
    }
}

// ----- Errors -----

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum WinErr {
    InvalidSurface,
    InvalidBuffer,
    InvalidFormat,
    GeometryRejected,
    SessionRevoked,
    NotFocused,
    Internal(u32),
}

// ----- Requests -----

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WinCreateRequest {
    pub session_token: Option<TokenHandle>,
    pub initial_size:  Size,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WinCreateReply { pub surface_id: SurfaceId }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WinAttachBufferRequest {
    pub surface_id:   SurfaceId,
    pub buffer_id:    BufferId,
    pub frame_token:  TokenHandle,
    pub pixel_format: PixelFormat,
    pub stride:       u32,
    pub width:        u32,
    pub height:       u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WinCommitRequest {
    pub surface_id: SurfaceId,
    pub buffer_id:  BufferId,
    pub damage:     Vec<Rect>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WinRequestFrameCallbackRequest { pub surface_id: SurfaceId }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WinSetTitleRequest { pub surface_id: SurfaceId, pub title: String }

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GeometryHints {
    pub min_size:       Option<Size>,
    pub max_size:       Option<Size>,
    pub preferred_size: Option<Size>,
    pub fixed_aspect:   Option<(u32, u32)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WinSetGeometryHintRequest {
    pub surface_id: SurfaceId,
    pub hints: GeometryHints,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WinRequestFocusRequest { pub surface_id: SurfaceId }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WinDestroyRequest { pub surface_id: SurfaceId }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WinDetachBufferRequest {
    pub surface_id: SurfaceId,
    pub buffer_id:  BufferId,
}

// ----- Async events (compositor → client) -----

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WinFrameReadyEvent     { pub surface_id: SurfaceId, pub timestamp_ms: u64 }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WinBufferReleasedEvent { pub surface_id: SurfaceId, pub buffer_id: BufferId }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WinConfigureEvent      { pub surface_id: SurfaceId, pub size: Size, pub scale: u32 }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WinInputEvent          { pub surface_id: SurfaceId, pub event: InputEvent }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WinFocusInEvent        { pub surface_id: SurfaceId }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WinFocusOutEvent       { pub surface_id: SurfaceId }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WinClosedEvent         { pub surface_id: SurfaceId }

// ----- Input -----

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum InputEvent {
    Key(KeyEvent),
    Pointer(PointerEvent),
    Wheel(WheelEvent),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyEvent {
    pub key:       Key,
    pub modifiers: Modifiers,
    pub state:     KeyState,
    pub char:      Option<char>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PointerEvent {
    pub kind:      PointerKind,
    pub pos:       (i32, i32),
    pub button:    Option<MouseButton>,
    pub state:     Option<KeyState>,
    pub modifiers: Modifiers,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WheelEvent {
    pub pos:       (i32, i32),
    pub delta_x:   i32,
    pub delta_y:   i32,
    pub modifiers: Modifiers,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Key {
    A, B, C, D, E, F, G, H, I, J, K, L, M,
    N, O, P, Q, R, S, T, U, V, W, X, Y, Z,
    Digit0, Digit1, Digit2, Digit3, Digit4,
    Digit5, Digit6, Digit7, Digit8, Digit9,
    Enter, Esc, Backspace, Tab, Space,
    ArrowUp, ArrowDown, ArrowLeft, ArrowRight,
    F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12,
    LeftCtrl, RightCtrl, LeftShift, RightShift,
    LeftAlt, RightAlt, LeftSuper, RightSuper,
    PageUp, PageDown, Home, End, Insert, Delete,
    Minus, Equal, LeftBracket, RightBracket, Backslash,
    Semicolon, Apostrophe, Comma, Period, Slash, Backtick,
    Unknown(u32),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum KeyState { Pressed, Released, Repeat }

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Modifiers {
    pub ctrl:   bool,
    pub shift:  bool,
    pub alt:    bool,
    pub super_: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum PointerKind { Motion, Button, Enter, Leave }

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum MouseButton { Left, Right, Middle, Side(u32) }

bitflags! {
    #[derive(Debug, Clone, Copy, Serialize, Deserialize)]
    pub struct PollEvents: u32 {
        const POLLIN  = 0x1;
        const POLLOUT = 0x2;
        const POLLHUP = 0x4;
        const POLLERR = 0x8;
    }
}
```

- [ ] **Step 3: Add round-trip tests**

Append:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn pixel_format_roundtrip() {
        let f = PixelFormat::Bgra8888;
        let bytes = postcard::to_allocvec(&f).unwrap();
        let decoded: PixelFormat = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, f);
        assert_eq!(decoded.bytes_per_pixel(), 4);
    }

    #[test]
    fn commit_with_damage_roundtrip() {
        let req = WinCommitRequest {
            surface_id: 7,
            buffer_id: 1,
            damage: vec![
                Rect { x: 0, y: 0, w: 100, h: 50 },
                Rect { x: 200, y: 100, w: 50, h: 50 },
            ],
        };
        let bytes = postcard::to_allocvec(&req).unwrap();
        let decoded: WinCommitRequest = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.damage.len(), 2);
        assert_eq!(decoded.surface_id, 7);
    }

    #[test]
    fn key_event_roundtrip() {
        let ev = KeyEvent {
            key: Key::A,
            modifiers: Modifiers { ctrl: false, shift: true, alt: false, super_: false },
            state: KeyState::Pressed,
            char: Some('A'),
        };
        let bytes = postcard::to_allocvec(&ev).unwrap();
        let decoded: KeyEvent = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.key, Key::A);
        assert_eq!(decoded.char, Some('A'));
        assert!(decoded.modifiers.shift);
    }

    #[test]
    fn input_event_pointer_roundtrip() {
        let ev = InputEvent::Pointer(PointerEvent {
            kind: PointerKind::Motion,
            pos: (123, 456),
            button: None,
            state: None,
            modifiers: Modifiers::default(),
        });
        let bytes = postcard::to_allocvec(&ev).unwrap();
        let decoded: InputEvent = postcard::from_bytes(&bytes).unwrap();
        match decoded {
            InputEvent::Pointer(p) => {
                assert_eq!(p.pos, (123, 456));
                assert_eq!(p.kind, PointerKind::Motion);
            }
            _ => panic!("expected pointer"),
        }
    }

    #[test]
    fn winerr_roundtrip() {
        let e = WinErr::InvalidBuffer;
        let bytes = postcard::to_allocvec(&e).unwrap();
        let decoded: WinErr = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, e);
    }
}
```

- [ ] **Step 4: Build + test**

```
cd /home/vlb2bp/git/cluu
cargo build -p cluu_proto
cargo test -p cluu_proto --features host-test
```

Expected: build clean; 5 new tests pass.

- [ ] **Step 5: Commit**

```bash
git add userspace/cluu_proto/src/lib.rs userspace/cluu_proto/src/window.rs
git commit -m "feat(cluu_proto): window module — 16 labels + Wayland-shape types"
```

---

## Task 2: `libcluu::window` client wrappers + `SurfaceBufferPool`

**Files:**
- Create: `userspace/libcluu/src/window.rs`
- Modify: `userspace/libcluu/src/lib.rs`

- [ ] **Step 1: Write `userspace/libcluu/src/window.rs`**

```rust
//! Client-side wrappers around the compositor's window verbs.

use cluu_proto::ABI_VERSION;
use cluu_proto::TokenHandle;
use cluu_proto::window::*;

fn build_words(payload_len: usize) -> [u64; 6] {
    let mut w = [0u64; 6];
    w[0] = payload_len as u64;
    w[1] = ABI_VERSION as u64;
    w
}

fn call_compositor<Req, Rep>(label: u32, request: Req) -> Result<Rep, WinErr>
where
    Req: serde::Serialize,
    Rep: for<'de> serde::Deserialize<'de>,
{
    let payload = postcard::to_allocvec(&request)
        .map_err(|_| WinErr::Internal(0xE_SER))?;
    let words = build_words(payload.len());
    let endpoint = compositor_client_endpoint()?;
    let reply = crate::ipc::call(endpoint, label, words, &payload)
        .map_err(|_| WinErr::Internal(0xE_COMPOSITOR_DEAD))?;
    let result: Rep = postcard::from_bytes(&reply.payload)
        .map_err(|_| WinErr::Internal(0xE_DESER))?;
    Ok(result)
}

fn compositor_client_endpoint() -> Result<crate::ipc::EndpointHandle, WinErr> {
    static CACHED: spin::Mutex<Option<crate::ipc::EndpointHandle>> = spin::Mutex::new(None);
    let mut g = CACHED.lock();
    if let Some(e) = *g { return Ok(e); }
    let e = crate::registry::lookup("compositor:client")
        .map_err(|_| WinErr::Internal(0xE_NO_COMPOSITOR))?;
    *g = Some(e);
    Ok(e)
}

pub fn create(session: Option<TokenHandle>, size: Size) -> Result<SurfaceId, WinErr> {
    let req = WinCreateRequest { session_token: session, initial_size: size };
    let reply: Result<WinCreateReply, WinErr> = call_compositor(WIN_CREATE_LABEL, req)?;
    reply.map(|r| r.surface_id)
}

pub fn destroy(surface_id: SurfaceId) -> Result<(), WinErr> {
    let reply: Result<(), WinErr> = call_compositor(WIN_DESTROY_LABEL,
        WinDestroyRequest { surface_id })?;
    reply
}

pub fn attach_buffer(
    surface_id: SurfaceId, buffer_id: BufferId, frame_token: TokenHandle,
    fmt: PixelFormat, stride: u32, size: Size,
) -> Result<(), WinErr> {
    let reply: Result<(), WinErr> = call_compositor(WIN_ATTACH_BUFFER_LABEL,
        WinAttachBufferRequest {
            surface_id, buffer_id, frame_token,
            pixel_format: fmt, stride, width: size.w, height: size.h,
        })?;
    reply
}

pub fn detach_buffer(surface_id: SurfaceId, buffer_id: BufferId) -> Result<(), WinErr> {
    let reply: Result<(), WinErr> = call_compositor(WIN_DETACH_BUFFER_LABEL,
        WinDetachBufferRequest { surface_id, buffer_id })?;
    reply
}

pub fn commit(surface_id: SurfaceId, buffer_id: BufferId, damage: &[Rect])
    -> Result<(), WinErr>
{
    let reply: Result<(), WinErr> = call_compositor(WIN_COMMIT_LABEL,
        WinCommitRequest { surface_id, buffer_id, damage: damage.to_vec() })?;
    reply
}

pub fn request_frame_callback(surface_id: SurfaceId) -> Result<(), WinErr> {
    let reply: Result<(), WinErr> = call_compositor(WIN_REQUEST_FRAME_CALLBACK_LABEL,
        WinRequestFrameCallbackRequest { surface_id })?;
    reply
}

pub fn set_title(surface_id: SurfaceId, title: &str) -> Result<(), WinErr> {
    let reply: Result<(), WinErr> = call_compositor(WIN_SET_TITLE_LABEL,
        WinSetTitleRequest { surface_id, title: alloc::string::String::from(title) })?;
    reply
}

pub fn set_geometry_hint(surface_id: SurfaceId, hints: GeometryHints) -> Result<(), WinErr> {
    let reply: Result<(), WinErr> = call_compositor(WIN_SET_GEOMETRY_HINT_LABEL,
        WinSetGeometryHintRequest { surface_id, hints })?;
    reply
}

pub fn request_focus(surface_id: SurfaceId) -> Result<bool, WinErr> {
    let reply: Result<bool, WinErr> = call_compositor(WIN_REQUEST_FOCUS_LABEL,
        WinRequestFocusRequest { surface_id })?;
    reply
}

// ----- Event delivery (client's per-client async endpoint) -----

#[derive(Clone, Debug)]
pub enum WindowEvent {
    FrameReady(WinFrameReadyEvent),
    BufferReleased(WinBufferReleasedEvent),
    Configure(WinConfigureEvent),
    Input(WinInputEvent),
    FocusIn(WinFocusInEvent),
    FocusOut(WinFocusOutEvent),
    Closed(WinClosedEvent),
}

pub fn recv_event() -> Result<WindowEvent, WinErr> {
    let ep = my_event_endpoint()?;
    let msg = crate::ipc::recv(ep).map_err(|_| WinErr::Internal(0xE_RECV))?;
    match msg.label {
        WIN_FRAME_READY_LABEL => {
            let e: WinFrameReadyEvent = postcard::from_bytes(&msg.payload)
                .map_err(|_| WinErr::Internal(0xE_DESER))?;
            Ok(WindowEvent::FrameReady(e))
        }
        WIN_BUFFER_RELEASED_LABEL => {
            let e: WinBufferReleasedEvent = postcard::from_bytes(&msg.payload)
                .map_err(|_| WinErr::Internal(0xE_DESER))?;
            Ok(WindowEvent::BufferReleased(e))
        }
        WIN_CONFIGURE_LABEL => {
            let e: WinConfigureEvent = postcard::from_bytes(&msg.payload)
                .map_err(|_| WinErr::Internal(0xE_DESER))?;
            Ok(WindowEvent::Configure(e))
        }
        WIN_INPUT_LABEL => {
            let e: WinInputEvent = postcard::from_bytes(&msg.payload)
                .map_err(|_| WinErr::Internal(0xE_DESER))?;
            Ok(WindowEvent::Input(e))
        }
        WIN_FOCUS_IN_LABEL => {
            let e: WinFocusInEvent = postcard::from_bytes(&msg.payload)
                .map_err(|_| WinErr::Internal(0xE_DESER))?;
            Ok(WindowEvent::FocusIn(e))
        }
        WIN_FOCUS_OUT_LABEL => {
            let e: WinFocusOutEvent = postcard::from_bytes(&msg.payload)
                .map_err(|_| WinErr::Internal(0xE_DESER))?;
            Ok(WindowEvent::FocusOut(e))
        }
        WIN_CLOSED_LABEL => {
            let e: WinClosedEvent = postcard::from_bytes(&msg.payload)
                .map_err(|_| WinErr::Internal(0xE_DESER))?;
            Ok(WindowEvent::Closed(e))
        }
        _ => Err(WinErr::Internal(0xE_UNKNOWN_LABEL)),
    }
}

fn my_event_endpoint() -> Result<crate::ipc::EndpointHandle, WinErr> {
    // Each WIN_CREATE reply includes the per-client event endpoint via
    // side channel (or via reading the surface's compositor-minted cap
    // in the caller's table). The engineer wires this to the actual
    // mechanism — likely a static set when the first WIN_CREATE returns
    // a reply containing the event endpoint.
    crate::process::my_compositor_event_endpoint()
        .ok_or(WinErr::Internal(0xE_NO_EVENT_EP))
}

// ----- SurfaceBufferPool helper -----

pub struct SurfaceBufferPool {
    pub surface_id: SurfaceId,
    pub size:       Size,
    pub format:     PixelFormat,
    pub stride:     u32,
    bufs:           [PoolBuffer; 2],
    next_writable:  usize,
    pending_release: Option<BufferId>,
}

pub struct PoolBuffer {
    pub id:          BufferId,
    pub frame_token: TokenHandle,
    pub mapped_addr: usize,
    pub in_use:      bool,
}

impl SurfaceBufferPool {
    pub fn new(surface_id: SurfaceId, size: Size, format: PixelFormat)
        -> Result<Self, WinErr>
    {
        let stride = size.w * format.bytes_per_pixel();
        let bufs = [
            Self::alloc_buffer(0, size, format, stride)?,
            Self::alloc_buffer(1, size, format, stride)?,
        ];
        for b in &bufs {
            attach_buffer(surface_id, b.id, b.frame_token, format, stride, size)?;
        }
        Ok(Self { surface_id, size, format, stride, bufs,
                  next_writable: 0, pending_release: None })
    }

    fn alloc_buffer(id: BufferId, size: Size, format: PixelFormat, stride: u32)
        -> Result<PoolBuffer, WinErr>
    {
        let bytes_needed = (stride * size.h) as usize;
        let (frame_token, mapped_addr) = crate::frame::alloc_user_data_frame(bytes_needed)
            .map_err(|_| WinErr::Internal(0xE_NO_FRAME))?;
        Ok(PoolBuffer { id, frame_token, mapped_addr, in_use: false })
    }

    /// Return a mutable view of the next-writable buffer (the one
    /// compositor isn't currently scanning out).
    pub fn next_writable(&mut self) -> (&mut PoolBuffer, &mut [u8]) {
        let idx = self.next_writable;
        let buf = &mut self.bufs[idx];
        let bytes_len = (self.stride * self.size.h) as usize;
        let slice = unsafe { core::slice::from_raw_parts_mut(buf.mapped_addr as *mut u8, bytes_len) };
        (buf, slice)
    }

    pub fn commit(&mut self, damage: &[Rect]) -> Result<(), WinErr> {
        let id = self.bufs[self.next_writable].id;
        commit(self.surface_id, id, damage)?;
        self.bufs[self.next_writable].in_use = true;
        self.next_writable = 1 - self.next_writable;
        Ok(())
    }

    pub fn on_release(&mut self, id: BufferId) {
        for b in self.bufs.iter_mut() {
            if b.id == id { b.in_use = false; }
        }
    }
}
```

- [ ] **Step 2: Add module to libcluu**

In `userspace/libcluu/src/lib.rs`:

```rust
pub mod window;
```

The helpers `crate::frame::alloc_user_data_frame` and `crate::process::my_compositor_event_endpoint` may need to be added — engineer wires them to existing typed-frame allocation and the compositor-event endpoint accessor (the latter populated when the first `WIN_CREATE` reply is received, see Task 4).

- [ ] **Step 3: Build**

```
cd /home/vlb2bp/git/cluu
cargo build -p libcluu
```

Expected: clean (after wiring placeholders).

- [ ] **Step 4: Commit**

```bash
git add userspace/libcluu/src/window.rs userspace/libcluu/src/lib.rs
git commit -m "feat(libcluu): window module + SurfaceBufferPool"
```

---

## Task 3: Compositor `Surface` typed object + buffer state machine

**Files:**
- Create: `userspace/compositor/src/surface.rs`
- Create: `userspace/compositor/src/buffer_table.rs`
- Modify: `userspace/compositor/src/state.rs`

- [ ] **Step 1: Write `userspace/compositor/src/buffer_table.rs`**

```rust
//! Per-surface buffer state machine. Spec 4 §6.

use alloc::collections::BTreeMap;
use cluu_proto::window::{BufferId, PixelFormat, Rect};
use cluu_proto::TokenHandle;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BufferState {
    Detached,
    Attached,
    Pending,
    Scanout,
    ReleasedLocked,
}

#[derive(Clone, Debug)]
pub struct BufferRecord {
    pub id:          BufferId,
    pub frame_token: TokenHandle,
    pub state:       BufferState,
    pub pixel_format: PixelFormat,
    pub stride:      u32,
    pub width:       u32,
    pub height:      u32,
    pub damage:      alloc::vec::Vec<Rect>,
    pub mapped_addr: Option<usize>,
}

#[derive(Default)]
pub struct BufferTable {
    pub entries: BTreeMap<BufferId, BufferRecord>,
}

impl BufferTable {
    pub fn attach(&mut self, b: BufferRecord) {
        self.entries.insert(b.id, b);
    }

    pub fn detach(&mut self, id: BufferId) -> Option<BufferRecord> {
        let e = self.entries.get(&id)?;
        // Cannot detach if Scanout/Pending/ReleasedLocked.
        match e.state {
            BufferState::Attached | BufferState::Detached => self.entries.remove(&id),
            _ => None,
        }
    }

    /// Mark a buffer as Pending (commit).
    pub fn commit(&mut self, id: BufferId, damage: alloc::vec::Vec<Rect>) -> bool {
        if let Some(e) = self.entries.get_mut(&id) {
            // If something else was Pending, release it immediately (it never scanned out).
            for (_, other) in self.entries.iter_mut() {
                if other.state == BufferState::Pending && other.id != id {
                    other.state = BufferState::ReleasedLocked;
                }
            }
            e.state = BufferState::Pending;
            e.damage = damage;
            true
        } else { false }
    }

    /// Render-tick: promote Pending → Scanout; previous Scanout → ReleasedLocked.
    pub fn promote_for_render(&mut self) {
        let mut had_scanout: Option<BufferId> = None;
        let mut new_scanout: Option<BufferId> = None;
        for (_, e) in self.entries.iter() {
            if e.state == BufferState::Scanout { had_scanout = Some(e.id); }
            if e.state == BufferState::Pending { new_scanout = Some(e.id); }
        }
        if let Some(id) = new_scanout {
            if let Some(e) = self.entries.get_mut(&id) {
                e.state = BufferState::Scanout;
            }
            if let Some(old) = had_scanout {
                if let Some(e) = self.entries.get_mut(&old) {
                    e.state = BufferState::ReleasedLocked;
                }
            }
        }
    }

    /// After render: transition ReleasedLocked → Attached and yield released ids.
    pub fn take_released(&mut self) -> alloc::vec::Vec<BufferId> {
        let mut out = alloc::vec::Vec::new();
        for (_, e) in self.entries.iter_mut() {
            if e.state == BufferState::ReleasedLocked {
                e.state = BufferState::Attached;
                out.push(e.id);
            }
        }
        out
    }

    pub fn scanout_record(&self) -> Option<&BufferRecord> {
        self.entries.values().find(|e| e.state == BufferState::Scanout)
    }
}
```

- [ ] **Step 2: Write `userspace/compositor/src/surface.rs`**

```rust
//! Per-surface typed object. Spec 4 §9.

use cluu_proto::window::{GeometryHints, Size, SurfaceId};
use crate::buffer_table::BufferTable;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceState {
    Created,
    BufferAttached,
    Mapped,
    Unmapped,
    Closing,
    Destroyed,
}

#[derive(Debug)]
pub struct Surface {
    pub id:                       SurfaceId,
    pub client_pid:               u32,
    pub client_endpoint:          u64,       // per-client async event endpoint
    pub session_id:               Option<u32>,
    pub state:                    SurfaceState,
    pub buffers:                  BufferTable,
    pub frame_callback_requested: bool,
    pub title:                    alloc::string::String,
    pub geometry_hints:           GeometryHints,
    pub size:                     Size,
    pub focused:                  bool,
}

impl Surface {
    pub fn new(id: SurfaceId, client_pid: u32, client_endpoint: u64,
               session_id: Option<u32>, size: Size) -> Self {
        Self {
            id, client_pid, client_endpoint, session_id,
            state: SurfaceState::Created,
            buffers: BufferTable::default(),
            frame_callback_requested: false,
            title: alloc::string::String::new(),
            geometry_hints: GeometryHints::default(),
            size, focused: false,
        }
    }
}
```

- [ ] **Step 3: Add modules + surface table to state**

In `userspace/compositor/src/state.rs`, add module decls + a `surfaces: BTreeMap<SurfaceId, Surface>` field on `Compositor`:

```rust
pub mod surface;
pub mod buffer_table;

pub struct Compositor {
    // ... existing fields ...
    pub surfaces: alloc::collections::BTreeMap<cluu_proto::window::SurfaceId, surface::Surface>,
    pub next_surface_id: u32,
    pub focused_surface: Option<cluu_proto::window::SurfaceId>,
    pub tracked_sessions: alloc::collections::BTreeSet<u32>,  // from plan 3 task 5
}
```

(The engineer fits the new fields alongside existing state.)

- [ ] **Step 4: Build**

```
cd /home/vlb2bp/git/cluu
cargo build -p compositor
```

Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add userspace/compositor/src/surface.rs userspace/compositor/src/buffer_table.rs userspace/compositor/src/state.rs
git commit -m "feat(compositor): formal Surface + BufferTable state machines"
```

---

## Task 4: Per-client async event endpoint

**Files:**
- Modify: `userspace/compositor/src/main.rs`
- Modify: `userspace/libcluu/src/window.rs`

- [ ] **Step 1: Mint per-client endpoint at WIN_CREATE**

In `userspace/compositor/src/main.rs` (Task 5 will add the handler; here just outline the helper):

```rust
fn mint_client_event_endpoint(&mut self, client_pid: u32) -> u64 {
    // Allocate a new endpoint via the kernel; derive an IPC_RECV cap for ourselves
    // and IPC_SEND cap for the client. Store the recv cap; return send cap as u64.
    // Engineer wires to existing endpoint-mint helper.
    libcluu::ipc::create_endpoint_for_client(client_pid)
        .expect("endpoint mint")
}
```

- [ ] **Step 2: WIN_CREATE returns the endpoint to the client**

Extend `WinCreateReply` to include the event endpoint handle:

In `userspace/cluu_proto/src/window.rs`, modify `WinCreateReply`:

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WinCreateReply {
    pub surface_id:        SurfaceId,
    pub event_endpoint:    TokenHandle,    // per-client async events
}
```

Update libcluu's `create` to extract `event_endpoint` and store it for the client:

```rust
pub fn create(session: Option<TokenHandle>, size: Size) -> Result<SurfaceId, WinErr> {
    let req = WinCreateRequest { session_token: session, initial_size: size };
    let reply: Result<WinCreateReply, WinErr> = call_compositor(WIN_CREATE_LABEL, req)?;
    let r = reply?;
    crate::process::set_compositor_event_endpoint(r.event_endpoint);
    Ok(r.surface_id)
}
```

The engineer adds `crate::process::set_compositor_event_endpoint` / `my_compositor_event_endpoint` to libcluu — they're simple static-cell setters.

- [ ] **Step 3: Build**

```
cd /home/vlb2bp/git/cluu
cargo build -p cluu_proto -p libcluu
```

Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add userspace/cluu_proto/src/window.rs userspace/libcluu/src/window.rs userspace/compositor/src/main.rs
git commit -m "feat(compositor): per-client event endpoint minted at WIN_CREATE"
```

---

## Task 5: Dispatch arms for 9 client-facing verbs

**Files:**
- Modify: `userspace/compositor/src/main.rs`
- Modify: `userspace/compositor/src/protocol.rs`

- [ ] **Step 1: Add dispatch arms**

In the main recv loop (find via `grep -n "msg.tag.label" userspace/compositor/src/main.rs`):

```rust
use cluu_proto::window::*;

if msg.tag.label == WIN_CREATE_LABEL                 { return self.handle_win_create(msg, payload, sender_tid); }
if msg.tag.label == WIN_DESTROY_LABEL                { return self.handle_win_destroy(msg, payload, sender_tid); }
if msg.tag.label == WIN_ATTACH_BUFFER_LABEL          { return self.handle_win_attach_buffer(msg, payload, sender_tid); }
if msg.tag.label == WIN_DETACH_BUFFER_LABEL          { return self.handle_win_detach_buffer(msg, payload, sender_tid); }
if msg.tag.label == WIN_COMMIT_LABEL                 { return self.handle_win_commit(msg, payload, sender_tid); }
if msg.tag.label == WIN_REQUEST_FRAME_CALLBACK_LABEL { return self.handle_win_request_frame_callback(msg, payload, sender_tid); }
if msg.tag.label == WIN_SET_TITLE_LABEL              { return self.handle_win_set_title(msg, payload, sender_tid); }
if msg.tag.label == WIN_SET_GEOMETRY_HINT_LABEL      { return self.handle_win_set_geometry_hint(msg, payload, sender_tid); }
if msg.tag.label == WIN_REQUEST_FOCUS_LABEL          { return self.handle_win_request_focus(msg, payload, sender_tid); }
```

- [ ] **Step 2: Implement each handler**

```rust
fn handle_win_create(&mut self, msg: Message, payload: &[u8], sender_tid: TidLike) -> ReplyResult {
    use cluu_proto::window::*;
    let req: WinCreateRequest = match postcard::from_bytes(payload) {
        Ok(r) => r,
        Err(_) => return self.reply_win_err::<WinCreateReply>(msg.tag.reply_id, WIN_CREATE_LABEL, WinErr::Internal(0xE_BADENV)),
    };
    let caller_pid = self.tid_to_pid(sender_tid).unwrap_or(0);

    // Resolve session_id if present.
    let session_id = match req.session_token {
        None => None,
        Some(t) => self.resolve_session_id(t, caller_pid),
    };

    // Mint per-client event endpoint (cached if already minted for this client).
    let event_endpoint = self.get_or_mint_client_endpoint(caller_pid);

    let sid = self.next_surface_id;
    self.next_surface_id += 1;
    let surface = crate::surface::Surface::new(
        sid, caller_pid, event_endpoint, session_id, req.initial_size);
    self.surfaces.insert(sid, surface);

    let reply: Result<WinCreateReply, WinErr> = Ok(WinCreateReply {
        surface_id: sid, event_endpoint,
    });
    self.reply_postcard(msg.tag.reply_id, WIN_CREATE_LABEL, &reply)
}

fn handle_win_destroy(&mut self, msg: Message, payload: &[u8], sender_tid: TidLike) -> ReplyResult {
    use cluu_proto::window::*;
    let req: WinDestroyRequest = match postcard::from_bytes(payload) {
        Ok(r) => r,
        Err(_) => return self.reply_win_err::<()>(msg.tag.reply_id, WIN_DESTROY_LABEL, WinErr::Internal(0xE_BADENV)),
    };
    let caller_pid = self.tid_to_pid(sender_tid).unwrap_or(0);
    let surface = match self.surfaces.get_mut(&req.surface_id) {
        Some(s) if s.client_pid == caller_pid => s,
        _ => return self.reply_win_err::<()>(msg.tag.reply_id, WIN_DESTROY_LABEL, WinErr::InvalidSurface),
    };
    surface.state = crate::surface::SurfaceState::Destroyed;
    // Drop frame_token refs.
    for (_, b) in surface.buffers.entries.iter() {
        let _ = self.unmap_frame(b.frame_token);  // existing helper
    }
    self.surfaces.remove(&req.surface_id);
    self.reply_postcard(msg.tag.reply_id, WIN_DESTROY_LABEL, &Ok::<(), WinErr>(()))
}

fn handle_win_attach_buffer(&mut self, msg: Message, payload: &[u8], sender_tid: TidLike) -> ReplyResult {
    use cluu_proto::window::*;
    let req: WinAttachBufferRequest = match postcard::from_bytes(payload) {
        Ok(r) => r,
        Err(_) => return self.reply_win_err::<()>(msg.tag.reply_id, WIN_ATTACH_BUFFER_LABEL, WinErr::Internal(0xE_BADENV)),
    };
    let caller_pid = self.tid_to_pid(sender_tid).unwrap_or(0);
    let surface = match self.surfaces.get_mut(&req.surface_id) {
        Some(s) if s.client_pid == caller_pid => s,
        _ => return self.reply_win_err::<()>(msg.tag.reply_id, WIN_ATTACH_BUFFER_LABEL, WinErr::InvalidSurface),
    };

    // Verify pixel_format supported.
    match req.pixel_format {
        PixelFormat::Bgra8888 | PixelFormat::Rgba8888 | PixelFormat::Rgb565 => {}
    }

    // Verify stride.
    if req.stride < req.width * req.pixel_format.bytes_per_pixel() {
        return self.reply_win_err::<()>(msg.tag.reply_id, WIN_ATTACH_BUFFER_LABEL, WinErr::InvalidBuffer);
    }

    // Map the frame into compositor's address space.
    let bytes_needed = (req.stride * req.height) as usize;
    let mapped_addr = match self.map_frame(req.frame_token, bytes_needed) {
        Ok(addr) => addr,
        Err(_) => return self.reply_win_err::<()>(msg.tag.reply_id, WIN_ATTACH_BUFFER_LABEL, WinErr::InvalidBuffer),
    };

    surface.buffers.attach(crate::buffer_table::BufferRecord {
        id: req.buffer_id, frame_token: req.frame_token,
        state: crate::buffer_table::BufferState::Attached,
        pixel_format: req.pixel_format,
        stride: req.stride, width: req.width, height: req.height,
        damage: alloc::vec::Vec::new(),
        mapped_addr: Some(mapped_addr),
    });
    if surface.state == crate::surface::SurfaceState::Created {
        surface.state = crate::surface::SurfaceState::BufferAttached;
    }
    self.reply_postcard(msg.tag.reply_id, WIN_ATTACH_BUFFER_LABEL, &Ok::<(), WinErr>(()))
}

fn handle_win_detach_buffer(&mut self, msg: Message, payload: &[u8], sender_tid: TidLike) -> ReplyResult {
    use cluu_proto::window::*;
    let req: WinDetachBufferRequest = match postcard::from_bytes(payload) {
        Ok(r) => r,
        Err(_) => return self.reply_win_err::<()>(msg.tag.reply_id, WIN_DETACH_BUFFER_LABEL, WinErr::Internal(0xE_BADENV)),
    };
    let caller_pid = self.tid_to_pid(sender_tid).unwrap_or(0);
    let surface = match self.surfaces.get_mut(&req.surface_id) {
        Some(s) if s.client_pid == caller_pid => s,
        _ => return self.reply_win_err::<()>(msg.tag.reply_id, WIN_DETACH_BUFFER_LABEL, WinErr::InvalidSurface),
    };
    let removed = surface.buffers.detach(req.buffer_id);
    match removed {
        Some(b) => {
            let _ = self.unmap_frame(b.frame_token);
            self.reply_postcard(msg.tag.reply_id, WIN_DETACH_BUFFER_LABEL, &Ok::<(), WinErr>(()))
        }
        None => self.reply_win_err::<()>(msg.tag.reply_id, WIN_DETACH_BUFFER_LABEL, WinErr::InvalidBuffer),
    }
}

fn handle_win_commit(&mut self, msg: Message, payload: &[u8], sender_tid: TidLike) -> ReplyResult {
    use cluu_proto::window::*;
    let req: WinCommitRequest = match postcard::from_bytes(payload) {
        Ok(r) => r,
        Err(_) => return self.reply_win_err::<()>(msg.tag.reply_id, WIN_COMMIT_LABEL, WinErr::Internal(0xE_BADENV)),
    };
    let caller_pid = self.tid_to_pid(sender_tid).unwrap_or(0);
    let surface = match self.surfaces.get_mut(&req.surface_id) {
        Some(s) if s.client_pid == caller_pid => s,
        _ => return self.reply_win_err::<()>(msg.tag.reply_id, WIN_COMMIT_LABEL, WinErr::InvalidSurface),
    };
    if !surface.buffers.commit(req.buffer_id, req.damage) {
        return self.reply_win_err::<()>(msg.tag.reply_id, WIN_COMMIT_LABEL, WinErr::InvalidBuffer);
    }
    if surface.state == crate::surface::SurfaceState::BufferAttached {
        surface.state = crate::surface::SurfaceState::Mapped;
    }
    self.reply_postcard(msg.tag.reply_id, WIN_COMMIT_LABEL, &Ok::<(), WinErr>(()))
}

fn handle_win_request_frame_callback(&mut self, msg: Message, payload: &[u8], sender_tid: TidLike) -> ReplyResult {
    use cluu_proto::window::*;
    let req: WinRequestFrameCallbackRequest = match postcard::from_bytes(payload) {
        Ok(r) => r,
        Err(_) => return self.reply_win_err::<()>(msg.tag.reply_id, WIN_REQUEST_FRAME_CALLBACK_LABEL, WinErr::Internal(0xE_BADENV)),
    };
    let caller_pid = self.tid_to_pid(sender_tid).unwrap_or(0);
    let surface = match self.surfaces.get_mut(&req.surface_id) {
        Some(s) if s.client_pid == caller_pid => s,
        _ => return self.reply_win_err::<()>(msg.tag.reply_id, WIN_REQUEST_FRAME_CALLBACK_LABEL, WinErr::InvalidSurface),
    };
    if surface.buffers.entries.is_empty() {
        return self.reply_win_err::<()>(msg.tag.reply_id, WIN_REQUEST_FRAME_CALLBACK_LABEL, WinErr::InvalidSurface);
    }
    surface.frame_callback_requested = true;
    self.reply_postcard(msg.tag.reply_id, WIN_REQUEST_FRAME_CALLBACK_LABEL, &Ok::<(), WinErr>(()))
}

fn handle_win_set_title(&mut self, msg: Message, payload: &[u8], sender_tid: TidLike) -> ReplyResult {
    use cluu_proto::window::*;
    let req: WinSetTitleRequest = match postcard::from_bytes(payload) {
        Ok(r) => r,
        Err(_) => return self.reply_win_err::<()>(msg.tag.reply_id, WIN_SET_TITLE_LABEL, WinErr::Internal(0xE_BADENV)),
    };
    let caller_pid = self.tid_to_pid(sender_tid).unwrap_or(0);
    let surface = match self.surfaces.get_mut(&req.surface_id) {
        Some(s) if s.client_pid == caller_pid => s,
        _ => return self.reply_win_err::<()>(msg.tag.reply_id, WIN_SET_TITLE_LABEL, WinErr::InvalidSurface),
    };
    surface.title = if req.title.len() > 256 { req.title[..256].into() } else { req.title };
    self.reply_postcard(msg.tag.reply_id, WIN_SET_TITLE_LABEL, &Ok::<(), WinErr>(()))
}

fn handle_win_set_geometry_hint(&mut self, msg: Message, payload: &[u8], sender_tid: TidLike) -> ReplyResult {
    use cluu_proto::window::*;
    let req: WinSetGeometryHintRequest = match postcard::from_bytes(payload) {
        Ok(r) => r,
        Err(_) => return self.reply_win_err::<()>(msg.tag.reply_id, WIN_SET_GEOMETRY_HINT_LABEL, WinErr::Internal(0xE_BADENV)),
    };
    let caller_pid = self.tid_to_pid(sender_tid).unwrap_or(0);
    let surface = match self.surfaces.get_mut(&req.surface_id) {
        Some(s) if s.client_pid == caller_pid => s,
        _ => return self.reply_win_err::<()>(msg.tag.reply_id, WIN_SET_GEOMETRY_HINT_LABEL, WinErr::InvalidSurface),
    };
    // Validate.
    if let (Some(min), Some(max)) = (req.hints.min_size, req.hints.max_size) {
        if min.w > max.w || min.h > max.h {
            return self.reply_win_err::<()>(msg.tag.reply_id, WIN_SET_GEOMETRY_HINT_LABEL, WinErr::GeometryRejected);
        }
    }
    surface.geometry_hints = req.hints;
    self.reply_postcard(msg.tag.reply_id, WIN_SET_GEOMETRY_HINT_LABEL, &Ok::<(), WinErr>(()))
}

fn handle_win_request_focus(&mut self, msg: Message, payload: &[u8], sender_tid: TidLike) -> ReplyResult {
    use cluu_proto::window::*;
    let req: WinRequestFocusRequest = match postcard::from_bytes(payload) {
        Ok(r) => r,
        Err(_) => return self.reply_win_err::<bool>(msg.tag.reply_id, WIN_REQUEST_FOCUS_LABEL, WinErr::Internal(0xE_BADENV)),
    };
    let caller_pid = self.tid_to_pid(sender_tid).unwrap_or(0);
    if !self.surfaces.contains_key(&req.surface_id) {
        return self.reply_win_err::<bool>(msg.tag.reply_id, WIN_REQUEST_FOCUS_LABEL, WinErr::InvalidSurface);
    }
    if self.surfaces[&req.surface_id].client_pid != caller_pid {
        return self.reply_win_err::<bool>(msg.tag.reply_id, WIN_REQUEST_FOCUS_LABEL, WinErr::InvalidSurface);
    }
    // Policy stub: accept-all. Real policy is future.
    self.transfer_focus(req.surface_id);
    self.reply_postcard(msg.tag.reply_id, WIN_REQUEST_FOCUS_LABEL, &Ok::<bool, WinErr>(true))
}

// Helpers:
fn reply_win_err<R: serde::Serialize>(&mut self, reply_id: u64, label: u32, err: WinErr) -> ReplyResult {
    let value: Result<R, WinErr> = Err(err);
    self.reply_postcard(reply_id, label, &value)
}

fn reply_postcard<R: serde::Serialize>(&mut self, reply_id: u64, label: u32, value: &R) -> ReplyResult {
    let bytes = postcard::to_allocvec(value).expect("ser");
    let mut words = [0u64; 6];
    words[0] = bytes.len() as u64;
    words[1] = cluu_proto::ABI_VERSION as u64;
    libcluu::ipc::reply(reply_id, label, words, &bytes)
}
```

Engineer wires `map_frame`/`unmap_frame` to existing MAP_SHARE_PHYS-based helpers in compositor's `shm.rs`. `transfer_focus` covered in Task 7.

- [ ] **Step 3: Build**

```
cd /home/vlb2bp/git/cluu
cargo build -p compositor
```

Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add userspace/compositor/src/main.rs userspace/compositor/src/protocol.rs
git commit -m "feat(compositor): dispatch for 9 WIN_* client verbs (labels 210-218)"
```

---

## Task 6: Render loop — per-frame callback + buffer state transitions

**Files:**
- Modify: `userspace/compositor/src/main.rs`
- Modify: `userspace/compositor/src/render.rs`
- Modify: `userspace/compositor/src/compose.rs`

- [ ] **Step 1: Rewrite the render-tick body**

Locate the existing render-tick (find via `grep -n "fn tick\|fn render\|broadcast_frame_ready" userspace/compositor/src/main.rs`). Replace with:

```rust
fn render_tick(&mut self) {
    let now_ms = self.monotonic_ms();

    // 1. Promote buffer states.
    for surface in self.surfaces.values_mut() {
        surface.buffers.promote_for_render();
    }

    // 2. Render all Scanout buffers.
    self.composite_frame();

    // 3. Emit WIN_BUFFER_RELEASED for buffers transitioning from ReleasedLocked.
    for surface in self.surfaces.values_mut() {
        let released = surface.buffers.take_released();
        for buf_id in released {
            let event = cluu_proto::window::WinBufferReleasedEvent {
                surface_id: surface.id, buffer_id: buf_id,
            };
            self.send_event(surface.client_endpoint,
                cluu_proto::window::WIN_BUFFER_RELEASED_LABEL, &event);
        }
    }

    // 4. Fire WIN_FRAME_READY for surfaces with pending callback.
    for surface in self.surfaces.values_mut() {
        if surface.frame_callback_requested {
            let event = cluu_proto::window::WinFrameReadyEvent {
                surface_id: surface.id, timestamp_ms: now_ms,
            };
            self.send_event(surface.client_endpoint,
                cluu_proto::window::WIN_FRAME_READY_LABEL, &event);
            surface.frame_callback_requested = false;
        }
    }
}

fn send_event<E: serde::Serialize>(&mut self, endpoint: u64, label: u32, event: &E) {
    let bytes = postcard::to_allocvec(event).expect("ser");
    let mut words = [0u64; 6];
    words[0] = bytes.len() as u64;
    words[1] = cluu_proto::ABI_VERSION as u64;
    let _ = libcluu::ipc::send_to_endpoint(endpoint, label, words, &bytes);
}

fn composite_frame(&mut self) {
    // Walk surfaces; for each Scanout buffer, blit its mapped pixels to
    // the framebuffer per damage rects.
    let scanout_records: alloc::vec::Vec<_> = self.surfaces.values()
        .filter_map(|s| s.buffers.scanout_record().map(|b| (s.id, b.clone())))
        .collect();
    for (_sid, buf) in scanout_records {
        if let Some(addr) = buf.mapped_addr {
            self.blit_to_framebuffer(addr, &buf);
        }
    }
}
```

`blit_to_framebuffer` already exists in `compose.rs` / `render.rs`; engineer adapts signature.

- [ ] **Step 2: Delete `broadcast_frame_ready`**

```
cd /home/vlb2bp/git/cluu
grep -n "broadcast_frame_ready" userspace/compositor/src/main.rs
```

Delete the function body and every call site. Replaced by step 1's per-surface flag walk.

- [ ] **Step 3: Build + boot smoke**

```
cd /home/vlb2bp/git/cluu
cargo build -p compositor
bash scripts/harness_run.sh
```

Expected: boot reaches `compositor: ready`. (No surface clients yet exercise new buffers; existing legacy paths still draw via old shm flow until Task 8 ships cluuterm.)

Note: the boot may produce a black screen until tasks 8-9 migrate clients. Frame-typing-already-correct clients will render fine after.

- [ ] **Step 4: Commit**

```bash
git add userspace/compositor/src/main.rs userspace/compositor/src/render.rs userspace/compositor/src/compose.rs
git commit -m "feat(compositor): per-frame callback render loop; retire broadcast_frame_ready"
```

---

## Task 7: Focus tracking + pre-translated input

**Files:**
- Modify: `userspace/compositor/src/main.rs`
- Modify: `userspace/compositor/src/state.rs`
- Modify: `userspace/compositor/src/hotkeys.rs`
- Create: `/etc/keymap/us.toml` (or embed default)

- [ ] **Step 1: Focus helper**

In `userspace/compositor/src/main.rs`:

```rust
fn transfer_focus(&mut self, new: cluu_proto::window::SurfaceId) {
    use cluu_proto::window::{KeyEvent, KeyState, Modifiers, InputEvent,
                              WinFocusInEvent, WinFocusOutEvent, WinInputEvent,
                              WIN_FOCUS_IN_LABEL, WIN_FOCUS_OUT_LABEL, WIN_INPUT_LABEL};
    if self.focused_surface == Some(new) { return; }

    // Synthetic modifier-release for outgoing focus.
    if let Some(prev_id) = self.focused_surface {
        if let Some(prev) = self.surfaces.get_mut(&prev_id) {
            for sig_key in [cluu_proto::window::Key::LeftShift,
                            cluu_proto::window::Key::LeftCtrl,
                            cluu_proto::window::Key::LeftAlt,
                            cluu_proto::window::Key::LeftSuper] {
                let ev = WinInputEvent {
                    surface_id: prev.id,
                    event: InputEvent::Key(KeyEvent {
                        key: sig_key, modifiers: Modifiers::default(),
                        state: KeyState::Released, char: None,
                    }),
                };
                self.send_event(prev.client_endpoint, WIN_INPUT_LABEL, &ev);
            }
            prev.focused = false;
            let ev = WinFocusOutEvent { surface_id: prev.id };
            self.send_event(prev.client_endpoint, WIN_FOCUS_OUT_LABEL, &ev);
        }
    }

    self.focused_surface = Some(new);
    if let Some(s) = self.surfaces.get_mut(&new) {
        s.focused = true;
        let ev = WinFocusInEvent { surface_id: new };
        self.send_event(s.client_endpoint, WIN_FOCUS_IN_LABEL, &ev);
    }
}
```

- [ ] **Step 2: Keymap loader (default US layout)**

Add a builtin keymap to `userspace/compositor/src/hotkeys.rs` (or new `keymap.rs`):

```rust
use cluu_proto::window::{Key, Modifiers};

/// Translate a raw scancode + modifier state → (logical key, unicode char).
/// US-layout default. Future: load from `/etc/keymap/<layout>.toml`.
pub fn translate(scancode: u32, mods: Modifiers) -> (Key, Option<char>) {
    use Key::*;
    let key = match scancode {
        0x1E => A, 0x30 => B, 0x2E => C, 0x20 => D, 0x12 => E, 0x21 => F,
        0x22 => G, 0x23 => H, 0x17 => I, 0x24 => J, 0x25 => K, 0x26 => L,
        0x32 => M, 0x31 => N, 0x18 => O, 0x19 => P, 0x10 => Q, 0x13 => R,
        0x1F => S, 0x14 => T, 0x16 => U, 0x2F => V, 0x11 => W, 0x2D => X,
        0x15 => Y, 0x2C => Z,
        0x02 => Digit1, 0x03 => Digit2, 0x04 => Digit3, 0x05 => Digit4, 0x06 => Digit5,
        0x07 => Digit6, 0x08 => Digit7, 0x09 => Digit8, 0x0A => Digit9, 0x0B => Digit0,
        0x1C => Enter, 0x01 => Esc, 0x0E => Backspace, 0x0F => Tab, 0x39 => Space,
        0x48 => ArrowUp, 0x50 => ArrowDown, 0x4B => ArrowLeft, 0x4D => ArrowRight,
        0x3B => F1, 0x3C => F2, 0x3D => F3, 0x3E => F4, 0x3F => F5, 0x40 => F6,
        0x41 => F7, 0x42 => F8, 0x43 => F9, 0x44 => F10, 0x57 => F11, 0x58 => F12,
        0x1D => LeftCtrl, 0x2A => LeftShift, 0x36 => RightShift, 0x38 => LeftAlt,
        0x47 => Home, 0x4F => End, 0x49 => PageUp, 0x51 => PageDown,
        0x52 => Insert, 0x53 => Delete,
        _ => Unknown(scancode),
    };
    let ch = char_for(&key, mods);
    (key, ch)
}

fn char_for(key: &Key, mods: Modifiers) -> Option<char> {
    use Key::*;
    let shift = mods.shift;
    Some(match key {
        A => if shift { 'A' } else { 'a' },
        B => if shift { 'B' } else { 'b' },
        C => if shift { 'C' } else { 'c' },
        D => if shift { 'D' } else { 'd' },
        E => if shift { 'E' } else { 'e' },
        F => if shift { 'F' } else { 'f' },
        G => if shift { 'G' } else { 'g' },
        H => if shift { 'H' } else { 'h' },
        I => if shift { 'I' } else { 'i' },
        J => if shift { 'J' } else { 'j' },
        K => if shift { 'K' } else { 'k' },
        L => if shift { 'L' } else { 'l' },
        M => if shift { 'M' } else { 'm' },
        N => if shift { 'N' } else { 'n' },
        O => if shift { 'O' } else { 'o' },
        P => if shift { 'P' } else { 'p' },
        Q => if shift { 'Q' } else { 'q' },
        R => if shift { 'R' } else { 'r' },
        S => if shift { 'S' } else { 's' },
        T => if shift { 'T' } else { 't' },
        U => if shift { 'U' } else { 'u' },
        V => if shift { 'V' } else { 'v' },
        W => if shift { 'W' } else { 'w' },
        X => if shift { 'X' } else { 'x' },
        Y => if shift { 'Y' } else { 'y' },
        Z => if shift { 'Z' } else { 'z' },
        Digit0 => if shift { ')' } else { '0' },
        Digit1 => if shift { '!' } else { '1' },
        Digit2 => if shift { '@' } else { '2' },
        Digit3 => if shift { '#' } else { '3' },
        Digit4 => if shift { '$' } else { '4' },
        Digit5 => if shift { '%' } else { '5' },
        Digit6 => if shift { '^' } else { '6' },
        Digit7 => if shift { '&' } else { '7' },
        Digit8 => if shift { '*' } else { '8' },
        Digit9 => if shift { '(' } else { '9' },
        Space => ' ',
        Enter => '\n',
        Tab => '\t',
        Backspace => '\x08',
        _ => return None,
    })
}
```

- [ ] **Step 3: Dispatch input via WIN_INPUT**

When the compositor receives a kbd event (existing path), call:

```rust
fn on_kbd_event(&mut self, scancode: u32, state: cluu_proto::window::KeyState) {
    use cluu_proto::window::*;
    self.update_modifier_state(scancode, state);
    let mods = self.modifiers.clone();
    let (key, ch) = crate::hotkeys::translate(scancode, mods);

    // Check compositor bindings first (Ctrl+Alt+F1 etc.).
    if self.try_consume_compositor_binding(key.clone(), mods, state) { return; }

    if let Some(sid) = self.focused_surface {
        if let Some(s) = self.surfaces.get(&sid) {
            let ev = WinInputEvent {
                surface_id: sid,
                event: InputEvent::Key(KeyEvent {
                    key, modifiers: mods, state, char: ch,
                }),
            };
            self.send_event(s.client_endpoint, WIN_INPUT_LABEL, &ev);
        }
    }
}
```

- [ ] **Step 4: Build**

```
cd /home/vlb2bp/git/cluu
cargo build -p compositor
```

Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add userspace/compositor/src/main.rs userspace/compositor/src/hotkeys.rs userspace/compositor/src/state.rs
git commit -m "feat(compositor): focus tracking + pre-translated KeyEvent dispatch"
```

---

## Task 8: Cluuterm flips to `libcluu::window`

**Files:**
- Modify: `userspace/cluuterm/src/main.rs`
- Modify: `userspace/cluuterm/src/render.rs`

- [ ] **Step 1: Replace legacy compositor IPC**

Read `userspace/cluuterm/src/main.rs` + `render.rs` for the existing `WIN_REGISTER` / `WIN_DAMAGE` / shm setup. Replace with `libcluu::window` calls:

```rust
fn ensure_surface(&mut self) -> Result<(), libcluu::window::WinErr> {
    if self.surface_id.is_some() { return Ok(()); }
    let size = cluu_proto::window::Size { w: self.px_w, h: self.px_h };
    let session_token = libcluu::process::self_session_token(); // None pre-spec-3
    let sid = libcluu::window::create(session_token, size)?;
    self.surface_id = Some(sid);
    self.buffer_pool = Some(libcluu::window::SurfaceBufferPool::new(
        sid, size, cluu_proto::window::PixelFormat::Bgra8888)?);
    Ok(())
}

fn draw_frame(&mut self) -> Result<(), libcluu::window::WinErr> {
    self.ensure_surface()?;
    let pool = self.buffer_pool.as_mut().unwrap();
    let (buf, slice) = pool.next_writable();
    self.render_grid_to_buffer(slice, pool.stride);

    let damage = self.take_damage_rects(); // existing helper that returns Vec<Rect>
    pool.commit(&damage)?;
    libcluu::window::request_frame_callback(pool.surface_id)?;
    Ok(())
}

fn event_loop(&mut self) {
    loop {
        match libcluu::window::recv_event() {
            Ok(libcluu::window::WindowEvent::Input(ev)) => self.on_input(ev),
            Ok(libcluu::window::WindowEvent::FrameReady(_)) => {
                let _ = self.draw_frame();
            }
            Ok(libcluu::window::WindowEvent::BufferReleased(r)) => {
                if let Some(p) = self.buffer_pool.as_mut() { p.on_release(r.buffer_id); }
            }
            Ok(libcluu::window::WindowEvent::Configure(c)) => self.on_resize(c),
            Ok(libcluu::window::WindowEvent::Closed(_)) => return,
            Ok(_) => {}
            Err(_) => return,
        }
    }
}
```

Engineer adapts the existing render-loop to fit this shape.

- [ ] **Step 2: Build + boot smoke**

```
cd /home/vlb2bp/git/cluu
cargo build -p cluuterm
bash scripts/harness_run.sh
```

Expected: graphical login → cluuterm window renders shell prompt.

- [ ] **Step 3: Commit**

```bash
git add userspace/cluuterm/src/main.rs userspace/cluuterm/src/render.rs
git commit -m "feat(cluuterm): render via libcluu::window + SurfaceBufferPool"
```

---

## Task 9: Login binary flips to `libcluu::window`

**Files:**
- Modify: `userspace/login/src/main.rs`

- [ ] **Step 1: Replace legacy register/damage with window API**

Mirror Task 8 — login has a simpler single-buffer flow:

```rust
fn draw_login_screen() -> Result<(), libcluu::window::WinErr> {
    let size = cluu_proto::window::Size { w: 640, h: 480 };
    let surface_id = libcluu::window::create(None, size)?;
    let pool = libcluu::window::SurfaceBufferPool::new(
        surface_id, size, cluu_proto::window::PixelFormat::Bgra8888)?;
    let (_buf, slice) = pool.next_writable();
    render_login_ui(slice, size.w, size.h);  // existing rendering code
    pool.commit(&[])?;  // damage-all
    libcluu::window::request_frame_callback(surface_id)?;
    Ok(())
}
```

- [ ] **Step 2: Build**

```
cd /home/vlb2bp/git/cluu
cargo build -p login
bash scripts/harness_run.sh
```

Expected: login window renders.

- [ ] **Step 3: Commit**

```bash
git add userspace/login/src/main.rs
git commit -m "feat(login): render via libcluu::window"
```

---

## Task 10: Session-aware window cleanup (consume `SESSION_ENDED`)

**Files:**
- Modify: `userspace/compositor/src/main.rs`

- [ ] **Step 1: Verify plan 3 task 5 wired the SESSION_ENDED arm**

```
cd /home/vlb2bp/git/cluu
grep -n "SESSION_ENDED_LABEL\|handle_session_ended" userspace/compositor/src/main.rs | head -5
```

If plan 3 task 5 landed, the arm exists. If not, add per plan 3 task 5 step 2/3.

- [ ] **Step 2: Update `handle_session_ended` to use new Surface model**

Replace the existing window-closing logic (if any) with:

```rust
fn handle_session_ended(&mut self, _msg: Message, payload: &[u8]) -> ReplyResult {
    use cluu_proto::session::SessionEndedEvent;
    use cluu_proto::window::{WinClosedEvent, WIN_CLOSED_LABEL};

    let event: SessionEndedEvent = match postcard::from_bytes(payload) {
        Ok(e) => e,
        Err(_) => return ReplyResult::Ok,
    };

    let to_close: alloc::vec::Vec<cluu_proto::window::SurfaceId> = self.surfaces.iter()
        .filter(|(_, s)| s.session_id == Some(event.session_id))
        .map(|(id, _)| *id)
        .collect();

    for sid in to_close {
        if let Some(s) = self.surfaces.get_mut(&sid) {
            let ev = WinClosedEvent { surface_id: sid };
            self.send_event(s.client_endpoint, WIN_CLOSED_LABEL, &ev);
            s.state = crate::surface::SurfaceState::Closing;
        }
    }

    self.tracked_sessions.remove(&event.session_id);
    self.spawn_login_window();
    ReplyResult::Ok
}
```

- [ ] **Step 3: Build + interactive login/logout test**

```
cd /home/vlb2bp/git/cluu
cargo build -p compositor
bash scripts/harness_run.sh
```

Expected: log in; `exit` in shell → cluuterm closes → fresh login window appears.

- [ ] **Step 4: Commit**

```bash
git add userspace/compositor/src/main.rs
git commit -m "feat(compositor): WIN_CLOSED fanout on SESSION_ENDED"
```

---

## Task 11: Cap-revocation force-destroy of dead clients

**Files:**
- Modify: `userspace/compositor/src/main.rs`

- [ ] **Step 1: Add a force-destroy sweep**

In the render-tick (Task 6), before composing:

```rust
fn force_destroy_dead_clients(&mut self) {
    let dead: alloc::vec::Vec<cluu_proto::window::SurfaceId> = self.surfaces.iter()
        .filter(|(_, s)| self.is_endpoint_revoked(s.client_endpoint))
        .map(|(id, _)| *id)
        .collect();
    for sid in dead {
        if let Some(s) = self.surfaces.remove(&sid) {
            for (_, b) in &s.buffers.entries {
                let _ = self.unmap_frame(b.frame_token);
            }
        }
    }
}
```

`is_endpoint_revoked` queries the kernel cap state for the endpoint. Engineer adapts to the existing cap-revocation primitive.

Call `force_destroy_dead_clients()` at the top of `render_tick()`.

- [ ] **Step 2: Build + boot smoke**

```
cd /home/vlb2bp/git/cluu
cargo build -p compositor
bash scripts/harness_run.sh
```

Expected: boot. Kill cluuterm externally (privileged test); on next render tick its surface gets force-destroyed; framebuffer reflects.

- [ ] **Step 3: Commit**

```bash
git add userspace/compositor/src/main.rs
git commit -m "feat(compositor): force-destroy surfaces of dead clients (cap-revocation)"
```

---

## Task 12: Delete dead code (legacy `COMP_WIN_*` + global `compositor:input`)

**Files:**
- Modify: `userspace/compositor/src/protocol.rs`
- Modify: `userspace/libcluu/src/ipc.rs`
- Modify: `userspace/compositor/src/main.rs`

- [ ] **Step 1: Find legacy labels + service registrations**

```
cd /home/vlb2bp/git/cluu
git grep -n "COMP_WIN_REGISTER_LABEL\|COMP_WIN_DAMAGE_LABEL\|COMP_WIN_DESTROY_LABEL\|COMP_WIN_REGISTER_REPLY\|COMP_FRAME_READY_LABEL\|COMP_WIN_SET_TITLE_LABEL"
git grep -n '"compositor:input"'
git grep -n "broadcast_frame_ready"
```

- [ ] **Step 2: Delete each match**

For each constant: delete the line.
For each handler: delete.
For `compositor:input` registration: delete (per-client endpoint replaces it).
For `broadcast_frame_ready`: already deleted in Task 6.

- [ ] **Step 3: Verify zero hits**

```
cd /home/vlb2bp/git/cluu
git grep -c "broadcast_frame_ready"           && echo "FAIL" || echo "PASS"
git grep -c '"compositor:input"'              && echo "FAIL" || echo "PASS"
git grep -c "COMP_WIN_REGISTER_LABEL"         && echo "FAIL" || echo "PASS"
git grep -c "COMP_WIN_DAMAGE_LABEL"           && echo "FAIL" || echo "PASS"
git grep -c "COMP_WIN_DESTROY_LABEL"          && echo "FAIL" || echo "PASS"
git grep -c "fn handle_win_register\b"        && echo "FAIL" || echo "PASS"
```

All must PASS.

- [ ] **Step 4: Build clean**

```
cd /home/vlb2bp/git/cluu
cargo xtask build
cargo clippy --workspace --all-targets -- -D warnings
```

- [ ] **Step 5: Boot smoke**

```
bash scripts/harness_run.sh
```

Expected: `compositor: ready`; login + cluuterm visible.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "refactor: delete legacy COMP_WIN_* labels + global compositor:input"
```

---

## Task 13: Acceptance markers

**Files:**
- Create: `userspace/probes/l4_*` (multiple)

Markers per spec 4 §11:

- `l4_double_buffer_alternates`
- `l4_detach_while_scanout_denied`
- `l4_format_mismatch_denied`
- `l4_frame_ready_one_shot`
- `l4_idle_no_callbacks`
- `l4_partial_damage_repaint`
- `l4_input_pretranslated`
- `l4_input_shift_modifier`
- `l4_focus_out_releases_modifiers`
- `l4_surface_session_id_set`
- `l4_session_ended_closes_surfaces`
- `l4_invalid_surface_id_denied`
- `l4_compositor_death_cap_revoke`

For each:

- [ ] **Step 1: Scaffold from `userspace/probes/argvprobe/`**

Add to workspace.

- [ ] **Step 2: Implement each probe**

Template (`l4_frame_ready_one_shot`):

```rust
#![no_std]
#![no_main]
extern crate alloc;
extern crate libcluu;

#[no_mangle]
pub extern "C" fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    let size = cluu_proto::window::Size { w: 100, h: 100 };
    let sid = match libcluu::window::create(None, size) {
        Ok(s) => s,
        Err(_) => { libcluu::print_log(b"l4_frame_ready_one_shot: SKIP create failed\n"); return 0; }
    };
    let _pool = libcluu::window::SurfaceBufferPool::new(
        sid, size, cluu_proto::window::PixelFormat::Bgra8888).ok();

    let _ = libcluu::window::request_frame_callback(sid);

    // Wait for one FrameReady.
    let mut got = 0;
    for _ in 0..20 {
        match libcluu::window::recv_event() {
            Ok(libcluu::window::WindowEvent::FrameReady(_)) => { got += 1; break; }
            _ => {}
        }
    }
    if got != 1 {
        libcluu::print_log(b"l4_frame_ready_one_shot: FAIL\n");
        return 1;
    }
    // Now confirm no further FrameReady arrives without re-request.
    let mut extra = 0;
    for _ in 0..5 {
        if let Ok(libcluu::window::WindowEvent::FrameReady(_)) = libcluu::window::recv_event() {
            extra += 1;
        }
    }
    if extra == 0 {
        libcluu::print_log(b"l4_frame_ready_one_shot: PASS\n"); 0
    } else {
        libcluu::print_log(b"l4_frame_ready_one_shot: FAIL extra callbacks\n"); 1
    }
}
```

Template (`l4_detach_while_scanout_denied`):

```rust
// Attach + commit → buffer enters Pending then Scanout on next tick.
// Try to detach → expect Err(WinErr::InvalidBuffer).
```

Template (`l4_input_pretranslated`):

```rust
// Probe needs an "inject scancode" test helper. If unavailable, skip and
// mark in commit.
```

- [ ] **Step 3: Run markers**

```
for m in l4_frame_ready_one_shot l4_idle_no_callbacks l4_double_buffer_alternates \
         l4_format_mismatch_denied l4_invalid_surface_id_denied; do
    HARNESS_FORCE_BUILD=1 CLUU_SHELL_AUTOSTART_CMD=$m MARKER_MODE=$m bash scripts/harness_run.sh
    grep "$m:" serial.log
done
```

Expected: each `<marker>: PASS`.

- [ ] **Step 4: Commit**

```bash
git add userspace/probes/l4_* Cargo.toml
git commit -m "test: spec 4 acceptance markers"
```

---

## Final verification

- [ ] **Spec 4 §11 grep proofs:**

```
cd /home/vlb2bp/git/cluu
echo "Zero-hit:"
git grep -c "broadcast_frame_ready"     # → 0
git grep -c '"compositor:input"'        # → 0
git grep -c "fn handle_win_register\b"  # → 0
git grep -c "COMP_WIN_REGISTER_LABEL"   # → 0

echo "One-match:"
git grep -c "WIN_CREATE_LABEL.*= 210"          # → 1
git grep -c "WIN_BUFFER_RELEASED_LABEL.*= 221" # → 1
git grep -c "fn handle_win_create" userspace/compositor/  # → 1
git grep -c "SurfaceBufferPool" userspace/libcluu/        # → 1
```

- [ ] **Functional smoke:**

```
bash scripts/harness_run.sh
```

- Boot reaches `compositor: ready`; login window visible.
- Login → cluuterm window; shell prompt; typing visible echo.
- Resize cluuterm → `WIN_CONFIGURE` fires → cluuterm reallocates + commits new size → no flicker.

- [ ] **No new timeouts:**

```
grep -rn "recv_with_timeout\|call_with_timeout" userspace/compositor/src/ | wc -l
```

Same as pre-plan-4.

- [ ] **Performance:**

- Idle compositor CPU < 1% (no surfaces requesting callbacks → tick body wakes briefly, does nothing).
- Visual: `scripts/fb_dump.sh` confirms login + cluuterm render correctly.

---

## Notes for the engineer

- **TDD:** Tasks 1 has unit tests. Compositor/cluuterm changes verified via harness markers + visual smoke.
- **DRY:** Compositor's verb handlers all follow the same shape (resolve surface, validate caller, mutate, reply). If the boilerplate hurts, factor a `fn dispatch<Req, Rep>(label, payload, sender_tid, handler) -> ReplyResult` helper.
- **YAGNI:** No sub-surfaces, touch, dmabuf, HiDPI policy, cursor management, drag-and-drop. All deferred per spec §12.
- **Cap discipline:** every frame the client attaches goes through frame-typing inc/dec. If frame counts leak after a few login/logout cycles, fix in this plan — don't carry the bug.
- **MAP_SHARE_PHYS semantics:** the existing primitive is used as-is. Spec 4 doesn't redefine it.
- **Cluuterm + login render diff:** cluuterm has continuous animation (cursor blink, scroll). Login is mostly static. Cluuterm requests callback every frame; login requests on input only.

---

## Spec 4 sections covered

| Spec § | Task(s) |
|---|---|
| §3 architecture | Task 3-5 |
| §4 verb set | Task 1, 5 |
| §5 wire format | Task 1, 2 |
| §6 buffer + damage | Task 3 (BufferTable), Task 5 (commit handler), Task 6 (render loop) |
| §7 frame callback | Task 6 |
| §8 input + focus | Task 7 |
| §9 lifecycle + session integration | Task 3 (Surface states), Task 5, Task 10 |
| §10 migration | Tasks 1-13 |
| §11 acceptance | Task 13, final verification |
| §12 follow-ups | OUT of plan 4 scope |
