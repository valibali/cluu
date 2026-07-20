# Cluuamp Classic-Winamp Restyle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restyle cluuamp to the classic Winamp 2.x three-window layout (MAIN + toggleable EQUALIZER + toggleable PLAYLIST) with block-digit time display, 24×3 spectrum box, and a corrected Winamp-faithful FFT scaling.

**Architecture:** All changes confined to `userspace/cluuamp` (one new doc chapter aside). Pure-logic modules (`fft`, `layout`, `widgets`) are host-testable via `cargo test`; runtime modules (`audio`, `model`, `view`) compile only for the CLUU target via `cargo xtask build`. Spec: `docs/superpowers/specs/2026-07-20-cluuamp-winamp-restyle-design.md` — cell positions there are normative; this plan restates them in code.

**Tech Stack:** Rust no_std + alloc, libtui (View/Cell), microfft, nanomp3, xterm-256 colors.

## Global Constraints

- Design target 80×25 (cluuterm default); `min_width() = 76`, `min_height() = 25`.
- All colors are xterm-256 indices; palette in `viscolor.rs` is UNCHANGED.
- Model split (user instruction): coding/QA/UT/docs tasks → **haiku** subagents; final verification → **opus** subagent. Follow this plan literally; no design judgment.
- Host tests: `cd /home/vlb2bp/git/cluu/userspace/cluuamp && cargo test` (pure modules only — `audio`/`model`/`view` are feature-gated behind `runtime` and do NOT compile on host).
- Target build: `cd /home/vlb2bp/git/cluu && cargo xtask build` — EXPECTED BROKEN during Tasks 4–6 (layout signature change lands before model/view catch up). Restored and verified at end of Task 6. Do not "fix" intermediate breakage by improvising.
- Commit after every task (working tree has unrelated WIP — `git add` ONLY the files named in the task, never `git add -A`).
- Do not use timeouts as deadlock guards in any test.

---

### Task 1: FFT scaling fix (fft.rs)

**Files:**
- Modify: `userspace/cluuamp/src/fft.rs`
- Tests: same file, `#[cfg(test)] mod tests`

**Interfaces:**
- Produces: `bar_height(x) -> u8` and `peak_height(x) -> u8` now return honest 0–15 levels (previously any loud band pegged 15). Signatures unchanged; view (Task 7) relies on 0–15 range.

Background: band values out of `compute_bands()` are 0–255 (Winamp sadata convention). Pixel height is `value / 16` → 0–15. The current code CLAMPS 0–255 to 15 instead of shifting — that is the saturation bug.

- [ ] **Step 1: Write the failing test**

Add to `mod tests` in `userspace/cluuamp/src/fft.rs`:

```rust
    #[test]
    fn sine_does_not_saturate_spectrum() {
        // Pre-fix bug: tick() clamped 0-255 band values to 15 instead of
        // shifting >>4, so a full-scale sine pegged a wide stripe of bars
        // at max. Post-fix: energy near the 440 Hz band (bar ~13, 4-group
        // 12..16), distant bars near zero, and only a narrow group may be
        // at high level.
        let mut sa = SpectrumAnalyzer::new();
        let freq = 440.0f32;
        let sample_rate = 44100.0f32;
        let mut pcm = [0.0f32; FFT_SIZE];
        for i in 0..FFT_SIZE {
            let t = i as f32 / sample_rate;
            pcm[i] = libm::sinf(2.0 * core::f32::consts::PI * freq * t);
        }
        sa.process_pcm(&pcm);
        sa.tick();
        let excited = (12..16).map(|x| sa.bar_height(x)).max().unwrap_or(0);
        assert!(excited >= 2, "440 Hz band should be visible, got {}", excited);
        let far: u16 = (40..75).map(|x| sa.bar_height(x) as u16).sum();
        assert!(far <= 8, "distant bars should be near zero, got {}", far);
        let pegged = (0..75).filter(|&x| sa.bar_height(x) == 15).count();
        assert!(pegged <= 8, "only a narrow group may peg at 15, got {}", pegged);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd /home/vlb2bp/git/cluu/userspace/cluuamp && cargo test sine_does_not_saturate_spectrum`
Expected: FAIL (with the old clamp, `pegged` is large and/or `far` is large).

- [ ] **Step 3: Apply the fix**

In `userspace/cluuamp/src/fft.rs`:

Change the constant (line ~16):

```rust
const SPEC_SCALE: f32 = 2.0;
```

(Rationale, keep as a comment above it: full-scale sine through the Hann
window yields peak-bin magnitude ≈ N/4 = 128; ×2.0 → ≈255 = top of the
0–255 sadata scale.)

In `tick()`, replace:

```rust
            let v = if v > MAX_LEVEL { MAX_LEVEL } else { v };
```

with:

```rust
            // sadata values are 0-255; pixel height is value/16 -> 0-15
            // (Winamp draw_sa convention). Shift, don't clamp.
            let v = (v >> 4).min(MAX_LEVEL);
```

- [ ] **Step 4: Run the full fft test suite**

Run: `cd /home/vlb2bp/git/cluu/userspace/cluuamp && cargo test fft`
Expected: ALL PASS, including the pre-existing tests
(`silence_produces_zero_bars`, `dc_offset_produces_zero_bars`,
`pure_sine_produces_nonzero_bars`, `gravity_decreases_bar_height_over_ticks`,
`bars_fall_to_zero_without_input`, `peak_decays_exponentially`, …).
If `pure_sine_produces_nonzero_bars` fails (total == 0), the shift landed
wrong — re-check Step 3; do NOT weaken the test.

- [ ] **Step 5: Commit**

```bash
cd /home/vlb2bp/git/cluu
git add userspace/cluuamp/src/fft.rs
git commit -m "fix(cluuamp): FFT band scaling — shift 0-255 sadata >>4, not clamp

Full-scale audio pegged every active bar at 15 (clamp of a 0-255 value
to a 0-15 range). Winamp convention: pixel height = value/16.
SPEC_SCALE recalibrated 0.5 -> 2.0 so a full-scale sine tops the scale."
```

---

### Task 2: Block-digit time widget (widgets.rs)

**Files:**
- Modify: `userspace/cluuamp/src/widgets.rs` (append)
- Tests: same file, `mod tests`

**Interfaces:**
- Produces: `pub const BLOCK_DIGITS: [[&str; 3]; 10]`, `pub const BLOCK_MINUS: [&str; 3]`, `pub const BLOCK_COLON: [&str; 3]`, and
  `pub fn draw_block_time(view: &mut View, top: usize, col: usize, negative: bool, mins: u64, secs: u64, fg: u8)`.
  Field is 20 cols × 3 rows starting at (top, col). Task 7 calls it with `top = layout.time_top`, `col = layout.time_col`, `fg = 46`.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `userspace/cluuamp/src/widgets.rs`:

```rust
    #[test]
    fn block_digit_glyphs_are_3x3() {
        for (d, glyph) in BLOCK_DIGITS.iter().enumerate() {
            for (r, row) in glyph.iter().enumerate() {
                assert_eq!(row.chars().count(), 3, "digit {} row {} must be 3 chars", d, r);
            }
        }
        for row in BLOCK_MINUS.iter() {
            assert_eq!(row.chars().count(), 2, "minus rows must be 2 chars");
        }
        for row in BLOCK_COLON.iter() {
            assert_eq!(row.chars().count(), 1, "colon rows must be 1 char");
        }
    }

    #[test]
    fn block_time_renders_digits_at_expected_columns() {
        // -12:34 at top=0, col=0. Field layout: minus cols 0-1, digit cols
        // 3/7/13/17 (3 wide each), colon col 11.
        let mut v = View::new(30, 4);
        draw_block_time(&mut v, 0, 0, true, 12, 34, 46);
        // minus: middle row shows "▀▀"
        assert_eq!(v.get(1, 0).unwrap().ch, '▀');
        assert_eq!(v.get(1, 1).unwrap().ch, '▀');
        // digit '1' top row is " █ " at cols 3-5
        assert_eq!(v.get(0, 4).unwrap().ch, '█');
        // digit '2' top row is "█▀█" at cols 7-9
        assert_eq!(v.get(0, 7).unwrap().ch, '█');
        assert_eq!(v.get(0, 8).unwrap().ch, '▀');
        // colon at col 11: top '▄', bottom '▀'
        assert_eq!(v.get(0, 11).unwrap().ch, '▄');
        assert_eq!(v.get(2, 11).unwrap().ch, '▀');
        // digit '3' at cols 13-15, digit '4' at cols 17-19
        assert_eq!(v.get(0, 13).unwrap().ch, '█');
        assert_eq!(v.get(2, 19).unwrap().ch, '█');
        // color
        assert_eq!(v.get(1, 0).unwrap().fg, 46);
    }

    #[test]
    fn block_time_positive_has_no_minus() {
        let mut v = View::new(30, 4);
        draw_block_time(&mut v, 0, 0, false, 0, 0, 46);
        // minus cells untouched -> default space
        assert_eq!(v.get(1, 0).unwrap().ch, ' ');
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd /home/vlb2bp/git/cluu/userspace/cluuamp && cargo test block_`
Expected: COMPILE ERROR — `BLOCK_DIGITS` / `draw_block_time` not defined.

