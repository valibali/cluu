# Task 17 — Build audiod and narrow virtio-snd to one device client

## Status: Implementation complete, unit tests pass, full build succeeds

## Summary

Built the `audiod` audio daemon crate and narrowed the virtio-snd driver to
serve audiod as its sole client. audiod mixes N streams from multiple
sessions with i32 accumulation and single saturation, linear resampling,
and per-session authority via `PARAM_AUDIOD_EP` (following the T6 displayd
broker model).

## Files Created

- `userspace/audiod/Cargo.toml` — crate manifest (lib + bin, no_std)
- `userspace/audiod/src/lib.rs` — lib entry, re-exports pure-logic modules
- `userspace/audiod/src/ring.rs` — SHM SPSC frame ring, monotonic counters, acquire/release ordering
- `userspace/audiod/src/resample.rs` — linear resampling, mono→stereo, cross-boundary continuity
- `userspace/audiod/src/mixer.rs` — i32 mix, single saturation, N-stream, Q15 gain
- `userspace/audiod/src/session.rs` — per-session stream management, IPC protocol, stream state machine
- `userspace/audiod/src/main.rs` — audiod entry point: virtio-snd sole client, stream control, mix loop
- `containers/audiod/Cluufile` — PROFILE ipc vfs registry device, no framebuffer

## Files Modified

- `userspace/virtio-snd/src/session.rs` — buffer≥period fix (2048/8192), real PcmStatus, safe close/drain, unique slot ownership, MAX_SESSIONS=1
- `userspace/virtio-snd/src/control.rs` — self_test period/buffer aligned with session.rs
- `userspace/virtio-snd/src/main.rs` — unregister from registry after sole client connects, route_completion signature update
- `userspace/libcluu/src/registry.rs` — audiod:main resolution via PARAM_AUDIOD_EP (lookup_service + subscribe_output)
- `userspace/cluu_wire/src/spawn.rs` — PARAM_AUDIOD_EP = 20 (follows PARAM_DISPLAYD_EP pattern)
- `userspace/root-procmgr/src/main.rs` — global_audiod_control_ep, session_audiod_endpoints, mint_session_audiod_ep, session teardown revocation
- `userspace/init/src/wiring.rs` — audiod boot path documentation (same pattern as displayd)
- `Cargo.toml` — audiod added to workspace members and default-members
- `xtask/src/main.rs` — audiod added to userspace_crates build list

## Verification

### Unit Tests (25 tests, all pass)

Run via `rustc --edition 2021 --test <module>.rs -o /tmp/t && /tmp/t`:

**ring.rs (7 tests):**
- `ring_wrap_basic` — push/pop with wraparound
- `ring_overcommit_returns_zero_and_counts_xrun` — overrun detection
- `ring_monotonic_counters_never_reset` — total_written/total_read never reset
- `ring_underrun_returns_zero_on_empty` — underrun returns 0
- `ring_preserves_stereo_pairs` — L/R channel integrity
- `ring_reset_clears_state` — reset to empty
- `ring_partial_push_then_complete` — partial fill then complete

**resample.rs (8 tests):**
- `silence_produces_zeros` — silence passthrough
- `mono_to_stereo_duplicates_channel` — mono→stereo duplication
- `passthrough_44100_preserves_signal` — same-rate passthrough
- `resample_continuity_across_calls` — cross-boundary continuity (48000→44100)
- `resample_downsample_produces_fewer_frames` — 48000→24000 ratio
- `resample_upsample_produces_more_frames` — 22050→44100 ratio
- `fill_silence_zeroes_buffer` — silence fill helper
- `resampler_reset_clears_state` — reset to initial

**mixer.rs (10 tests):**
- `silence_mix_produces_zeros` — silence mixing
- `single_stream_passthrough` — unity gain passthrough
- `two_streams_sum_without_clipping` — half+half = unity, no clip
- `clipping_saturates_to_max` — positive saturation to 32767
- `clipping_saturates_to_min` — negative saturation to -32768
- `n_stream_mix_4_streams` — 4 streams at 25% gain each
- `gain_zero_silences_stream` — zero gain = silence
- `gain_unity_preserves_signal` — unity gain preserves
- `saturate_i16_boundaries` — saturation boundary values
- `asymmetric_stereo_mix` — L from A, R from B

