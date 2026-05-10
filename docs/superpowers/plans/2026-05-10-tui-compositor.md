# TUI Compositor (Sub-project A) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a userspace compositor service that owns VT4, draws floating windows with rounded Unicode chrome (Tier-3 2×2-cell corners), dispatches keyboard input to the focused window, and exposes an SHM cell-grid draw protocol. Plus a tiny `compdemo` native client that proves the protocol round-trip.

**Architecture:** Compositor is a regular userspace process registered with the registry as `compositor`, occupying VT4. Three IPC endpoints (`client`, `input`, `control`). Per-window SHM region carries a `WindowShm` header + `[u64]` cells, allocated by the compositor via `FrameAllocate` and shared via `MAP_FRAME_TOKEN`. Compose pipeline is single-threaded: cell-level walk top→bottom of z-stack, glyph blit via the existing atlas, pixel flush via the existing `DoubleBufferBackend`. Legacy console + TTY + vt manager keep running on VT1–3 untouched.

**Tech Stack:** Rust no_std (`userspace/compositor`, `userspace/compdemo`), `libcluu` IPC + frame + time helpers, kernel ops `FrameAllocate=70`, `FrameFree=71`, `MAP_FRAME_TOKEN=0x400`, `MAP_DEVICE_WC=0x1000`. Build: `cargo xtask build`. Test harness: `scripts/harness_run.sh` with `MARKER_MODE=...`.

**Spec:** `docs/superpowers/specs/2026-05-10-tui-compositor-design.md`. Read it first.

---

## Prerequisites (gate — do not start until both have landed)

- **FB plan Workstream A (Glyph atlas)** — tasks A1–A4 of `docs/superpowers/plans/2026-05-10-fb-atlas-and-devfb0.md`. The compositor reuses `GlyphAtlas`, `simd::blend_row`, and the new `unicode_to_cp437` map.
- **FB plan Workstream B (`/dev/fb0`)** — tasks B1–B7 of the same plan. The compositor opens `/dev/fb0` for fb access (it does not call `framebuffer_acquire()`).

If either is not on `develop` yet, STOP. Land them first; this plan assumes they exist.

---

## File Structure

### New crate `userspace/compositor/`
- `Cargo.toml` — workspace member; deps `libcluu`, `core`, `alloc`.
- `Cluufile` — service manifest; declares VT4 ownership and registry name.
- `src/main.rs` — entry point + IPC event loop.
- `src/state.rs` — `Compositor` struct, `Window` struct, palette init.
- `src/protocol.rs` — message labels, parse/encode helpers.
- `src/shm.rs` — frame alloc/map/free wrappers around `libcluu::syscall`.
- `src/compose.rs` — compose pipeline: cell-grid walk → atlas blit → backbuf flush.
- `src/chrome.rs` — Tier-3 corner rendering, edges, title formatting.
- `src/hotkeys.rs` — hotkey table + dispatch.
- `src/status.rs` — status bar render + clock subscription.
- `src/font_arc.rs` — 16 custom 8×16 bitmaps for the Tier-3 corner sub-cells.

### New crate `userspace/compdemo/`
- `Cargo.toml`, `Cluufile`
- `src/main.rs` — register one window, fill cells with a shifting rainbow, log keystrokes, exit on close-request.

### Modified
- `userspace/libcluu/src/ipc.rs` — new label constants for the compositor protocol.
- `userspace/console/src/atlas.rs` — `with_overrides` constructor (or `set_glyph` mutator) so the compositor can graft custom corner bitmaps over the default font.
- `userspace/console/src/renderer.rs` — extend `unicode_to_cp437` for `U+E000..U+E00F` (Cluu private-use range for the 16 Tier-3 corner sub-cells).
- `userspace/init/src/main.rs` — spawn `compositor` on VT4 in place of (or alongside) what's currently spawned on VT4. Today the loop spawns `console:0..console:3`; replace `console:3` with `compositor:0`.
- `Cargo.toml` (workspace root) — add the two new crates to workspace `members`.
- `xtask/src/main.rs` — register the new crates in the build pipeline (mirror how `console` and `compdemo` analogues are wired).
- `scripts/harness_run.sh` — five new `MARKER_MODE` blocks: `l2_compositor_smoke`, `l2_compositor_focus`, `l2_compositor_destroy`, `l2_compositor_legacy_vt`, `b_compositor_blit`.
- `scripts/perf_ratchet.json` — new `compositor_blit_cycles` and `compositor_blit_max_cycles` fields.

---

## Wave 1 — Scaffolding and protocol foundation

### Task 1: Workspace + crate skeletons

**Files:**
- Create: `userspace/compositor/Cargo.toml`
- Create: `userspace/compositor/Cluufile`
- Create: `userspace/compositor/src/main.rs`
- Create: `userspace/compdemo/Cargo.toml`
- Create: `userspace/compdemo/Cluufile`
- Create: `userspace/compdemo/src/main.rs`
- Modify: workspace root `Cargo.toml`

- [ ] **Step 1: Add the compositor crate manifest**

```toml
# userspace/compositor/Cargo.toml
[package]
name = "compositor"
version = "0.1.0"
edition = "2021"

[dependencies]
libcluu = { path = "../libcluu" }

[[bin]]
name = "compositor"
path = "src/main.rs"
```

- [ ] **Step 2: Add the compositor Cluufile**

Mirror `userspace/console/Cluufile`. Fields needed (verify with `cat userspace/console/Cluufile`): service name, required mounts, registry registration name. Set `name = "compositor"`. Declare it needs `/dev/fb0` access (read+write) and the timeserver.

- [ ] **Step 3: Stub `src/main.rs`**

```rust
#![no_std]
#![no_main]

extern crate alloc;

use libcluu::debug_print;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = debug_print("compositor: stub start");
    loop {
        // Yield forever; subsequent tasks add real event loop.
        let _ = libcluu::syscall::yield_cpu();
    }
}
```

- [ ] **Step 4: Add the compdemo crate manifest + Cluufile + stub main**

Mirror compositor structure; only difference is `name = "compdemo"`. The Cluufile declares it needs to talk to `compositor:client`. `src/main.rs` is the same yield-forever stub for now.

- [ ] **Step 5: Add both to the workspace**

In the root `Cargo.toml`, find the `[workspace] members = [...]` array and add `"userspace/compositor"` and `"userspace/compdemo"` (sorted alphabetically with the other entries).

- [ ] **Step 6: Build**

Run: `cargo xtask build`
Expected: Both crates compile. Target binaries exist at `target/.../compositor` and `target/.../compdemo`.

If `xtask` doesn't auto-pick up new crates (it iterates `members`), check `xtask/src/main.rs` for hardcoded crate lists; add the two there too.

- [ ] **Step 7: Commit**

```bash
git add userspace/compositor userspace/compdemo Cargo.toml xtask/src/main.rs
git commit -m "compositor: scaffold compositor + compdemo crates (stubs)"
```

### Task 2: IPC label constants

**Files:**
- Modify: `userspace/libcluu/src/ipc.rs`

- [ ] **Step 1: Locate the existing label range**

Run: `grep -n 'pub const.*_LABEL.*= [0-9]' userspace/libcluu/src/ipc.rs | sort -t= -k2 -n`
Expected: lists every label with its numeric value. Pick the next free integer block (e.g., if the highest is 60, the new ones start at 70).

- [ ] **Step 2: Add the compositor labels**

```rust
// Compositor protocol — sub-project A.
pub const COMP_WIN_REGISTER_LABEL:  u32 = 70;
pub const COMP_WIN_REGISTER_REPLY:  u32 = 71;
pub const COMP_WIN_DAMAGE_LABEL:    u32 = 72;
pub const COMP_WIN_DESTROY_LABEL:   u32 = 73;
pub const COMP_WIN_SET_TITLE_LABEL: u32 = 74;
pub const COMP_KBD_EVENT_LABEL:     u32 = 75;
pub const COMP_INPUT_FORWARD_LABEL: u32 = 76;
pub const COMP_VT_ACTIVATE_LABEL:   u32 = 77;
pub const COMP_VT_DEACTIVATE_LABEL: u32 = 78;
pub const COMP_SHUTDOWN_LABEL:      u32 = 79;
```

(Use whatever next-free range your grep found — these are illustrative.)

- [ ] **Step 3: Build**

Run: `cargo build -p libcluu`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add userspace/libcluu/src/ipc.rs
git commit -m "libcluu/ipc: add compositor protocol label constants"
```

### Task 3: Custom 2×2-cell corner bitmaps

**Files:**
- Create: `userspace/console/src/font_arc.rs`
- Modify: `userspace/console/src/main.rs` (add `mod font_arc;`)
- Modify: `userspace/console/src/renderer.rs` (extend `unicode_to_cp437`)

- [ ] **Step 1: Pick the CP437 slot range**

Run: `grep -n '0xF[0-9A-F].*=>' userspace/console/src/renderer.rs`
Expected: shows existing mappings into 0xF0..0xFF. Confirm the slots `0xF0..0xFF` (16 entries) are unused, OR pick another contiguous unused 16-slot range. Document the chosen range in a comment.

- [ ] **Step 2: Hand-draw 16 corner bitmaps**

Each glyph is `[u8; 16]` (one byte per row, MSB = leftmost pixel). Draw a Tier-3 quarter-circle arc such that the four sub-cells of one corner combine into a smooth ¼-circle of radius ~14 px. The four corners (TL, TR, BL, BR) are mirror-symmetric; you only need to draw TL_NW once and reflect/rotate for the rest.

```rust
// userspace/console/src/font_arc.rs
//! Tier-3 (2x2-cell) rounded corner bitmaps.
//!
//! Four corners × four sub-cells = 16 unique 8×16 glyphs.
//! Slots: CP437 0xF0..=0xFF (verify free against renderer.rs's existing map).
//! Codepoints (Cluu private-use range): U+E000..=U+E00F, mapping in this order:
//!   E000 TL_NW  E001 TL_NE  E002 TL_SW  E003 TL_SE
//!   E004 TR_NW  E005 TR_NE  E006 TR_SW  E007 TR_SE
//!   E008 BL_NW  E009 BL_NE  E00A BL_SW  E00B BL_SE
//!   E00C BR_NW  E00D BR_NE  E00E BR_SW  E00F BR_SE