- [ ] **Step 3: Implement**

Append to `userspace/cluuamp/src/widgets.rs` (before `mod tests`):

```rust
/// 3x3 block-digit glyphs for the Winamp-style time display (spec §2).
pub const BLOCK_DIGITS: [[&str; 3]; 10] = [
    ["█▀█", "█ █", "█▄█"], // 0
    [" █ ", " █ ", "▄█▄"], // 1
    ["█▀█", "▄▀▀", "█▄▄"], // 2
    ["█▀█", " ▀█", "█▄█"], // 3
    ["█ █", "▀▀█", "  █"], // 4
    ["█▀▀", "▀▀█", "▄▄█"], // 5
    ["█▀▀", "█▀█", "█▄█"], // 6
    ["▀▀█", "  █", "  █"], // 7
    ["█▀█", "█▀█", "█▄█"], // 8
    ["█▀█", "▀▀█", "▄▄█"], // 9
];

/// Minus sign (2 cols) and colon (1 col) glyphs for the time display.
pub const BLOCK_MINUS: [&str; 3] = ["  ", "▀▀", "  "];
pub const BLOCK_COLON: [&str; 3] = ["▄", " ", "▀"];

/// Winamp-style block time "-mm:ss": 20 cols x 3 rows at (top, col).
/// Field layout: minus cols +0..1, digits at cols +3/+7/+13/+17 (3 wide),
/// colon at col +11. When `negative` is false the minus cells are left
/// untouched.
pub fn draw_block_time(
    view: &mut View,
    top: usize,
    col: usize,
    negative: bool,
    mins: u64,
    secs: u64,
    fg: u8,
) {
    let digits = [
        ((mins / 10) % 10) as usize,
        (mins % 10) as usize,
        ((secs / 10) % 10) as usize,
        (secs % 10) as usize,
    ];
    let digit_cols = [3usize, 7, 13, 17];
    for row in 0..3 {
        if negative {
            for (i, ch) in BLOCK_MINUS[row].chars().enumerate() {
                view.set(top + row, col + i, Cell::new(ch).fg(fg));
            }
        }
        for (i, ch) in BLOCK_COLON[row].chars().enumerate() {
            view.set(top + row, col + 11 + i, Cell::new(ch).fg(fg));
        }
        for (di, &dv) in digits.iter().enumerate() {
            for (i, ch) in BLOCK_DIGITS[dv][row].chars().enumerate() {
                view.set(top + row, col + digit_cols[di] + i, Cell::new(ch).fg(fg));
            }
        }
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cd /home/vlb2bp/git/cluu/userspace/cluuamp && cargo test block_`
Expected: 3 PASS.

- [ ] **Step 5: Commit**

```bash
cd /home/vlb2bp/git/cluu
git add userspace/cluuamp/src/widgets.rs
git commit -m "feat(cluuamp): block-digit time widget (Winamp face)"
```

---

### Task 3: Eighth-block vis widgets (widgets.rs)

**Files:**
- Modify: `userspace/cluuamp/src/widgets.rs` (append)
- Tests: same file, `mod tests`

**Interfaces:**
- Produces:
  - `pub const EIGHTH_BLOCKS: [char; 9]` (index = filled eighths 0–8)
  - `pub fn eighth_block(filled: usize) -> char`
  - `pub fn draw_spectrum_column(view: &mut View, top: usize, col: usize, level: u8, peak: u8)` — one column of the 24×3 vis box; `level`/`peak` 0–15
  - `pub fn draw_eq_slider(view: &mut View, top: usize, col: usize, value: i8, focused: bool)` — 2-row vertical slider, value −12..12
  - `pub fn curve_char(value: i8) -> char` — EQ curve strip glyph
  - `pub fn draw_scope_box(view: &mut View, top: usize, left: usize, width: usize, points: &[i8])` — oscilloscope in width×3 box, points −32..31
- Old `draw_spectrum_bar` and `draw_scope` stay for now (view.rs still calls them); they are DELETED in Task 7.

- [ ] **Step 1: Write the failing tests**

Add to `mod tests` in `userspace/cluuamp/src/widgets.rs`:

```rust
    #[test]
    fn eighth_block_table() {
        assert_eq!(eighth_block(0), ' ');
        assert_eq!(eighth_block(1), '▁');
        assert_eq!(eighth_block(4), '▄');
        assert_eq!(eighth_block(8), '█');
        assert_eq!(eighth_block(99), '█'); // clamps
    }

    #[test]
    fn spectrum_column_level_15_fills_all_three_rows() {
        let mut v = View::new(4, 4);
        draw_spectrum_column(&mut v, 0, 0, 15, 0);
        assert_eq!(v.get(0, 0).unwrap().ch, '█'); // top
        assert_eq!(v.get(1, 0).unwrap().ch, '█');
        assert_eq!(v.get(2, 0).unwrap().ch, '█'); // bottom
        // color zones bottom->top: green(2), yellow-ish(7), red-ish(12)
        assert_eq!(v.get(2, 0).unwrap().fg, crate::viscolor::bar_color(2));
        assert_eq!(v.get(1, 0).unwrap().fg, crate::viscolor::bar_color(7));
        assert_eq!(v.get(0, 0).unwrap().fg, crate::viscolor::bar_color(12));
    }

    #[test]
    fn spectrum_column_level_0_draws_nothing() {
        let mut v = View::new(4, 4);
        draw_spectrum_column(&mut v, 0, 0, 0, 0);
        for r in 0..3 {
            assert_eq!(v.get(r, 0).unwrap().ch, ' ');
        }
    }

    #[test]
    fn spectrum_column_partial_fill_bottom_up() {
        // level 8 -> h = 8*24/15 = 12 eighths: bottom row full (8),
        // middle row 4 eighths, top row empty.
        let mut v = View::new(4, 4);
        draw_spectrum_column(&mut v, 0, 0, 8, 0);
        assert_eq!(v.get(2, 0).unwrap().ch, '█');
        assert_eq!(v.get(1, 0).unwrap().ch, '▄');
        assert_eq!(v.get(0, 0).unwrap().ch, ' ');
    }

    #[test]
    fn spectrum_peak_marker_on_top_of_empty_bar() {
        // bar 0, peak 15 -> '▀' fg 255 in top row
        let mut v = View::new(4, 4);
        draw_spectrum_column(&mut v, 0, 0, 0, 15);
        assert_eq!(v.get(0, 0).unwrap().ch, '▀');
        assert_eq!(v.get(0, 0).unwrap().fg, crate::viscolor::PEAK_COLOR);
    }

    #[test]
    fn spectrum_peak_not_drawn_over_full_cell() {
        // level 15 fills all cells; peak 15 must NOT overwrite the full '█'.
        let mut v = View::new(4, 4);
        draw_spectrum_column(&mut v, 0, 0, 15, 15);
        assert_eq!(v.get(0, 0).unwrap().ch, '█');
    }

    #[test]
    fn eq_slider_fill_math() {
        // v=-12 -> 0 eighths: nothing drawn (unfocused)
        let mut v = View::new(4, 4);
        draw_eq_slider(&mut v, 0, 0, -12, false);
        assert_eq!(v.get(0, 0).unwrap().ch, ' ');
        assert_eq!(v.get(1, 0).unwrap().ch, ' ');
        // v=0 -> 8 eighths: bottom row full, top empty
        let mut v = View::new(4, 4);
        draw_eq_slider(&mut v, 0, 0, 0, false);
        assert_eq!(v.get(1, 0).unwrap().ch, '█');
        assert_eq!(v.get(0, 0).unwrap().ch, ' ');
        // v=12 -> 16 eighths: both rows full
        let mut v = View::new(4, 4);
        draw_eq_slider(&mut v, 0, 0, 12, false);
        assert_eq!(v.get(0, 0).unwrap().ch, '█');
        assert_eq!(v.get(1, 0).unwrap().ch, '█');
    }

    #[test]
    fn eq_slider_focused_shows_track() {
        let mut v = View::new(4, 4);
        draw_eq_slider(&mut v, 0, 0, -12, true);
        assert_eq!(v.get(0, 0).unwrap().ch, '░');
        assert_eq!(v.get(1, 0).unwrap().ch, '░');
    }

    #[test]
    fn curve_char_range() {
        assert_eq!(curve_char(-12), '▁');
        assert_eq!(curve_char(12), '█');
    }

    #[test]
    fn scope_box_flat_line_at_center() {
        // all-zero points -> y = 3 -> row 1, lower half block
        let mut v = View::new(24, 4);
        let pts = [0i8; 75];
        draw_scope_box(&mut v, 0, 0, 24, &pts);
        for j in 0..24 {
            assert_eq!(v.get(1, j).unwrap().ch, '▄', "col {}", j);
            assert_eq!(v.get(0, j).unwrap().ch, ' ');
            assert_eq!(v.get(2, j).unwrap().ch, ' ');
        }
    }

    #[test]
    fn scope_box_extremes_clamp() {
        let mut v = View::new(4, 4);
        draw_scope_box(&mut v, 0, 0, 2, &[-32i8, 31]);
        // -32 -> y=0 -> row0 '▀'; 31 -> y=5 -> row2 '▄'
        assert_eq!(v.get(0, 0).unwrap().ch, '▀');
        assert_eq!(v.get(2, 1).unwrap().ch, '▄');
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd /home/vlb2bp/git/cluu/userspace/cluuamp && cargo test eighth_ spectrum_column eq_slider curve_char scope_box 2>&1 | tail -5`
(Individual filters: `cargo test eighth_block_table` etc.)
Expected: COMPILE ERROR — functions not defined.

