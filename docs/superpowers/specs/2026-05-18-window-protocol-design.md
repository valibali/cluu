# Window protocol formalization — design

**Date:** 2026-05-18
**Status:** spec — pre-implementation
**Predecessor inventory:** `docs/superpowers/specs/2026-05-18-spawn-window-pty-inventory.md`
**Companion specs:**
- spec 1: `docs/superpowers/specs/2026-05-18-unified-spawn-protocol-design.md`
- spec 2: `docs/superpowers/specs/2026-05-18-terminal-pty-unification-design.md`
- spec 3: `docs/superpowers/specs/2026-05-18-session-lifecycle-design.md`

**Position in decomposition:** spec 4 of inventory §12.

## 1. Why

Today's compositor protocol has working primitives but informal
semantics (inventory §11.5 + §12 line 4):

- `broadcast_frame_ready` (compositor/main.rs:32) fans out a "render
  next frame" signal indiscriminately to every subscriber, every tick.
- Window registration / damage / destroy verbs exist (`WIN_REGISTER`,
  `WIN_DAMAGE`, `WIN_DESTROY`) but their state machine and buffer
  ownership are implicit.
- Buffers are shared via `MAP_SHARE_PHYS` with no explicit
  "compositor is done with this buffer; you may reuse it" event;
  clients guess.
- Input arrives as informal scancodes; clients reproduce keymap logic
  ad hoc.
- Session-to-surface association is nowhere formalized — needed for
  spec 3's `SESSION_ENDED` fanout to close session windows.

Spec 4 lifts the de-facto Wayland-shape into a formal protocol:
client-owned shared frames, per-frame request frame-callbacks,
explicit buffer-release events, surface-local damage rects,
pre-translated input, and surface-to-session association. Surfaces
become first-class procmgr-grade typed objects inside the compositor.

## 2. Goals and non-goals

### Goals

1. One verb set for windows. 16 labels covering create / destroy /
   buffer attach-commit-release / frame-callback / configure / input /
   focus / closed.
2. Client-owned shared-memory buffers (typed frames per spec 1's
   frame typing). Zero-copy. Compositor maps via `MAP_SHARE_PHYS`,
   refcount inc/dec on attach/detach.
3. Per-frame request frame-callback (Wayland-strict). Idle clients
   receive no `WIN_FRAME_READY` events.
4. Explicit buffer-release event. Client may reuse a buffer only after
   `WIN_BUFFER_RELEASED` fires. No tearing.
5. Surface-local damage rects in `WIN_COMMIT`. Empty list = damage-all.
6. Pre-translated input events. Compositor reads keymap; emits
   `KeyEvent { key, modifiers, state, char }`. Pointer + wheel
   similarly pre-shaped. Clients don't reproduce keymap logic.
7. Surface-to-session integration. Every surface carries
   `session_id: Option<u32>`. `SESSION_ENDED { session_id }` from
   spec 3 → compositor `WIN_CLOSED`s matching surfaces.
8. Per-client async event endpoint (no global `compositor:input`
   bottleneck).

### Non-goals

- Sub-surfaces, surface roles, popup/transient/menu semantics.
  Defer; spec 4 lands 1:1 surface = window.
