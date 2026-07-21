# Cluuamp Classic-Winamp Restyle — Design Spec

Date: 2026-07-20
Status: approved (brainstorm session)
Scope: `userspace/cluuamp` only (view.rs, layout.rs, widgets.rs, model.rs, fft.rs, audio.rs getters). No kernel, no libtui API changes expected (libtui components used as-is).

## Goal

Restyle cluuamp to the classic Winamp 2.x three-window layout (as recreated by
webamp): MAIN + EQUALIZER + PLAYLIST stacked vertically, EQ and PLAYLIST
toggleable, block-digit time display, small spectrum box, and a corrected
Winamp-faithful spectrum analyzer.

Fix two defects:

1. **FFT scaling bug** — `fft.rs` `tick()` clamps 0–255 band values to 15
   instead of shifting (`>> 4`); every active bar saturates → blocky
   all-or-nothing spectrum.
2. **Layout not Winamp** — current single flat stack with full-width 8-row
   visualizer bears no resemblance to Winamp.

## Execution model

- Spec/plan: authored by main session (fable).
- Coding, QA, unit tests, docs: **haiku** subagents. This spec is therefore
  EXACT — cell positions, glyph tables, color indices. Haiku implements
  literally, no design judgment.
- Verification: **opus** subagent pass at the end.

---

## 1. Window model

Screen is 80×25 minimum design target (cluuterm default). Rows 0–24,
cols 0–79. All positions below given for width `W` (≥ 76); fixed columns are
absolute, right-anchored elements use `W`.

Three stacked pseudo-windows, each with a 1-row titlebar:

| Window     | Rows (all visible)      | Visibility            |
|------------|-------------------------|-----------------------|
| MAIN       | 0–6 (7 rows)            | always                |
| EQUALIZER  | 7–11 (5 rows)           | `model.show_eq`       |
| PLAYLIST   | 12–23 (EQ shown)        | `model.show_playlist` |
| footer     | 24                      | always                |

Reflow rules:
- EQ hidden → PLAYLIST starts at row 7 (gains 5 rows).
- PLAYLIST hidden → rows below MAIN (or below EQ) render as blank
  (default bg, spaces).
- Both hidden → only MAIN + footer.

New model state: `show_eq: bool` (default `true`), `show_playlist: bool`
(default `true`).

`Layout::calculate(width, height, show_eq, show_playlist)` — recomputed on
resize AND on toggle.

New/changed keys:

| Key  | Action                                        |
|------|-----------------------------------------------|
| `e`  | toggle EQ **window** visibility (was: eq_enabled) |
| `E`  | toggle `eq_enabled` (DSP on/off)              |
| `p`  | toggle PLAYLIST window visibility             |
| `r`  | remove selected playlist track (§4 [REM])     |

All other keys unchanged (`Space`, `n`, `b`, `s`, `v`, `o`, `q`, `Esc`,
`Tab`, arrows, `Enter`).

Focus cycling (`Tab`): skip `FocusArea::Eq` when `!show_eq`, skip
`FocusArea::Playlist` when `!show_playlist`.

### Titlebar style (all three windows)

Full row, bg 236. Title string centered, bold. Fill chars `─` fg 240 bg 236
on both sides of title. One space padding inside title.

- MAIN title: ` CLUUAMP ` fg 46. Right side of the titlebar (before the
  trailing 2 cols) shows state label ` PLAYING ` / ` PAUSED ` / ` STOPPED `
  (fg 46 / 226 / 244, bg 236).
- EQ title: ` EQUALIZER ` fg 252.
- PLAYLIST title: ` PLAYLIST (N tracks) ` fg 252, N = playlist length.

## 2. MAIN window — exact cell map (rows 0–6)

Full-screen reference mockup (80 cols):

```
──────────────────────────────── CLUUAMP ──────────────────────────  PLAYING ──
 ▶ ▄▄ █▀█ ▄█ ▄ █▀█ █▀█          ▂▄▆█▅▃▂▄▆█▅▃▂▁▂▄▆█▅▃▂▁▂▄  1. DJ Mike Llama - L
      █ █  █ █ ▄▀▀ ▀▀█          ▃▅▇█▆▄▃▅▇█▆▄▃▂▃▅▇█▆▄▃▂▃▅  192 kbps   44 khz
      █▄█ ▄█▄ ▀ █▄▄ ▄▄█         ▅▆██▇▅▄▆██▇▅▄▃▄▆██▇▅▄▃▄▆  mono  STEREO
 V ────────────o─────  B ──────o──────   [EQ] [PL]
 ──────────────o───────────────────────────────────────────────────────────────
 (|<) (>) (||) ([]) (>|) (^)   (SHUF) (REP)
```