- [ ] **Step 3: Implement**

Append to `userspace/cluuamp/src/widgets.rs` (before `mod tests`):

```rust
/// Eighth-block fill characters: index = number of filled eighths (0-8).
pub const EIGHTH_BLOCKS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Fill char for `filled` eighths, clamped to 8.
pub fn eighth_block(filled: usize) -> char {
    EIGHTH_BLOCKS[filled.min(8)]
}

/// One spectrum column of the 24x3 vis box (spec §5). `level` and `peak`
/// are 0-15; 3 rows = 24 vertical eighths, drawn bottom-up. Row colors
/// bottom->top come from viscolor levels 2 / 7 / 12 (green/yellow/red).
pub fn draw_spectrum_column(view: &mut View, top: usize, col: usize, level: u8, peak: u8) {
    let h = (level.min(15) as usize) * 24 / 15;
    for r in 0..3usize {
        let fill = (h as i32 - (2 - r as i32) * 8).clamp(0, 8) as usize;
        if fill > 0 {
            let color_level = ((2 - r) * 5 + 2) as u8;
            view.set(
                top + r,
                col,
                Cell::new(eighth_block(fill)).fg(viscolor::bar_color(color_level)),
            );
        }
    }
    let pk = (peak.min(15) as usize) * 24 / 15;
    if pk > 0 {
        let pr = 2usize.saturating_sub(pk / 8);
        let bar_fill_at_pr = (h as i32 - (2 - pr as i32) * 8).clamp(0, 8);
        if bar_fill_at_pr < 8 {
            view.set(top + pr, col, Cell::new('▀').fg(viscolor::PEAK_COLOR));
        }
    }
}

/// Two-row vertical EQ slider (spec §3). `value` in [-12,12] ->
/// filled eighths f = (value+12)*16/24 (0-16), bottom-up. Focused slider
/// renders '░' track in empty cells and a brighter fill color.
pub fn draw_eq_slider(view: &mut View, top: usize, col: usize, value: i8, focused: bool) {
    let f = ((value as i32 + 12) * 16 / 24).clamp(0, 16) as usize;
    let fg = if focused { 226 } else { 46 };
    let fills = [f.saturating_sub(8), f.min(8)]; // [top row, bottom row]
    for (r, &fill) in fills.iter().enumerate() {
        if fill > 0 {
            view.set(top + r, col, Cell::new(eighth_block(fill)).fg(fg));
        } else if focused {
            view.set(top + r, col, Cell::new('░').fg(238));
        }
    }
}

/// EQ curve strip glyph for a band value in [-12,12] (8 steps, spec §3).
pub fn curve_char(value: i8) -> char {
    const CURVE: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let idx = ((value as i32 + 12) * 7 / 24).clamp(0, 7) as usize;
    CURVE[idx]
}

/// Oscilloscope in a `width` x 3 box (spec §5). `points` are -32..31
/// (Oscilloscope::point()). Vertical resolution 6 half-cells:
/// y = 3 + val*6/64 clamped 0-5; upper half-cell '▀', lower '▄'.
pub fn draw_scope_box(view: &mut View, top: usize, left: usize, width: usize, points: &[i8]) {
    if points.is_empty() || width == 0 {
        return;
    }
    for j in 0..width {
        let idx = j * points.len() / width;
        let val = points[idx] as i32;
        let y = (3 + val * 6 / 64).clamp(0, 5) as usize;
        let row = y / 2;
        let ch = if y % 2 == 0 { '▀' } else { '▄' };
        let dist = (y as i32 - 3).unsigned_abs() as usize;
        let color = viscolor::SCOPE_COLORS[dist.min(viscolor::SCOPE_COLORS.len() - 1)];
        view.set(top + row, left + j, Cell::new(ch).fg(color));
    }
}
```

- [ ] **Step 4: Run the widget test suite**

Run: `cd /home/vlb2bp/git/cluu/userspace/cluuamp && cargo test widgets 2>&1 | tail -3`
Expected: ALL PASS (new + pre-existing widget tests).

- [ ] **Step 5: Commit**

```bash
cd /home/vlb2bp/git/cluu
git add userspace/cluuamp/src/widgets.rs
git commit -m "feat(cluuamp): eighth-block vis widgets — spectrum column, EQ slider, scope box"
```

---

### Task 4: Layout rewrite + FocusArea relocation (layout.rs, model.rs)

**Files:**
- Rewrite: `userspace/cluuamp/src/layout.rs` (full replacement below)
- Modify: `userspace/cluuamp/src/model.rs` (mechanical: FocusArea moves out; temporary `.next()` shim keeps nothing — model catches up in Task 6)
- Tests: `layout.rs` `mod tests`

**Interfaces:**
- Produces:
  - `Layout::calculate(width, height, show_eq: bool, show_playlist: bool) -> Layout` (SIGNATURE CHANGE — old 2-arg form is gone)
  - `pub enum FocusArea` now lives in `layout.rs`; `FocusArea::next(self, show_eq: bool, show_playlist: bool) -> FocusArea` skips hidden windows
  - Layout fields exactly as in the struct below (Tasks 6–7 use these names verbatim)
- NOTE: after this task `cargo xtask build` FAILS (model.rs/view.rs still use the old API). That is expected per Global Constraints; host `cargo test` must be green.

- [ ] **Step 1: Replace `userspace/cluuamp/src/layout.rs` entirely with:**

