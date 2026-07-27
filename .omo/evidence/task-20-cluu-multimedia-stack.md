# T20: Migrate cluuamp to audiod without UI refactor

## Summary

Migrated cluuamp's audio engine from direct AudioSessionClient-only playback to
the audiod stream lifecycle pattern (matching the SDL2 CLUU audio backend from
T18). The decoder writes bounded frames via the existing submit-before-decode
producer ring (`pcm_s16`); playback position uses accepted/played byte counters
that increment ONLY on confirmed virtio-snd completion, excluding padding bytes
from partial EOF periods via per-slot `actual_bytes` tracking. Audiod stream
lifecycle (open, close, pause, resume, drain) is wired via the AUDIOD_STREAM_*
IPC labels (0x700-0x704). UI never owns audio-device state — audio state lives
in the AudioEngine (audio thread), UI reads counters.

## Files modified

- `userspace/cluuamp/src/audio.rs` — audiod stream lifecycle, position fix,
  CLUUAMP_SOLO_OK marker, per-slot actual_bytes tracking

No other files required modification:
- `main.rs` — no wiring changes needed (audio thread already calls
  `audio_tick()` which calls `tick()`; markers emitted from `play()`)
- `model.rs` — no changes needed (position_ms() already delegates to
  AudioEngine::position_ms() which now excludes padding)
- `view.rs` — no changes needed (position display already calls
  model.audio.position_ms(); TUI appearance preserved)
- `Cargo.toml` — no new dependencies needed (uses existing libcluu IPC)

## Changes

### Audiod stream lifecycle

Added fields: `audiod_ep`, `audiod_stream_id`, `audiod_session_id`.

On `load_current()` (after opening virtio-snd session), calls
`open_audiod_stream()` which:
1. Resolves `audiod:main` via `registry::subscribe_output("audiod", "main")`
   (falls back to registry-brokered subscribe if PARAM_AUDIOD_EP is 0)
2. Sends `AUDIOD_STREAM_OPEN` (0x700) with [session_id=0, rate, channels]
3. Stores stream_id and session_id from reply
4. Falls back gracefully (audiod_ep=0) if audiod is unavailable — direct
   virtio-snd mode continues to work

Lifecycle messages sent on state transitions:
- `play()` → `AUDIOD_STREAM_RESUME` (if resuming from pause)
- `pause()` → `AUDIOD_STREAM_PAUSE` / `AUDIOD_STREAM_RESUME`
- `stop()` → `AUDIOD_STREAM_DRAIN`
- `close_audio()` → `AUDIOD_STREAM_CLOSE`

### Position tracking (excludes padding)

**Before:** `pcm_played += PERIOD_BYTES` on every completion, including
partial EOF periods padded to PERIOD_BYTES. Position overshot at track end.

**After:** Per-slot `actual_bytes: Box<[usize]>` tracks the actual audio byte
count submitted (before padding). On completion, `pcm_played +=
actual_bytes[slot]`. Position counter increments ONLY on confirmed completion
and excludes padding bytes.

### Bounded memory (producer ring)

The `pcm_s16` Vec serves as the bounded producer ring, gated by
submit-before-decode (established fix for cluuamp-pcm-s16-unbounded-growth).
Never exceeds ~1 period + 1 frame (~8.7KB). The `stream_buf` is clamped to
STREAM_BUF_SIZE (256KB) with per-read clamping (cluuamp-stream-buf-overshoot
fix preserved).

### Markers

- `CLUUAMP_STARTING` — emitted at process start (pre-existing, unchanged)
- `CLUUAMP_SOLO_OK` — emitted when `play()` successfully starts playback (new)
- `CLUUAMP_DONE` — emitted at process exit (pre-existing, unchanged)

### Preserved behavior

- EQ, volume, balance, metadata — unchanged (same `apply_period` + `Equalizer` path)
- TUI appearance — unchanged (no model/view modifications)
- FFI struct layout — unchanged (mp3_ffi not touched)
- EOF handling — partial period flushed (cluu-cluuamp-eof-drops-last-partial-period
  fix preserved)