### Row 0 — titlebar
As per titlebar style above.

### Rows 1–3 — time / vis / track info band

**State glyph** — row 2, col 1:
`▶` fg 46 (playing), `║` fg 226 (paused), `■` fg 244 (stopped).

**Block-digit time** — rows 1–3, cols 3–22 (20 cols). Renders `-mm:ss`
remaining time when playing/paused (`duration_ms - position_ms`), `00:00`
elapsed style without minus when stopped. Layout within the field, left to
right: minus sign (2 cols, blank when positive), 1 space, digit (3), 1 space,
digit (3), 1 space, colon (1), 1 space, digit (3), 1 space, digit (3).

All digit glyphs 3 cols × 3 rows, fg 46, drawn with `█ ▀ ▄` and space.
Glyph table (rows top→bottom, exact strings):

```
0: "█▀█","█ █","█▄█"    5: "█▀▀","▀▀█","▄▄█"
1: " █ "," █ ","▄█▄"    6: "█▀▀","█▀█","█▄█"
2: "█▀█","▄▀▀","█▄▄"    7: "▀▀█","  █","  █"
3: "█▀█"," ▀█","█▄█"    8: "█▀█","█▀█","█▄█"
4: "█ █","▀▀█","  █"    9: "█▀█","▀▀█","▄▄█"
minus (2 cols): "  ","▀▀","  "
colon (1 col):  "▄"," ","▀"
```

**Vis box** — rows 1–3, cols 24–47 (24 cols × 3 rows). See §5.

**Track info** — right block, cols 49 to `W-1`:
- Row 1: marquee, scrolling current title, existing marquee widget, fg 252.
  Marquee width = `W - 49 - 1`. `model.tick()` must use this width for
  scroll wrap (new layout field `marquee_col`, `marquee_width`).
- Row 2: `{kbps:>3} kbps  {khz:>2} khz` fg 244; kbps/khz from audio engine
  (§7); render `---`/`--` when zero.
- Row 3: `mono  STEREO` — active word bold fg 46, inactive fg 240
  (active = `audio.channels()`: 1 → mono, 2 → STEREO).

### Row 4 — volume / balance / window toggles
- Col 1: `V` fg 244. Cols 3–22: volume h-slider, width 20, value 0–100,
  focused when `focus == Volume` (existing widget).
- Col 25: `B` fg 244. Cols 27–40: balance h-slider, width 14.
- Cols 44–47: `[EQ]` — fg 46 bold when `show_eq`, else fg 240.
- Cols 49–52: `[PL]` — fg 46 bold when `show_playlist`, else fg 240.

### Row 5 — seekbar
Cols 1 to `W-2`: position h-slider (existing widget), value =
`position*255/duration`, focused when `focus == Position`.

### Row 6 — transport
Buttons via existing `draw_button` (adds `( )` or highlight), starting
col 1, 1 space between:
labels `|<`, `>`, `||`, `[]`, `>|`, `^`  (prev, play, pause, stop, next,
eject/open-browser). Then 3 spaces, `SHUF`, `REP` buttons — active state
lit fg 46 when `model.shuffle` / `model.repeat`.

`transport_selected` indices remap: 0=prev 1=play 2=pause 3=stop 4=next
5=eject 6=shuf 7=rep. `Enter` actions: play→`play()`, pause→`pause()`
(toggle when paused), eject→`open_browser("/host")`, others as named.
Default `transport_selected = 1`.

## 3. EQUALIZER window — exact cell map (5 rows, base row `T` = 7)

```
T+0: ── EQUALIZER ──────────────... (titlebar)
T+1:  [ON] [AUTO]      ▄▅▆▅▄▃▂▃▄▅▆ (curve, 22 cols)          [PRESETS]
T+2:   █    ▄    █    ▄ ... (upper slider halves)
T+3:   █    █    █    █ ... (lower slider halves)
T+4:  pre  60  170  310  600  1k  3k  6k  12k  14k  16k
```

