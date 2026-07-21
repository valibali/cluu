# CLUUamp

CLUUamp is the Winamp-classic-styled TUI audio player. MP3 decode via
nanomp3, playback through the virtio-snd audio session, visualization from
a PCM tap (512-point Hann FFT, Winamp semitone band mapping), 10-band RBJ
peaking equalizer with SSE2 stereo lane cascade.

## Layout

Three stacked windows on one screen (min 76x25):

- **MAIN** (rows 0-9, always visible): block-digit time, 24x3
  spectrum/scope box, title marquee, kbps/khz, mono/STEREO, volume/balance
  sliders, seekbar, transport.
- **EQUALIZER** (toggle `e`, 6 rows): ON/AUTO, curve strip, preamp + 10
  band sliders (60 Hz - 16 kHz), band labels.
- **PLAYLIST** (toggle `p`): track list with current-track marker and
  bottom button bar.

Hidden windows are skipped by Tab focus cycling and do not occupy rows;
the playlist grows into the freed space.

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

`userspace/cluuamp/src/`:

| File | Role |
|------|------|
| `fft.rs` | Winamp-exact spectrum temporal dynamics + 75-bar semitone mapping |
| `scope.rs` | Oscilloscope point extraction (75 -> 24 columns) |
| `viscolor.rs` | Spectrum/scope color palettes |
| `equalizer.rs` | SSE2 10-band RBJ peaking EQ + preamp |
| `gain.rs` | Per-period volume + balance transform |
| `audio.rs` | `AudioEngine`: decode, EQ -> gain, submit, completion-aligned tap |
| `layout.rs` | Three-window cell map (MAIN / EQ / PLAYLIST / footer) |
| `widgets.rs` | Block digits, eighth-block sliders/columns, transport glyphs |
| `model.rs` | MVU state + key dispatch; `sync_equalizer()` forwards to engine |
| `view.rs` | Cell rendering + modal browser overlay |
| `lib.rs` | Module wiring; `runtime` feature gates audio/model/view |

Pure modules (`fft`, `scope`, `viscolor`, `equalizer`, `gain`, `layout`,
`widgets`) are host-tested with `cargo test`; runtime modules build only
for the CLUU target under the `runtime` feature.

## Spectrum — Winamp-exact temporal dynamics

Ported from Winamp `draw_sa.cpp`. The 75-bar spectrum follows Winamp's
temporal ordering precisely:

1. **Retained target on no-new-frame.** When no new PCM period has
   arrived since the last frame, the bar targets hold their previous
   values. The spectrum does not decay to zero between frames.
2. **Instant attack.** `peak = max(peak, target)` — a louder frame snaps
   the bar up immediately.
3. **Falloff subtracts toward zero, not toward target.** Each frame the
   peak loses `falloff = 12` (in 1/16 units): `peak = peak.saturating_sub(falloff)`.
   The subtraction never clamps to the target — bars fall at a fixed
   rate regardless of where the target sits.
4. **Peak snap before display.** After falloff, `peak = max(peak, target)`
   is applied again so a still-loud target prevents the peak from
   dropping below it.
5. **Peak velocity.** `peak -= peak * 3 * 1.1 / scale` — exponential
   decay scalar on top of the linear falloff.

DC removal runs in `process_pcm` before the FFT. Band values are 0-255
(Winamp sadata); pixel height is `value >> 4` (0-15) — see spec
`docs/superpowers/specs/2026-07-20-cluuamp-winamp-restyle-design.md`.

The 512-point Hann FFT uses `microfft` with `size-512`. 75 bands map to
semitone spans (Winamp's band table); each band takes the max magnitude
in its span.

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

## Audio pipeline

`AudioEngine` (in `audio.rs`) owns the decode -> EQ -> gain -> submit
loop and the FFT/scope tap. The pipeline ordering is load-bearing:

```
MP3 frame -> nanomp3 decode -> PCM (s16 interleaved)
         -> f32 conversion -> EQ cascade (SSE2 or scalar)
         -> gain (volume + balance)
         -> s16 write to virtio-snd ring
         -> tap the final submitted period for FFT + scope
```

The tap is **completion-aligned**: it reads from the *final submitted
period's* tail, not from decoder-time buffers. This guarantees the
spectrum and scope reflect what the listener actually hears, not a
decoder ahead of the audible output.

`submission_target()` caps in-flight periods at 2-3 to bound latency.
Without this cap, 8 periods of 4096 bytes at 5512 Hz stereo = ~1.486s of
buffered audio between decode and DAC — visibly decoupled from the
spectrum.

`TapMetadata` is keyed by `PcmHandle` so a `stop()` followed by a fresh
`play()` cannot leak the previous track's tap into the new track's
visualization.

### Heap-safe construction

The CLUU user stack is 128 KiB. The original `AudioEngine` held
~113 KiB of fixed-size arrays (`pcm_f32`, `eq_scratch`, `pcm_mono`,
`pcm_scope`, `tap_metadata`) as inline fields — construction exhausted
the stack.

Those fields are now `Box<[T]>` via `vec![].into_boxed_slice()`. A
compile-time assertion enforces `size_of::<AudioEngine>() < 16 * 1024`.
Max release-frame stack usage drops to ~34 KiB, leaving headroom for the
decode loop and the render path that runs in the same thread.

## File browser modal

`o` opens a modal file browser overlaid on the player. The browser is
implemented in `userspace/libtui/src/components/browser.rs` and shared
with other TUI apps; cluuamp passes `BrowserRenderOptions::borderless(8)`
to get the Winamp-style look:

- No box border (borderless mode)
- Gray background (color 8) matching the login screen
- Visible `>` cursor on the selected row
- Full-height list (rows 2..height-1)
- Scrollbar only when entries overflow the viewport; thumb stays inside
  the track with correct top/bottom bounds

`BrowserRenderOptions::default()` (bordered, default colors) is the path
other callers use; `render_with_options()` is the cluuamp-specific entry
point, `render()` is unchanged for existing users.

## Supporting libcluu / libtui changes

CLUUamp required a few supporting APIs in the shared libraries:

- `libcluu::ipc::call_with_payload_timeout` — blocking IPC call with
  deadline, used by `VfsClient::read_grant_timeout`.
- `VfsClient::read_grant_timeout` — PCM reads with a deadline so a
  backlogged VFS cannot wedge the decode loop.
- `libcluu::posix::file::read_stdin_timeout` — dispatches to TTY or
  VFS-grant read with timeout; consumed by `libtui::StdinReader`.
- `libtui` 256-color SGR (`COLOR_256_THRESHOLD = 100`; `38;5;N` /
  `48;5;N` paths) for the spectrum palette.
- `libtui::ATTR_REVERSE` for selection highlights in the browser.
- `libtui::Renderer::write` loops on short writes (VFS chunks large
  buffers; dropping the tail corrupts terminal state).
- `libtui::ScreenBuffer::diff` tracks the last *emitted* style, not the
  prev buffer cell — a colored cell's style was bleeding into the
  following default-style cell.

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

## Spec

Full design spec with cell-by-cell layout coordinates, color tables, and
Winamp reference links:
`docs/superpowers/specs/2026-07-20-cluuamp-winamp-restyle-design.md`.

Winamp reference source:
<https://github.com/alexfreud/winamp/tree/community/Src>