```rust
//! Three-window Winamp layout (spec §1/§8): MAIN (rows 0-6, fixed),
//! EQUALIZER (5 rows, toggleable), PLAYLIST (rest, toggleable),
//! footer on the last row. Positions are for width >= 76, height >= 25.

/// Focus areas cycle with Tab; hidden windows are skipped.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FocusArea {
    Transport,
    Volume,
    Balance,
    Position,
    Eq,
    Playlist,
}

impl FocusArea {
    /// Next focus area, skipping Eq/Playlist when their window is hidden.
    pub fn next(self, show_eq: bool, show_playlist: bool) -> Self {
        let mut f = self;
        loop {
            f = match f {
                FocusArea::Transport => FocusArea::Volume,
                FocusArea::Volume => FocusArea::Balance,
                FocusArea::Balance => FocusArea::Position,
                FocusArea::Position => FocusArea::Eq,
                FocusArea::Eq => FocusArea::Playlist,
                FocusArea::Playlist => FocusArea::Transport,
            };
            let hidden = (f == FocusArea::Eq && !show_eq)
                || (f == FocusArea::Playlist && !show_playlist);
            if !hidden {
                return f;
            }
        }
    }
}

pub struct Layout {
    pub width: usize,
    pub height: usize,
    pub show_eq: bool,
    pub show_playlist: bool,
    // MAIN window (rows 0-6, fixed)
    pub main_title_row: usize, // 0
    pub time_top: usize,       // 1 (3 rows tall)
    pub time_col: usize,       // 3 (20 cols wide)
    pub state_glyph_row: usize, // 2
    pub state_glyph_col: usize, // 1
    pub vis_top: usize,        // 1
    pub vis_left: usize,       // 24
    pub vis_width: usize,      // 24
    pub vis_height: usize,     // 3
    pub marquee_row: usize,    // 1
    pub marquee_col: usize,    // 49
    pub marquee_width: usize,  // width - 50
    pub info_row: usize,       // 2 (kbps/khz)
    pub stereo_row: usize,     // 3 (mono/STEREO)
    pub sliders_row: usize,    // 4
    pub position_row: usize,   // 5
    pub transport_row: usize,  // 6
    // EQUALIZER window (5 rows; fields valid only when show_eq)
    pub eq_title_row: usize,
    pub eq_buttons_row: usize,
    pub eq_slider_top: usize, // 2 rows tall
    pub eq_labels_row: usize,
    pub eq_band_x: [usize; 11],
    // PLAYLIST window (fields valid only when show_playlist)
    pub pl_title_row: usize,
    pub playlist_top: usize,
    pub playlist_height: usize,
    pub pl_buttons_row: usize, // height - 2
    // always
    pub footer_row: usize, // height - 1
    pub scrollbar_col: usize, // width - 1
}

impl Layout {
    pub fn calculate(width: usize, height: usize, show_eq: bool, show_playlist: bool) -> Self {
        let mut eq_band_x = [0usize; 11];
        for i in 0..11 {
            eq_band_x[i] = 3 + i * 7;
        }
        let mut next_row = 7; // first row after MAIN
        let (eq_title_row, eq_buttons_row, eq_slider_top, eq_labels_row) = if show_eq {
            let t = next_row;
            next_row += 5;
            (t, t + 1, t + 2, t + 4)
        } else {
            (0, 0, 0, 0)
        };
        let pl_buttons_row = height.saturating_sub(2);
        let (pl_title_row, playlist_top, playlist_height) = if show_playlist {
            let t = next_row;
            let top = t + 1;
            (t, top, pl_buttons_row.saturating_sub(top))
        } else {
            (0, 0, 0)
        };
        Layout {
            width,
            height,
            show_eq,
            show_playlist,
            main_title_row: 0,
            time_top: 1,
            time_col: 3,
            state_glyph_row: 2,
            state_glyph_col: 1,
            vis_top: 1,
            vis_left: 24,
            vis_width: 24,
            vis_height: 3,
            marquee_row: 1,
            marquee_col: 49,
            marquee_width: width.saturating_sub(50),
            info_row: 2,
            stereo_row: 3,
            sliders_row: 4,
            position_row: 5,
            transport_row: 6,
            eq_title_row,
            eq_buttons_row,
            eq_slider_top,
            eq_labels_row,
            eq_band_x,
            pl_title_row,
            playlist_top,
            playlist_height,
            pl_buttons_row,
            footer_row: height.saturating_sub(1),
            scrollbar_col: width.saturating_sub(1),
        }
    }

    pub fn min_width() -> usize {
        76
    }

    pub fn min_height() -> usize {
        25
    }

    pub fn fits(&self) -> bool {
        self.width >= Self::min_width()
            && self.height >= Self::min_height()
            && (!self.show_playlist || self.playlist_height >= 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_visible_80x25() {
        let l = Layout::calculate(80, 25, true, true);
        assert!(l.fits());
        assert_eq!(l.main_title_row, 0);
        assert_eq!(l.time_top, 1);
        assert_eq!(l.time_col, 3);
        assert_eq!(l.vis_top, 1);
        assert_eq!(l.vis_left, 24);
        assert_eq!(l.vis_width, 24);
        assert_eq!(l.vis_height, 3);
        assert_eq!(l.marquee_row, 1);
        assert_eq!(l.marquee_col, 49);
        assert_eq!(l.marquee_width, 30);
        assert_eq!(l.info_row, 2);
        assert_eq!(l.stereo_row, 3);
        assert_eq!(l.sliders_row, 4);
        assert_eq!(l.position_row, 5);
        assert_eq!(l.transport_row, 6);
        assert_eq!(l.eq_title_row, 7);
        assert_eq!(l.eq_buttons_row, 8);
        assert_eq!(l.eq_slider_top, 9);
        assert_eq!(l.eq_labels_row, 11);
        assert_eq!(l.pl_title_row, 12);
        assert_eq!(l.playlist_top, 13);
        assert_eq!(l.pl_buttons_row, 23);
        assert_eq!(l.playlist_height, 10);
        assert_eq!(l.footer_row, 24);
        assert_eq!(l.scrollbar_col, 79);
    }

    #[test]
    fn eq_hidden_playlist_slides_up() {
        let l = Layout::calculate(80, 25, false, true);
        assert!(l.fits());
        assert_eq!(l.pl_title_row, 7);
        assert_eq!(l.playlist_top, 8);
        assert_eq!(l.playlist_height, 15);
        assert_eq!(l.pl_buttons_row, 23);
    }

    #[test]
    fn playlist_hidden() {
        let l = Layout::calculate(80, 25, true, false);
        assert!(l.fits());
        assert_eq!(l.eq_title_row, 7);
        assert_eq!(l.playlist_height, 0);
    }

    #[test]
    fn both_hidden() {
        let l = Layout::calculate(80, 25, false, false);
        assert!(l.fits());
        assert_eq!(l.footer_row, 24);
    }

    #[test]
    fn eq_band_positions() {
        let l = Layout::calculate(80, 25, true, true);
        assert_eq!(l.eq_band_x[0], 3);
        assert_eq!(l.eq_band_x[1], 10);
        assert_eq!(l.eq_band_x[10], 73);
    }

    #[test]
    fn min_size_fits_76x25() {
        let l = Layout::calculate(76, 25, true, true);
        assert!(l.fits());
    }

    #[test]
    fn too_narrow_75_does_not_fit() {
        let l = Layout::calculate(75, 25, true, true);
        assert!(!l.fits());
    }

    #[test]
    fn too_short_24_does_not_fit() {
        let l = Layout::calculate(80, 24, true, true);
        assert!(!l.fits());
    }

    #[test]
    fn zero_size_does_not_panic() {
        let l = Layout::calculate(0, 0, true, true);
        assert!(!l.fits());
    }

    #[test]
    fn focus_next_full_cycle() {
        let f = FocusArea::Transport;
        assert!(f.next(true, true) == FocusArea::Volume);
        assert!(FocusArea::Position.next(true, true) == FocusArea::Eq);
        assert!(FocusArea::Eq.next(true, true) == FocusArea::Playlist);
        assert!(FocusArea::Playlist.next(true, true) == FocusArea::Transport);
    }

    #[test]
    fn focus_next_skips_hidden_eq() {
        assert!(FocusArea::Position.next(false, true) == FocusArea::Playlist);
    }

    #[test]
    fn focus_next_skips_hidden_playlist() {
        assert!(FocusArea::Eq.next(true, false) == FocusArea::Transport);
    }

    #[test]
    fn focus_next_skips_both_hidden() {
        assert!(FocusArea::Position.next(false, false) == FocusArea::Transport);
    }
}
```

