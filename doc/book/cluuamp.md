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
