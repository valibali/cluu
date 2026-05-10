# TUI Compositor Core + Draw Protocol — Design Spec

**Status:** Draft, awaiting review
**Date:** 2026-05-10
**Sub-project:** A of TUI compositor workstream (B = `cluuterm` terminal emulator, C = PS/2 mouse, D = primitives library extraction)

## 1. Goal

Add a userspace compositor service to CLUU that owns one VT, draws floating windows with rounded Unicode chrome, dispatches keyboard input to the focused window, and exposes a shared-memory cell-grid protocol for native apps. Legacy console + TTY + vt manager keep running unchanged on the other VTs.

This is the foundation everything visual hangs on (terminal emulator, mouse, status bar widgets, etc.). It deliberately ships *without* a terminal emulator, mouse, or app ecosystem; those are separate sub-projects.

## 2. Out of scope

- Terminal emulator (`cluuterm`) — sub-project B, follows immediately after A
- PS/2 mouse driver + `/dev/input/mice` — sub-project C, blocked on Phase 5 raw-input work
- Primitives library extraction (`userspace/libtui`) — sub-project D, only worth doing once a second consumer exists
- Multi-window per app (1 app = 1 window assumed throughout v1)
- Theme/palette config (`/etc/compositor.toml`) — uses hardcoded xterm-256 palette
- Compositor taking over all VTs (replacing vt manager) — future migration only
- Animations, transparency, RGB cells beyond the protocol's reserved bits
- Mouse pointer rendering

## 3. Constraints

- Kernel freeze active through ~2026-10-21. Spec is **userspace-only**. All needed kernel primitives already exist: `MAP_DEVICE_WC` (commit `f6ae39f`), `FrameAllocate`/`FrameFree` (op 70/71), `MAP_FRAME_TOKEN` (flag 0x400), `PROC_EXIT_LABEL`.
- Must not regress legacy stack on VT1-3.
- Glyph atlas (Workstream A of `2026-05-10-fb-atlas-and-devfb0.md`) is a prerequisite — compositor reuses it for fast glyph blits.
- `/dev/fb0` (Workstream B of the same plan) is a prerequisite — compositor opens fb via Unix path, not the legacy `framebuffer_acquire()` helper.

## 4. System placement

```
                                        ┌──────────────┐
                                        │ procmgr      │
                                        └─────┬────────┘
       ┌─────────┐    Ctrl+Alt+F1..F4         │ spawn / exit notify
       │   kbd   │──────────────┐             │
       └────┬────┘              │             ▼
            │ raw events        ▼      ┌──────────────┐
            │              ┌─────────┐ │ vt manager   │
            │              │  vt mgr │ │ (existing)   │
            │              └────┬────┘ └──────────────┘
            │ active VT decides where input goes
            ▼
   ┌────────────────────┐                      ┌────────────────────┐
   │ console (VT1..VT3) │  legacy TTY apps     │ compositor (VT4)   │
   │ ─ today's stack    │                      │ ─ NEW              │
   └────────┬───────────┘                      └────────┬───────────┘
            │ active VT writes /dev/fb0                 │ active VT writes /dev/fb0
            ▼                                           ▼
                       ┌─────────────────────┐
                       │  /dev/fb0 (PAT WC)  │
                       └─────────────────────┘
```

The compositor process is `userspace/compositor/`, registered via the registry at boot like any other service. Init Cluufile spawns it and assigns VT4 (configurable). Three IPC endpoints:

- `compositor:client` — apps register windows, send damage, destroy windows
- `compositor:input` — kbd routes raw key events here while VT4 is active
- `compositor:control` — vt manager + init send VT activate/deactivate; future shutdown handshake