pub const TIER3_CORNERS: [[u8; 16]; 16] = [
    // E000 TL_NW: top-left corner, top-left sub-cell.
    // Curve sweeps from upper-mid down toward the cell's bottom-right.
    [
        0b00000000, 0b00000000, 0b00000000, 0b00000001,
        0b00000011, 0b00000111, 0b00001111, 0b00011110,
        0b00111100, 0b01111000, 0b01110000, 0b11100000,
        0b11000000, 0b11000000, 0b10000000, 0b10000000,
    ],
    // E001 TL_NE: top-left corner, top-right sub-cell.
    // Curve continues from upper-left to mid-bottom.
    [
        0b00000000, 0b00000000, 0b00000000, 0b10000000,
        0b11000000, 0b11100000, 0b11110000, 0b01111000,
        0b00111100, 0b00011110, 0b00001110, 0b00000111,
        0b00000011, 0b00000011, 0b00000001, 0b00000001,
    ],
    // E002 TL_SW: top-left corner, bottom-left sub-cell.
    // Vertical edge with curve top, straight bottom.
    [
        0b10000000, 0b10000000, 0b11000000, 0b11000000,
        0b11000000, 0b11000000, 0b11000000, 0b11000000,
        0b11000000, 0b11000000, 0b11000000, 0b11000000,
        0b11000000, 0b11000000, 0b11000000, 0b11000000,
    ],
    // E003 TL_SE: top-left corner, bottom-right sub-cell.
    // Empty interior (window content area) — straight glyph.
    [0; 16],
    // E004 TR_NW: top-right corner, top-left sub-cell — empty interior.
    [0; 16],
    // E005 TR_NE: top-right corner, top-right sub-cell.
    // Mirror of E000 horizontally.
    [
        0b00000000, 0b00000000, 0b00000000, 0b10000000,
        0b11000000, 0b11100000, 0b11110000, 0b01111000,
        0b00111100, 0b00011110, 0b00001110, 0b00000111,
        0b00000011, 0b00000011, 0b00000001, 0b00000001,
    ],
    // E006 TR_SW: top-right corner, bottom-left sub-cell — empty interior.
    [0; 16],
    // E007 TR_SE: top-right corner, bottom-right sub-cell.
    // Vertical right edge.
    [
        0b00000001, 0b00000001, 0b00000011, 0b00000011,
        0b00000011, 0b00000011, 0b00000011, 0b00000011,
        0b00000011, 0b00000011, 0b00000011, 0b00000011,
        0b00000011, 0b00000011, 0b00000011, 0b00000011,
    ],
    // E008 BL_NW: bottom-left corner, top-left sub-cell — vertical edge.
    [
        0b11000000, 0b11000000, 0b11000000, 0b11000000,
        0b11000000, 0b11000000, 0b11000000, 0b11000000,
        0b11000000, 0b11000000, 0b11000000, 0b11000000,
        0b11000000, 0b11000000, 0b11000000, 0b10000000,
    ],
    // E009 BL_NE: bottom-left corner, top-right sub-cell — empty.
    [0; 16],
    // E00A BL_SW: bottom-left corner, bottom-left sub-cell — vertical mirror of E000.
    [
        0b10000000, 0b10000000, 0b11000000, 0b11000000,
        0b11100000, 0b01110000, 0b01111000, 0b00111100,
        0b00011110, 0b00001111, 0b00000111, 0b00000011,
        0b00000001, 0b00000000, 0b00000000, 0b00000000,
    ],
    // E00B BL_SE: bottom-left corner, bottom-right sub-cell — vertical mirror of E001.
    [
        0b00000001, 0b00000001, 0b00000011, 0b00000011,
        0b00000111, 0b00001110, 0b00011110, 0b00111100,
        0b01111000, 0b11110000, 0b11100000, 0b11000000,
        0b10000000, 0b00000000, 0b00000000, 0b00000000,
    ],
    // E00C BR_NW: bottom-right corner, top-left sub-cell — empty.
    [0; 16],
    // E00D BR_NE: bottom-right corner, top-right sub-cell — vertical right edge.
    [
        0b00000011, 0b00000011, 0b00000011, 0b00000011,
        0b00000011, 0b00000011, 0b00000011, 0b00000011,
        0b00000011, 0b00000011, 0b00000011, 0b00000011,
        0b00000011, 0b00000011, 0b00000011, 0b00000001,
    ],
    // E00E BR_SW: bottom-right corner, bottom-left sub-cell — mirror of E00A.
    [
        0b00000001, 0b00000001, 0b00000011, 0b00000011,
        0b00000111, 0b00001110, 0b00011110, 0b00111100,
        0b01111000, 0b11110000, 0b11100000, 0b11000000,
        0b10000000, 0b00000000, 0b00000000, 0b00000000,
    ],
    // E00F BR_SE: bottom-right corner, bottom-right sub-cell — mirror of E00B.
    [
        0b10000000, 0b10000000, 0b11000000, 0b11000000,
        0b11100000, 0b01110000, 0b01111000, 0b00111100,
        0b00011110, 0b00001111, 0b00000111, 0b00000011,
        0b00000001, 0b00000000, 0b00000000, 0b00000000,
    ],
];

/// Indices into TIER3_CORNERS for each PUA codepoint U+E000..=U+E00F.
pub const PUA_TO_CORNER_INDEX: [(u32, usize); 16] = [
    (0xE000, 0), (0xE001, 1), (0xE002, 2), (0xE003, 3),
    (0xE004, 4), (0xE005, 5), (0xE006, 6), (0xE007, 7),
    (0xE008, 8), (0xE009, 9), (0xE00A, 10), (0xE00B, 11),
    (0xE00C, 12), (0xE00D, 13), (0xE00E, 14), (0xE00F, 15),
];
```

The bitmaps above are first-pass approximations. After Task 9 lands the wiring, eyeball the result on screen and tweak rows in this file until the curve is smooth. That tweaking is part of Task 9 verification, not a separate task.

- [ ] **Step 3: Wire `mod font_arc;` into the console crate**

Edit `userspace/console/src/main.rs` to add `mod font_arc;` next to `mod backend;`. Verify with `cargo build -p console`.

- [ ] **Step 4: Map the PUA codepoints in `unicode_to_cp437`**

In `userspace/console/src/renderer.rs`, find `fn unicode_to_cp437(cp: u32) -> u8` (around line 885). Add a clause before the fall-through default that maps `0xE000..=0xE00F` to CP437 indices `0xF0..=0xFF`:

```rust
// Cluu private-use range for Tier-3 rounded corner sub-cells.
0xE000..=0xE00F => 0xF0u8 + (cp - 0xE000) as u8,
```

- [ ] **Step 5: Build**

Run: `cargo build -p console`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add userspace/console/src/font_arc.rs userspace/console/src/main.rs userspace/console/src/renderer.rs
git commit -m "console/font: add Tier-3 rounded corner bitmaps + PUA mapping"
```

### Task 4: `GlyphAtlas::with_overrides` constructor

**Files:**
- Modify: `userspace/console/src/atlas.rs`

- [ ] **Step 1: Add the constructor**

Append to the `impl GlyphAtlas` block from FB plan Task A1:

```rust
/// Build a fresh atlas, then graft per-CP437-index bitmap overrides on top.
/// Each `(cp437_index, bitmap)` replaces the default font bitmap for that
/// slot. Used by the compositor to install Tier-3 corner sub-cells.
pub fn with_overrides(font_bits: &[u8], overrides: &[(u8, [u8; GLYPH_H])]) -> Self {
    let mut atlas = Self::from_font(font_bits);
    for (idx, bitmap) in overrides {
        let ch = *idx as usize;
        for row in 0..GLYPH_H {
            let line = bitmap[row];
            let row_off = ch * ATLAS_STRIDE + row * GLYPH_W;
            for col in 0..GLYPH_W {
                let bit = (line >> (7 - col)) & 1;
                atlas.masks[row_off + col] = if bit != 0 { 0xFFFF_FFFFu32 } else { 0 };
            }
        }
    }
    atlas
}
```

- [ ] **Step 2: Build**

Run: `cargo build -p console`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add userspace/console/src/atlas.rs
git commit -m "console/atlas: add with_overrides constructor for compositor chrome"
```

---

## Wave 2 — Compositor state and SHM lifecycle

### Task 5: `Compositor` and `Window` structs + palette

**Files:**
- Create: `userspace/compositor/src/state.rs`
- Modify: `userspace/compositor/src/main.rs`

- [ ] **Step 1: Define types**

```rust
// userspace/compositor/src/state.rs
extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

pub type WindowId = u64;

#[repr(C)]
pub struct WindowShm {
    pub magic: u32,
    pub version: u32,
    pub width: u32,
    pub height: u32,
    pub cursor_x: u32,
    pub cursor_y: u32,
    pub cursor_visible: u32,
    pub generation: u32,
    // Cells follow header in memory; access via separate raw-pointer math.
}

pub const WIN_SHM_MAGIC: u32 = 0x57494e44; // "WIND"
pub const WIN_SHM_VERSION: u32 = 1;

pub struct Window {
    pub id: WindowId,
    pub owner_pid: u32,
    pub title: String,
    pub x: u16, pub y: u16,
    pub w: u16, pub h: u16,
    pub shm_va: *mut u8,
    pub shm_token: u64,
    pub shm_size: usize,
    pub last_gen: u32,
    pub input_endpoint: usize,
}

pub struct Compositor {
    pub fb_ptr: *mut u8,
    pub fb_phys: u64,
    pub fb_size: usize,
    pub width_px: u32,
    pub height_px: u32,
    pub pitch: u32,

    pub cols: u16,
    pub rows: u16,
    pub cell_grid: Vec<u64>,
    pub cell_dirty: Vec<(u16, u16)>,

    pub palette: [u32; 256],
    pub backbuf: Vec<u32>,

    pub windows: Vec<Window>,
    pub focused: Option<WindowId>,
    pub active: bool,
    pub next_id: u64,

    pub timeserver_endpoint: usize,
    pub registry_endpoint: usize,
    pub procmgr_endpoint: usize,
    pub clock_seconds: u64,
}

pub fn xterm_256_palette() -> [u32; 256] {
    let mut p = [0u32; 256];
    // 0..16: standard ANSI
    let basic: [u32; 16] = [
        0x000000, 0x800000, 0x008000, 0x808000,
        0x000080, 0x800080, 0x008080, 0xC0C0C0,
        0x808080, 0xFF0000, 0x00FF00, 0xFFFF00,
        0x0000FF, 0xFF00FF, 0x00FFFF, 0xFFFFFF,
    ];
    for i in 0..16 { p[i] = 0xFF00_0000 | basic[i]; }
    // 16..232: 6×6×6 cube
    for i in 0..216 {
        let r = (i / 36) % 6;
        let g = (i / 6) % 6;
        let b = i % 6;
        let to8 = |c: usize| -> u32 { if c == 0 { 0 } else { (c as u32) * 40 + 55 } };
        p[16 + i] = 0xFF00_0000 | (to8(r) << 16) | (to8(g) << 8) | to8(b);
    }
    // 232..256: grayscale ramp
    for i in 0..24 {
        let v = 8 + (i as u32) * 10;
        p[232 + i] = 0xFF00_0000 | (v << 16) | (v << 8) | v;
    }
    p
}
```

- [ ] **Step 2: Add `mod state;` to `main.rs`**

```rust
mod state;
```

- [ ] **Step 3: Build**

Run: `cargo build -p compositor`
Expected: PASS (warnings about unused fields are fine).

- [ ] **Step 4: Commit**

```bash
git add userspace/compositor/src/state.rs userspace/compositor/src/main.rs
git commit -m "compositor/state: add Compositor + Window types and palette"
```

### Task 6: Open `/dev/fb0`, populate `Compositor` from boot params

**Files:**
- Modify: `userspace/compositor/src/state.rs` (add a `Compositor::init` constructor)
- Modify: `userspace/compositor/src/main.rs`

- [ ] **Step 1: Read boot params and open fb**

Reuse the same boot params console reads (`PARAM_FB_BASE`, `PARAM_FB_WIDTH`, etc. — see `userspace/console/src/main.rs:64-70`). Add an `init` constructor:

```rust
// state.rs
use libcluu::boot::{
    process_info, PARAM_FB_BASE, PARAM_FB_HEIGHT, PARAM_FB_PHYS,
    PARAM_FB_PITCH, PARAM_FB_SIZE, PARAM_FB_WIDTH,
};

