# Runtime TTF Loading — Implementation Plan

> Decision record: `~/agentic-knowledge/decisions/cluu/cluu-runtime-ttf-loading.md`
> Predecessor: Unicode glyph extension (build-time fontdue rasterization), see
> `decisions/cluu/cluu-unicode-glyph-extension.md`.

## Goal

Allow CLUU binaries to render text from user-supplied TTF files at runtime,
without rebuilding the disk image. The fixed 8×16 cell grid stays; only the
glyph source changes from build-time-baked `.rodata` banks to a runtime-
rasterized lazy atlas.

## Non-goals

- Variable cell width / proportional fonts (still 8×16)
- Subpixel glyph positioning
- Text shaping (kerning, ligatures, bidirectional)
- Multi-font families per weight (one TTF per regular/bold/italic)
- A shared `fontsrv` IPC service (deferred to a possible Phase 6)

## Current state

- `userspace/libcluu/build.rs` rasterizes `0xProto-{Regular,Bold,Italic}.ttf`
  at build time via `fontdue` into three 32 KB CP437 banks
  (`FONT_0XPROTO_*_ALPHA`) plus three 16 KB box-drawing extension banks
  (`FONT_BOX_*_ALPHA`).
- `userspace/libcluu/src/font.rs` exposes:
  - `glyph_alpha_for_codepoint(cp: u32, bold: bool, italic: bool) -> Option<[u8; 128]>`
    — called by compositor + console renderers.
  - `glyph_alpha_for_cp437(ch: u8, bold, italic) -> [u8; 128]` — CP437 fallback.
- Hand-coded 1-bit overrides exist and stay authoritative for their codepoints:
  `ARC_CORNERS` (4), `EIGHTH_BLOCKS` (6), `thinned_box_glyph` (10),
  `dashed_box_glyph` (2, added 2026-07-21). These outperform any TTF at 8×16.
- `blend_alpha_row` (`libcluu/src/simd.rs:67`) does sRGB-correct alpha blending
  with a SIMD fast path — already consumed by both renderers, unchanged here.
- TTFs live at `userspace/libcluu/fonts/` (~210 KB each, OFL-1.1).

## Architecture

```
            ┌─────────────────────────────────────────────────┐
            │  libcluu::font_runtime (new module)             │
            │                                                 │
  TTF file  │  ┌────────────┐    ┌──────────────────────┐    │
  /etc/ ────┼─▶│ ab_glyph   │───▶│ GlyphAtlas           │    │
  fonts/    │  │ OwnedFont  │    │ HashMap<(cp, bold,   │    │
            │  │ (3 weights)│    │   italic), [u8;128]> │    │
            │  └────────────┘    │ spin::Mutex          │    │
            │                    └────────┬─────────────┘    │
            │                             │                  │
            │           ┌─────────────────┴──────────────┐   │
            │           ▼                                ▼   │
            │  glyph_alpha_for_codepoint    glyph_alpha_     │
            │  (existing API, unchanged     for_cp437        │
            │   signature)                  (existing API)   │
            └─────────────┬──────────────────────────────────┘
                          │
            ┌─────────────┴─────────────────┐
            ▼                               ▼
   compositor::render::             console::renderer::
   flush_grid_to_backbuf            (cell blit)
   (unchanged consumer)             (unchanged consumer)
```

Two consumers (compositor + console) each link libcluu → each gets its own
`lazy_static` atlas. Memory cost is doubled but isolation is clean and the
SHM `u64` cell protocol is untouched. A shared `fontsrv` IPC service is a
possible Phase 6 refinement.

## Lookup precedence (final)

1. **Hand-coded 1-bit overrides** — `dashed_box_glyph`, `thinned_box_glyph`,
   `ARC_CORNERS`, `EIGHTH_BLOCKS`, `TRANSPORT_GLYPHS`. Always win for
   sharpness at 8×16.
2. **Runtime atlas** (`RUNTIME_ATLAS`) — covers everything the TTF has.
3. **Build-time programmatic banks** — `FONT_BLOCK_ALPHA`,
   `FONT_BRAILLE_ALPHA`. Covers what the TTF lacks (geometric shapes).
4. **CP437 fallback** — `FONT_0XPROTO_*_ALPHA` + `FONT_CP437_BOXES`.
   Deleted in Phase 4.

## Phased implementation

### Phase 1 — Dependency & TTF packaging

- Add `ab_glyph = "0.2"` to `userspace/libcluu/Cargo.toml` `[dependencies]`.
  Verify `no_std + alloc` build.
- Stage the three TTFs into the initrd at `etc/fonts/`. Modify whatever
  script currently stages `etc/users.toml` / `etc/welcome.txt` into
  `userdisk.img` to include `etc/fonts/0xProto-{Regular,Bold,Italic}.ttf`.
- Verify with `cargo xtask build` that `target/cluu.img` mounts
  `/etc/fonts/` readable from the running system.

### Phase 2 — Runtime rasterizer core

- New file `userspace/libcluu/src/font_runtime.rs`:
  - `pub struct FontSet { regular: OwnedFont, bold: OwnedFont, italic: OwnedFont }`
    where `OwnedFont` wraps `ab_glyph::FontRef` + the owned `Vec<u8>` bytes.
  - `pub struct GlyphAtlas { fonts: FontSet, cache: spin::Mutex<HashMap<(u32, bool, bool), [u8; 128]>> }`.
  - `pub fn rasterize(atlas: &GlyphAtlas, cp: u32, bold: bool, italic: bool) -> Option<[u8; 128]>`
    — rasterize at 13.5 pt into an 8×16 cell, baseline 13 (matches existing
    `build.rs` constants). Returns `None` for codepoints not in the TTF cmap.
  - Lazy `static RUNTIME_ATLAS: OnceCell<GlyphAtlas>` initialised on first
    call by reading `/etc/fonts/*.ttf` via `libcluu::fs` (or the existing
    VFS IPC path used by other libcluu file reads).
