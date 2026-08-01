# audiod — audio server

audiod is the CLUU audio server. It sits between any number of client
processes (cluuamp, SDL2 applications, future tools) and the single
virtio-snd hardware driver. Only audiod talks to virtio-snd; clients
talk to audiod.

## Architecture

```
  cluuamp ─┐
  SDL app ─┼─→ AUDIOD_STREAM_OPEN → SHM FrameRing ─→ audiod mix ─→ virtio-snd ─→ host PA
  future  ─┘     (per-stream)        (SPSC)          (resample+gain+pan+mix)
```

audiod owns:

- **Per-stream SHM rings** — one `FrameRing` per client, allocated by
  audiod via `FrameAllocate` + `space_map_auto`, granted to the client.
  The client writes negotiated mono or stereo S16 frames; audiod reads them. Frame-based,
  not period-based — clients push arbitrary chunk sizes.
- **Resampling** — `LinearResampler` converts each stream's input rate
  to the negotiated output rate. Fixed-point i64 arithmetic (no float
  in the hot path). Carries fractional position + last sample across
  calls for cross-boundary continuity.
- **Per-stream gain** — Q15 fixed-point. Set via `AUDIOD_STREAM_GAIN`.
- **Per-stream panorama** — constant-power pan law (cos/sin, Q15
  fixed-point via 201-entry lookup table). Set via
  `AUDIOD_STREAM_PANORAMA`. Center = −3 dB on both channels.
- **Mixing** — i32 accumulation across all active streams, single
  `saturate_i16` at output (the normalize stage).
- **Submission** — mixes one period (`period_bytes` frames) into a
  scratch page, calls `submit_grant` to virtio-snd. 8 ring slots,
  target 3 in-flight, completion-driven pacing.

## Negotiation protocol

### audiod ↔ virtio-snd

1. **`AUDIO_QUERY_CAPS` (0x605)** — audiod queries virtio-snd's
   supported format/rate/channel bitmasks before opening a session.
2. **`AUDIO_OPEN_SESSION` (0x600)** — audiod sends
   `PcmParams { format, rate, channels, period_bytes }`. virtio-snd
   clamps `period_bytes` to `[64, 4096]` aligned 4, calls
   `pcm_set_params`, returns the actual `period_bytes`.
3. **Rate selection** — audiod prefers 44100 Hz (music-native), falls
   back to 48000 Hz if unsupported. Both are always in the caps.

### audiod ↔ clients

1. **`AUDOD_QUERY_CAPS` (0x708)** — client queries audiod's supported
   format/rate/channel bitmasks. Currently: S16 only, all standard
   rates (audiod resamples), mono + stereo.
2. **`AUDIOD_STREAM_OPEN` (0x700)** — client sends
   `[session_id, rate, channels, period_bytes, format]`. audiod
   validates format and channels against caps, allocates a SHM ring,
   returns `[status, stream_id, session_id, frame_token, ring_bytes,
   period_bytes]`.

## IPC labels

| Label | Hex | Direction | Purpose |
|-------|-----|-----------|---------|
| `AUDOD_QUERY_CAPS` | 0x708 | client→audiod | query supported formats/rates/channels |
| `AUDIOD_STREAM_OPEN` | 0x700 | client→audiod | open a stream, get SHM ring |
| `AUDIOD_STREAM_CLOSE` | 0x701 | client→audiod | close a stream |
| `AUDIOD_STREAM_PAUSE` | 0x702 | client→audiod | pause (contributes silence) |
| `AUDIOD_STREAM_RESUME` | 0x703 | client→audiod | resume |
| `AUDIOD_STREAM_DRAIN` | 0x704 | client→audiod | drain ring then close |
| `AUDIOD_STREAM_GAIN` | 0x705 | client→audiod | set per-stream gain (Q15) |
| `AUDIOD_STREAM_STATUS` | 0x706 | client→audiod | query stream status |
| `AUDIOD_STREAM_PANORAMA` | 0x707 | client→audiod | set per-stream pan (i8, -100..+100) |
| `AUDIOD_SESSION_DESTROYED` | 0x710 | procmgr→audiod | client died, reap stream |

## Layered processing model

Clients and audiod do **independent** processing. This is deliberate:

- **Client layer** (cluuamp): local EQ, local gain, local balance —
  applied to PCM *before* pushing to the SHM ring. Client owns its
  sound shaping. cluuamp's `Gain::new(volume, balance, channels)` and
  10-band RBJ equalizer stay client-side.
- **Server layer** (audiod): per-stream gain, pan, normalize — applied
  *after* pulling from the ring, *before* mixing. audiod adds its own
  server-side shaping on top of whatever the client did.

Both layers coexist. A client can apply volume=50% locally; audiod can
then apply server-side gain=2x + pan=left + normalize. The layers
compose multiplicatively.

## Scratch ring + slot stride (critical invariant)

audiod grants 8 scratch pages to virtio-snd via `space_grant`. Because
`space_grant` requires page-aligned VAs, the slot stride is
page-aligned:

```
slot_stride = (period_bytes + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
```

For `period_bytes=2048`, `slot_stride=4096`. Each slot occupies one
full page but only the first `period_bytes` are written; the rest is
padding.

**virtio-snd MUST use the same page-aligned stride** when computing
the PCM source VA:

```rust
let slot_stride = (session.period_bytes + 4095) & !4095;
let pcm_va = session.grant_target_va + page_index * slot_stride;
```

If virtio-snd uses `period_bytes` as the stride instead, every odd
period reads from unwritten padding (silence), producing a 43 Hz buzz
that sounds like robotic distortion. See
`gotchas/cluu-audiod-slot-stride-mismatch.md`.

## Constants

| Constant | Value | Notes |
|----------|-------|-------|
| `OUTPUT_RATE_PREFERRED` | 44100 | music-native |
| `OUTPUT_RATE_FALLBACK` | 48000 | if 44100 unsupported |
| `OUTPUT_CHANNELS` | 2 | mixed output is stereo; inputs may be mono or stereo |
| `PERIOD_BYTES` | 2048 | requested from virtio-snd (may be clamped) |
| `RING_SLOTS` | 8 | scratch pages granted to virtio-snd |
| `RING_CAPACITY_FRAMES` | 6144 | SHM ring capacity (~128 ms at 48k) |
| `MAX_PERIOD_FRAMES` | 1024 | max period_bytes / 4 (4096-byte periods) |
| `SCRATCH_VA` | 0x7000_0000 | scratch ring base in audiod's space |

## Main loop

audiod's main loop waits on three endpoints:
- `listen_ep` — client IPC (stream open/close/gain/pan/etc.)
- `completion_ep` — virtio-snd period completions
- `registry_ep` — registry control

On each wakeup:
1. `drain_queues()` — pop used virtio-snd TX descriptors, route
   completions.
2. If completion: `process_completion` decrements `inflight_slots`.
3. `tick()` — drain pending completions, refill to `target_inflight=3`.

The 10 ms `recv` timeout is the audio period wakeup — audiod must wake
periodically to mix and submit. This is NOT polling; it is the standard
ALSA/JACK/PulseAudio periodic-wakeup pattern. No deadlock risk because
audiod has no downstream IPC dependencies (virtio-snd is a leaf driver).