When VT4 is the active VT, the compositor opens `/dev/fb0` and renders. When inactive, it stops drawing (matches today's per-VT console behavior on activate/deactivate). Ctrl+Alt+F1..F4 stays consumed by kbd → vt manager and is **not** delivered to the compositor.

## 5. Cell payload

A cell is one packed `u64`, little-endian. Layout:

| Bits  | Size | Field        | Notes                                          |
|-------|------|--------------|------------------------------------------------|
| 0..21 | 21   | codepoint    | Unicode scalar value, 0..=0x10FFFF             |
| 21..29| 8    | fg_index     | 0..=255, lookup into compositor palette        |
| 29..37| 8    | bg_index     | 0..=255                                        |
| 37..40| 3    | attrs        | bit 0 bold, bit 1 underline, bit 2 reverse     |
| 40..64| 24   | reserved     | must be zero; future RGB / extended attrs      |

Compositor holds one `[u32; 256]` palette of ARGB values, initialised to standard xterm-256.

## 6. SHM window region

Per-window region, page-aligned, allocated by compositor via `FrameAllocate`. Header + cells, contiguous:

```rust
#[repr(C)]
struct WindowShm {
    magic:           u32,    // 0x57494e44 = "WIND"
    version:         u32,    // 1
    width:           u32,    // cells
    height:          u32,    // cells
    cursor_x:        u32,
    cursor_y:        u32,
    cursor_visible:  u32,
    generation:      u32,    // app bumps before signalling damage
    cells:           [u64],  // length = width * height
}
```

Total bytes = `round_up(32 + width * height * 8, 4096)`. Example: 80×24 grid → 32 + 15360 = 15392 → 16384 bytes (4 pages).

**Concurrency rule:** the app writes cells, then bumps `generation` with a release-store, then sends `WIN_DAMAGE`. The compositor reads `generation` with an acquire-load before and after blitting. If `generation` advanced mid-blit, the compositor accepts the partial frame as-is (no replay) and trusts the next `WIN_DAMAGE` to settle the picture. This is lockless and sufficient because every cell write is itself atomic at the hardware level (8-byte aligned writes on x86_64).

## 7. IPC protocol

All messages are `libcluu::types::Message` with a 6×u64 word payload + tag. New labels (numeric values to be assigned during implementation, not colliding with existing labels in `libcluu/src/ipc.rs`):

### App → `compositor:client`

- `WIN_REGISTER { req_w: u32, req_h: u32, title_len: u32 }` + payload bytes (title, max 31 bytes + NUL).
  Reply: `{ window_id: u64, frame_token: u64, granted_w: u32, granted_h: u32, error: u32 }`.
  `error == 0` on success. Compositor may shrink width/height to fit the screen; app must respect granted dimensions.
  After receiving the reply, the app invokes `space_map_range(addr, frame_token, size, MAP_FRAME_TOKEN | FLAGS_USER | FLAGS_RW)` to map the SHM at any address.

- `WIN_DAMAGE { window_id: u64, x: u32, y: u32, w: u32, h: u32 }`. Compositor clamps the rect to the window's interior and rebuilds that region on the next compose.

- `WIN_DESTROY { window_id: u64 }`. Compositor frees the frame and removes the window. (Implicit destroy on app exit also supported; see §10.)

- `WIN_SET_TITLE { window_id: u64, title_len: u32 }` + payload bytes. Re-renders chrome.

### `compositor:input` (from kbd)

- `KBD_EVENT { keycode: u32, modifiers: u32, codepoint: u32, kind: u32 }`. `kind` ∈ {0 = down, 1 = up, 2 = repeat}.

  Compositor inspects `modifiers + keycode` against the hotkey table (§9). If matched, consumes locally. Otherwise forwards to focused window's owner via:

- `INPUT_FORWARD { window_id: u64, keycode: u32, modifiers: u32, codepoint: u32, kind: u32 }` to a per-app input endpoint registered at `WIN_REGISTER` time (compositor records the sender's reply endpoint as the input-event sink for that window).

### `compositor:control` (from vt manager / init)

- `COMP_VT_ACTIVATE` — compositor reopens fb if needed, marks `active = true`, repaints all.
- `COMP_VT_DEACTIVATE` — compositor pauses drawing, retains all window state.
- `COMP_SHUTDOWN` — compositor frees all frames, exits cleanly.

## 8. Compositor internals

```rust
struct Compositor {
    fb: FbMapping,                        // /dev/fb0 mmap, MAP_DEVICE_WC
    cell_grid: Vec<u64>,                  // composite output cells (cols * rows)
    palette: [u32; 256],                  // xterm-256
    atlas: GlyphAtlas,                    // from FB plan Workstream A
    backbuf: Vec<u32>,                    // pixel back-buffer
    dirty: DirtyRegion,                   // pixel-level dirty bbox
    cell_dirty: Vec<(u16, u16)>,          // cells changed since last compose
    windows: Vec<Window>,                 // z-order: index 0 = bottom, last = top
    focused: Option<WindowId>,
    active: bool,                         // VT4 on screen
    status: StatusBar,
    cols: u16,
    rows: u16,
    next_id: u64,
}

struct Window {
    id: WindowId,
    owner_pid: u32,
    title: ArrayString<31>,
    x: u16, y: u16,                       // top-left in cell coords
    w: u16, h: u16,                       // total incl. chrome (>= 5,5)
    shm_va: *mut WindowShm,
    shm_token: FrameId,
    shm_size: usize,
    last_gen: u32,
    input_endpoint: usize,                // for INPUT_FORWARD
}
```

### Compose pipeline

Triggered after every IPC event, gated on `active`:

1. **Collect damage:** every `WIN_DAMAGE` plus any move/resize/focus change marks owning rects in `cell_dirty`.
2. **Recompute composite cells:** for each dirty cell `(cx, cy)`, walk windows top → bottom looking for one whose total rect covers `(cx, cy)`. The first hit decides the output cell:
   - If the cell is in the chrome strip (top 2 rows, bottom 2 rows, left 2 cols, right 2 cols of the window's total rect) → render chrome glyph (§9).
   - Else → translate to interior coords `(ix, iy) = (cx - win.x - 2, cy - win.y - 2)`; read `cells[iy * (w-4) + ix]` from the window's SHM with an acquire-load; copy into `cell_grid`.
   - If no window covers the cell → fill with the desktop background cell (a configurable u64 default).
3. **Render status bar:** unconditionally overwrite cell row 0 with the status string (§9).
4. **Glyph blit:** for each cell that changed in `cell_grid` since last compose, decode `(codepoint, fg_idx, bg_idx, attrs)`, look up `palette[fg_idx]` / `palette[bg_idx]`, look up atlas mask for the glyph, call `simd::blend_row` per glyph row, push via `backbuf.put_pixels_row`. The atlas covers CP437 today; codepoints outside CP437 fall through `unicode_to_cp437` (existing helper), which now also maps `U+256D..U+2570` (rounded arc forms) and the new 2×2 tier-3 corner glyphs to their custom CP437 slots.
5. **Flush back-buffer:** call `DoubleBufferBackend::flush()` (existing). Only the dirty pixel rect is copied to fb.

The pipeline is single-threaded inside the compositor process. The whole loop is `recv_any → parse → mutate state → compose → block again`. No locks anywhere.

## 9. Chrome (Tier 3 — 2×2-cell rounded corners)

Each window reserves chrome cells:

```
row 0:        TL_NW TL_NE  ─ ─ ─ ... ─ ─  TR_NW TR_NE
row 1:        TL_SW TL_SE  <title row>    TR_SW TR_SE
rows 2..h-3:  │              <interior>             │
row h-2:      BL_NW BL_NE                  BR_NW BR_NE
row h-1:      BL_SW BL_SE  ─ ─ ─ ... ─ ─  BR_SW BR_SE
```

Minimum window total dimensions: 5 cells × 5 cells (gives a 1×1 interior).

**Custom 8×16 bitmaps** required, 16 unique glyphs: 4 corners × 4 sub-cells each. Slotted into otherwise-unused CP437 indices `0xF0..0xFF`. Drawn by hand, stroke width 2 px on the curve, smooth quarter-circle arc reaching from sub-cell outer edge to sub-cell inner corner. Bitmaps live in `userspace/console/src/font_arc.rs` (new) and are merged into the atlas at startup via a new `GlyphAtlas::with_overrides` constructor that overlays specified CP437 indices.

**Edge characters** (between corners) are CP437-native and need no font extension:
- `─` U+2500 → CP437 0xC4
- `│` U+2502 → CP437 0xB3
- `═` U+2550 → CP437 0xCD (used for double / focused style fallback)
- `║` U+2551 → CP437 0xBA

**Style table:**

| Style       | Use                | TL/TR/BL/BR | H | V |
|-------------|--------------------|-------------|---|---|
| `Rounded`   | unfocused windows  | 2×2 arc     | ─ | │ |
| `Focused`   | focused window     | 2×2 arc + bold attr applied to corner glyphs | ─ | │ |

Both unfocused and focused use the same Tier-3 corner bitmaps; focused additionally sets the `bold` attr bit on those cells, which the renderer can interpret as a brighter palette entry (e.g., index `8 + (idx & 7)` for the standard 16 colors). Title color also brightens on focus.

**Title text:** rendered into row 1 between corner sub-cells, left-padded by 1 cell, ellipsised with `…` (U+2026) if longer than `w - 6` cells.

### Hotkey table (hardcoded v1)

| Chord                | Action                                                             |
|----------------------|--------------------------------------------------------------------|
| `Alt+Tab`            | focus next window (raise to top of z-order, mark prev unfocused)   |
| `Alt+Shift+Tab`      | focus previous window                                              |
| `Super+Arrow`        | move focused window 1 cell                                         |
| `Super+Shift+Arrow`  | resize focused window 1 cell (anchor top-left)                     |
| `Super+Q`            | send `INPUT_FORWARD { kind = close-request }` to focused window    |
| `Super+N`            | spawn `compdemo` via procmgr SPAWN; first registration becomes new focused window |

`compdemo` is the v1 placeholder app: a small native binary that fills its window with a slowly-shifting rainbow pattern, prints any received keystroke as a glyph, and exits on `close-request`. Becomes "spawn `cluuterm`" once sub-project B ships.

### Status bar

Cell row 0 is reserved for the compositor's status bar and is **not part of the window placement area**. Window `y` coordinates are constrained to `1 <= y <= rows - h`. Format of the row 0 contents:

```
[HH:MM:SS]  focused: <title>   |   windows: N
```

Refreshed on focus change and on a 1 Hz timer subscription to the timeserver. Because windows can never overlap row 0, the status bar is rendered as a final pass each compose with no overlap arbitration needed.

## 10. Lifecycle and error handling

**Window create:** app sends `WIN_REGISTER`. Compositor finds a free `(x, y)` slot (cascade from top-left, offset by N for the Nth window), allocates the frame, replies. Initial cells are zeroed. Compositor records the app's reply endpoint as the `input_endpoint` for forwarding events.

**Window destroy:**

- Explicit: app sends `WIN_DESTROY`.
- Implicit: procmgr forwards `PROC_EXIT_LABEL` to the compositor's exit endpoint. Compositor matches `pid` to all owned windows, drops each.

In both cases: free the frame token via `FrameFree`, remove from `windows`, mark covered cells dirty, recompute focus (next-z-order window becomes focused), redraw.

**Compositor death:** init monitors compositor as a per-VT primordial under restart policy `OnFailure` (memory: `Phase I`). On restart, all clients lose their windows; init also re-broadcasts a `compositor_ready` registry event so clients can re-register. Compositor itself does not persist state across restarts. (Acceptable for v1 — clients can be killed too.)

**Bad clients:**

- Invalid `window_id` → reply with `error = EBADF`-equivalent label, no state change.
- Damage rect out of bounds → clamp silently to interior bounds.
- Cell payload with codepoint > `0x10FFFF` → render as `U+FFFD` (replacement character).
- App never bumps `generation` → compositor uses last cells; no special signal.

**MAP_SHARE_PHYS UAF guard:** memory `project_map_share_phys_uaf.md` records that `invalidate_cache_after_mutation` is a no-op until refcount-aware invalidation lands. The compositor reads SHM cells via plain volatile reads with the generation acquire-load; this is safe under the current no-op invalidation.

## 11. Testing

All new harness markers added to `scripts/harness_run.sh`.

- `l2_compositor_smoke` — boot init with compositor on VT4, run a probe that registers a 20×10 window, writes a known cell pattern, signals damage, then re-reads `cell_grid` via a debug syscall (or asserts `BENCH_*`-style serial markers from compositor), expects the pattern to appear inside the chrome bounds.
- `l2_compositor_focus` — register 3 windows in sequence, inject Alt+Tab thrice from a synthetic `KBD_EVENT`, assert the focused pid in compositor's debug-print output cycles through the expected order.
- `l2_compositor_destroy` — register a window, kill the owner via `kill`, assert compositor emits `compositor: window N destroyed via exit` and that the cell_grid region is repainted.
- `l2_compositor_legacy_vt` — boot, switch to VT1 (legacy shell), back to VT4, assert both render correctly and no fb stomping. Watches for the pre-existing `vt/manifest` flake (memory: `project_vt_manifest_flake_2026_05_09.md`); retry once on NotFound.
- `b_compositor_blit` — perf bench analogous to `b_console_blit`. Probe writes a full-screen damage event through the compositor protocol; harness extracts `cycles_per_full_screen` and gates against `scripts/perf_ratchet.json`'s new `compositor_blit_cycles` field. Target: ≤ 1.5× the post-atlas `b_console_blit` baseline.

## 12. Files touched

New userspace crate `userspace/compositor/`:
- `Cargo.toml`, `Cluufile`
- `src/main.rs` — IPC event loop, registry registration, VT activate/deactivate
- `src/state.rs` — `Compositor` struct, window list, focus
- `src/protocol.rs` — message labels, parse/encode
- `src/compose.rs` — compose pipeline (§8)
- `src/chrome.rs` — Tier-3 corner rendering, title formatting, status bar
- `src/hotkeys.rs` — hotkey table + dispatch
- `src/shm.rs` — frame alloc/map/free wrappers

New userspace crate `userspace/compdemo/`:
- `Cargo.toml`, `Cluufile`, `src/main.rs` — rainbow + keystroke demo

Modified:
- `userspace/console/src/font_arc.rs` (new) — 16 custom 2×2 corner bitmaps
- `userspace/console/src/atlas.rs` — `with_overrides` constructor (or expose mutator)
- `userspace/console/src/renderer.rs` — extend `unicode_to_cp437` for `U+256D..0x2570` plus the new tier-3 corner codepoints (assigned in a private `cluu` Unicode private-use range, e.g., `U+E000..U+E00F`)
- `userspace/libcluu/src/ipc.rs` — new label constants
- `userspace/init/Cluufile` (or wherever vt assignments live) — spawn compositor on VT4
- `scripts/harness_run.sh` — new MARKER_MODEs
- `scripts/perf_ratchet.json` — new `compositor_blit_cycles` field

Reused without modification:
- Glyph atlas (post-Workstream A)
- `/dev/fb0` (post-Workstream B)
- `DoubleBufferBackend` from console crate (or import paths only — backend is moved into a shared module if needed)
- `MAP_DEVICE_WC`, `MAP_FRAME_TOKEN`, `FrameAllocate`/`FrameFree`

## 13. Open questions to resolve during implementation

1. Exact CP437 slots for the 16 tier-3 corner bitmaps (`0xF0..0xFF` is the candidate range; verify nothing in CLUU's font table currently uses those slots).
2. Numeric label assignments for `WIN_*`, `INPUT_FORWARD`, `COMP_*` — pick from `libcluu/src/ipc.rs`'s next free range.
3. Whether `DoubleBufferBackend` moves to `userspace/libcluu` or `userspace/libfb` (new crate) so the compositor doesn't depend on the console crate. Decision deferred to plan.
4. Whether init Cluufile picks VT4 by name or by a `compositor_vt = N` parameter.
5. Whether `compdemo` lives as its own crate or inside `userspace/compositor/examples/`.

## 14. Future sub-projects (after this spec ships)

1. **Sub-project B — `cluuterm` terminal emulator.** Spawned via Super+N. Reuses console's ANSI parser by extracting it into `libcluu/term`. Owns child shell's TTY. Becomes the path to running legacy apps as compositor windows. Brainstorm separately.
2. **Sub-project C — PS/2 mouse driver + `/dev/input/mice`.** Phase 5 work. Compositor adds a mouse pointer overlay, click-to-focus, drag-to-move/resize, passes events to focused window via `INPUT_FORWARD` with mouse `kind` values.
3. **Sub-project D — Primitives library extraction (`userspace/libtui`).** Worthwhile only once a second consumer beyond compositor exists (likely after `cluuterm` and a third TUI app).
4. **Multi-window per app, themes, RGB cells, all-VT compositor takeover.** Each on its own when the need shows up.