- Wire `glyph_alpha_for_codepoint` (font.rs) to consult `RUNTIME_ATLAS`
  after the hand-coded overrides, before falling through to build-time banks.
- Wire `glyph_alpha_for_cp437` similarly — convert `u8` → Unicode via the
  existing `cp437_to_unicode` table, then call the runtime path.
- Keep `FONT_0XPROTO_*_ALPHA` and `FONT_BOX_*_ALPHA` as fallbacks for now
  (deletion is Phase 4).

### Phase 3 — Override precedence audit

- Audit the final lookup order against the precedence list above.
- Update the `font.rs` module doc to describe precedence explicitly so the
  next person doesn't have to reverse-engineer it.
- Verify the dashed border glyphs (added 2026-07-21) still render via
  `dashed_box_glyph`, NOT via the runtime atlas — they must stay crisp.

### Phase 4 — Build-time bank deletion

Deferred until Phase 2-3 are stable in a running system.

- Remove `FONT_0XPROTO_*_ALPHA` (3 × 32 KB = 96 KB) and
  `FONT_BOX_*_ALPHA` (3 × 16 KB = 48 KB) from `build.rs` output. Keep
  `FONT_BLOCK_ALPHA` and `FONT_BRAILLE_ALPHA` — programmatic, TTF lacks
  them.
- Simplify `build.rs` — drop the `fontdue` build-dependency (replaced by
  `ab_glyph` at runtime). Keep only the block/braille generators.
- Verify the −144 KB `.rodata` reduction via
  `size target/x86_64-cluu-user/debug/libcluu.rlib`.

### Phase 5 — Validation

- Boot test: `cargo xtask run`, log in, verify shell + compositor render
  all glyph classes (Latin, CP437 box, dashed border, Braille spectrum in
  cluuamp, block elements).
- Memory audit: `top` should show compositor RSS up by ~500-800 KB
  (parsed TTFs ~3 × 250 KB + atlas cache). Verify no OOM.
- Latency: first-frame cost measured via the existing `BENCH_COMP_BLIT`
  serial marker. Expected <1 ms additional.
- Harness: add a `l2_runtime_font` case to `python/cluu_harness/` — boot,
  log in, dump the cell grid, assert non-empty rasterization for a few
  representative codepoints.
- **Definition of done**: replace `/etc/fonts/0xProto-Regular.ttf` with a
  different TTF, reboot, and the rendered font changes. This is the
  user-visible win.

## Risks & mitigations

| Risk | Likelihood | Mitigation |
|---|---|---|
| Heap pressure in compositor / console | Medium | Atlas is lazy + cached. TTFs parsed once. If too tight, move to a `fontsrv` IPC service (Phase 6). |
| `ab_glyph` pulls `std` | Low | Verified `no_std + alloc`. Fallback: `rusttype` (older, definitely `no_std`). |
| First-frame latency spike | Low | Pre-rasterize ASCII `0x20-0x7E` + CP437 box glyphs at atlas init (~100 glyphs × 1 µs ≈ 0.1 ms). |
| TTF not present at `/etc/fonts/` (e.g. shell-spawned test compositor before initrd is wired) | Medium | `RUNTIME_ATLAS` init returns `None` gracefully; existing build-time banks cover this case until Phase 4 deletes them. |
| Two consumers each load TTFs (memory doubled) | Medium | Accept for v1. Phase 6: shared `fontsrv` IPC service. |
| Container font visibility (per AGENTS.md §2, §4) | Medium | Default: only root session sees `/etc/fonts/`. Other containers need explicit `MOUNT /etc/fonts ro` in their Cluufile. Document in `containers.md`. |

## Open decisions

1. **`fontsrv` IPC service vs. inline atlas in each binary.** Plan above
   chooses inline for v1 simplicity. Revisit if memory pressure forces it.
2. **TTF location: `/etc/fonts/` vs. `/usr/share/fonts/`.** `/etc/fonts/`
   matches existing CLUU convention (`/etc/users.toml`,
   `/etc/welcome.txt`). Sticking with `/etc/fonts/` unless convention
   diverges.
3. **Default font: keep 0xProto or switch to a smaller TTF?** 0xProto is
   ~210 KB per weight. A smaller font (e.g. unifont at ~300 KB for
   everything in one file) might be more efficient. Out of scope for this
   plan.
4. **Malformed TTF handling.** `ab_glyph` returns `Err` on parse.
   `RUNTIME_ATLAS.init` should fail gracefully and fall through to
   build-time banks (until Phase 4 deletes them). After Phase 4, a
   malformed TTF means no glyphs — render `?` and surface a serial
   diagnostic.

## Out of scope (deferred to Phase 6+)

- Shared `fontsrv` IPC service (single atlas, all consumers).
- Variable cell width / proportional fonts.
- Subpixel positioning.
- Text shaping (kerning, ligatures, bidirectional).
- Font fallback chains (e.g. try 0xProto first, fall back to unifont for
  missing glyphs).