- [ ] **Step 2: Remove the old FocusArea from model.rs**

In `userspace/cluuamp/src/model.rs`: delete the entire `pub enum FocusArea { ... }` block AND its `impl FocusArea { pub fn next ... }` block (lines ~31–52), and add near the other `use crate::` imports:

```rust
pub use crate::layout::FocusArea;
```

(`view.rs` imports `crate::model::FocusArea` — the re-export keeps that path alive. model.rs's `self.focus.next()` call is now a compile error FOR THE TARGET ONLY; host tests don't build model.rs. Task 6 fixes it.)

- [ ] **Step 3: Run host tests**

Run: `cd /home/vlb2bp/git/cluu/userspace/cluuamp && cargo test 2>&1 | grep -E "test result|error"`
Expected: layout + widgets + fft + scope + viscolor suites ALL PASS, zero compile errors (host build excludes model/view/audio).

- [ ] **Step 4: Commit**

```bash
cd /home/vlb2bp/git/cluu
git add userspace/cluuamp/src/layout.rs userspace/cluuamp/src/model.rs
git commit -m "feat(cluuamp): three-window layout calculator + FocusArea window-skip

Target build intentionally broken until model/view land (next commits)."
```

---

### Task 5: Audio engine getters (audio.rs)

**Files:**
- Modify: `userspace/cluuamp/src/audio.rs`

**Interfaces:**
- Produces (Task 6/7 rely on these exact signatures):
  - `pub fn sample_rate(&self) -> u32`
  - `pub fn bitrate_kbps(&self) -> u32` — 0 until a track is probed
  - `pub fn remove_track(&mut self, idx: usize)`
- `probe_format` return type changes to `Result<(u32, u8, u32)>` (rate, channels, bitrate) — internal only.

No host tests possible (`audio` is runtime-gated). Verified by `cargo xtask build` at end of Task 6 and by opus in Task 9.

- [ ] **Step 1: Add the bitrate field**

In `struct AudioEngine` (after `channels: u8,`):

```rust
    bitrate_kbps: u32,
```

In `AudioEngine::new` (after `channels: 2,`):

```rust
            bitrate_kbps: 0,
```

- [ ] **Step 2: Extend probe_format**

Replace the whole `probe_format` function with:

```rust
    fn probe_format(&mut self, data: &[u8]) -> Result<(u32, u8, u32)> {
        let mut decoder = Decoder::new();
        let mut pcm = [0f32; MAX_SAMPLES_PER_FRAME];
        let mut pos = 0;
        for _ in 0..200 {
            if pos >= data.len() {
                break;
            }
            let (consumed, info) = decoder.decode(&data[pos..], &mut pcm);
            if consumed > 0 {
                pos += consumed;
            }
            if let Some(fi) = info {
                return Ok((fi.sample_rate, fi.channels.num(), fi.bitrate));
            }
        }
        Err(Error::InvalidState)
    }
```

In `load_current`, replace:

```rust
        let (rate, channels) = self.probe_format(&mp3_data)?;
        self.sample_rate = rate;
        self.channels = channels;
```

with:

```rust
        let (rate, channels, bitrate) = self.probe_format(&mp3_data)?;
        self.sample_rate = rate;
        self.channels = channels;
        self.bitrate_kbps = bitrate;
```

- [ ] **Step 3: Add the public getters and remove_track**

After `pub fn channels(&self) -> u8 { ... }` add:

```rust
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Current track's MP3 bitrate in kbps; 0 before the first probe.
    pub fn bitrate_kbps(&self) -> u32 {
        self.bitrate_kbps
    }

    /// Remove a playlist entry. Removing the playing/current track stops
    /// playback and closes the audio session; indices behind the removal
    /// shift down. current_index clamps to the new playlist end.
    pub fn remove_track(&mut self, idx: usize) {
        if idx >= self.playlist.len() {
            return;
        }
        self.playlist.remove(idx);
        if self.playlist.is_empty() {
            self.stop();
            self.close_audio();
            self.current_index = 0;
            return;
        }
        if idx == self.current_index {
            self.stop();
            self.close_audio();
            if self.current_index >= self.playlist.len() {
                self.current_index = self.playlist.len() - 1;
            }
        } else if idx < self.current_index {
            self.current_index -= 1;
        }
    }
```

- [ ] **Step 4: Syntax sanity check on host**

Run: `cd /home/vlb2bp/git/cluu/userspace/cluuamp && cargo test 2>&1 | tail -3`
Expected: still ALL PASS (audio.rs not compiled on host; this catches accidental damage to shared files only).

- [ ] **Step 5: Commit**

```bash
cd /home/vlb2bp/git/cluu
git add userspace/cluuamp/src/audio.rs
git commit -m "feat(cluuamp): audio getters — sample_rate, bitrate_kbps, remove_track"
```

---

### Task 6: Model update (model.rs)

**Files:**
- Modify: `userspace/cluuamp/src/model.rs`

**Interfaces:**
- Consumes: `Layout::calculate(w, h, show_eq, show_playlist)`, `FocusArea::next(show_eq, show_playlist)` (Task 4), `audio.remove_track(idx)` (Task 5).
- Produces (view relies on): `model.show_eq`, `model.show_playlist` (bools), `model.transport_selected` in 0..=7 with mapping 0=prev 1=play 2=pause 3=stop 4=next 5=eject 6=shuf 7=rep, default 1.

- [ ] **Step 1: Add window-visibility state**

In `struct CluuampModel` (after `pub eq_enabled: bool,`):

```rust
    pub show_eq: bool,
    pub show_playlist: bool,
```

In `CluuampModel::new` (after `eq_enabled: false,`):

```rust
            show_eq: true,
            show_playlist: true,
```

Change `transport_selected: 2,` to `transport_selected: 1,`.

Change the `layout:` initializer to:

```rust
            layout: Layout::calculate(width, height, true, true),
```

- [ ] **Step 2: Recalculate layout on resize/toggle**

Replace `on_resize` with:

```rust
    pub fn on_resize(&mut self, width: usize, height: usize) {
        self.layout = Layout::calculate(width, height, self.show_eq, self.show_playlist);
    }

    fn recalc_layout(&mut self) {
        self.layout = Layout::calculate(
            self.layout.width,
            self.layout.height,
            self.show_eq,
            self.show_playlist,
        );
    }

    /// If focus sits on a window that was just hidden, move it home.
    fn fix_focus_after_toggle(&mut self) {
        if (self.focus == FocusArea::Eq && !self.show_eq)
            || (self.focus == FocusArea::Playlist && !self.show_playlist)
        {
            self.focus = FocusArea::Transport;
        }
    }
```

- [ ] **Step 3: Marquee width from layout**

In `tick()`, replace:

```rust
        let marquee_width = self.layout.width.saturating_sub(12);
```

with:

```rust
        let marquee_width = self.layout.marquee_width;
```

- [ ] **Step 4: Key handling**

In `handle_key`, replace the `KeyEvent::Char('e')` arm with:

```rust
            KeyEvent::Char('e') => {
                self.show_eq = !self.show_eq;
                self.recalc_layout();
                self.fix_focus_after_toggle();
            }
            KeyEvent::Char('E') => {
                self.eq_enabled = !self.eq_enabled;
            }
            KeyEvent::Char('p') => {
                self.show_playlist = !self.show_playlist;
                self.recalc_layout();
                self.fix_focus_after_toggle();
            }
            KeyEvent::Char('r') => {
                self.audio.remove_track(self.playlist_selected);
                let len = self.audio.playlist().len();
                if len == 0 {
                    self.playlist_selected = 0;
                } else if self.playlist_selected >= len {
                    self.playlist_selected = len - 1;
                }
            }
```

Replace the `KeyEvent::Tab` arm with:

```rust
            KeyEvent::Tab => {
                self.focus = self.focus.next(self.show_eq, self.show_playlist);
            }
```