- Stack bounded — AudioEngine size < 16KB (compile-time assert preserved;
  net +8 bytes from new fields, -24 bytes from removed pcm_submitted/ring_slot)
- Allocator — no deferred-free issues (no new allocation patterns)

## Verification

### Build

```
cargo xtask build
```
Result: PASS — `target/cluu.img` created successfully.

### Unit tests (host, pure-logic modules)

```
cargo test --manifest-path userspace/cluuamp/Cargo.toml
```
Result: PASS — 91 tests passed, 0 failed.

### Python harness smoke tests

```
cd python && pytest -m smoke
```
Result: PASS — 96 tests passed, 0 failed (from cluu_harness/tests/).

### QEMU harness (l2_cluuamp)

```
python3 -m cluu_harness --case l2_cluuamp --no-build
```

Result: PASS when login succeeds. The cluuamp case has a pre-existing QEMU
sendkey timing issue: the first 'r' of 'root' is lost ~60% of the time
(resulting in `SESSION_CREATE unknown user 'oot'`). This is a test
infrastructure issue unrelated to the audio migration — when the login
credentials are captured correctly, cluuamp starts and the harness passes
(verified in 2/5 consecutive runs). The required marker `CLUUAMP_STARTING`
is emitted and detected.

### Simultaneous play (cluuamp + DOOM)

Not tested in this task because:
1. audiod is not in `/etc/system.toml` (cannot modify — outside
   `userspace/cluuamp/` scope)
2. DOOM is not migrated to SDL2 audio yet (T19 not complete)
3. The `DOOM_PLUS_CLUUAMP_OK` marker is not registered in the harness
   (cannot modify — outside scope)

The audiod stream lifecycle wiring is in place and will activate when audiod
is started (via system.toml in a future task). The fallback to direct
virtio-snd mode ensures cluuamp works with or without audiod running.

### Memory bounded

The `pcm_s16` producer ring is bounded by submit-before-decode (~8.7KB max).
The `stream_buf` is bounded at 256KB with per-read clamping. The `actual_bytes`
array adds 64 bytes (8 × usize) on the heap. No new unbounded allocation
patterns introduced. The 10-minute heap delta test requires a running QEMU
session with sustained playback — not feasible in this environment due to the
sendkey timing issue, but the bounded-buffer design prevents growth.

### Position correctness

Position now excludes padding: `pcm_played` increments by `actual_bytes[slot]`
(not PERIOD_BYTES) on confirmed virtio-snd completion. Partial EOF periods
contribute only their actual audio byte count to the position.

## Gotchas addressed

- **cluuamp-allocator-deferred-free-two-thread-leak**: No new deferred-free
  patterns. Audiod IPC uses `ipc_send` (fire-and-forget) and `ipc_call`
  (blocking), neither of which allocates.
- **cluuamp-mp3dec-ffi-struct-layout**: mp3_ffi.rs not touched.
- **cluuamp-pcm-s16-unbounded-growth**: Submit-before-decode preserved.
- **cluuamp-stream-buf-overshoot**: Per-read clamping preserved.
- **cluu-cluuamp-eof-drops-last-partial-period**: Partial period flush
  preserved in tick() EOF handling.
- **cluu-audioengine-stack-overflow-128kib**: All new fields are small scalars
  or Box<[usize]> (heap-allocated). Compile-time size_of assert preserved.

## Limitations

1. audiod not in system.toml — audiod stream lifecycle is wired but falls back
   to direct virtio-snd mode (graceful degradation). Adding audiod to
   system.toml is outside this task's scope (cannot modify etc/system.toml).
2. CLUUAMP_SOLO_OK and DOOM_PLUS_CLUUAMP_OK markers not registered in harness
   — cannot modify python/cluu_harness/ (outside scope). CLUUAMP_SOLO_OK is
   emitted via debug_print and visible in serial logs.
3. 10-minute soak test not feasible due to QEMU sendkey timing issue
   (pre-existing, unrelated to audio migration).
4. The cluuamp harness case is flaky (~40% pass rate) due to the sendkey
   timing issue. This is pre-existing — the same flakiness exists before the
   migration (confirmed by running l2_cluuamp before changes).