- Row T+1: `[ON]` cols 2–5 — fg 46 bold when `eq_enabled`, else fg 240.
  `[AUTO]` cols 7–12 fg 240 (visual placeholder, no action).
  Curve strip: cols 20–41 (22 cols, 2 per band): each band value
  `v ∈ [-12,12]` → eighth-block char index `(v+12)*7/24` → char from
  `▁▂▃▄▅▆▇█`, fg 51, repeated twice. `[PRESETS]` right-anchored ending
  col `W-3`, fg 240 (placeholder).
- Rows T+2..T+3: 11 vertical sliders, 1 col each, x positions
  `eq_band_x[i] = 3 + i*7` (3,10,17,24,31,38,45,52,59,66,73).
  Value `v ∈ [-12,12]` → filled eighths `f = (v+12)*16/24` (0–16) drawn
  bottom-up: bottom row char = eighth-block of `min(f,8)`, top row char =
  eighth-block of `f-8` when `f>8` else space. Eighth-block table:
  0→space, 1→`▁`, 2→`▂`, 3→`▃`, 4→`▄`, 5→`▅`, 6→`▆`, 7→`▇`, 8→`█`.
  Color: fg 46; focused band (`focus==Eq && eq_selected==i`): fg 226 and
  the empty cells render `░` fg 238 to show the slider track.
- Row T+4: labels `pre 60 170 310 600 1k 3k 6k 12k 14k 16k`, each centered
  on its `eq_band_x[i]`, fg 244.

## 4. PLAYLIST window — exact cell map (base row `P`, extends to row 23)

- Row P: titlebar.
- Rows P+1 .. 22: track rows.
  - Cols 1–2: track number, right-aligned, fg 238 (fg matches row style when
    current/selected).
  - Col 3: `.` fg 238.
  - Col 5: `▶` fg 46 when current track, else space.
  - Cols 7 .. `W-10`: filename (basename, truncated).
  - Cols `W-7` .. `W-3`: duration `m:ss` fg 244 — current track only (engine
    knows only current duration); other rows blank.
  - Current: fg 46. Selected: ATTR_REVERSE. Both: fg 46 + reverse.
  - Rows past playlist end: cleared to spaces.
  - Col `W-1`: scrollbar (existing widget) when playlist longer than
    visible rows.
- Row 23 (bottom bar): `[ADD] [REM]` from col 1 — functional; `[SEL] [MISC]`
  fg 240 placeholders; right-anchored `{pos}/{dur} ` (current track, mm:ss/mm:ss,
  fg 244) then `[LIST]` fg 240 placeholder ending col `W-2`.
  - `[ADD]`: opens file browser (same as `o`).
  - `[REM]`: removes `playlist_selected` from playlist (new
    `AudioEngine::remove_track(idx)`; if removing the playing track: stop,
    current_index clamps; selection clamps).
  - Buttons are display-only shortcuts activated via keys (`o` add,
    `Delete`… not available → use `r` = remove selected). Add `r` key.

Playlist visible-rows count feeds existing scroll clamp
(`layout.playlist_height` = 23 − (P+1)).

## 5. Visualizer — spectrum + scope in 24×3 box

Box: rows 1–3, cols 24–47. Unlit cells are plain space (bg default).
No box border.

### Spectrum (VisMode::Spectrum)
- Source: 75 Winamp bars → 24 columns. Column `j` (0–23) uses bar index
  `j*75/24` after `tick()` (values already 4-grouped/averaged).
- Height: `bar_height` 0–15 → total eighths `h = level*24/15` (0–24) over
  3 rows (24 vertical eighths), drawn bottom-up like EQ sliders: for
  screen-row r (0=top,1,2), filled-eighths-in-row =
  `clamp(h - (2-r)*8, 0, 8)` → eighth-block char (table in §3).
- Color per cell row: level_at = row r → use `viscolor::bar_color(l)` where
  `l = (2-r)*5 + 2` (rows bottom→top get colors ~2, 7, 12: green, yellow,
  red zones). Keep existing palette (`viscolor.rs` unchanged).
- Peak: `peak_height` 0–15 → eighths `pk = peak*24/15`; peak cell row
  `r = 2 - min(pk/8, 2)`; draw `▀` fg 255 in that cell ONLY if the cell is
  not already a full `█` from the bar.

### Oscilloscope (VisMode::Oscilloscope)
- 75 points → 24 columns, point idx = `j*75/24`.
- Vertical resolution 6 half-cells (3 rows × 2). `y = 3 + val*6/64`
  clamped 0–5 (val is i8 −32..31 from existing `scope.point()`).
  Cell row = `y/2`; char = `▀` if `y%2==0` else `▄`.