- [ ] **Step 5: Transport remap (8 buttons: 0=prev 1=play 2=pause 3=stop 4=next 5=eject 6=shuf 7=rep)**

In `handle_right`, change the bound `if self.transport_selected < 5` to `if self.transport_selected < 7`.

In `handle_enter`, replace the `FocusArea::Transport` arm with:

```rust
            FocusArea::Transport => {
                match self.transport_selected {
                    0 => {
                        let _ = self.audio.prev();
                    }
                    1 => {
                        let _ = self.audio.play();
                    }
                    2 => self.audio.pause(),
                    3 => self.audio.stop(),
                    4 => {
                        let _ = self.audio.next();
                    }
                    5 => self.open_browser("/host"),
                    6 => {
                        self.shuffle = !self.shuffle;
                    }
                    7 => {
                        self.repeat = !self.repeat;
                    }
                    _ => {}
                }
            }
```

- [ ] **Step 6: Verify host tests still green**

Run: `cd /home/vlb2bp/git/cluu/userspace/cluuamp && cargo test 2>&1 | tail -3`
Expected: ALL PASS.

(Target build is STILL broken — view.rs uses old layout fields. Task 7 restores it. Do not run `cargo xtask build` yet.)

- [ ] **Step 7: Commit**

```bash
cd /home/vlb2bp/git/cluu
git add userspace/cluuamp/src/model.rs
git commit -m "feat(cluuamp): window toggles e/p, EQ-enable on E, remove-track r, 8-button transport"
```

---

### Task 7: View rewrite (view.rs) + old widget removal

**Files:**
- Rewrite: `userspace/cluuamp/src/view.rs` (full replacement below)
- Modify: `userspace/cluuamp/src/widgets.rs` — DELETE `draw_spectrum_bar`, `draw_scope`, and their 5 tests (`draw_spectrum_bar` calls at test lines ~454–482 and the scope-widget tests that call `draw_scope`); DELETE `draw_v_slider` and its tests IF no other caller remains (check with grep — view.rs was the only caller).
- Modify: `userspace/cluuamp/src/main.rs` — NO changes expected; listed for the final build check only.

**Interfaces:**
- Consumes: everything produced by Tasks 2–6 (exact names as specified there).
- Produces: `pub fn render(model: &CluuampModel) -> View` (unchanged signature, called by main.rs).

- [ ] **Step 1: Replace `userspace/cluuamp/src/view.rs` entirely with:**