- Touch input. Defer.
- Buffer formats beyond `Bgra8888` / `Rgba8888` / `Rgb565`.
- Mid-stream pixel-format change on a buffer.
- HiDPI rendering policy (`WIN_CONFIGURE.scale` is published; client
  interpretation is the client's policy).
- Cancellable frame callbacks. `WIN_CANCEL_FRAME_CALLBACK` only if a
  later use case demands it.
- Raw-scancode focus mode. Reserved as a future per-surface
  capability.

## 3. Architecture

Wayland-shape protocol over CLUU IPC. Compositor is the seat owner
(spec 3); clients hold windows. Buffers are client-owned typed frames
shared via `MAP_SHARE_PHYS`. Frame pacing is per-frame request.
Buffer release is explicit. Input is pre-translated.

**Three endpoints (unchanged from today):**

| Service name | Purpose | Caller |
|---|---|---|
| `compositor:client` | per-client surface ops | client → compositor |
| `compositor:control` | session handoff (spec 3) + seat control | privileged callers (login) |
| per-client async endpoint | events back to the client (input, buffer-release, frame-ready, configure, focus, closed) | compositor → client |

**Wayland alignment (parallel):**

| Concept | Wayland | Spec 4 |
|---|---|---|
| Drawable | `wl_surface` | `Surface` (`surface_id: u32`) |
| Window with role | `xdg_toplevel` | implicit (1:1 with Surface) |
| Buffer | `wl_buffer` | typed-frame mapped via MAP_SHARE_PHYS |
| Frame callback | `wl_surface.frame` | `WIN_REQUEST_FRAME_CALLBACK` + `WIN_FRAME_READY` |
| Buffer release | `wl_buffer.release` | `WIN_BUFFER_RELEASED` |
| Damage | `wl_surface.damage_buffer` | `damage: Vec<Rect>` in `WIN_COMMIT` |
| Configure | `xdg_toplevel.configure` | `WIN_CONFIGURE` |
| Input | `wl_keyboard.key` etc. | pre-translated `KeyEvent`/`PointerEvent` |

**SOLID anchors:**

- Single-responsibility: each verb does one thing. Render loop reads
  state; verbs mutate state.
- Open/closed: future sub-surfaces are an additive role.
- Liskov: `WIN_COMMIT` semantics are identical regardless of pixel
  format, source pid, surface size.

## 4. Verb set

`cluu_proto::window` module exports 16 labels. No conflict with
spec 1 (80-81), spec 2 (100-110), spec 3 (82-88), spec 3's
COMPOSITOR_HANDOFF (200).

```rust
// Client → compositor (compositor:client):
pub const WIN_CREATE_LABEL:                u32 = 210;
pub const WIN_DESTROY_LABEL:               u32 = 211;
pub const WIN_ATTACH_BUFFER_LABEL:         u32 = 212;
pub const WIN_DETACH_BUFFER_LABEL:         u32 = 213;
pub const WIN_COMMIT_LABEL:                u32 = 214;
pub const WIN_REQUEST_FRAME_CALLBACK_LABEL:u32 = 215;
pub const WIN_SET_TITLE_LABEL:             u32 = 216;
pub const WIN_SET_GEOMETRY_HINT_LABEL:     u32 = 217;
pub const WIN_REQUEST_FOCUS_LABEL:         u32 = 218;

// Compositor → client (per-client async endpoint):
pub const WIN_FRAME_READY_LABEL:           u32 = 220;
pub const WIN_BUFFER_RELEASED_LABEL:       u32 = 221;
pub const WIN_CONFIGURE_LABEL:             u32 = 222;
pub const WIN_INPUT_LABEL:                 u32 = 223;
pub const WIN_FOCUS_IN_LABEL:              u32 = 224;
pub const WIN_FOCUS_OUT_LABEL:             u32 = 225;
pub const WIN_CLOSED_LABEL:                u32 = 226;
```

**Semantic summary:**

| Verb | Request | Reply / event |
|---|---|---|
| WIN_CREATE | `{ session_token, initial_size }` | `Result<{ surface_id }, WinErr>` |
| WIN_DESTROY | `{ surface_id }` | `Result<(), WinErr>` |
| WIN_ATTACH_BUFFER | `{ surface_id, buffer_id, frame_token, pixel_format, stride, w, h }` | `Result<(), WinErr>` |
| WIN_DETACH_BUFFER | `{ surface_id, buffer_id }` | `Result<(), WinErr>` |
| WIN_COMMIT | `{ surface_id, buffer_id, damage: Vec<Rect> }` | `Result<(), WinErr>` |
| WIN_REQUEST_FRAME_CALLBACK | `{ surface_id }` | `Result<(), WinErr>` (callback fires via WIN_FRAME_READY) |
| WIN_SET_TITLE | `{ surface_id, title }` | `Result<(), WinErr>` |
| WIN_SET_GEOMETRY_HINT | `{ surface_id, hints }` | `Result<(), WinErr>` |
| WIN_REQUEST_FOCUS | `{ surface_id }` | `Result<bool, WinErr>` |
| WIN_FRAME_READY | `{ surface_id, timestamp_ms }` | async event |
| WIN_BUFFER_RELEASED | `{ surface_id, buffer_id }` | async event |
| WIN_CONFIGURE | `{ surface_id, size, scale }` | async event |
| WIN_INPUT | `{ surface_id, event: InputEvent }` | async event |
| WIN_FOCUS_IN | `{ surface_id }` | async event |
| WIN_FOCUS_OUT | `{ surface_id }` | async event |
| WIN_CLOSED | `{ surface_id }` | async event |

## 5. Wire format

Same encoding pattern as specs 1-3.

```
words[0] = payload_len
words[1] = ABI_VERSION (= 1)
words[2..6] = 0 (reserved)
payload  = postcard::to_slice(&Request)   // or &Reply / &Event
```

**Types (`cluu_proto::window`):**

```rust
pub type SurfaceId = u32;
pub type BufferId  = u32;

pub struct Rect { pub x: i32, pub y: i32, pub w: u32, pub h: u32 }
pub struct Size { pub w: u32, pub h: u32 }

pub struct WinCreateRequest {
    pub session_token: Option<TokenHandle>,
    pub initial_size:  Size,
}
pub struct WinCreateReply { pub surface_id: SurfaceId }

pub struct WinAttachBufferRequest {
    pub surface_id:   SurfaceId,
    pub buffer_id:    BufferId,
    pub frame_token:  TokenHandle,
    pub pixel_format: PixelFormat,
    pub stride:       u32,
    pub width:        u32,
    pub height:       u32,
}

pub struct WinCommitRequest {
    pub surface_id: SurfaceId,
    pub buffer_id:  BufferId,
    pub damage:     Vec<Rect>,
}

pub struct WinRequestFrameCallbackRequest { pub surface_id: SurfaceId }
pub struct WinSetTitleRequest             { pub surface_id: SurfaceId, pub title: String }
pub struct WinSetGeometryHintRequest      { pub surface_id: SurfaceId, pub hints: GeometryHints }
pub struct GeometryHints {
    pub min_size:       Option<Size>,
    pub max_size:       Option<Size>,
    pub preferred_size: Option<Size>,
    pub fixed_aspect:   Option<(u32, u32)>,
}
pub struct WinRequestFocusRequest { pub surface_id: SurfaceId }
pub struct WinDestroyRequest      { pub surface_id: SurfaceId }
pub struct WinDetachBufferRequest { pub surface_id: SurfaceId, pub buffer_id: BufferId }

pub struct WinFrameReadyEvent     { pub surface_id: SurfaceId, pub timestamp_ms: u64 }
pub struct WinBufferReleasedEvent { pub surface_id: SurfaceId, pub buffer_id: BufferId }
pub struct WinConfigureEvent      { pub surface_id: SurfaceId, pub size: Size, pub scale: u32 }
pub struct WinInputEvent          { pub surface_id: SurfaceId, pub event: InputEvent }
pub struct WinFocusInEvent        { pub surface_id: SurfaceId }
pub struct WinFocusOutEvent       { pub surface_id: SurfaceId }
pub struct WinClosedEvent         { pub surface_id: SurfaceId }

pub enum InputEvent {
    Key(KeyEvent),
    Pointer(PointerEvent),
    Wheel(WheelEvent),
}

pub struct KeyEvent {
    pub key:       Key,
    pub modifiers: Modifiers,
    pub state:     KeyState,
    pub char:      Option<char>,
}
pub struct PointerEvent {
    pub kind:      PointerKind,
    pub pos:       (i32, i32),
    pub button:    Option<MouseButton>,
    pub state:     Option<KeyState>,
    pub modifiers: Modifiers,
}
pub struct WheelEvent {
    pub pos:       (i32, i32),
    pub delta_x:   i32,
    pub delta_y:   i32,
    pub modifiers: Modifiers,
}

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
    Unknown(u32),
}
pub enum KeyState { Pressed, Released, Repeat }
pub struct Modifiers { pub ctrl: bool, pub shift: bool, pub alt: bool, pub super_: bool }
pub enum PointerKind { Motion, Button, Enter, Leave }
pub enum MouseButton { Left, Right, Middle, Side(u32) }

pub enum PixelFormat { Bgra8888, Rgba8888, Rgb565 }

pub enum WinErr {
    InvalidSurface, InvalidBuffer, InvalidFormat, GeometryRejected,
    SessionRevoked, NotFocused, Internal(u32),
}
```

**libcluu wrapper (`libcluu::window`):**

```rust
pub fn create(session: Option<TokenHandle>, size: Size) -> Result<SurfaceId, WinErr>;
pub fn destroy(surface_id: SurfaceId) -> Result<(), WinErr>;
pub fn attach_buffer(surface_id: SurfaceId, buffer_id: BufferId,
                     frame_token: TokenHandle, fmt: PixelFormat,
                     stride: u32, size: Size) -> Result<(), WinErr>;
pub fn detach_buffer(surface_id: SurfaceId, buffer_id: BufferId) -> Result<(), WinErr>;
pub fn commit(surface_id: SurfaceId, buffer_id: BufferId, damage: &[Rect])
    -> Result<(), WinErr>;
pub fn request_frame_callback(surface_id: SurfaceId) -> Result<(), WinErr>;
pub fn set_title(surface_id: SurfaceId, title: &str) -> Result<(), WinErr>;
pub fn set_geometry_hint(surface_id: SurfaceId, hints: GeometryHints)
    -> Result<(), WinErr>;
pub fn request_focus(surface_id: SurfaceId) -> Result<bool, WinErr>;
pub fn recv_event() -> Result<WindowEvent, WinErr>;

pub enum WindowEvent {
    FrameReady(WinFrameReadyEvent),
    BufferReleased(WinBufferReleasedEvent),
    Configure(WinConfigureEvent),
    Input(WinInputEvent),
    FocusIn(WinFocusInEvent),
    FocusOut(WinFocusOutEvent),
    Closed(WinClosedEvent),
}
```

**Error semantics:**

Every reply is `Result<_, WinErr>`. No timeouts. Compositor death
surfaced via cap revocation: kernel revokes endpoints → client's
pending IPC returns `EBADTOKEN` → libcluu translates to
`WinErr::Internal(ECOMPOSITOR_DEAD)`. Client typically exits or
re-registers.

## 6. Buffer + damage protocol detail

**Buffer state machine (per surface, per buffer_id):**

```
Detached       — buffer doesn't exist for this surface
Attached       — buffer registered; not committed
Pending        — committed; compositor will use on next render
Scanout        — compositor reading this buffer for current frame
ReleasedLocked — compositor done reading; will emit WIN_BUFFER_RELEASED
```

**Transitions:**

```
WIN_ATTACH_BUFFER          → Attached
WIN_COMMIT                 → Pending (replaces previous Pending if any)
compositor renders         Pending → Scanout
                            (previous Scanout → ReleasedLocked)
compositor finalizes frame ReleasedLocked → Attached
                            + emit WIN_BUFFER_RELEASED
WIN_DETACH_BUFFER          → Detached (refused if Scanout)
```

**Double-buffering pattern (client side):**

```rust
struct SurfaceBufferPool {
    bufs: [Buffer; 2],
    next: usize,
    pending_release: Option<BufferId>,
}
impl SurfaceBufferPool {
    fn next_writable(&mut self) -> &mut Buffer { &mut self.bufs[self.next] }
    fn on_release(&mut self, id: BufferId) { /* mark buffer free */ }
    fn commit(&mut self, surface: SurfaceId, damage: &[Rect])
        -> Result<(), WinErr>
    {
        let b = &self.bufs[self.next];
        window::commit(surface, b.id, damage)?;
        self.next = 1 - self.next;
        Ok(())
    }
}
```

**Damage semantics:**

- Coordinates: surface-local px; (0,0) = top-left.
- Rects clipped to surface bounds by compositor.
- `damage: Vec<Rect>` empty → damage-all.
- One rect equal to surface bounds → equivalent to damage-all but
  explicit.
- Damage does NOT accumulate between commits; each commit declares
  its own damage.

**Pixel format and stride:**

- `pixel_format` declared at `WIN_ATTACH_BUFFER`; subsequent commits
  for that buffer assume same format.
- `stride` is bytes-per-row; must be `≥ width × bytes_per_pixel(fmt)`.
- Buffer size must satisfy `stride * height ≤ typed_frame_size`.
  Compositor verifies; rejects with `InvalidBuffer` otherwise.

**Mid-stream format change:** not supported. Detach + attach new.

**Buffer detach while Scanout / Pending / ReleasedLocked:**
`Err(InvalidBuffer)`. Client must wait for `WIN_BUFFER_RELEASED` or
commit a different buffer first.

**Compositor crash mid-frame:**

Typed-frame refcount holds the buffer alive while compositor mapped
it. On compositor death, kernel revokes; refcount drops only when
client also drops the frame_token. No leak; frame returns to Untyped
pool when both sides release.

## 7. Frame callback + present cycle

**Frame-callback contract:**

`WIN_REQUEST_FRAME_CALLBACK` registers a one-shot callback. Compositor
emits `WIN_FRAME_READY { surface_id, timestamp_ms }` once after the
next render tick that includes this surface; then forgets. Client
must re-request to get another callback.

**`timestamp_ms`:** monotonic millisecond clock at the moment the
frame was committed to the framebuffer (or scheduled).

**Compositor's render loop (formal):**

```
loop {
    drain_request_queue();
    wait_for_frame_tick();
    let now_ms = monotonic_ms();

    for surface in surfaces_with_pending_commits() {
        promote_buffer_states(surface);
        // Pending -> Scanout; previous Scanout -> ReleasedLocked
    }
    render_compositor_frame();

    for surface in surfaces {
        if let Some(buf) = surface.take_released_locked() {
            ipc_send(surface.client_endpoint, WIN_BUFFER_RELEASED_LABEL,
                     &postcard(&WinBufferReleasedEvent { surface_id: surface.id, buffer_id: buf }), &[]);
        }
    }

    for surface in surfaces_with_pending_frame_callback() {
        ipc_send(surface.client_endpoint, WIN_FRAME_READY_LABEL,
                 &postcard(&WinFrameReadyEvent { surface_id: surface.id, timestamp_ms: now_ms }), &[]);
        surface.clear_frame_callback_request();
    }
}
```

**Tick source:**

Today 500 ms (commit `bc6b61e`); future hook into framebuffer vsync.
Spec 4 is timer-vs-vsync agnostic; the `wait_for_frame_tick`
abstraction swaps as the framebuffer driver gains vsync.

**Client animation pattern:**

```rust
fn on_frame_ready(timestamp_ms: u64) {
    let dt = timestamp_ms - last_timestamp;
    update_state(dt);
    let buf = pool.next_writable();
    render(buf);
    window::commit(surface, buf.id, damage)?;
    window::request_frame_callback(surface)?;
    last_timestamp = timestamp_ms;
}
```

Static clients (cluuterm idle, status bar) request the callback only
when content changes. Idle = no traffic.

**Edge cases:**

- Request before initial commit → `Err(InvalidSurface)` (no attached
  buffer to render).
- Multiple commits between frames → second commit replaces first's
  Pending; first buffer released immediately.
- Cancellation → not in spec 4; client ignores stale events.

**`broadcast_frame_ready` retired:**

Compositor maintains `frame_callback_requested: bool` per surface.
Render → walk surfaces with flag set → emit → clear. No blanket fanout.

## 8. Input events + focus

**Focus model:** one focused surface per compositor (single-seat).
Compositor decides; clients request via `WIN_REQUEST_FOCUS`.

**Focus transitions:**

```
compositor decides new focus F:
  ├ if previous focus P != None: emit WIN_FOCUS_OUT to P's client
  ├ emit WIN_FOCUS_IN to F's client
  ├ synthetic Released KeyEvent for each held modifier (to P)
  └ subsequent WIN_INPUT routes to F until next focus change
```

**Focus policy (spec 4 landing):**
- Click-to-focus.
- Compositor-bound keyboard shortcuts (Alt+Tab etc.) — landing leaves
  policy open; compositor may dispatch focus changes at any time.
- VT-switch: per `project_input_routing_design`. Compositor
  surrenders input when its VT becomes inactive.

**Input routing:**

```
kbd → compositor (raw scancode + state)
  compositor:
    ├ apply active keymap → KeyEvent { key, modifiers, state, char }
    ├ check compositor bindings — consumed if matched
    ├ otherwise: route to focused_surface's client via WIN_INPUT
    └ if no focused surface: drop
```

**Keymap source:** `/etc/keymap/<layout>.toml` or compiled-in default.
Runtime keymap change deferred.

**Repeat:** compositor's internal repeat machinery emits
`KeyEvent { state: Repeat, ... }`. Apps treat Repeat as Pressed
unless they care to distinguish.

**Pointer events:**

- Motion: every pointer position update; compositor coalesces stale
  events under load.
- Button: press / release.
- Enter / Leave: pointer entered / left surface bounds.

**Wheel events:** `delta_x` / `delta_y` in Windows convention (±120
per detent).

**Touch:** not in spec 4.

**Per-client async endpoint:** events delivered on a per-client
endpoint minted at `WIN_CREATE` (compositor stores in surface state).
Replaces global `compositor:input` for delivery.

## 9. Window lifecycle + session integration

**Surface lifecycle states:**

```
Created          — WIN_CREATE returned; no buffer
BufferAttached   — first buffer attached; not committed
Mapped           — first commit landed; visible
Unmapped         — buffer detached while Mapped; not visible
Closing          — compositor emitted WIN_CLOSED; awaiting WIN_DESTROY
Destroyed        — resources reclaimed
```

**Transitions:**

```
                 WIN_CREATE
                     │
                     ▼
                  Created
                     │ WIN_ATTACH_BUFFER
                     ▼
              BufferAttached
                     │ WIN_COMMIT
                     ▼
                  Mapped ◄─────────┐
                     │             │
                     │ WIN_COMMIT  │ WIN_DETACH_BUFFER (last buffer)
                     │  (more)     ▼
                     │           Unmapped
                     │             │
                     │             ▼ WIN_ATTACH_BUFFER + WIN_COMMIT
                     │             └────►┐
                     │                   │
                     ▼                   ▼
                 (compositor decides to close
                  OR SESSION_ENDED for surface's session)
                     │
                     ▼  emit WIN_CLOSED
                  Closing
                     │ WIN_DESTROY from client (or cap-revocation
                     │   force-destroy if client died)
                     ▼
                 Destroyed
```

**Session integration (spec 3 link):**

Every surface carries `session_id: Option<u32>`. Set at `WIN_CREATE`
from the request's `session_token`:

```rust
fn handle_win_create(req: WinCreateRequest, caller_pid: u32)
    -> Result<WinCreateReply, WinErr>
{
    let session_id = match req.session_token {
        None => None,
        Some(t) => Some(procmgr::session_query(t)?.session_id),
    };
    let surface = Surface {
        id: next_surface_id(),
        client_pid: caller_pid,
        client_endpoint: mint_per_client_event_endpoint(caller_pid),
        session_id,
        state: SurfaceState::Created,
        buffers: vec![],
        frame_callback_requested: false,
        title: String::new(),
        geometry_hints: GeometryHints::default(),
    };
    surfaces.insert(surface);
    Ok(WinCreateReply { surface_id: surface.id })
}
```

**Session-ended fanout (consumes spec 3's SESSION_ENDED event):**

```rust
fn on_session_ended(event: SessionEndedEvent) {
    for surface in surfaces.iter_mut() {
        if surface.session_id == Some(event.session_id) {
            ipc_send(surface.client_endpoint, WIN_CLOSED_LABEL,
                     &postcard(&WinClosedEvent { surface_id: surface.id }), &[]);
            surface.state = SurfaceState::Closing;
        }
    }
    spawn_fresh_login_if_no_active_sessions();
}
```

Force-destroy of `Closing` surfaces happens via cap revocation: when
the client dies, its endpoint is revoked; compositor's next send fails;
compositor force-destroys the surface (no timeout-based sweep).

**Sessionless surfaces (login, status bar):** `session_id = None`.
Not affected by `SESSION_ENDED`. Persist across session lifecycles.

**Compositor crash:** state lost; clients see `EBADTOKEN` on next
IPC; cluuterm exits → session destroy cascade.

**Client crash:** procmgr revokes endpoints; compositor's send fails;
force-destroy on next render tick. Refcounts on typed-frame buffers
drop; frames return to Untyped pool.

**Multi-surface clients:** one process may hold many surfaces;
independent state and session_id per surface.

**Geometry hints:** advisory. `WIN_CONFIGURE` carries the actual
resolved size after compositor's resize policy.

**Title:** UTF-8; bounded to 256 bytes; oversize truncated silently.

## 10. Migration plan

Depends on spec 1 (typed frames, envelope used for compositor's
autostart), spec 2 (per-session /dev/pts overlay), spec 3 (compositor
lifecycle + session integration).

1. **`cluu_proto::window` module.** Verb labels (210-226), request /
   reply / event types, `WinErr`, `InputEvent` variants,
   `PixelFormat`. libcluu `window` wrapper type surface. Build clean.

2. **Compositor: per-surface state machine.** Replace ad-hoc window
   tracking with `Surface { id, client_pid, client_endpoint,
   session_id, state, buffers, ... }`. Implement state transitions.

3. **Compositor: per-surface client event endpoint.** Mint per-client
   async endpoint at `WIN_CREATE`. Old global `compositor:input`
   service retired in favor of per-client endpoints.

4. **Compositor: buffer state + release events.** Attached / Pending
   / Scanout / ReleasedLocked state machine per buffer. Emit
   `WIN_BUFFER_RELEASED` after render finishes. Frame typing inc/dec
   per attach/detach.

5. **Compositor: per-frame callback request.** Replace
   `broadcast_frame_ready` with per-surface `frame_callback_requested`
   flag. Render → walk surfaces with flag set → emit → clear.

6. **Compositor: pre-translated input.** Read keymap from
   `/etc/keymap/<layout>.toml`. Translate raw scancodes →
   `KeyEvent { key, modifiers, state, char }`. Wire to per-client
   endpoint via `WIN_INPUT`.

7. **Compositor: focus tracking.** Single focused surface;
   `WIN_REQUEST_FOCUS` handler with policy stub (accept all). Emit
   `WIN_FOCUS_IN` / `WIN_FOCUS_OUT`. Synthetic modifier-release on
   focus-out.

8. **Compositor: session-ended fanout.** Subscribe to `SESSION_ENDED`
   per spec 3. On event: emit `WIN_CLOSED` to surfaces with matching
   session_id; mark Closing; cap-revocation drives force-destroy.

9. **libcluu native window API.** `libcluu::window::*`,
   `SurfaceBufferPool` helper.

10. **Cluuterm flips to spec 4 verbs.** Rebuilds window code against
    `libcluu::window`. Two-frame buffer pool. Per-frame request
    callback pattern. Damage rects for changed cells.

11. **Login binary flips.** Draws via `libcluu::window`; passes
    `session_token: None`. Single buffer suffices.

12. **Other clients.** `compdemo` flipped or retired.

13. **Delete dead code.**
    - `broadcast_frame_ready`.
    - Global `compositor:input` service registration.
    - Today's informal window-verb payloads superseded by the new
      labels (210-226).
    - Any 60 Hz saturating tick logic.

14. **Verify.** Acceptance criteria pass.

**Per-step gate:** `bash scripts/harness_run.sh` reaches `compositor:
ready`, login window visible, interactive login → cluuterm window
with shell prompt.

**Cross-spec sequencing:**

```
spec 1 (steps 1-4 first)
   ▼
spec 2
   ▼
spec 3
   ▼
spec 4
```

## 11. Acceptance criteria

### Build

- `cargo xtask build` clean.
- `cargo clippy --workspace --all-targets -- -D warnings` clean.

### Grep zero-hit proofs

- `git grep broadcast_frame_ready` → 0.
- `git grep '"compositor:input"'` (registered as a *service name*)
  → 0.
- `git grep "fn handle_win_register"` (legacy informal handler) → 0.

### Grep one-match proofs

- `git grep "WIN_CREATE_LABEL.*= 210"` → one in `cluu_proto::window`.
- `git grep "WIN_BUFFER_RELEASED_LABEL.*= 221"` → one.
- `git grep "fn handle_win_create" userspace/compositor/` → one.
- `git grep "SurfaceBufferPool" userspace/libcluu/` → one.

### Functional smoke

- Boot reaches `compositor: ready` and login window visible.
- Login → cluuterm window; shell prompt renders; typing produces
  visible echo.
- Click-to-focus on multi-window setup routes input correctly.
- Resize cluuterm via compositor IPC → `WIN_CONFIGURE` →
  cluuterm reallocates buffer + commits new size → no flicker, no
  tearing.

### Buffer-protocol markers

- `l4_double_buffer_alternates`: two-buffer alternation; release
  events ordered before reuse.
- `l4_detach_while_scanout_denied`: detach during Scanout →
  `InvalidBuffer`.
- `l4_format_mismatch_denied`: mid-stream format change rejected.
- `l4_oversize_stride_rejected`: stride too small → `InvalidBuffer`.

### Frame-callback markers

- `l4_frame_ready_one_shot`: one request → one event.
- `l4_idle_no_callbacks`: no requests → zero events over 5 s.
- `l4_animation_loop_pacing`: per-frame interval matches tick.

### Damage markers

- `l4_partial_damage_repaint`: small dirty rect = small framebuffer
  region updated (proof via `fb_dump.sh` diff).
- `l4_empty_damage_repaints_full`: empty list = full repaint.

### Input markers

- `l4_input_pretranslated`: unmodified 'A' → `char: Some('a')`.
- `l4_input_shift_modifier`: Shift+A → `char: Some('A')`, shift bit
  set.
- `l4_input_routes_to_focus`: events only at focused client.
- `l4_focus_out_releases_modifiers`: synthetic Released for held
  modifiers on focus-out.

### Session-integration markers

- `l4_surface_session_id_set`: surface created with token →
  internal session_id matches.
- `l4_session_ended_closes_surfaces`: kill session leader → matching
  surfaces receive `WIN_CLOSED`; sessionless not closed.
- `l4_sessionless_persists`: login window survives session
  create/destroy cycle (fresh login spawn, sessionless).

### Cap-discipline markers

- `l4_invalid_surface_id_denied`: foreign surface_id → `InvalidSurface`.
- `l4_compositor_death_cap_revoke`: client's pending verb returns
  `ECOMPOSITOR_DEAD` after compositor crash.
- `l4_client_death_force_destroy`: cluuterm killed → compositor
  force-destroys surface on next render tick; refcounts drop; no
  leak.

### No-timeout proof

`grep -rn "recv_with_timeout\|call_with_timeout"
userspace/compositor/src/` returns same set as today (no new
timeouts introduced).

### Performance gate

- Commit → screen visible: under 16 ms p99 (when vsync-paced;
  500 ms today).
- Idle compositor CPU < 1%.
- Damage-rect optimization: 1920×1080 full-screen → 100×100 dirty
  rect ≈ 1/200 the blit cost of full repaint.

### Documentation

- File at `docs/superpowers/specs/2026-05-18-window-protocol-design.md`.
- Cross-referenced from `docs/ROADMAP.md` and `docs/CURRENT_PHASE.md`.
- Linked from specs 1, 2, 3.

### Cross-spec dependency

- Verb labels 210-226 do not conflict with spec 1 (80-81), spec 2
  (100-110), spec 3 (82-88), or spec 3's COMPOSITOR_HANDOFF (200).
- Typed-frame Grant semantics from spec 1's frame-typing redesign
  honored on every attach/detach.
- `session_token` field consumed per spec 3 §10.

## 12. Open follow-ups (out of spec 4)

- Sub-surfaces, surface roles (popup, transient, menu).
- Touch input.
- Buffer formats: YUV, planar, dmabuf.
- HiDPI rendering policy (`scale` interpretation).
- Cancellable frame callbacks.
- Raw-scancode focus mode (per-surface capability).
- Runtime keymap change.
- Mouse cursor / cursor surface management.
- Drag-and-drop primitives.
- Clipboard / selection primitives.

## 13. Related memory

- `[[no-timeouts]]` — cap-revocation honored; no new timeouts.
- `[[frame-typing-redesign-landed-2026-05-18]]` — buffer-as-typed-frame
  underpinning.
- `[[map-share-phys-uaf]]` — closed bug whose discipline this spec
  consumes (CacheRegion invalidation policy).
- `[[fb-wc-landed]]` — framebuffer WC mapping; spec 4 doesn't change
  fb write semantics, just defines what clients submit.
- `[[input-routing-design]]` — VT-switch + kbd → active-VT routing.
- `[[next-direction-fb-tui]]` — TUI compositor work this spec
  formalizes.

## 14. Related committed work

- `1a8c218` docs(spawn-window-pty): inventory of current pipeline.
- `bc6b61e` compositor: tick at 500 ms (current frame-pacing source).
- `06fcf1f` compositor: drop blink ownership; strip per-recv log
  (idle-friendliness improvement spec 4 builds on).
- `9fda763` compositor recv_any 30 s loop driven by TIME_TICK
  (matches `feedback_no_timeouts` discipline).
- `5c62468` compositor: double-line chrome for focused window
  (focus tracking precursor).

## 15. Related specs

- Spec 1: unified spawn protocol (typed frames + envelope used for
  compositor's autostart entry).
- Spec 2: terminal + PTY unification (per-session `/dev/pts/`; cluuterm
  spec-4-side compositor client + spec-2-side pts owner concurrently).
- Spec 3: session lifecycle (`SESSION_ENDED` event consumed by spec 4
  to close session windows; COMPOSITOR_SESSION_HANDOFF verb at label
  200 is spec 3's; spec 4 uses 210+).
