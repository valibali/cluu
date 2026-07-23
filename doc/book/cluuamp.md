# CLUUamp

CLUUamp is the Winamp-classic-styled TUI audio player. MP3 decode via
vendored minimp3 (SSE2 SIMD, CC0 public domain), playback through the
virtio-snd audio session, visualization from a PCM tap (512-point Hann
FFT, Winamp semitone band mapping), 10-band RBJ peaking equalizer with
SSE2 stereo lane cascade.

## Layout

Three stacked windows on one screen (min 76x36):

- **MAIN** (11 rows, always visible): block-digit time, 24x3
  spectrum/scope box, title marquee, kbps/khz, mono/STEREO, volume/balance
  sliders, seekbar, transport.
- **EQUALIZER** (toggle `e`, 10 rows): ON/AUTO, curve strip, gap row,
  preamp + 10 band sliders (60 Hz - 16 kHz), band labels.
- **PLAYLIST** (toggle `p`): track list with current-track marker and
  bottom button bar.
- **FOOTER** (1 row): key hints.

Hidden windows are skipped by Tab/Shift+Tab focus cycling and do not
occupy rows; the playlist grows into the freed space.

EQ layout sections are `[3, 1, 3, 1]` (graph, gap, sliders, labels).
Bands are centered via `(2*i+1)*width/(2*11)`.

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
| o | open file dialog (add tracks) |
| Tab / Shift+Tab | cycle focus forward / backward (skips hidden windows) |
| arrows / Enter | operate focused control |
| q / Esc | quit |

## Architecture

`userspace/cluuamp/src/`:

| File | Role |
|------|------|
| `fft.rs` | Winamp-exact spectrum temporal dynamics + 75-bar semitone mapping |
| `scope.rs` | Oscilloscope point extraction (75 -> 24 columns) |
| `viscolor.rs` | Spectrum/scope/EQ-curve color palettes |
| `equalizer.rs` | SSE2 10-band RBJ peaking EQ + preamp |
| `gain.rs` | Per-period volume + balance transform |
| `id3.rs` | ID3v2 + ID3v1 metadata parser, `TrackMeta` cache |
| `terminal.rs` | Terminal size negotiation (`ensure_terminal_size`) |
| `audio.rs` | `AudioEngine`: two-thread decode/submit, EQ -> gain, completion-aligned tap, ID3 metadata |
| `layout.rs` | Three-window cell map (MAIN / EQ / PLAYLIST / footer), `FocusArea` with `next()`/`prev()` |
| `widgets.rs` | Braille spectrum, oscilloscope |
| `mp3_ffi.rs` | FFI bindings to vendored minimp3 (SSE2 SIMD decoder) |
| `model.rs` | MVU state + key dispatch; `sync_equalizer()` forwards to engine |
| `view.rs` | Cell rendering + modal browser overlay |
| `lib.rs` | Module wiring; `runtime` feature gates audio/model/view |

Pure modules (`fft`, `scope`, `viscolor`, `equalizer`, `gain`, `layout`,
`widgets`) are host-tested with `cargo test`; runtime modules build only
for the CLUU target under the `runtime` feature.

## Spectrum — Winamp-style bar dynamics

Ported from Winamp `draw_sa.cpp`. The 75-bar spectrum follows Winamp's
temporal ordering:

1. **Retained target on no-new-frame.** When no new PCM period has
   arrived since the last frame, the bar targets hold their previous
   values. The spectrum does not decay to zero between frames.
2. **Instant attack.** A louder frame snaps the bar up immediately to
   the new target.
3. **Falloff subtracts toward zero, not toward target.** Each frame the
   bar loses `falloff = 12` (in 1/16 units). The subtraction never
   clamps to the target — bars fall at a fixed rate regardless of where
   the target sits.

No peak-holds are drawn. The spectrum shows only the bar fill with
gravity falloff — no held-peak markers above the bars.