- Color: existing `SCOPE_COLORS[|y-3| ]` clamped (keep current formula
  adapted to 6-range).

## 6. FFT fix (fft.rs)

1. In `tick()`, replace the clamp
   `let v = if v > MAX_LEVEL { MAX_LEVEL } else { v };`
   with the Winamp mapping `let v = (v >> 4).min(MAX_LEVEL);`
   (band values are 0–255; pixel height is value/16 → 0–15).
2. Recalibrate `SPEC_SCALE` from `0.5` to `2.0`: full-scale sine through
   Hann window gives peak-bin magnitude ≈ N/4 = 128; ×2.0 ≈ 255 → top of
   scale. (Coherent gain of Hann = 0.5.)
3. Falloff/peak state machines unchanged (already operate in 16ths units —
   consistent once v is 0–15).
4. Update tests: full-scale 440 Hz sine at 44.1 kHz → the excited band's
   `bar_height` in 12–15 after one `process_pcm`+`tick`; bands more than
   15 semitone-bars away < 4. Existing saturation-shaped tests adjusted.

## 7. Audio engine additions (audio.rs)

- `pub fn sample_rate(&self) -> u32`
- `pub fn bitrate_kbps(&self) -> u32` — from current MP3 frame info if
  minimp3 `FrameInfo` exposes it (check `probe_format` site); else return 0
  and view renders `---`.
- `pub fn remove_track(&mut self, idx: usize)` — per §4 [REM].

## 8. Layout struct (layout.rs) — new shape

```rust
pub struct Layout {
    pub width: usize, pub height: usize,
    // main
    pub main_title_row: usize,        // 0
    pub time_top: usize,              // 1 (3 rows)
    pub vis_top: usize, pub vis_left: usize,   // 1, 24
    pub vis_width: usize, pub vis_height: usize, // 24, 3
    pub marquee_row: usize, pub marquee_col: usize, pub marquee_width: usize,
    pub info_row: usize, pub stereo_row: usize,  // 2, 3
    pub sliders_row: usize,           // 4
    pub position_row: usize,          // 5
    pub transport_row: usize,         // 6
    // eq (Option-like: valid only when show_eq)
    pub show_eq: bool,
    pub eq_title_row: usize, pub eq_buttons_row: usize,
    pub eq_slider_top: usize,         // 2 rows
    pub eq_labels_row: usize,
    pub eq_band_x: [usize; 11],
    // playlist
    pub show_playlist: bool,
    pub pl_title_row: usize,
    pub playlist_top: usize, pub playlist_height: usize,
    pub pl_buttons_row: usize,        // 23
    pub footer_row: usize,            // 24 (height-1)
    pub scrollbar_col: usize,
}
```

`min_width() = 76`, `min_height() = 25`. `fits()` requires
`width ≥ 76 && height ≥ 25` and, when `show_playlist`,
`playlist_height ≥ 1`.

## 9. Testing

Host tests (`cargo test -p cluuamp` + libtui unaffected):

- **layout**: all 4 visibility combos at 80×25 — exact row assertions per
  tables above; no overlap; reflow (EQ hidden → `pl_title_row == 7`);
  76×25 fits; 75×25 does not.
- **fft**: scaling tests per §6; silence → 0; DC → 0; gravity/peak decay
  tests retained.
- **widgets**: block-digit glyph table (every digit renders its 3 strings);
  eighth-block fill math (v=−12→0 eighths, v=0→8, v=12→16); spectrum column
  render for level 0/8/15; peak placement.
- **model**: `e`/`p`/`E`/`r` key behavior; focus skip over hidden windows;
  transport remap enter actions.
- **view** (smoke): render 80×25 all-visible → titlebars at rows 0/7/12,
  footer at 24, no panic at 76×25 and 200×60.

QEMU harness: `cd python && python3 -m cluu_harness --no-build --case l2_cluuamp`
(startup smoke) + fb_dump visual check (`bash scripts/fb_dump.sh`) by opus
verification pass.

## 10. Out of scope

- Winamp skin bitmap fidelity / pixel-art mode.
- Presets, AUTO, SEL/MISC/LIST button functionality (placeholders).
- Seek (no seek API), per-track durations (no metadata scan).
- Mouse.