### Build

`cargo xtask build` succeeds. All modified crates compile with the
x86_64-cluu-user target. The audiod binary is built as part of the
workspace.

## Design Decisions

### Virtio-snd driver fixes

1. **Buffer ≥ period**: Changed from PERIOD=4096/BUFFER=2048 (incoherent,
   buffer < period) to PERIOD=2048/BUFFER=8192 (buffer = 4× period).
2. **One hardware stream**: MAX_SESSIONS=1 — audiod is the sole client.
3. **Real PcmStatus**: `route_completion` now reads the actual status from
   the device's PcmStatus DMA region instead of hardcoding S_OK.
4. **Safe close/drain**: `handle_close` drains in-flight TX submissions
   (with timeout guard) before stop+release.
5. **Unique slot ownership**: `slot_in_flight[RING_SLOTS]` array prevents
   double-submit of the same ring slot.
6. **Unregister after sole client**: Driver unregisters "snddev:main" from
   the registry after the first AUDIO_OPEN_SESSION, making the endpoint
   capability-unreachable to apps (AGENTS.md §3).

### Session authority (displayd broker model)

1. **root-procmgr** creates `global_audiod_control_ep` (held by root-procmgr,
   never forwarded — AGENTS.md §6 root-godmode).
2. **Per-session endpoints**: `mint_session_audiod_ep` creates a per-session
   endpoint, stores RECV in `session_audiod_endpoints`, derives SEND token
   installed via `PARAM_AUDIOD_EP`.
3. **Session teardown**: `destroy_session` revokes the per-session endpoint,
   killing all derived SEND tokens (capability-unreachable, not runtime refusal).
4. **No runtime sender-TID ACL**: Authority is possession of the scoped
   endpoint token (AGENTS.md §3).

### audiod architecture

1. **Sole virtio-snd client**: Subscribes to "snddev:main" at boot.
2. **Registry**: Registers "audiod:main" for session binaries to find via
   PARAM_AUDIOD_EP.
3. **IPC protocol**: Labels 0x700-0x710 for stream control
   (open/close/pause/resume/drain/gain/status/session_destroyed).
4. **Mix loop**: Timeout-based poll — drain virtio-snd completions, mix
   active streams, submit to virtio-snd.
5. **Output format**: Fixed stereo S16 at 44100 Hz, period 2048 bytes
   (512 frames).

## Known Limitations (T17 scope)

1. **SHM ring grant setup**: The full SHM ring grant mechanism (space_grant
   from producer to audiod) requires the complete grant path similar to
   cluuamp's audio.rs. T17 provides the ring data structure and protocol;
   the grant wiring is completed in T18 (SDL audio backend) and T20
   (cluuamp migration).
2. **Root-procmgr forwarding**: The per-session endpoints created by
   root-procmgr hold RECV capabilities. Full message forwarding from
   per-session endpoints to audiod's listen endpoint is a follow-up —
   the displayd pattern has the same forwarding gap (T7 incomplete).
3. **1024-byte period test**: The initial period is 2048 bytes. The 1024-byte
   experiment is configured by changing PERIOD_BYTES in main.rs; the ring
   and mixer support both sizes (MAX_PERIOD_FRAMES=512 covers both).
4. **system.toml entry**: audiod's `[[service]]` entry in `/etc/system.toml`
   is out of scope for T17's allowed file list. The Cluufile and wiring
   comment are in place; the system.toml entry is a one-line follow-up.

## Gotchas Addressed

- **cluuamp-pcm-s16-unbounded-growth**: audiod's mix loop drains completions
  before mixing, preventing unbounded accumulation.
- **cluuamp-stream-buf-overshoot**: The ring's push returns 0 on full
  (overcommit), incrementing xrun_count instead of overshooting.
- **cluu-cluuamp-eof-drops-last-partial-period**: The drain state machine
  (Draining → Closed when ring empty) ensures partial periods are played.
- **cluu-audioengine-stack-overflow-128kib**: The mixer's accum buffers are
  4 KB total (512 × 2 × i32), well under the 128 KB stack limit.