```rust
//! View rendering: classic Winamp three-window layout (spec §2-§5).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libtui::{Cell, View, ATTR_BOLD, ATTR_REVERSE};

use crate::audio::PlaybackState;
use crate::model::{CluuampModel, FocusArea, VisMode};
use crate::widgets;

pub fn render(model: &CluuampModel) -> View {
    let layout = &model.layout;
    let mut view = View::new(layout.width, layout.height);
    if !layout.fits() {
        draw_too_small(&mut view, layout.width, layout.height);
        return view;
    }
    draw_main_window(&mut view, model);
    if layout.show_eq {
        draw_eq_window(&mut view, model);
    }
    if layout.show_playlist {
        draw_playlist_window(&mut view, model);
    }
    draw_footer(&mut view, model);
    if model.browser.is_some() {
        draw_browser_overlay(&mut view, model);
    }
    view
}

fn draw_too_small(view: &mut View, w: usize, h: usize) {
    let msg = "Terminal too small. Need at least 76x25.";
    let col = w.saturating_sub(msg.len()) / 2;
    let row = h / 2;
    for (i, ch) in msg.chars().enumerate() {
        view.set(row, col + i, Cell::new(ch).fg(196).attrs(ATTR_BOLD));
    }
}

/// Winamp-style titlebar: full row bg 236, '─' fill fg 240, centered title.
fn draw_titlebar(view: &mut View, row: usize, w: usize, title: &str, title_fg: u8) {
    for i in 0..w {
        view.set(row, i, Cell::new('─').fg(240).bg(236));
    }
    let tlen = title.chars().count();
    let start = w.saturating_sub(tlen) / 2;
    for (i, ch) in title.chars().enumerate() {
        view.set(row, start + i, Cell::new(ch).fg(title_fg).bg(236).attrs(ATTR_BOLD));
    }
}

fn draw_main_window(view: &mut View, model: &CluuampModel) {
    let layout = &model.layout;
    let w = layout.width;

    // Row 0: titlebar + state label right (ends 2 cols before edge)
    draw_titlebar(view, layout.main_title_row, w, " CLUUAMP ", 46);
    let (state_label, state_fg) = match model.audio.state() {
        PlaybackState::Playing => (" PLAYING ", 46u8),
        PlaybackState::Paused => (" PAUSED ", 226),
        PlaybackState::Stopped => (" STOPPED ", 244),
    };
    let state_col = w.saturating_sub(state_label.len() + 2);
    for (i, ch) in state_label.chars().enumerate() {
        view.set(layout.main_title_row, state_col + i, Cell::new(ch).fg(state_fg).bg(236));
    }

    // State glyph (row 2, col 1)
    let (glyph, glyph_fg) = match model.audio.state() {
        PlaybackState::Playing => ('▶', 46u8),
        PlaybackState::Paused => ('║', 226),
        PlaybackState::Stopped => ('■', 244),
    };
    view.set(layout.state_glyph_row, layout.state_glyph_col, Cell::new(glyph).fg(glyph_fg));

    // Block-digit time: remaining (negative) while playing/paused,
    // elapsed (positive) when stopped.
    let pos = model.audio.position_ms();
    let dur = model.audio.duration_ms();
    let (negative, shown_ms) = match model.audio.state() {
        PlaybackState::Stopped => (false, pos),
        _ => (true, dur.saturating_sub(pos)),
    };
    let mins = shown_ms / 60000;
    let secs = (shown_ms / 1000) % 60;
    widgets::draw_block_time(view, layout.time_top, layout.time_col, negative, mins, secs, 46);

    // Vis box 24x3
    draw_visualizer(view, model);

    // Marquee (row 1, right block)
    widgets::draw_marquee(
        view,
        layout.marquee_row,
        layout.marquee_col,
        layout.marquee_width,
        model.audio.current_title(),
        model.title_scroll_offset,
    );

    // Row 2 right: kbps / khz
    let kbps = model.audio.bitrate_kbps();
    let khz = model.audio.sample_rate() / 1000;
    let kbps_str = if kbps == 0 { String::from("---") } else { format!("{:>3}", kbps) };
    let khz_str = if khz == 0 { String::from("--") } else { format!("{:>2}", khz) };
    let info = format!("{} kbps  {} khz", kbps_str, khz_str);
    for (i, ch) in info.chars().enumerate() {
        if layout.marquee_col + i < w {
            view.set(layout.info_row, layout.marquee_col + i, Cell::new(ch).fg(244));
        }
    }

    // Row 3 right: mono / STEREO
    let stereo = model.audio.channels() >= 2;
    let mono_fg = if stereo { 240 } else { 46 };
    let stereo_fg = if stereo { 46 } else { 240 };
    let mono_attrs = if stereo { 0 } else { ATTR_BOLD };
    let stereo_attrs = if stereo { ATTR_BOLD } else { 0 };
    for (i, ch) in "mono".chars().enumerate() {
        view.set(layout.stereo_row, layout.marquee_col + i, Cell::new(ch).fg(mono_fg).attrs(mono_attrs));
    }
    for (i, ch) in "STEREO".chars().enumerate() {
        view.set(layout.stereo_row, layout.marquee_col + 6 + i, Cell::new(ch).fg(stereo_fg).attrs(stereo_attrs));
    }

    // Row 4: volume + balance + [EQ] [PL]
    let row = layout.sliders_row;
    view.set(row, 1, Cell::new('V').fg(244));
    widgets::draw_h_slider(view, row, 3, 20, model.audio.volume(), 100, model.focus == FocusArea::Volume);
    view.set(row, 25, Cell::new('B').fg(244));
    let bal_val = (model.audio.balance() + 50) as u8;
    widgets::draw_h_slider(view, row, 27, 14, bal_val, 100, model.focus == FocusArea::Balance);
    let eq_fg = if model.show_eq { 46 } else { 240 };
    for (i, ch) in "[EQ]".chars().enumerate() {
        view.set(row, 44 + i, Cell::new(ch).fg(eq_fg).attrs(ATTR_BOLD));
    }
    let pl_fg = if model.show_playlist { 46 } else { 240 };
    for (i, ch) in "[PL]".chars().enumerate() {
        view.set(row, 49 + i, Cell::new(ch).fg(pl_fg).attrs(ATTR_BOLD));
    }

    // Row 5: seekbar
    let dur_nz = model.audio.duration_ms().max(1);
    let pos_pct = ((model.audio.position_ms() as u32 * 255) / dur_nz as u32).min(255) as u8;
    widgets::draw_h_slider(
        view,
        layout.position_row,
        1,
        w.saturating_sub(2),
        pos_pct,
        255,
        model.focus == FocusArea::Position,
    );

    // Row 6: transport (0=prev 1=play 2=pause 3=stop 4=next 5=eject 6=shuf 7=rep)
    let row = layout.transport_row;
    let state = model.audio.state();
    let buttons: [(&str, bool); 6] = [
        ("|<", false),
        (">", state == PlaybackState::Playing),
        ("||", state == PlaybackState::Paused),
        ("[]", state == PlaybackState::Stopped),
        (">|", false),
        ("^", false),
    ];
    let mut col = 1;
    for (i, (label, active)) in buttons.iter().enumerate() {
        let focused = model.focus == FocusArea::Transport && model.transport_selected == i;
        widgets::draw_button(view, row, col, label, *active, focused);
        col += label.len() + 3;
    }
    col += 3;
    let shuf_focused = model.focus == FocusArea::Transport && model.transport_selected == 6;
    widgets::draw_button(view, row, col, "SHUF", model.shuffle, shuf_focused);
    col += "SHUF".len() + 3;
    let rep_focused = model.focus == FocusArea::Transport && model.transport_selected == 7;
    widgets::draw_button(view, row, col, "REP", model.repeat, rep_focused);
}

fn draw_visualizer(view: &mut View, model: &CluuampModel) {
    let layout = &model.layout;
    match model.vis_mode {
        VisMode::Spectrum => {
            for j in 0..layout.vis_width {
                let bar_idx = j * 75 / layout.vis_width;
                widgets::draw_spectrum_column(
                    view,
                    layout.vis_top,
                    layout.vis_left + j,
                    model.fft.bar_height(bar_idx),
                    model.fft.peak_height(bar_idx),
                );
            }
        }
        VisMode::Oscilloscope => {
            let mut points = [0i8; 75];
            for i in 0..75 {
                points[i] = model.scope.point(i);
            }
            widgets::draw_scope_box(view, layout.vis_top, layout.vis_left, layout.vis_width, &points);
        }
    }
}

fn draw_eq_window(view: &mut View, model: &CluuampModel) {
    let layout = &model.layout;
    let w = layout.width;
    draw_titlebar(view, layout.eq_title_row, w, " EQUALIZER ", 252);

    // Buttons row: [ON] [AUTO] ... curve (cols 20-41) ... [PRESETS]
    let row = layout.eq_buttons_row;
    let on_fg = if model.eq_enabled { 46 } else { 240 };
    for (i, ch) in "[ON]".chars().enumerate() {
        view.set(row, 2 + i, Cell::new(ch).fg(on_fg).attrs(ATTR_BOLD));
    }
    for (i, ch) in "[AUTO]".chars().enumerate() {
        view.set(row, 7 + i, Cell::new(ch).fg(240));
    }
    for (b, &val) in model.eq_bands.iter().enumerate() {
        let ch = widgets::curve_char(val);
        view.set(row, 20 + b * 2, Cell::new(ch).fg(51));
        view.set(row, 20 + b * 2 + 1, Cell::new(ch).fg(51));
    }
    let presets = "[PRESETS]";
    let pcol = w.saturating_sub(presets.len() + 3);
    for (i, ch) in presets.chars().enumerate() {
        view.set(row, pcol + i, Cell::new(ch).fg(240));
    }

    // Sliders (2 rows) + labels
    let freqs = ["pre", "60", "170", "310", "600", "1k", "3k", "6k", "12k", "14k", "16k"];
    for i in 0..11 {
        let x = layout.eq_band_x[i];
        let focused = model.focus == FocusArea::Eq && model.eq_selected == i;
        widgets::draw_eq_slider(view, layout.eq_slider_top, x, model.eq_bands[i], focused);
        let label = freqs[i];
        let start = x.saturating_sub(label.len() / 2);
        for (j, ch) in label.chars().enumerate() {
            if start + j < w {
                view.set(layout.eq_labels_row, start + j, Cell::new(ch).fg(244));
            }
        }
    }
}

fn draw_playlist_window(view: &mut View, model: &CluuampModel) {
    let layout = &model.layout;
    let w = layout.width;
    let pl = model.audio.playlist();
    draw_titlebar(
        view,
        layout.pl_title_row,
        w,
        &format!(" PLAYLIST ({} tracks) ", pl.len()),
        252,
    );

    let scroll = model.playlist_scroll;
    let selected = model.playlist_selected;
    let current = model.audio.current_index();

    for row_idx in 0..layout.playlist_height {
        let track_idx = scroll + row_idx;
        let row = layout.playlist_top + row_idx;
        if track_idx >= pl.len() {
            continue;
        }
        let is_current = track_idx == current;
        let is_selected = track_idx == selected;
        let fg = if is_current { 46 } else if is_selected { 255 } else { 252 };
        let attrs = if is_selected { ATTR_REVERSE } else { 0 };
        // track number, right-aligned in cols 1-2, '.' at col 3
        let num = format!("{:>2}", (track_idx + 1).min(99));
        for (i, ch) in num.chars().enumerate() {
            view.set(row, 1 + i, Cell::new(ch).fg(if is_current || is_selected { fg } else { 238 }).attrs(attrs));
        }
        view.set(row, 3, Cell::new('.').fg(238).attrs(attrs));
        // current marker at col 5
        if is_current {
            view.set(row, 5, Cell::new('▶').fg(46).attrs(attrs));
        }
        // name cols 7 .. w-10
        let max_name = w.saturating_sub(17); // 7 + name + gap + duration(5) + margin
        let name: String = pl[track_idx]
            .rsplit('/')
            .next()
            .unwrap_or(&pl[track_idx])
            .chars()
            .take(max_name)
            .collect();
        for (i, ch) in name.chars().enumerate() {
            view.set(row, 7 + i, Cell::new(ch).fg(fg).attrs(attrs));
        }
        // duration (current track only) cols w-7 .. w-3
        if is_current {
            let dur = model.audio.duration_ms();
            let dstr = format!("{}:{:02}", dur / 60000, (dur / 1000) % 60);
            let dcol = w.saturating_sub(7);
            for (i, ch) in dstr.chars().enumerate() {
                if dcol + i <= w.saturating_sub(3) {
                    view.set(row, dcol + i, Cell::new(ch).fg(244).attrs(attrs));
                }
            }
        }
    }

    if !pl.is_empty() && layout.playlist_height > 0 && pl.len() > layout.playlist_height {
        widgets::draw_scrollbar(
            view,
            layout.playlist_top,
            layout.scrollbar_col,
            layout.playlist_height,
            pl.len(),
            layout.playlist_height,
            scroll,
        );
    }

    // Bottom bar
    let row = layout.pl_buttons_row;
    let mut col = 1;
    for (label, fg) in [("[ADD]", 252u8), ("[REM]", 252), ("[SEL]", 240), ("[MISC]", 240)] {
        for (i, ch) in label.chars().enumerate() {
            view.set(row, col + i, Cell::new(ch).fg(fg));
        }
        col += label.len() + 1;
    }
    let pos = model.audio.position_ms();
    let dur = model.audio.duration_ms();
    let times = format!(
        "{}:{:02}/{}:{:02}",
        pos / 60000,
        (pos / 1000) % 60,
        dur / 60000,
        (dur / 1000) % 60
    );
    let list_label = "[LIST]";
    let list_col = w.saturating_sub(list_label.len() + 2);
    let times_col = list_col.saturating_sub(times.len() + 1);
    for (i, ch) in times.chars().enumerate() {
        view.set(row, times_col + i, Cell::new(ch).fg(244));
    }
    for (i, ch) in list_label.chars().enumerate() {
        view.set(row, list_col + i, Cell::new(ch).fg(240));
    }
}

fn draw_footer(view: &mut View, model: &CluuampModel) {
    let row = model.layout.footer_row;
    let w = model.layout.width;
    let focus_label = match model.focus {
        FocusArea::Transport => "Transport",
        FocusArea::Volume => "Volume",
        FocusArea::Balance => "Balance",
        FocusArea::Position => "Position",
        FocusArea::Eq => "EQ",
        FocusArea::Playlist => "Playlist",
    };
    let help = format!(
        " Tab:focus[{focus_label}] Space:play e/p:windows E:eq r:rem v:vis n/b:track o:open q:quit "
    );
    let help_chars: Vec<char> = help.chars().collect();
    for i in 0..w.min(help_chars.len()) {
        view.set(row, i, Cell::new(help_chars[i]).fg(238));
    }
}

fn draw_browser_overlay(view: &mut View, model: &CluuampModel) {
    let w = model.layout.width;
    let h = model.layout.height;

    for r in 0..h {
        for c in 0..w {
            if r < view.height && c < view.width {
                let cell = view.get(r, c).unwrap();
                view.set(r, c, Cell::new(cell.ch).fg(cell.fg).bg(236));
            }
        }
    }

    let box_w = (w * 4 / 5).min(80).max(40);
    let box_h = (h * 4 / 5).min(24).max(10);
    let col = w.saturating_sub(box_w) / 2;
    let row = h.saturating_sub(box_h) / 2;

    for r in row..row + box_h {
        for c in col..col + box_w {
            if r < view.height && c < view.width {
                view.set(r, c, Cell::new(' ').fg(252).bg(234));
            }
        }
    }

    if let Some(browser) = &model.browser {
        browser.render(row, col, box_w, box_h, view);
    }
}
```