4. **Schmitt trigger on display.** The internal `bar_state` tracks the
   raw attack/decay dynamics above, but the *displayed* level reads a
   separate `display_state` that only updates when `bar_state` exits a
   ±12-unit hysteresis band. Glyph levels are 16 units apart (the `>> 4`
   step), so a 12-unit threshold requires ¾ of a level of movement
   before the rendered glyph changes. This suppresses the "dancing"
   effect where a bar oscillates between two adjacent glyphs when the
   target hovers near a level boundary (e.g. bar_state cycling
   100↔112 would flip the displayed glyph between level 6 and 7 every
   frame without the trigger).

> **Note:** The Schmitt trigger was removed in a later refactor.
> `display_state` now directly tracks `bar_state`. The hysteresis
> added visual lag without meaningfully reducing glyph flicker at
> normal playback volumes.

DC removal runs in `process_pcm` before the FFT. Band values are 0-255
(Winamp sadata); pixel height is `value >> 4` (0-15) — see spec
`docs/superpowers/specs/2026-07-20-cluuamp-winamp-restyle-design.md`.

The 512-point Hann FFT uses `microfft` with `size-512`. 75 bands map to
semitone spans (Winamp's band table); each band takes the max magnitude
in its span.

### Color palette

Green → orange → red gradient (bottom to top). Bottom rows use xterm
256-color green indices (22-41), middle rows orange (130-214), top rows
red (196-52). The palette is defined in `viscolor.rs::BAR_COLORS`.

### Tap point

The FFT/scope tap reads from `eq_scratch` — post-EQ, pre-volume. This
matches Winamp, where `SAAddPCMData` is called from the decoder with
raw decoded PCM before any output-stage processing. The spectrum is
independent of the volume slider: turning the volume up or down does
not change the spectrum amplitude, and high volume cannot cause
overdrive artifacts in the FFT.

## Equalizer — SSE2 10-band RBJ peaking

`equalizer.rs` is a cascade of 10 biquad peaking filters plus a preamp
gain:

| Band | Frequency |
|------|-----------|
| 0 | 60 Hz |
| 1 | 170 Hz |
| 2 | 310 Hz |
| 3 | 600 Hz |
| 4 | 1 kHz |
| 5 | 3 kHz |
| 6 | 6 kHz |
| 7 | 12 kHz |
| 8 | 14 kHz |
| 9 | 16 kHz |

Coefficients use the RBJ cookbook peaking formulas. Each band has -12..+12
dB gain (user range 0..24, mapped through `pow(10, (g-12)/12 * 0.5)`).

### SSE2 stereo lane cascade

IIR biquad recurrence is inherently sequential in time — you cannot
vectorize across samples. But L and R channels are independent, so they
run as the two lanes of an SSE2 `__m128`:

```rust
#[target_feature(enable = "sse2")]
unsafe fn process_stereo_sse2(biquads: &[BiquadSse2], l: f32, r: f32) -> (f32, f32)
```

Each `BiquadSse2` holds `(b0, b1, b2, a1, a2)` as `__m128` and the two
state registers `(z1, z2)` as `__m128`. The transposed-direct-form-II
update is two FMA-equivalent ops per stage, 10 stages per channel pair.

A scalar mono fallback handles single-channel PCM. Byte-exact parity
between SSE2 and scalar paths is asserted in the test suite.

x86_64 baseline is `+sse2` (see `triplets/x86_64-cluu-user.json`), so the
SSE2 path is unconditional on the CLUU target. The `#[target_feature]`
gate exists for host tests on architectures without SSE2.

### Wiring

`model.rs::sync_equalizer()` forwards the enable toggle and every band
mutation to `AudioEngine::set_equalizer`. The engine rebuilds the biquad
cascade in place; no re-allocation on gain changes.

### EQ slider visual

The EQ window occupies 7 rows: title, buttons/curve, blank gap, 3 slider
rows, labels. The gap row separates the curve strip from the slider bar
tops so the bars don't visually touch the curve.

Each slider is 3 rows tall (24 eighths). The bar fills bottom-up from
the center (value 0 = 12 eighths = half-filled). Unfilled cells show
`░` (light shade) as a track — the same style as the horizontal progress
bar — so the slider is always visible even at minimum value. Focused
sliders use bright yellow (226); unfocused use dim gray (238) for the
track and green (46) for the fill.

### EQ response curve — braille + gradient

The curve strip (3 cell rows, `eq_graph` area) renders the interpolated
EQ response as braille dots. Each dot column maps to a position in the
10-band frequency axis; the curve height at that position is linearly
interpolated between the two nearest band gains (plus preamp).

**Color is per-cell, not per-row.** Each braille cell's color is the
average `f` value (0..=24, where 0 = -12 dB and 24 = +12 dB) of the dot
columns that landed in that cell. A flat +12 curve is all green (46); a
flat -12 curve is all red (196); a sloping curve shows a horizontal
gradient across the graph width. Coloring by row position would produce
only 3 colors (the graph is 3 rows tall); coloring by curve height
produces the full 25-step gradient. The palette is
`viscolor.rs::EQ_CURVE_COLORS` (red → yellow-orange → green).

### EQ level indicator — left-side hairline + T-joint ticks

To the left of the curve (`graph.x - 1`) a vertical hairline spans the
-12..+12 dB range with three tick markers:

| Glyph | dB level | Color |
|-------|----------|-------|
| `┌` | +12 | green (46) |
| `├` | 0 | yellow-orange (214) |
| `└` | -12 | red (196) |

Bare `│` segments between ticks are gradient-colored by row. The
`db_to_tick_row(db, height)` helper maps a dB value to a graph cell row
using the same dot-row math as the curve (4 dot-rows per cell row).

## Two-thread architecture

CLUUamp runs two threads via `libcluu::thread::{Shared, spawn, join,
sleep_ms}`:

- **Audio thread**: locks `Shared<AppState>`, calls `audio_tick()`,
  checks `ring_saturated()`, sleeps 13ms (or 50ms when saturated).
  No `unsafe` in consumer code — all threading primitives are
  encapsulated in `libcluu/src/thread.rs`.
- **UI thread**: locks `Shared<AppState>`, runs `ui_tick()`, renders
  if needed, diffs against previous buffer, writes to terminal. Waits
  for stdin with 33ms timeout (`RENDER_MS`), drains all available
  keys per iteration.

The `Shared<T>` mutex ensures only one thread touches the model at a
time. The audio thread never blocks on IPC while holding the lock —
it releases between ticks via `sleep_ms`.

## Audio pipeline

`AudioEngine` (in `audio.rs`) owns the decode -> EQ -> gain -> submit
loop and the FFT/scope tap. The pipeline ordering is load-bearing:

```
MP3 frame -> minimp3 decode (SSE2 SIMD) -> PCM (s16 interleaved)
          -> EQ cascade (SSE2 or scalar)
          -> gain (volume + balance)
          -> s16 write to virtio-snd ring
          -> tap the final submitted period for FFT + scope
```

The tap is **completion-aligned**: it reads from the *final submitted
period's* tail, not from decoder-time buffers. This guarantees the
spectrum and scope reflect what the listener actually hears, not a
decoder ahead of the audible output.

`submission_target()` caps in-flight periods at `RING_SLOTS` (8) to bound
latency. Batch decode submits up to `DECODE_BATCH` (4) frames per tick.

### Memory-bounded tick loop

`tick()` enforces a strict submit-before-decode order to keep `pcm_s16`
bounded at ~1 period + 1 frame (≈8.7KB):

1. Submit pending periods (drain `pcm_s16` → ring) first.
2. Only decode when `pcm_s16.len() < PERIOD_BYTES` — prevents
   accumulation when the ring is nearly full.
3. Submit again after decoding.

The original code decoded up to 4 frames before submitting, which grew
`pcm_s16` unbounded (+14KB/tick when the ring had 1 free slot) and
triggered Vec doubling to 65MB.

`refill_stream` clamps each VFS read to `want.min(STREAM_BUF_SIZE -
stream_buf.len())` so the buffer never exceeds its 256KB cap — without
the clamp, a 64KB read chunk overshooting the 256KB limit caused Vec
doubling to 512KB that never shrank.

`TapMetadata` is keyed by `PcmHandle` so a `stop()` followed by a fresh
`play()` cannot leak the previous track's tap into the new track's
visualization.

### EOF flush

When the decoder reaches the end of the MP3 data, the last partial
period in `pcm_s16` may be shorter than `PERIOD_BYTES` (4096 bytes). At
low bitrates this tail can be several seconds of audio. `tick()` checks
for EOF after the normal submit loop: if `pcm_s16` is non-empty, the
partial period is submitted as-is; only when `pcm_s16` is fully drained
AND `ring_inflight == 0` does `advance_to_next` fire. This ensures the
last seconds of every track are audible.

Decode failures (bad frame, ID3 tag bytes) skip 1 byte and retry rather
than jumping to EOF — this prevents truncation when the decoder
encounters non-MP3 data at the end of the file.

### Position and duration tracking

Position and duration follow the Winamp pattern:

- **`pcm_played`** — incremented by `PERIOD_BYTES` each time a
  completion arrives from the hardware in `drain_completions()`. This
  is the actual played position (like Winamp's `GetOutputTime()`).
- **`pcm_submitted`** — total bytes pushed to the ring buffer. Used
  internally but NOT for position display (it runs ahead of playback).
- **`pcm_total_decoded`** — accumulated in `decode_one_frame()` as
  `total_samples * 2`. Once `decode_complete` is set (decode_pos reaches
  EOF), `duration_ms()` switches from bitrate estimate to
  `pcm_total_decoded / bytes_per_ms` — the true audio length excluding
  ID3 tags and padding.
- **`duration_ms()`** — before decode completes, uses bitrate-based
  estimate (like Winamp's `GetLength()`). After decode completes, uses
  actual decoded PCM byte count.

This two-phase approach means the progress bar starts with an estimated
duration, then snaps to the exact value once the full file is decoded,
and `position_ms()` (from `pcm_played`) reaches exactly 100% at track
end with no jump.

### Replay from stopped state

`play()` checks for `PlaybackState::Stopped` and calls `close_audio()`
before reloading. This resets `decode_pos`, `pcm_submitted`,
`pcm_played`, and `file_loaded`, so the track reloads from the
beginning. Without this, `file_loaded` would still be true and playback
would resume from a stale position.

### Heap-safe construction

The CLUU user stack is 128 KiB. The original `AudioEngine` held
~113 KiB of fixed-size arrays (`pcm_f32`, `eq_scratch`, `pcm_mono`,
`pcm_scope`, `tap_metadata`) as inline fields — construction exhausted
the stack.

Those fields are now `Box<[T]>` via `vec![].into_boxed_slice()`. A
compile-time assertion enforces `size_of::<AudioEngine>() < 16 * 1024`.
Max release-frame stack usage drops to ~34 KiB, leaving headroom for the
decode loop and the render path that runs in the same thread.

## File dialog modal

`o` opens a `FileDialog` modal (`libtui/src/components/filedialog.rs`).
The dialog supports four modes:

| Mode | Buttons | Purpose |
|------|---------|---------|
| `OpenFile` | Open, Cancel | Single file selection |
| `OpenMulti` | Add, Open Dir, Cancel | Multi-select files or import a directory |
| `SaveFile` | Save, Cancel | Filename input + file list |
| `SelectDir` | Select, Cancel | Directory-only selection |

CLUUamp uses `OpenMulti`: Space marks files, Enter opens/adds, "Open Dir"
imports all `.mp3` files from the highlighted or current directory.

### Modal rendering

`FileDialog::draw_modal(screen_w, screen_h, buf)` fills the entire screen
with black (bg 0), then draws the dialog centered at 4/5 screen size
(clamped 40x10 to 80x24). The caller's `render()` early-returns after
drawing the modal — the main UI is not rendered underneath.

### Browser entries

`list_directory()` (in `main.rs`) prepends `./` (current dir) and `../`
(parent dir) entries when not at root. Both are exempt from the
hidden-file filter. Enter on `./` refreshes the current directory; Enter
on `../` navigates to the parent.

### Cursor style

The selected row is rendered as a filled line: yellow background (color
226) with black foreground (color 0) across the full entry width. No
`>` prefix glyph is used. This applies to the borderless render path
used by FileDialog; the framed `FileBrowser.render()` path (used
standalone) uses blue background.

### Header suppression

`BrowserRenderOptions::borderless_no_header(bg)` skips the browser's
built-in title and cwd bar — FileDialog draws its own title and path bar
above the file list, so rendering both would duplicate them.

### Directory import flow

1. User highlights a directory and presses "Open Dir" (or highlights
   `./` and presses "Open Dir").
2. `FileDialog::confirm()` returns `DialogAction::OpenDir(path)`.
3. `model.rs` sets `pending_dir_import` and closes the dialog.
4. `main.rs` UI loop calls `list_directory(path)`, filters `.mp3` files,
   sorts, and calls `audio.extend_playlist(paths)`.
5. `force_redraw` is set so the main UI renders on the next frame.

### Dialog actions

| Action | Trigger | Effect |
|--------|---------|--------|
| `Open(paths)` | Enter on file / "Add" button | Add selected/marked files to playlist |
| `EnterDir(path)` | Enter on directory | Navigate into directory |
| `OpenDir(path)` | "Open Dir" button | Import all .mp3 from directory |
| `Cancel` | Esc / "Cancel" button | Close dialog |
| `Save(path)` | "Save" button (SaveFile mode) | Return filename |

### ID3 metadata

`id3.rs` parses ID3v2 (header at file start) and ID3v1 (128-byte trailer
at file end) tags into `TrackMeta { title, artist, album, duration_ms }`.
The parser runs lazily: `main.rs` reads the first `META_READ_LEN` bytes
of each track via `read_file_head_into()` and calls `id3::parse()`. A
second read past the ID3v2 tag provides audio data for bitrate-based
duration estimation. Results are cached in
`AudioEngine::track_metas`. `write_display_title()` writes the ID3 title
(if available, else filename) into a caller-provided `&mut String` to
avoid per-frame heap allocation — the old `display_title()` that returned
a `String` was a per-frame allocation source.

## Supporting libcluu / libtui changes

CLUUamp required a few supporting APIs in the shared libraries:

- `libcluu::ipc::call_with_payload_timeout` — blocking IPC call with
  deadline, used by `VfsClient::read_grant_timeout`.
- `VfsClient::read_grant_timeout` — PCM reads with a deadline so a
  backlogged VFS cannot wedge the decode loop.
- `libcluu::posix::file::read_stdin_timeout` — dispatches to TTY or
  VFS-grant read with timeout; consumed by `libtui::StdinReader`.
- `libtui` 256-color SGR — all non-default colors use `CSI 38;5;Nm` /
  `CSI 48;5;Nm`. No basic-SGR path: palette indices like 46 (green), 51
  (cyan), 8 (gray) are all 256-color, and emitting them as basic SGR
  (`CSI 46m`) would set background cyan instead of foreground green.
- `libtui::ATTR_REVERSE` for selection highlights in the browser.
- `libtui::Renderer::write` loops on short writes (VFS chunks large
  buffers; dropping the tail corrupts terminal state).
- `libtui::ScreenBuffer::diff` tracks the last *emitted* style, not the
  prev buffer cell — a colored cell's style was bleeding into the
  following default-style cell.
- `View::write_styled_n(row, col, s, max_chars, cell)` — clips a styled
  string to at most `max_chars` characters, for sub-area width clipping.
- `View::write_field(row, col, s, width, cell)` — truncates or pads a
  string to exactly `width` chars, no allocation. Used by `top` for
  column-aligned output.
- `AudioSessionClient::drain_completions_into(&mut Vec)` — reuses a
  caller-provided Vec instead of allocating via `core::mem::take`.
- `libcluu::allocator::AllocStats::leaked_deallocs` — counter for
  deferred-free leaks (should always read 0 with the blocking-lock fix).

## Render loop

The UI thread renders at 30 FPS (33ms cadence). `force_redraw` is set
on every key press and structural change (browser open/close, resize,
track import). The render decision:

```
needs_render = force_redraw || browser_just_closed
            || title_scroll_changed || was_playing || browser_is_open
```

`force_redraw` and `browser_just_closed` are captured into locals
before being consumed (reset to false), so `needs_render` sees the
value from this frame, not the reset. Without this, closing the
browser would clear the screen but skip the render, leaving a black
screen.

When the file dialog is open, `render()` draws only the modal (black
fill + centered dialog) and returns early — the main UI is not drawn
underneath.

## Testing

Pure-logic modules have host tests that run without the CLUU kernel:

```bash
cd userspace/cluuamp
cargo test                              # all tests
cargo test --lib fft                    # FFT dynamics (21 tests)
cargo test --lib equalizer              # SSE2/scalar parity + band response
cargo test --lib scope                  # oscilloscope point mapping
cargo test --lib gain                   # volume/balance transform
```

Runtime modules (audio, model, view) are gated behind the `runtime`
feature and build only for `x86_64-cluu-user`; they cannot host-link
because they reference CLUU kernel symbols.

The `l2_cluuamp` harness case boots the player in QEMU and asserts the
startup markers (`VIRTIO_SND_PCI`, `VIRTIO_SND_OK`, `CLUUAMP_STARTING`).
Manual verification still required for: FFT latency, EQ effect, audio
playback continuity, modal browser interaction.

```bash
cd python
python -m cluu_harness --case l2_cluuamp --no-build
```

## Terminal size negotiation

On startup, `terminal::ensure_terminal_size()` requests a minimum
76x36 terminal from the compositor via `COMP_WIN_RESIZE_LABEL` (IPC
label 105). The compositor's `resize_window_by_id()` resizes the
window, and cluuterm forwards the new size via `PTS_SET_WINSIZE` →
compositor `COMP_WIN_CONFIGURE`. cluuterm does NOT call `resize_grid`
on the outgoing request — it waits for the configure event to avoid
a double-resize.

## Resize handling

cluuamp does not use `libtui::Program` — it has its own event loop that
polls `ioctl(TIOCGWINSZ)` every iteration (33ms). The terminal size is
never cached: every tick re-queries the PTS winsize via `ioctl(fd=1,
TIOCGWINSZ)`, which translates to a `PTS_GET_WINSIZE` IPC to cluuterm.
When the compositor resizes the window, cluuterm updates the PTS winsize
and emits `SIGWINCH` to the foreground process group; cluuamp sees the
new dimensions on the next tick, clears the screen, resets the diff
buffer, and recalculates the layout.

cluuterm's `resize_grid` resizes both the active cell grid and the
alt-screen buffer (when in alt screen) so the alt-screen exit swap does
not panic on an empty buffer.

## Spec

Full design spec with cell-by-cell layout coordinates, color tables, and
Winamp reference links:
`docs/superpowers/specs/2026-07-20-cluuamp-winamp-restyle-design.md`.

Winamp reference source:
<https://github.com/alexfreud/winamp/tree/community/Src>