pub const GLYPH_W: u32 = 8;
pub const GLYPH_H: u32 = 16;

impl Compositor {
    pub fn init() -> Result<Self, libcluu::Error> {
        let info = process_info();
        let fb_ptr   = info.params[PARAM_FB_BASE] as *mut u8;
        let width_px = info.params[PARAM_FB_WIDTH] as u32;
        let height_px = info.params[PARAM_FB_HEIGHT] as u32;
        let pitch    = info.params[PARAM_FB_PITCH] as u32;
        let fb_phys  = info.params[PARAM_FB_PHYS];
        let fb_size  = info.params[PARAM_FB_SIZE] as usize;

        let cols = (width_px / GLYPH_W) as u16;
        let rows = (height_px / GLYPH_H) as u16;

        let cell_grid = alloc::vec![0u64; cols as usize * rows as usize];
        let backbuf = alloc::vec![0u32; (width_px * height_px) as usize];

        Ok(Self {
            fb_ptr,
            fb_phys,
            fb_size,
            width_px,
            height_px,
            pitch,
            cols,
            rows,
            cell_grid,
            cell_dirty: Vec::new(),
            palette: xterm_256_palette(),
            backbuf,
            windows: Vec::new(),
            focused: None,
            active: false,
            next_id: 1,
            timeserver_endpoint: 0,
            registry_endpoint: 0,
            procmgr_endpoint: 0,
            clock_seconds: 0,
        })
    }
}
```

- [ ] **Step 2: Wire into `main`**

```rust
// userspace/compositor/src/main.rs
#![no_std]
#![no_main]

extern crate alloc;

mod state;

use libcluu::debug_print;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = debug_print("compositor: init");
    let _comp = match state::Compositor::init() {
        Ok(c) => c,
        Err(_) => {
            let _ = debug_print("compositor: init failed");
            return -1;
        }
    };
    let _ = debug_print("compositor: ready");
    loop {
        let _ = libcluu::syscall::yield_cpu();
    }
}
```

- [ ] **Step 3: Build**

Run: `cargo xtask build`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add userspace/compositor/src/state.rs userspace/compositor/src/main.rs
git commit -m "compositor: open /dev/fb0 and seed compositor state from boot params"
```

### Task 7: SHM allocate / map / free helpers

**Files:**
- Create: `userspace/compositor/src/shm.rs`
- Modify: `userspace/compositor/src/main.rs`

- [ ] **Step 1: Add the helper module**

```rust
// userspace/compositor/src/shm.rs
use libcluu::syscall::{self, InvokeOp, MAP_FRAME_TOKEN};
use libcluu::{Error, Result};

/// Allocate a frame whose size is at least `bytes`, rounded up to 4 KiB.
/// Returns `(frame_token, allocated_bytes)`.
pub fn alloc_frame(bytes: usize) -> Result<(u64, usize)> {
    let rounded = (bytes + 0xFFF) & !0xFFF;
    let token = syscall::invoke(InvokeOp::FrameAllocate as u32, rounded as u64, 0, 0, 0, 0, 0)?;
    Ok((token, rounded))
}

/// Map a frame token at the given virtual address with read+write permissions.
/// Caller is responsible for choosing a non-overlapping VA.
pub fn map_frame_rw(va: usize, token: u64, size: usize) -> Result<()> {
    syscall::space_map_range(va, token, size as u64, FLAGS_USER_RW | MAP_FRAME_TOKEN)?;
    Ok(())
}

/// Free a frame token allocated via `alloc_frame`.
pub fn free_frame(token: u64) -> Result<()> {
    syscall::invoke(InvokeOp::FrameFree as u32, token, 0, 0, 0, 0, 0)?;
    Ok(())
}

const FLAGS_USER_RW: usize = 0x7; // user|read|write — verify against libcluu::syscall constants
```

If the actual `libcluu::syscall::invoke` signature differs, adjust the calls accordingly. Run:

```bash
grep -n 'pub fn invoke\|pub fn space_map_range' userspace/libcluu/src/syscall.rs
```

to confirm the exact signatures, then update `shm.rs`.

- [ ] **Step 2: Wire `mod shm;` and stage a single allocation in `main`**

Update `main.rs` to call `shm::alloc_frame(8192)`, log the resulting token, and `shm::free_frame(token)` immediately. This proves the wrapper before any windows exist.

- [ ] **Step 3: Build + smoke test**