- [ ] **Step 2: Delete dead widgets**

In `userspace/cluuamp/src/widgets.rs`:
1. `grep -rn "draw_spectrum_bar\|draw_scope\b\|draw_v_slider" /home/vlb2bp/git/cluu/userspace/cluuamp/src/` — confirm the only non-widgets.rs references are gone (view.rs no longer calls them).
2. Delete `pub fn draw_spectrum_bar(...)`, `pub fn draw_scope(...)`, `pub fn draw_v_slider(...)` and every test that calls them.
3. If `COLOR_DEFAULT` or other imports become unused, remove them from the `use` line (compiler warnings will name them).

- [ ] **Step 3: Host tests**

Run: `cd /home/vlb2bp/git/cluu/userspace/cluuamp && cargo test 2>&1 | tail -3`
Expected: ALL PASS, no warnings about unused widget imports.

- [ ] **Step 4: Full target build — first green build since Task 4**

Run: `cd /home/vlb2bp/git/cluu && cargo xtask build 2>&1 | tail -5`
Expected: build completes with no errors. If view.rs or model.rs fail to compile, fix ONLY signature/typo mismatches against this plan — do not redesign.

- [ ] **Step 5: Commit**

```bash
cd /home/vlb2bp/git/cluu
git add userspace/cluuamp/src/view.rs userspace/cluuamp/src/widgets.rs
git commit -m "feat(cluuamp): classic Winamp three-window view — block time, 24x3 vis, EQ+PL windows"
```

---

### Task 8: Docs chapter (doc/book/cluuamp.md)

**Files:**
- Create: `doc/book/cluuamp.md`

- [ ] **Step 1: Write the chapter**

Create `doc/book/cluuamp.md` with this content (adjust nothing structurally; fill the mockup verbatim from the spec):

```markdown
# CLUUamp

CLUUamp is the Winamp-classic-styled TUI audio player. MP3 decode via
nanomp3, playback through the virtio-snd audio session, visualization from
a PCM tap (512-point Hann FFT, Winamp semitone band mapping).

## Layout

Three stacked windows on one screen (min 76x25):

- **MAIN** (always visible): block-digit time, 24x3 spectrum/scope box,
  title marquee, kbps/khz, mono/STEREO, volume/balance sliders, seekbar,
  transport.
- **EQUALIZER** (toggle `e`): ON/AUTO, curve strip, preamp + 10 band
  sliders (60 Hz - 16 kHz).
- **PLAYLIST** (toggle `p`): track list with current-track marker and
  bottom button bar.

## Keys

| Key | Action |
|-----|--------|
| Space | play/pause |
| s | stop |
| n / b | next / previous track |
| v | spectrum <-> oscilloscope |
| e / p | toggle EQUALIZER / PLAYLIST window |
| E | EQ DSP on/off |
| r | remove selected playlist entry |
| o | open file browser (add tracks) |
| Tab | cycle focus (skips hidden windows) |
| arrows / Enter | operate focused control |
| q / Esc | quit |

## Architecture

`userspace/cluuamp/src/`: `fft.rs` + `scope.rs` + `viscolor.rs` (pure
Winamp-ported vis pipeline), `layout.rs` (three-window cell map),
`widgets.rs` (block digits, eighth-block sliders/columns), `model.rs`
(state + keys), `view.rs` (cell rendering), `audio.rs` (decode + playback).
Pure modules are host-tested (`cargo test`); runtime modules build only
for the CLUU target.

Spectrum scaling: band values are 0-255 (Winamp sadata); pixel height is
`value >> 4` (0-15) — see spec
`docs/superpowers/specs/2026-07-20-cluuamp-winamp-restyle-design.md`.
```

- [ ] **Step 2: Commit**

```bash
cd /home/vlb2bp/git/cluu
git add doc/book/cluuamp.md
git commit -m "docs: CLUUamp chapter — layout, keys, vis pipeline"
```

---

### Task 9: Verification (OPUS subagent)

**Files:** none modified (report only; trivial fixes allowed with commit).

- [ ] **Step 1: Host tests**

Run: `cd /home/vlb2bp/git/cluu/userspace/cluuamp && cargo test 2>&1 | grep "test result"`
Expected: every suite `ok`, 0 failed.

- [ ] **Step 2: Full build**

Run: `cd /home/vlb2bp/git/cluu && cargo xtask build 2>&1 | tail -5`
Expected: success.

- [ ] **Step 3: QEMU harness smoke**

Run: `cd /home/vlb2bp/git/cluu/python && python3 -m cluu_harness --no-build --case l2_cluuamp`
Expected: PASS. On flake, retry exactly once (harness rules).

- [ ] **Step 4: Visual check**

Run: `bash /home/vlb2bp/git/cluu/scripts/fb_dump.sh` while cluuamp is on
screen (see `reference_fb_dump_smoke_workflow` memory / script header for
the boot+launch flow). Read the PNG and compare against spec §2 mockup:
titlebars at rows 0/7/12, block digits visible, vis box 24 cols starting
col 24, EQ sliders row, playlist below. Then press `e` and `p` (via
harness input) and capture again — verify reflow.

- [ ] **Step 5: Spec conformance review**

Read `docs/superpowers/specs/2026-07-20-cluuamp-winamp-restyle-design.md`
section by section against `git diff <pre-task-1-commit>..HEAD -- userspace/cluuamp doc/book`.
Report per section: CONFORMS / DEVIATES (with file:line). Explicitly check:
- §5 peak-marker overwrite rule
- §6 `>> 4` present, SPEC_SCALE == 2.0
- §1 key table complete (e/E/p/r)
- §8 layout field values via the layout tests
- FFT scaling: no bar saturation on real MP3 playback (fb_dump frame:
  spectrum shows varied bar heights, not a solid block)

Output: verification report. Any DEVIATES → hand back to a haiku coding
subagent with the exact file:line and the spec quote.
```

---

## Execution notes

- Tasks 1–8: haiku subagents (one per task, fresh context, this plan is their full instruction set).
- Task 9: opus subagent.
- Review between tasks per subagent-driven-development.