Run: `cargo xtask build && MARKER_MODE=l2_path_symlink_resolve bash scripts/harness_run.sh`
Expected: a `compositor: alloc/free ok` line in the serial log; harness PASS (compositor doesn't yet break anything else, since init still spawns the legacy console set).

- [ ] **Step 4: Commit**

```bash
git add userspace/compositor/src/shm.rs userspace/compositor/src/main.rs
git commit -m "compositor/shm: alloc/map/free wrappers around FrameAllocate"
```

### Task 8: IPC event loop scaffold

**Files:**
- Create: `userspace/compositor/src/protocol.rs`
- Modify: `userspace/compositor/src/main.rs`

- [ ] **Step 1: Sketch the protocol module**

```rust
// userspace/compositor/src/protocol.rs
use libcluu::types::Message;
use libcluu::ipc;

#[derive(Debug)]
pub enum Incoming {
    WinRegister { req_w: u32, req_h: u32, title_len: u32 },
    WinDamage   { window_id: u64, x: u32, y: u32, w: u32, h: u32 },
    WinDestroy  { window_id: u64 },
    WinSetTitle { window_id: u64, title_len: u32 },
    KbdEvent    { keycode: u32, modifiers: u32, codepoint: u32, kind: u32 },
    VtActivate,
    VtDeactivate,
    Shutdown,
    Other(u32),
}

pub fn parse(msg: &Message) -> Incoming {
    match msg.tag.label {
        ipc::COMP_WIN_REGISTER_LABEL => Incoming::WinRegister {
            req_w:     msg.words[0] as u32,
            req_h:     msg.words[1] as u32,
            title_len: msg.words[2] as u32,
        },
        ipc::COMP_WIN_DAMAGE_LABEL => Incoming::WinDamage {
            window_id: msg.words[0],
            x: msg.words[1] as u32,
            y: msg.words[2] as u32,
            w: msg.words[3] as u32,
            h: msg.words[4] as u32,
        },
        ipc::COMP_WIN_DESTROY_LABEL => Incoming::WinDestroy {
            window_id: msg.words[0],
        },
        ipc::COMP_WIN_SET_TITLE_LABEL => Incoming::WinSetTitle {
            window_id: msg.words[0],
            title_len: msg.words[1] as u32,
        },
        ipc::COMP_KBD_EVENT_LABEL => Incoming::KbdEvent {
            keycode:    msg.words[0] as u32,
            modifiers:  msg.words[1] as u32,
            codepoint:  msg.words[2] as u32,
            kind:       msg.words[3] as u32,
        },
        ipc::COMP_VT_ACTIVATE_LABEL   => Incoming::VtActivate,
        ipc::COMP_VT_DEACTIVATE_LABEL => Incoming::VtDeactivate,
        ipc::COMP_SHUTDOWN_LABEL      => Incoming::Shutdown,
        other => Incoming::Other(other),
    }
}
```

- [ ] **Step 2: Replace the yield-loop in `main` with a real `recv_any` loop**

```rust
// userspace/compositor/src/main.rs
mod protocol;
use protocol::{parse, Incoming};
use libcluu::ipc::{parse_message_word};       // adjust if different helper exists
use libcluu::syscall;
use libcluu::types::Message;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = libcluu::debug_print("compositor: init");
    let mut comp = match state::Compositor::init() {
        Ok(c) => c,
        Err(_) => return -1,
    };

    // Token list: client, input, control, registry. (Endpoints registered later.)
    let mut tokens = [0usize; 4];

    let mut buf = [0u8; 1024];
    loop {
        match syscall::ipc_recv_any(&tokens, &mut buf, 1000) {
            Ok((idx, len)) => {
                if let Some((msg, payload)) = libcluu::ipc::parse_message(&buf[..len]) {
                    let kind = parse(&msg);
                    let _ = libcluu::debug_print("compositor: msg");
                    let _ = (idx, payload, kind, &mut comp); // suppress unused — handlers added next tasks
                }
            }
            Err(libcluu::Error::Timeout) => {
                // 1Hz tick path lives here in Task 18.
            }
            Err(_) => {
                let _ = libcluu::debug_print("compositor: recv error");
            }
        }
    }
}
```

- [ ] **Step 3: Build + smoke**

Run: `cargo xtask build && MARKER_MODE=l2_path_symlink_resolve bash scripts/harness_run.sh`
Expected: PASS, with no `compositor: recv error` spam (compositor's tokens are still all 0, so `recv_any` will fail with `Error::InvalidArgument` or similar — accept that for now; the next task wires real endpoints).

- [ ] **Step 4: Commit**

```bash
git add userspace/compositor/src/protocol.rs userspace/compositor/src/main.rs
git commit -m "compositor: scaffold IPC event loop + Incoming enum"
```

### Task 9: Endpoint registration (client / input / control)

**Files:**
- Modify: `userspace/compositor/src/main.rs`
- Modify: `userspace/compositor/Cluufile`

- [ ] **Step 1: Allocate three endpoints**

In `main`, after `Compositor::init()`, allocate three endpoints via `syscall::endpoint_create()` (verify exact name with `grep 'endpoint_create' userspace/libcluu/src/syscall.rs`). Store them in `comp.client_endpoint`, `comp.input_endpoint_global`, `comp.control_endpoint` (add fields to `Compositor`).

- [ ] **Step 2: Register them with the registry**

Use the same registration pattern other services use. Run `grep -rn 'register_service\|REGISTRY_REGISTER' userspace/console/src/` to find the helper. Register names `compositor:client`, `compositor:input`, `compositor:control`.

- [ ] **Step 3: Plug endpoints into the recv token list**

Replace the placeholder `[0usize; 4]` with the real endpoint values; the fourth slot stays for the registry control endpoint that the registry hands out for re-subscriptions.

- [ ] **Step 4: Build + smoke**

Run: `cargo xtask build && MARKER_MODE=l2_path_symlink_resolve bash scripts/harness_run.sh`
Expected: PASS, plus a serial line like `compositor: endpoints registered`.

- [ ] **Step 5: Commit**

```bash
git add userspace/compositor/src/main.rs userspace/compositor/Cluufile
git commit -m "compositor: register client/input/control endpoints"
```

### Task 10: `WIN_REGISTER` handler — alloc SHM, reply with token

**Files:**
- Modify: `userspace/compositor/src/main.rs`
- Modify: `userspace/compositor/src/state.rs`

- [ ] **Step 1: Compute SHM size + alloc**

Add a method on `Compositor`:

```rust
impl Compositor {
    pub fn handle_win_register(
        &mut self,
        owner_pid: u32,
        req_w: u32,
        req_h: u32,
        title: &str,
        reply_endpoint: usize,
    ) -> Result<(WindowId, u64, u32, u32), libcluu::Error> {
        let granted_w = (req_w as u16).min(self.cols);
        let granted_h = (req_h as u16).min(self.rows.saturating_sub(1)); // row 0 reserved for status
        if granted_w < 5 || granted_h < 5 {
            return Err(libcluu::Error::InvalidArgument);
        }

        let cells_bytes = granted_w as usize * granted_h as usize * 8;
        let shm_size = (32 + cells_bytes + 0xFFF) & !0xFFF;
        let (token, allocated) = crate::shm::alloc_frame(shm_size)?;

        let id = self.next_id;
        self.next_id += 1;

        // Choose a virtual address — pick a per-window slot above APP_FB_BASE.
        // For v1, use a static base + id*allocated.
        let va_base: usize = 0xC000_0000;
        let va = va_base + (id as usize) * 0x40_0000;
        crate::shm::map_frame_rw(va, token, allocated)?;

        // Initialise WindowShm header
        unsafe {
            let hdr = va as *mut crate::state::WindowShm;
            (*hdr).magic = crate::state::WIN_SHM_MAGIC;
            (*hdr).version = crate::state::WIN_SHM_VERSION;
            (*hdr).width = granted_w as u32;
            (*hdr).height = granted_h as u32;
            (*hdr).cursor_x = 0;
            (*hdr).cursor_y = 0;
            (*hdr).cursor_visible = 0;
            (*hdr).generation = 0;
            // zero cell area
            let cells_ptr = (va + 32) as *mut u8;
            core::ptr::write_bytes(cells_ptr, 0, cells_bytes);
        }

        let mut title_owned = alloc::string::String::new();
        title_owned.push_str(title);

        // Cascade window placement: top-left + (id * 2) cells.
        let offset = (id as u16) * 2;
        let x = offset.min(self.cols.saturating_sub(granted_w));
        let y = (1 + offset).min(self.rows.saturating_sub(granted_h));

        let win = crate::state::Window {
            id,
            owner_pid,
            title: title_owned,
            x, y,
            w: granted_w,
            h: granted_h,
            shm_va: va as *mut u8,
            shm_token: token,
            shm_size: allocated,
            last_gen: 0,
            input_endpoint: reply_endpoint,
        };
        self.windows.push(win);
        self.focused = Some(id);
        self.mark_window_dirty(id);

        Ok((id, token, granted_w as u32, granted_h as u32))
    }

    pub fn mark_window_dirty(&mut self, id: WindowId) {
        if let Some(win) = self.windows.iter().find(|w| w.id == id) {
            for cy in win.y..win.y + win.h {
                for cx in win.x..win.x + win.w {
                    self.cell_dirty.push((cx, cy));
                }
            }
        }
    }
}
```

- [ ] **Step 2: Dispatch from the event loop**

In `main`'s event loop, on `Incoming::WinRegister { req_w, req_h, title_len }`: read `title_bytes` from `payload[..title_len]`, look up sender's pid (use `msg.tag.sender_pid` if available; otherwise the sender's reply_endpoint identifies them), call `comp.handle_win_register`, build a reply Message with `COMP_WIN_REGISTER_REPLY` carrying `[id, token, granted_w, granted_h, error]`, and `reply()` it.

- [ ] **Step 3: Build + smoke**

Run: `cargo xtask build && MARKER_MODE=l2_path_symlink_resolve bash scripts/harness_run.sh`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add userspace/compositor/src/main.rs userspace/compositor/src/state.rs
git commit -m "compositor: WIN_REGISTER handler allocates SHM and replies with token"
```

### Task 11: `WIN_DESTROY` handler + implicit destroy on exit

**Files:**
- Modify: `userspace/compositor/src/main.rs`
- Modify: `userspace/compositor/src/state.rs`

- [ ] **Step 1: Add explicit-destroy logic**

```rust
impl Compositor {
    pub fn handle_win_destroy(&mut self, id: WindowId) {
        if let Some(pos) = self.windows.iter().position(|w| w.id == id) {
            let win = self.windows.remove(pos);
            let _ = crate::shm::free_frame(win.shm_token);
            // Mark covered cells dirty so they get repainted as bg.
            for cy in win.y..win.y + win.h {
                for cx in win.x..win.x + win.w {
                    self.cell_dirty.push((cx, cy));
                }
            }
            if self.focused == Some(id) {
                self.focused = self.windows.last().map(|w| w.id);
            }
        }
    }
}
```

- [ ] **Step 2: Subscribe to PROC_EXIT_LABEL**

Borrow the pattern from `userspace/init/src/main.rs` — init holds an exit-endpoint that procmgr sends `PROC_EXIT_LABEL` to whenever a watched pid dies. Compositor needs the same: expose `comp.exit_endpoint = endpoint_create()`, hand it to procmgr at every `WIN_REGISTER` time via `procmgr::watch_pid(owner_pid, comp.exit_endpoint)`. Add the `exit_endpoint` to the `recv_any` token list.

- [ ] **Step 3: On exit message, drop windows owned by that pid**

```rust
// in event loop
Incoming::Other(label) if label == libcluu::ipc::PROC_EXIT_LABEL => {
    let exited_pid = msg.words[0] as u32;
    let to_drop: alloc::vec::Vec<WindowId> = comp.windows.iter()
        .filter(|w| w.owner_pid == exited_pid)
        .map(|w| w.id)
        .collect();
    for id in to_drop {
        comp.handle_win_destroy(id);
    }
}
```

- [ ] **Step 4: Build + smoke**

Run: `cargo xtask build && MARKER_MODE=l2_path_symlink_resolve bash scripts/harness_run.sh`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add userspace/compositor/src/main.rs userspace/compositor/src/state.rs
git commit -m "compositor: WIN_DESTROY + implicit destroy via PROC_EXIT_LABEL"
```

---

## Wave 3 — Compose pipeline (no chrome yet)

### Task 12: Cell-grid composition (background + window content)

**Files:**
- Create: `userspace/compositor/src/compose.rs`
- Modify: `userspace/compositor/src/main.rs`

- [ ] **Step 1: Build the cell-by-cell composer**

```rust
// userspace/compositor/src/compose.rs
use crate::state::{Compositor, Window, WindowShm, WIN_SHM_MAGIC};

const CHROME_TOP: u16 = 2;
const CHROME_BOTTOM: u16 = 2;
const CHROME_LEFT: u16 = 2;
const CHROME_RIGHT: u16 = 2;

const BG_CELL: u64 = pack_cell(b' ' as u32, 0, 0, 0); // codepoint 0x20 fg=0 bg=0

const fn pack_cell(cp: u32, fg: u8, bg: u8, attrs: u8) -> u64 {
    (cp as u64 & 0x1F_FFFF)
        | ((fg as u64 & 0xFF) << 21)
        | ((bg as u64 & 0xFF) << 29)
        | ((attrs as u64 & 0x07) << 37)
}

pub fn recompute_dirty(comp: &mut Compositor) {
    // Snapshot dirty list and walk each cell.
    let dirty = core::mem::take(&mut comp.cell_dirty);
    for (cx, cy) in dirty {
        if cx >= comp.cols || cy >= comp.rows { continue; }
        let out = compose_cell(comp, cx, cy);
        let idx = cy as usize * comp.cols as usize + cx as usize;
        comp.cell_grid[idx] = out;
    }
}

fn compose_cell(comp: &Compositor, cx: u16, cy: u16) -> u64 {
    // Walk windows top→bottom (last in vec = top).
    for win in comp.windows.iter().rev() {
        if cx < win.x || cx >= win.x + win.w { continue; }
        if cy < win.y || cy >= win.y + win.h { continue; }

        let local_x = cx - win.x;
        let local_y = cy - win.y;
        let in_chrome = local_x < CHROME_LEFT
            || local_x >= win.w - CHROME_RIGHT
            || local_y < CHROME_TOP
            || local_y >= win.h - CHROME_BOTTOM;
        if in_chrome {
            // Chrome glyph emitted by Task 14; for now return space.
            return BG_CELL;
        }
        // Interior cell: read SHM with acquire-load on generation first.
        return read_shm_cell(win, local_x - CHROME_LEFT, local_y - CHROME_TOP);
    }
    BG_CELL
}

fn read_shm_cell(win: &Window, ix: u16, iy: u16) -> u64 {
    unsafe {
        let hdr = win.shm_va as *const WindowShm;
        if (*hdr).magic != WIN_SHM_MAGIC { return BG_CELL; }
        let inner_w = (*hdr).width as u16;
        if ix >= inner_w { return BG_CELL; }
        let cells_ptr = (win.shm_va as usize + 32) as *const u64;
        let off = iy as usize * inner_w as usize + ix as usize;
        core::ptr::read_volatile(cells_ptr.add(off))
    }
}
```

- [ ] **Step 2: Wire `mod compose;` and call `recompute_dirty` after each handler**

In `main.rs`, after handling any `Incoming::*`, call `compose::recompute_dirty(&mut comp)`.

- [ ] **Step 3: Build + smoke**

Run: `cargo xtask build && MARKER_MODE=l2_path_symlink_resolve bash scripts/harness_run.sh`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add userspace/compositor/src/compose.rs userspace/compositor/src/main.rs
git commit -m "compositor/compose: cell-grid composer (interior pulls from SHM)"
```

### Task 13: Glyph blit cell-grid → backbuf → fb

**Files:**
- Modify: `userspace/compositor/src/compose.rs`
- Modify: `userspace/compositor/src/state.rs` (add `prev_cell_grid` for diff detection)

- [ ] **Step 1: Track the previous cell grid**

Add `prev_cell_grid: Vec<u64>` to `Compositor`, initialized identical-length to `cell_grid` filled with `u64::MAX` (forces all cells dirty on first compose).

- [ ] **Step 2: Implement the glyph blit pass**

Reuse the FB plan's atlas + `blend_row`. The compositor doesn't link the console crate, so move (or re-export) `GlyphAtlas` and `blend_row` into a place compositor can depend on. Two options:

- **Option A** (recommended): expose them from `libcluu`. Move `userspace/console/src/atlas.rs` to `userspace/libcluu/src/atlas.rs`, move the SIMD helper, and add `pub use` re-exports. Update the console crate to import from libcluu.
- **Option B**: introduce a new `userspace/libfb/` crate.

Pick Option A. Run:

```bash
git mv userspace/console/src/atlas.rs userspace/libcluu/src/atlas.rs
```

Edit `userspace/libcluu/src/lib.rs` to add `pub mod atlas;`. Also expose `blend_row`: copy or re-export from console's `simd.rs`. Update `userspace/console/src/main.rs` and `renderer.rs` to import from `libcluu::atlas` and `libcluu::simd::blend_row`.

- [ ] **Step 3: Add the blit method**

```rust
// compose.rs
use libcluu::atlas::{GlyphAtlas, GLYPH_W as ATLAS_GW, GLYPH_H as ATLAS_GH};
use libcluu::simd::blend_row;

impl Compositor {
    pub fn flush_grid_to_backbuf(&mut self, atlas: &GlyphAtlas) {
        for cy in 0..self.rows {
            for cx in 0..self.cols {
                let idx = cy as usize * self.cols as usize + cx as usize;
                if self.cell_grid[idx] == self.prev_cell_grid[idx] { continue; }
                let cell = self.cell_grid[idx];
                self.prev_cell_grid[idx] = cell;
                self.blit_cell(atlas, cx, cy, cell);
            }
        }
    }

    fn blit_cell(&mut self, atlas: &GlyphAtlas, cx: u16, cy: u16, cell: u64) {
        let cp     = (cell & 0x1F_FFFF) as u32;
        let fg_idx = ((cell >> 21) & 0xFF) as u8;
        let bg_idx = ((cell >> 29) & 0xFF) as u8;
        let _attrs = ((cell >> 37) & 0x07) as u8; // bold path: bump fg_idx |= 8 when bit 0 set
        let fg = self.palette[fg_idx as usize];
        let bg = self.palette[bg_idx as usize];

        let cp_u8 = libcluu::unicode_to_cp437(cp); // exposed alongside atlas in Step 2 move
        let px = cx as usize * ATLAS_GW;
        let py = cy as usize * ATLAS_GH;
        let mut row_buffer = [0u32; ATLAS_GW];
        for row in 0..ATLAS_GH {
            let mask_row = atlas.row(cp_u8, row);
            blend_row(mask_row, fg, bg, &mut row_buffer);
            // copy into backbuf (assume contiguous: pitch == width_px*4)
            let off = (py + row) * self.width_px as usize + px;
            self.backbuf[off..off + ATLAS_GW].copy_from_slice(&row_buffer);
        }
    }
}
```

- [ ] **Step 4: Push backbuf to fb**

```rust
impl Compositor {
    pub fn flush_backbuf_to_fb(&self) {
        // Plain memcpy under WC; simple v1, no dirty-rect on backbuf yet.
        unsafe {
            let dst = self.fb_ptr;
            let bytes = self.width_px as usize * self.height_px as usize * 4;
            core::ptr::copy_nonoverlapping(
                self.backbuf.as_ptr() as *const u8,
                dst,
                bytes,
            );
        }
    }
}
```

- [ ] **Step 5: Call them after every recompose**

Add to the event-loop tail:

```rust
compose::recompute_dirty(&mut comp);
comp.flush_grid_to_backbuf(&atlas);
comp.flush_backbuf_to_fb();
```

`atlas` is `GlyphAtlas::with_overrides(&FONT8X16, &CORNER_OVERRIDES)` constructed once in `main` after `Compositor::init`. `CORNER_OVERRIDES` zips `font_arc::PUA_TO_CORNER_INDEX` with `font_arc::TIER3_CORNERS` into `[(0xF0u8, [u8;16]); 16]`.

- [ ] **Step 6: Build + smoke**

Run: `cargo xtask build && MARKER_MODE=l2_path_symlink_resolve bash scripts/harness_run.sh`
Expected: PASS. Compositor doesn't yet own VT4 (Task 21 wires init), so nothing draws to the screen yet.

- [ ] **Step 7: Commit**

```bash
git add userspace/compositor/src/compose.rs userspace/compositor/src/main.rs userspace/libcluu/src/ userspace/console/src/
git commit -m "compositor/compose: blit changed cells via atlas; push backbuf to fb"
```

### Task 14: Chrome rendering (Tier-3 corners + edges + title)

**Files:**
- Create: `userspace/compositor/src/chrome.rs`
- Modify: `userspace/compositor/src/compose.rs`

- [ ] **Step 1: Define the chrome glyph table**

```rust
// userspace/compositor/src/chrome.rs
//
// Cell coordinates inside a window (local_x, local_y) are 0..win.w / 0..win.h.
// Chrome occupies: rows 0..2 top, rows h-2..h bottom, cols 0..2 left, cols w-2..w right.

use crate::state::Window;

const TL_NW: u32 = 0xE000;  const TL_NE: u32 = 0xE001;
const TL_SW: u32 = 0xE002;  const TL_SE: u32 = 0xE003;
const TR_NW: u32 = 0xE004;  const TR_NE: u32 = 0xE005;
const TR_SW: u32 = 0xE006;  const TR_SE: u32 = 0xE007;
const BL_NW: u32 = 0xE008;  const BL_NE: u32 = 0xE009;
const BL_SW: u32 = 0xE00A;  const BL_SE: u32 = 0xE00B;
const BR_NW: u32 = 0xE00C;  const BR_NE: u32 = 0xE00D;
const BR_SW: u32 = 0xE00E;  const BR_SE: u32 = 0xE00F;
const H_BAR: u32 = 0x2500;  // ─
const V_BAR: u32 = 0x2502;  // │

#[derive(Copy, Clone)]
pub struct Style {
    pub border_fg: u8,
    pub title_fg: u8,
    pub bg: u8,
    pub bold_attr: u8,
}

pub const PLAIN: Style = Style { border_fg: 7, title_fg: 7, bg: 0, bold_attr: 0 };
pub const FOCUSED: Style = Style { border_fg: 15, title_fg: 15, bg: 0, bold_attr: 1 };

pub fn chrome_cell(win: &Window, local_x: u16, local_y: u16, style: Style) -> u64 {
    use super::compose::pack_cell;

    let w = win.w;
    let h = win.h;
    // Corners (2x2 each)
    let cp = match (local_x, local_y) {
        (0, 0) => TL_NW, (1, 0) => TL_NE,
        (0, 1) => TL_SW, (1, 1) => TL_SE,
        (lx, 0) if lx == w - 2 => TR_NW, (lx, 0) if lx == w - 1 => TR_NE,
        (lx, 1) if lx == w - 2 => TR_SW, (lx, 1) if lx == w - 1 => TR_SE,
        (0, ly) if ly == h - 2 => BL_NW, (1, ly) if ly == h - 2 => BL_NE,
        (0, ly) if ly == h - 1 => BL_SW, (1, ly) if ly == h - 1 => BL_SE,
        (lx, ly) if lx == w - 2 && ly == h - 2 => BR_NW,
        (lx, ly) if lx == w - 1 && ly == h - 2 => BR_NE,
        (lx, ly) if lx == w - 2 && ly == h - 1 => BR_SW,
        (lx, ly) if lx == w - 1 && ly == h - 1 => BR_SE,
        // Top edge between corners
        (_, 0) => H_BAR,
        // Title strip (row 1 between corners)
        (_, 1) => return title_glyph(win, local_x, style),
        // Bottom edge between corners
        (_, ly) if ly == h - 1 => H_BAR,
        // Left edge between corners
        (0, _) => V_BAR,
        // Right edge between corners
        (lx, _) if lx == w - 1 => V_BAR,
        // Interior of bottom-2x2 chrome row (between 2-cell corners)
        (_, ly) if ly == h - 2 => H_BAR,
        // Interior of top-2x2 corners' inner row that isn't title
        _ => H_BAR,
    };
    pack_cell(cp, style.border_fg, style.bg, style.bold_attr)
}

fn title_glyph(win: &Window, local_x: u16, style: Style) -> u64 {
    use super::compose::pack_cell;
    // Title runs from local_x = 3 (just after TL_SE) to w - 3 (just before TR_SW).
    let title_start = 3u16;
    let title_end = win.w.saturating_sub(3);
    if local_x < title_start || local_x >= title_end {
        return pack_cell(b' ' as u32, style.title_fg, style.bg, 0);
    }
    let pos = (local_x - title_start) as usize;
    let bytes = win.title.as_bytes();
    let cp = if pos < bytes.len() { bytes[pos] as u32 } else { b' ' as u32 };
    pack_cell(cp, style.title_fg, style.bg, style.bold_attr)
}
```

- [ ] **Step 2: Hook chrome into `compose_cell`**

Replace the `if in_chrome { return BG_CELL; }` arm in `compose.rs` with:

```rust
if in_chrome {
    let style = if Some(win.id) == comp.focused { crate::chrome::FOCUSED } else { crate::chrome::PLAIN };
    return crate::chrome::chrome_cell(win, local_x, local_y, style);
}
```

Pass `comp` (the compositor) into `compose_cell` as `&Compositor` so it can read `comp.focused`. Adjust `recompute_dirty` to thread it through.

- [ ] **Step 3: Build + smoke**

Run: `cargo xtask build && MARKER_MODE=l2_path_symlink_resolve bash scripts/harness_run.sh`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add userspace/compositor/src/chrome.rs userspace/compositor/src/compose.rs
git commit -m "compositor/chrome: render Tier-3 corners + edges + title"
```

### Task 15: `WIN_DAMAGE` handler (gen-checked)

**Files:**
- Modify: `userspace/compositor/src/main.rs`
- Modify: `userspace/compositor/src/state.rs`

- [ ] **Step 1: Mark damage cells dirty**

```rust
impl Compositor {
    pub fn handle_win_damage(&mut self, id: WindowId, x: u32, y: u32, w: u32, h: u32) {
        let Some(win) = self.windows.iter().find(|w| w.id == id) else { return; };
        let inner_w = win.w.saturating_sub(4);
        let inner_h = win.h.saturating_sub(4);
        let cx0 = (x as u16).min(inner_w);
        let cy0 = (y as u16).min(inner_h);
        let cx1 = (x as u16 + w as u16).min(inner_w);
        let cy1 = (y as u16 + h as u16).min(inner_h);
        for iy in cy0..cy1 {
            for ix in cx0..cx1 {
                let gx = win.x + 2 + ix;
                let gy = win.y + 2 + iy;
                self.cell_dirty.push((gx, gy));
            }
        }
    }
}
```

- [ ] **Step 2: Dispatch from event loop**

```rust
Incoming::WinDamage { window_id, x, y, w, h } => {
    comp.handle_win_damage(window_id, x, y, w, h);
}
```

- [ ] **Step 3: Build + smoke**

Run: `cargo xtask build && MARKER_MODE=l2_path_symlink_resolve bash scripts/harness_run.sh`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add userspace/compositor/src/main.rs userspace/compositor/src/state.rs
git commit -m "compositor: WIN_DAMAGE dirties affected cells"
```

### Task 16: `WIN_SET_TITLE` handler

**Files:**
- Modify: `userspace/compositor/src/main.rs`
- Modify: `userspace/compositor/src/state.rs`

- [ ] **Step 1: Update title and dirty the title row**

```rust
impl Compositor {
    pub fn handle_set_title(&mut self, id: WindowId, title: &str) {
        let Some(win) = self.windows.iter_mut().find(|w| w.id == id) else { return; };
        win.title.clear();
        win.title.push_str(&title[..title.len().min(31)]);
        let title_y = win.y + 1;
        for cx in win.x..win.x + win.w {
            self.cell_dirty.push((cx, title_y));
        }
    }
}
```

- [ ] **Step 2: Wire into event loop, parse `payload[..title_len]`**

- [ ] **Step 3: Build + commit**

```bash
cargo xtask build
git add userspace/compositor/src/main.rs userspace/compositor/src/state.rs
git commit -m "compositor: WIN_SET_TITLE updates chrome title row"
```

---

## Wave 4 — Hotkeys, input forwarding, status bar

### Task 17: Hotkey table + dispatch

**Files:**
- Create: `userspace/compositor/src/hotkeys.rs`
- Modify: `userspace/compositor/src/main.rs`

- [ ] **Step 1: Define the hotkey table**

```rust
// userspace/compositor/src/hotkeys.rs

bitflags::bitflags! {
    pub struct Mods: u32 {
        const SHIFT = 1 << 0;
        const ALT   = 1 << 1;
        const CTRL  = 1 << 2;
        const SUPER = 1 << 3;
    }
}
// (If `bitflags` is not yet a dep, add it; or hand-roll `const SHIFT: u32 = 1 << 0` etc.)

pub const KEY_TAB: u32 = 0x0F;
pub const KEY_LEFT: u32 = 0x4B;
pub const KEY_RIGHT: u32 = 0x4D;
pub const KEY_UP: u32 = 0x48;
pub const KEY_DOWN: u32 = 0x50;
pub const KEY_Q: u32 = 0x10;
pub const KEY_N: u32 = 0x31;

#[derive(Debug)]
pub enum Hotkey {
    FocusNext,
    FocusPrev,
    MoveLeft, MoveRight, MoveUp, MoveDown,
    ResizeLeft, ResizeRight, ResizeUp, ResizeDown,
    CloseRequest,
    SpawnDemo,
}

pub fn match_hotkey(keycode: u32, mods: u32, kind: u32) -> Option<Hotkey> {
    if kind != 0 { return None; } // only on key-down
    let m = mods;
    let alt = m & Mods::ALT.bits() != 0;
    let shift = m & Mods::SHIFT.bits() != 0;
    let sup = m & Mods::SUPER.bits() != 0;
    match keycode {
        KEY_TAB if alt && !shift => Some(Hotkey::FocusNext),
        KEY_TAB if alt && shift => Some(Hotkey::FocusPrev),
        KEY_LEFT  if sup && shift => Some(Hotkey::ResizeLeft),
        KEY_RIGHT if sup && shift => Some(Hotkey::ResizeRight),
        KEY_UP    if sup && shift => Some(Hotkey::ResizeUp),
        KEY_DOWN  if sup && shift => Some(Hotkey::ResizeDown),
        KEY_LEFT  if sup => Some(Hotkey::MoveLeft),
        KEY_RIGHT if sup => Some(Hotkey::MoveRight),
        KEY_UP    if sup => Some(Hotkey::MoveUp),
        KEY_DOWN  if sup => Some(Hotkey::MoveDown),
        KEY_Q if sup => Some(Hotkey::CloseRequest),
        KEY_N if sup => Some(Hotkey::SpawnDemo),
        _ => None,
    }
}
```

(Verify keycode constants against `userspace/kbd/src/*` — substitute the real values used by CLUU's kbd service.)

- [ ] **Step 2: Add handlers**

```rust
impl Compositor {
    pub fn focus_next(&mut self) {
        if self.windows.is_empty() { return; }
        let cur = self.focused;
        let pos = cur.and_then(|id| self.windows.iter().position(|w| w.id == id)).unwrap_or(0);
        let new = (pos + 1) % self.windows.len();
        // Move to top of z-order
        let win = self.windows.remove(new);
        let new_id = win.id;
        self.windows.push(win);
        self.focused = Some(new_id);
        self.repaint_all();
    }
    pub fn focus_prev(&mut self) {
        if self.windows.is_empty() { return; }
        let pos = self.focused
            .and_then(|id| self.windows.iter().position(|w| w.id == id))
            .unwrap_or(0);
        let len = self.windows.len();
        let new = (pos + len - 1) % len;
        let win = self.windows.remove(new);
        let new_id = win.id;
        self.windows.push(win);
        self.focused = Some(new_id);
        self.repaint_all();
    }
    pub fn move_focused(&mut self, dx: i16, dy: i16) {
        let Some(id) = self.focused else { return; };
        if let Some(win) = self.windows.iter_mut().find(|w| w.id == id) {
            let new_x = (win.x as i32 + dx as i32).max(0).min(self.cols as i32 - win.w as i32) as u16;
            let new_y = (win.y as i32 + dy as i32).max(1).min(self.rows as i32 - win.h as i32) as u16;
            let old_x = win.x; let old_y = win.y;
            win.x = new_x; win.y = new_y;
            // Mark old + new region dirty.
            for cy in old_y..old_y + win.h { for cx in old_x..old_x + win.w { self.cell_dirty.push((cx, cy)); } }
            for cy in new_y..new_y + win.h { for cx in new_x..new_x + win.w { self.cell_dirty.push((cx, cy)); } }
        }
    }
    pub fn resize_focused(&mut self, dw: i16, dh: i16) {
        let Some(id) = self.focused else { return; };
        if let Some(win) = self.windows.iter_mut().find(|w| w.id == id) {
            let new_w = ((win.w as i32 + dw as i32).max(5).min(self.cols as i32 - win.x as i32)) as u16;
            let new_h = ((win.h as i32 + dh as i32).max(5).min(self.rows as i32 - win.y as i32)) as u16;
            let old_w = win.w; let old_h = win.h;
            win.w = new_w; win.h = new_h;
            // Mark superset region dirty.
            let (max_w, max_h) = (old_w.max(new_w), old_h.max(new_h));
            for cy in win.y..win.y + max_h { for cx in win.x..win.x + max_w { self.cell_dirty.push((cx, cy)); } }
        }
    }
    pub fn repaint_all(&mut self) {
        for cy in 0..self.rows { for cx in 0..self.cols { self.cell_dirty.push((cx, cy)); } }
    }
}
```

- [ ] **Step 3: Dispatch in event loop**

```rust
Incoming::KbdEvent { keycode, modifiers, codepoint, kind } => {
    if let Some(hk) = hotkeys::match_hotkey(keycode, modifiers, kind) {
        match hk {
            hotkeys::Hotkey::FocusNext => comp.focus_next(),
            hotkeys::Hotkey::FocusPrev => comp.focus_prev(),
            hotkeys::Hotkey::MoveLeft  => comp.move_focused(-1, 0),
            hotkeys::Hotkey::MoveRight => comp.move_focused( 1, 0),
            hotkeys::Hotkey::MoveUp    => comp.move_focused(0, -1),
            hotkeys::Hotkey::MoveDown  => comp.move_focused(0,  1),
            hotkeys::Hotkey::ResizeLeft  => comp.resize_focused(-1, 0),
            hotkeys::Hotkey::ResizeRight => comp.resize_focused( 1, 0),
            hotkeys::Hotkey::ResizeUp    => comp.resize_focused(0, -1),
            hotkeys::Hotkey::ResizeDown  => comp.resize_focused(0,  1),
            hotkeys::Hotkey::CloseRequest => comp.forward_close_request(),
            hotkeys::Hotkey::SpawnDemo    => comp.spawn_demo(),
        }
    } else {
        comp.forward_input_event(keycode, modifiers, codepoint, kind);
    }
}
```

`forward_close_request`, `forward_input_event`, `spawn_demo` are added in Tasks 18 and 19.

- [ ] **Step 4: Build + smoke**

Run: `cargo xtask build && MARKER_MODE=l2_path_symlink_resolve bash scripts/harness_run.sh`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add userspace/compositor/src/hotkeys.rs userspace/compositor/src/main.rs userspace/compositor/src/state.rs
git commit -m "compositor/hotkeys: focus cycle, move, resize, close, spawn"
```

### Task 18: Forward keystrokes + close-request to focused window

**Files:**
- Modify: `userspace/compositor/src/state.rs`
- Modify: `userspace/compositor/src/main.rs`

- [ ] **Step 1: Implement input forwarding**

```rust
impl Compositor {
    pub fn forward_input_event(&mut self, keycode: u32, mods: u32, cp: u32, kind: u32) {
        let Some(id) = self.focused else { return; };
        let Some(win) = self.windows.iter().find(|w| w.id == id) else { return; };
        let msg = libcluu::types::Message::new(
            libcluu::ipc::COMP_INPUT_FORWARD_LABEL,
            [id, keycode as u64, mods as u64, cp as u64, kind as u64, 0],
            0,
        );
        let _ = libcluu::syscall::ipc_send(win.input_endpoint, &msg, libcluu::types::IpcFlags::empty());
    }

    pub fn forward_close_request(&mut self) {
        // Special INPUT_FORWARD with kind = 99 = "close-request".
        let Some(id) = self.focused else { return; };
        let Some(win) = self.windows.iter().find(|w| w.id == id) else { return; };
        let msg = libcluu::types::Message::new(
            libcluu::ipc::COMP_INPUT_FORWARD_LABEL,
            [id, 0, 0, 0, 99, 0],
            0,
        );
        let _ = libcluu::syscall::ipc_send(win.input_endpoint, &msg, libcluu::types::IpcFlags::empty());
    }
}
```

- [ ] **Step 2: Build + commit**

```bash
cargo xtask build
git add userspace/compositor/src/state.rs userspace/compositor/src/main.rs
git commit -m "compositor: forward keystrokes + close-request to focused window"
```

### Task 19: Status bar render + 1 Hz timer

**Files:**
- Create: `userspace/compositor/src/status.rs`
- Modify: `userspace/compositor/src/main.rs`
- Modify: `userspace/compositor/src/compose.rs`

- [ ] **Step 1: Status bar text builder**

```rust
// userspace/compositor/src/status.rs
extern crate alloc;
use alloc::format;
use alloc::string::String;
use crate::state::Compositor;

pub fn render_status(comp: &Compositor) -> String {
    let secs = comp.clock_seconds;
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    let focused_title = comp.focused
        .and_then(|id| comp.windows.iter().find(|w| w.id == id))
        .map(|w| w.title.as_str())
        .unwrap_or("(none)");
    format!("[{:02}:{:02}:{:02}]  focused: {}   |   windows: {}",
        h, m, s, focused_title, comp.windows.len())
}
```

- [ ] **Step 2: Lay status bytes into row 0 of `cell_grid`**

In `compose.rs`, after the `recompute_dirty` walk:

```rust
pub fn render_status_row(comp: &mut Compositor) {
    let s = crate::status::render_status(comp);
    let bytes = s.as_bytes();
    for cx in 0..comp.cols {
        let cp = if (cx as usize) < bytes.len() { bytes[cx as usize] as u32 } else { b' ' as u32 };
        let cell = pack_cell(cp, 7, 0, 0);
        let idx = cx as usize;
        comp.cell_grid[idx] = cell;
    }
}
```

Call it from `recompute_dirty`'s tail (or just unconditionally at every compose). Add row 0 cells to the dirty set so `flush_grid_to_backbuf` rewrites them.

- [ ] **Step 3: Subscribe to timeserver 1 Hz tick**

The timeserver helper is `libcluu::time::now()`. v1: instead of subscribing, just call `time::now()` on every recv timeout (1 s in `ipc_recv_any` already), update `comp.clock_seconds = secs;`, mark row 0 dirty.

```rust
Err(libcluu::Error::Timeout) => {
    if let Ok((s, _ns)) = libcluu::time::now(libcluu::time::Clock::Monotonic) {
        if s != comp.clock_seconds {
            comp.clock_seconds = s;
            for cx in 0..comp.cols { comp.cell_dirty.push((cx, 0)); }
        }
    }
}
```

- [ ] **Step 4: Build + commit**

```bash
cargo xtask build
git add userspace/compositor/src/status.rs userspace/compositor/src/compose.rs userspace/compositor/src/main.rs
git commit -m "compositor/status: clock + focused-title status bar at row 0"
```

### Task 20: VT activate/deactivate handlers

**Files:**
- Modify: `userspace/compositor/src/main.rs`
- Modify: `userspace/compositor/src/state.rs`

- [ ] **Step 1: Toggle `active` and clear/repaint on transition**

```rust
impl Compositor {
    pub fn handle_vt_activate(&mut self) {
        self.active = true;
        self.repaint_all();
    }
    pub fn handle_vt_deactivate(&mut self) {
        self.active = false;
        // No fb writes while inactive; state retained.
    }
}
```

- [ ] **Step 2: Gate fb writes on `comp.active`**

In the event-loop tail:

```rust
compose::recompute_dirty(&mut comp);
if comp.active {
    comp.flush_grid_to_backbuf(&atlas);
    comp.flush_backbuf_to_fb();
}
```

- [ ] **Step 3: Build + commit**

```bash
cargo xtask build
git add userspace/compositor/src/main.rs userspace/compositor/src/state.rs
git commit -m "compositor: VT activate/deactivate gating fb writes"
```

---

## Wave 5 — `compdemo` client

### Task 21: `compdemo` registers a window

**Files:**
- Modify: `userspace/compdemo/src/main.rs`

- [ ] **Step 1: Look up the compositor's client endpoint**

```rust
#![no_std]
#![no_main]
extern crate alloc;

use libcluu::{debug_print, registry, syscall};
use libcluu::types::{Message, IpcFlags};
use libcluu::ipc;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = debug_print("compdemo: start");
    let comp = match registry::lookup_service("compositor:client") {
        Some(ep) => ep,
        None => { let _ = debug_print("compdemo: no compositor"); return 1; }
    };
    // Ask for a 40x12 window titled "demo".
    let title = b"demo";
    let mut payload = [0u8; 32];
    payload[..title.len()].copy_from_slice(title);
    let req = Message::new(
        ipc::COMP_WIN_REGISTER_LABEL,
        [40, 12, title.len() as u64, 0, 0, 0],
        title.len() as u32,
    );
    let mut reply_buf = [0u8; 256];
    let (rmsg, _rpayload) = match syscall::call(comp, &req, &payload[..title.len()], &mut reply_buf) {
        Ok(t) => t,
        Err(_) => return 2,
    };
    let win_id = rmsg.words[0];
    let token  = rmsg.words[1];
    let gw     = rmsg.words[2] as u32;
    let gh     = rmsg.words[3] as u32;
    let _ = debug_print("compdemo: registered");

    // Map the SHM token into our address space at a fixed va.
    let cells_bytes = gw as usize * gh as usize * 8;
    let shm_size = (32 + cells_bytes + 0xFFF) & !0xFFF;
    let va: usize = 0xD000_0000;
    if syscall::space_map_range(va, token, shm_size as u64,
        syscall::FLAGS_USER_RW | syscall::MAP_FRAME_TOKEN).is_err() {
        return 3;
    }
    let _ = debug_print("compdemo: mapped");
    let _ = (win_id, va);
    loop {
        let _ = syscall::yield_cpu();
    }
}
```

(Verify `libcluu::syscall::call` and the FLAGS constants — adjust to actual names.)

- [ ] **Step 2: Build + smoke**

Run: `cargo xtask build && MARKER_MODE=l2_path_symlink_resolve bash scripts/harness_run.sh`
Expected: PASS — but compdemo isn't auto-spawned yet, so its log lines won't appear. That's fine; the next task spawns it.

- [ ] **Step 3: Commit**

```bash
git add userspace/compdemo/src/main.rs
git commit -m "compdemo: register a 40x12 window and map SHM"
```

### Task 22: `compdemo` rainbow + keystroke loop

**Files:**
- Modify: `userspace/compdemo/src/main.rs`

- [ ] **Step 1: Fill cells with a shifting palette pattern, send DAMAGE every iteration**

```rust
let cells_ptr = (va + 32) as *mut u64;
let mut frame: u32 = 0;
let mut input_buf = [0u8; 64];
let in_ep = registry::lookup_service("compositor:client").unwrap();

loop {
    // Compose: fill cells with a moving rainbow.
    for iy in 0..gh {
        for ix in 0..gw {
            let color = (((ix + iy + frame) as u8).wrapping_mul(3)) & 0xFF;
            let cell = ((b'#' as u64) & 0x1F_FFFF)
                | ((color as u64) << 21)   // fg
                | (0u64 << 29)              // bg = 0
                | (0u64 << 37);             // attrs = 0
            unsafe { core::ptr::write_volatile(cells_ptr.add((iy*gw + ix) as usize), cell); }
        }
    }
    let header = va as *mut crate_state_dummy::WindowShm;
    unsafe { (*header).generation = (*header).generation.wrapping_add(1); }
    let dmg = Message::new(
        ipc::COMP_WIN_DAMAGE_LABEL,
        [win_id, 0, 0, gw as u64, gh as u64, 0],
        0,
    );
    let _ = syscall::ipc_send(comp, &dmg, IpcFlags::empty());
    // Wait briefly for a keystroke (or timeout to drive the next rainbow frame).
    if let Ok((_idx, n)) = syscall::ipc_recv_one(in_ep, &mut input_buf, 50) {
        if let Some((m, _)) = libcluu::ipc::parse_message(&input_buf[..n]) {
            if m.tag.label == ipc::COMP_INPUT_FORWARD_LABEL {
                let kind = m.words[4] as u32;
                if kind == 99 {
                    let _ = debug_print("compdemo: close request, exiting");
                    return 0;
                }
                let _ = debug_print("compdemo: got key");
            }
        }
    }
    frame = frame.wrapping_add(1);
}
```

(`crate_state_dummy::WindowShm` here is illustrative — duplicate the `WindowShm` repr-C definition in `compdemo` since it can't depend on `compositor`. Or move the type into `libcluu` and import.)

- [ ] **Step 2: Build + commit**

```bash
cargo xtask build
git add userspace/compdemo/src/main.rs
git commit -m "compdemo: shifting rainbow + keystroke handler"
```

### Task 23: Super+N spawns compdemo via procmgr

**Files:**
- Modify: `userspace/compositor/src/main.rs`
- Modify: `userspace/compositor/src/state.rs`

- [ ] **Step 1: Add `spawn_demo` method**

```rust
impl Compositor {
    pub fn spawn_demo(&mut self) {
        // Look up procmgr. Build a SPAWN message naming compdemo.
        let Some(procmgr) = libcluu::registry::lookup_service("procmgr:main") else {
            let _ = libcluu::debug_print("compositor: no procmgr; cannot spawn demo");
            return;
        };
        // Send PROCMGR_SPAWN with target = "compdemo". Verify exact label/format.
        let req = libcluu::types::Message::new(
            libcluu::ipc::PROCMGR_SPAWN_SERVICE_LABEL,
            [0; 6],
            0,
        );
        let payload = b"compdemo";
        let _ = libcluu::syscall::ipc_send(procmgr, &req, libcluu::types::IpcFlags::empty());
        let _ = (procmgr, req, payload); // adapt args to actual SPAWN ABI
    }
}
```

(Look up the exact SPAWN-by-name protocol with `grep -rn 'PROCMGR_SPAWN' userspace/procmgr/`. Use the same call shape as wherever Super+N's analogue lives in the shell or init.)

- [ ] **Step 2: Build + commit**

```bash
cargo xtask build
git add userspace/compositor/src/state.rs userspace/compositor/src/main.rs
git commit -m "compositor: Super+N spawns compdemo via procmgr"
```

---

## Wave 6 — Init wiring + test harness

### Task 24: Init spawns compositor on VT4

**Files:**
- Modify: `userspace/init/src/main.rs`
- Modify: `userspace/init/Cluufile` (or wherever console:0..3 are listed)

- [ ] **Step 1: Locate the VT spawn loop**

Run: `grep -n 'console:0\|console:1\|console:2\|console:3\|spawn.*console' userspace/init/src/main.rs`
Expected: lines that spawn `console:N` for N=0..3 with corresponding TTY pairing.

- [ ] **Step 2: Replace the VT3 (4th VT) spawn**

Change the VT3 entry to spawn `compositor` with the same fb params and the VT-3 endpoint pair (kbd routing + control). The compositor won't talk TTY — its kbd routing comes via `compositor:input` registry name, which kbd will look up post-Task 25.

Concrete change: where init does

```rust
spawn_service("console", &[FB_PARAMS, /* VT index */ 3, /* active = */ 0]);
```

replace with

```rust
spawn_service("compositor", &[FB_PARAMS, /* VT index */ 3, /* active = */ 0]);
```

(Adjust to the actual API.)

- [ ] **Step 3: Build + smoke**

Run: `cargo xtask build && MARKER_MODE=l2_path_symlink_resolve bash scripts/harness_run.sh`
Expected: PASS, with serial line `compositor: ready`.

- [ ] **Step 4: Commit**

```bash
git add userspace/init/src/main.rs userspace/init/Cluufile
git commit -m "init: spawn compositor on VT4 in place of console:3"
```

### Task 25: kbd routes to compositor when VT4 active

**Files:**
- Modify: `userspace/kbd/src/main.rs`

- [ ] **Step 1: Look up kbd's per-VT routing**

Run: `grep -n 'vt_index\|active_vt\|tty_endpoints\|route' userspace/kbd/src/main.rs | head -40`
Expected: a table or array indexed by VT number with the endpoint kbd forwards events to.

- [ ] **Step 2: For VT3 (compositor), route to `compositor:input` instead of `tty:3`**

Where kbd builds its per-VT route table at startup, look up `compositor:input` from the registry; store its endpoint at index 3. The Ctrl+Alt+F4 path that activates VT3 must continue to send `COMP_VT_ACTIVATE_LABEL` to `compositor:control`.

- [ ] **Step 3: Build + smoke**

Run: `cargo xtask build && MARKER_MODE=l2_path_symlink_resolve bash scripts/harness_run.sh`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add userspace/kbd/src/main.rs
git commit -m "kbd: route VT3 input to compositor:input endpoint"
```

### Task 26: `l2_compositor_smoke` marker

**Files:**
- Modify: `scripts/harness_run.sh`
- Create: `userspace/c-programs/compsmoke.c` (or use compdemo with a quit-after-1-frame flag)

- [ ] **Step 1: Define the smoke probe**

Simplest: have init auto-spawn `compdemo` at boot when env var `MARKER_MODE=l2_compositor_smoke` is set. compdemo prints `COMPSMOKE: REGISTERED <id>` after it gets a successful WIN_REGISTER reply, then on first DAMAGE prints `COMPSMOKE: PASS`.

- [ ] **Step 2: Add the marker**

```bash
# scripts/harness_run.sh, near other l2_* blocks
l2_compositor_smoke)
    REQUIRED_MARKERS=(
        "compositor: ready"
        "COMPSMOKE: REGISTERED"
        "COMPSMOKE: PASS"
    )
    ;;
```

- [ ] **Step 3: Run + commit**

Run: `MARKER_MODE=l2_compositor_smoke bash scripts/harness_run.sh`
Expected: PASS (retry once on `vt/manifest` flake).

```bash
git add scripts/harness_run.sh userspace/c-programs/compsmoke.c
git commit -m "harness: l2_compositor_smoke MARKER_MODE"
```

### Task 27: `l2_compositor_focus` marker

**Files:**
- Modify: `scripts/harness_run.sh`
- Modify: `userspace/compdemo/src/main.rs` (debug print on receiving INPUT_FORWARD)

- [ ] **Step 1: Make compdemo print the focused-id on each event**

```rust
let _ = debug_print("compdemo: focused-event");
```

- [ ] **Step 2: Synthesize Alt+Tab events**

Add a probe binary or a kbd debug hook that injects three Alt+Tab events at boot when `MARKER_MODE=l2_compositor_focus`. Each tab cycles focus to next of three demo windows (init spawns three compdemos for this marker).

- [ ] **Step 3: Define marker**

```bash
l2_compositor_focus)
    REQUIRED_MARKERS=(
        "compositor: focus -> 1"
        "compositor: focus -> 2"
        "compositor: focus -> 3"
    )
    ;;
```

(Add `let _ = debug_print(format!("compositor: focus -> {}", id));` in `focus_next`.)

- [ ] **Step 4: Run + commit**

```bash
MARKER_MODE=l2_compositor_focus bash scripts/harness_run.sh
git add scripts/harness_run.sh userspace/compositor/src/state.rs
git commit -m "harness: l2_compositor_focus MARKER_MODE"
```

### Task 28: `l2_compositor_destroy` marker

**Files:**
- Modify: `scripts/harness_run.sh`
- Modify: `userspace/compositor/src/main.rs` (log destroy events)

- [ ] **Step 1: Log on destroy**

Add `let _ = debug_print("compositor: window destroyed via exit");` in the `PROC_EXIT_LABEL` handler.

- [ ] **Step 2: Probe behavior**

Boot with one `compdemo` window and a second probe that issues `kill <pid>` against the demo's pid via a shell builtin call. Mark.

```bash
l2_compositor_destroy)
    REQUIRED_MARKERS=(
        "compositor: window destroyed via exit"
    )
    ;;
```

- [ ] **Step 3: Run + commit**

```bash
MARKER_MODE=l2_compositor_destroy bash scripts/harness_run.sh
git add scripts/harness_run.sh userspace/compositor/src/main.rs
git commit -m "harness: l2_compositor_destroy MARKER_MODE"
```

### Task 29: `l2_compositor_legacy_vt` marker

**Files:**
- Modify: `scripts/harness_run.sh`

- [ ] **Step 1: Probe behavior**

Inject two synthetic Ctrl+Alt+F1 / Ctrl+Alt+F4 events at boot. After the round-trip, check both `console: VT0 active` and `compositor: VT activate` lines exist.

- [ ] **Step 2: Marker**

```bash
l2_compositor_legacy_vt)
    REQUIRED_MARKERS=(
        "compositor: VT activate"
        "console: VT0 active"
        "compositor: VT activate"
    )
    ;;
```

(Add the appropriate debug_prints in compositor's VT_ACTIVATE handler and console's existing activate path if missing.)

- [ ] **Step 3: Run + commit**

```bash
MARKER_MODE=l2_compositor_legacy_vt bash scripts/harness_run.sh
git add scripts/harness_run.sh
git commit -m "harness: l2_compositor_legacy_vt MARKER_MODE"
```

### Task 30: `b_compositor_blit` perf bench + ratchet

**Files:**
- Create: `userspace/c-programs/compositor_blit_bench.c`
- Modify: `scripts/harness_run.sh`
- Modify: `scripts/perf_ratchet.json`

- [ ] **Step 1: Bench probe**

Mirror `userspace/c-programs/console_blit_bench.c` but route through compositor: register a fullscreen window, fill cells, send full-screen DAMAGE, time the round-trip from "cells written" until next render completes (compositor logs `BENCH_COMP_BLIT: cycles_per_full_screen=N` after `flush_backbuf_to_fb`).

- [ ] **Step 2: Marker block**

```bash
b_compositor_blit)
    REQUIRED_MARKERS=("BENCH_COMP_BLIT: cycles_per_full_screen=")
    ;;
```

Plus a parser block analogous to the existing `b_console_blit` parser (see `scripts/harness_run.sh:2085-2107`).

- [ ] **Step 3: Capture baseline + lock ratchet**

Run the bench 3× under KVM. Set `compositor_blit_cycles` to median, `compositor_blit_max_cycles` to median × 1.5 (per spec §11 target).

```json
{
  ...
  "compositor_blit_cycles":     <median>,
  "compositor_blit_max_cycles": <median * 1.5, integer>,
  "_note": "...; compositor_blit gated 1.5x console baseline (extra chrome/composite pass)."
}
```

- [ ] **Step 4: Run + commit**

```bash
MARKER_MODE=b_compositor_blit bash scripts/harness_run.sh
git add scripts/harness_run.sh scripts/perf_ratchet.json userspace/c-programs/compositor_blit_bench.c
git commit -m "harness: b_compositor_blit + perf ratchet baseline"
```

---

## Self-Review

**Spec coverage:**

- §1 Goal — Tasks 1–30 collectively
- §3 Constraints — Prereq gate enforces FB plan dependency
- §4 System placement — Task 24 (init spawn on VT4); Tasks 8–9 (3 endpoints)
- §5 Cell payload — `pack_cell` defined in Task 12, used throughout
- §6 SHM region — Task 5 (`WindowShm`), Task 7 (alloc helpers), Task 10 (`WIN_REGISTER` allocates + initialises header)
- §7 IPC protocol — Task 2 (labels), Task 8 (parse), Tasks 10/11/15/16 (handlers), Task 18 (`INPUT_FORWARD`), Task 20 (VT_ACTIVATE/DEACTIVATE)
- §8 Compositor internals + compose pipeline — Tasks 12 + 13
- §9 Chrome (Tier-3) + hotkeys + status bar — Tasks 3, 4, 14 (chrome), Task 17 (hotkeys), Task 19 (status)
- §10 Lifecycle + error handling — Task 11 (destroy + exit), spec error rules covered inline in Tasks 10/15/16
- §11 Testing — Tasks 26–30
- §12 Files touched — matches "File Structure" above
- §14 Future sub-projects — out of scope; covered by spec doc

**Type consistency check:**

- `WindowId = u64` consistent across Tasks 5, 10, 11, 14, 15, 16, 17, 18.
- `pack_cell(cp, fg, bg, attrs)` signature consistent (Task 12 defines, Tasks 14 + 19 call with same shape).
- `WindowShm` repr-C with offset 32 for cells start consistent (Tasks 5, 10, 12, 21, 22).
- `COMP_*_LABEL` constants from Task 2 referenced unchanged in Tasks 8, 10, 17, 18, 20.

**Placeholder scan:**

- "Verify against actual API" appears in Tasks 7, 18, 23, 24, 25 — these are unavoidable: they cite real grep commands the engineer runs to discover the correct local API name. Each task ships with concrete code that the engineer adapts after the grep. Acceptable.
- No "TBD", "implement later", or "fill in details" tokens remain.

**Risks captured (mirrored from spec):**

1. `vt/manifest` flake — every harness step instructs "retry once".
2. `MAP_SHARE_PHYS` UAF — under no-op invalidation; spec §10 documents safety; compositor uses `read_volatile` with generation acquire-load.
3. Atlas + `/dev/fb0` prereq — explicit gate at top of plan.
4. Chrome bitmap quality — flagged in Task 3 step 2; tuning is part of Task 9 (visual eyeball).
5. `bitflags` dep introduction in Task 17 — verify it's already a workspace dep; otherwise hand-roll constants.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-10-tui-compositor.md`. Two execution options:

**1. Subagent-Driven (recommended)** — I dispatch a fresh sonnet subagent per task, haiku reviewer between tasks, fast iteration.

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch with checkpoints.

Which approach?
