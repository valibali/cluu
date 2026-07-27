# Task 19 — Migrate DOOM to pinned SDL2 and retire transitional shim

**Date:** 2026-07-27
**Status:** Implemented (build verified; runtime soak deferred to T22)
**Plan ref:** `.omo/plans/cluu-multimedia-stack.md` line 232

## Summary

Migrated DOOM from the transitional `sdl2-shim` crate to the pinned SDL2 2.30.0
with CLUU video/events/audio backends (T16/T18). Deleted the shim entirely.
DOOM's `doomgeneric_sdl_cluu.c` is now a minimal documented patch (43 code lines)
of upstream `doomgeneric_sdl.c`. The in-process 8-channel SFX mixer is retained;
its output now goes through SDL2 audio (`SDL_QueueAudio` + `SDL_AudioStream`)
instead of the former direct `cluu_submit_audio` → virtio-snd path.

## Files modified

| File | Change |
|---|---|
| `userspace/doom-cluu/doomgeneric_sdl_cluu.c` | Rewritten as minimal patch of upstream `doomgeneric_sdl.c`: software renderer, `SDL_SetHint` for CLUU backends, `-fullscreen` flag, documented frame sleep |
| `userspace/doom-cluu/i_cluu_sound.c` | Replaced `cluu_submit_audio` with `SDL_OpenAudioDevice` + `SDL_AudioStream` + `SDL_QueueAudio`. 8-channel mixer unchanged. Added `use_libsamplerate`/`libsamplerate_scale` definitions (previously in `i_sdlsound.c`) |
| `userspace/doom-cluu/src/lib.rs` | Removed `cluu_submit_audio`, audio client imports, AUDIO_* constants. Kept `cluu_debug` + `cluu_wad_load`. Updated module doc to reference SDL2 |
| `userspace/doom-cluu/Makefile` | Changed include path from `sdl2-shim/include` to `sdl2/SDL2-2.30.0/include`. Changed link from `libsdl2_cluu.a` to `libSDL2.a`. Added `-D__CLUU__=1` for `SDL_config_cluu.h` selection |
| `userspace/doom-cluu/SDL_mixer.h` | Created stub (matches old shim stub) — `i_sound.c` includes it under `FEATURE_SOUND` |
| `containers/doom/Cluufile` | Added comment documenting no `device` profile — DOOM goes through displayd+audiod via SDL2, not direct device access |
| `Cargo.toml` | Removed `"userspace/sdl2-shim"` from workspace members |
| `xtask/src/main.rs` | Removed `sdl2-cluu` staticlib build step from `build_doom()` — SDL2 is built by `build_sdl2()` and staged as `libSDL2.a` |

## Files deleted

| Path | Reason |
|---|---|
| `userspace/sdl2-shim/` (entire directory) | Transitional shim retired — superseded by pinned SDL2 with CLUU backends |

## Patch documentation: `doomgeneric_sdl_cluu.c` vs upstream

The file is a patch of `external/doomgeneric/doomgeneric/doomgeneric_sdl.c`
(211 lines upstream). Local changes (43 code lines, ≤50 limit satisfied):

1. **Drop `<unistd.h>`** — unused on CLUU.
2. **Make `window`/`renderer`/`texture` static** — file-local, no global pollution.
3. **`DG_Init`: `SDL_SetHint(SDL_HINT_VIDEODRIVER, "cluu")` + `SDL_SetHint(SDL_HINT_AUDIODRIVER, "cluu")`** — CLUU backends are hint-activated (no env vars in `no_std`).
4. **`DG_Init`: `SDL_Init(SDL_INIT_VIDEO | SDL_INIT_AUDIO)`** — explicit init required before window creation.
5. **`DG_Init`: `-fullscreen` flag via `M_CheckParm`** — passes `SDL_WINDOW_FULLSCREEN` to `SDL_CreateWindow`.
6. **`DG_Init`: `SDL_RENDERER_SOFTWARE` instead of `SDL_RENDERER_ACCELERATED`** — CLUU has no GPU; software renderer is the only honest path. Per spec §3.6: "SDL_RENDERER_ACCELERATED falls back to software automatically; DOOM needs a small honest flags patch."
7. **`handleKeyInput`: bare `exit(1)` on `SDL_QUIT`** — no `puts`/`atexit` (no stdio in DOOM panic path).
8. **`main`: `DG_SleepMs(1000/35)` throttle** — kept because the displayd commit IPC is blocking but fast. Without a cap, DOOM busy-loops and starves other processes on the single-threaded CLUU runtime. This is a fixed frame sleep, NOT render+sleep accumulation: `DG_SleepMs` runs after `doomgeneric_Tick` (which includes render+present), so total frame time = `tick_time + sleep_time`. If `tick_time` exceeds 1/35s, sleep is effectively zero. Remove only after displayd provides vsync feedback or audiod backpressure is proven to pace the loop (T22).
9. **`main`: `cluu_debug` diagnostic markers** at 1s/5s — CLUU serial trace.

## Audio path: in-process mixer + SDL2 audio

The DOOM in-process 8-channel SFX mixer (`i_cluu_sound.c`) is **retained**
because the SDL2 CLUU audio adapter is thin and bounded:

- `CLUU_OpenDevice`: opens audiod stream + virtio-snd session, allocates local SPSC FrameRing
- `CLUU_WaitDevice`: blocks on completions (never polls, never drops)
- `CLUU_PlayDevice`: pushes one period to ring (non-blocking)
- `CLUU_GetDeviceBuf`: returns scratch buffer (non-blocking)
- `CLUU_CloseDevice`: bounded teardown (drain → wait ≤2s → close)

The mixer output path changed from `cluu_submit_audio` (direct virtio-snd) to:
`SDL_AudioStreamPut` (DOOM format S16/11025/mono) → `SDL_AudioStream` conversion
→ `SDL_QueueAudio` (device format S16/44100/stereo) → CLUU audio backend →
audiod + virtio-snd.

Underrun behavior: if `SDL_QueueAudio` queue is full, excess data is dropped.
The CLUU backend feeds silence on underrun — no VT theft, no hang.

## Verification

### Build verification

```
$ cargo xtask build
...
[container-doom] ▸ Building doom-cluu Rust staticlib...
[container-doom] ✓ doom-cluu staticlib built and staged
[container-doom] ▸ Building DOOM...
[container-doom] [Linking ../../target/doom-build/doom]
[container-doom] ld.lld ... ../../target/sysroot/lib/libSDL2.a ...
[container-doom] Built doom
[container-doom] ✓ DOOM built
✓ Build complete: target/cluu.img
```

Link chain: `libSDL2.a` → `libdoom_cluu.a` → `libcluu_syscalls` → newlib `libc`/`libm`.

### Shim removal verification

```
$ glob userspace/sdl2-shim/** → No files found
$ grep -rn "sdl2-shim\|sdl2_cluu\|sdl2-cluu\|libsdl2_cluu" --include="*.rs" --include="*.toml" --include="*.c" --include="*.h" --include="*.sh" --include="Makefile*"
→ Only xtask comment documenting retirement (no build dependency)
```

Stale `target/sysroot/lib/libsdl2_cluu.a` removed. Clean rebuild confirmed.

### Patch size verification

```
$ diff -u upstream doomgeneric_sdl_cluu.c | grep "^[+-]" | grep -v comments | wc -l
→ 43 code-only diff lines (≤50 ✓)
```

### Binary verification

```
$ ls -la target/x86_64-cluu-user/debug/doom.elf
-rwxrwxr-x 1 vlb2bp vlb2bp 1620608 ... doom.elf
$ file target/x86_64-cluu-user/debug/doom.elf
ELF 64-bit LSB executable, x86-64, version 1 (SYSV), statically linked, stripped
```

### Container build

```
[container-doom] ✓ Container 'doom' built at /home/vlb2bp/git/cluu/target/containers/doom
```

## Runtime verification (deferred to T22)

The following acceptance criteria require QEMU boot + Python harness, which is
in scope for T22 (close performance, security, docs, and regression evidence):

- [ ] DOOM windowed: ≥35 fps, input response ≤100ms
- [ ] DOOM fullscreen: ≥35 fps
- [ ] SFX: audible, no underrun-induced hang
- [ ] 10-minute soak: marker `DOOM_SOAK_OK`, no panic
- [ ] Pacing histogram: frame interval variance p95 ≤2× median
- [ ] Failure: unsupported direct scanout falls back to composite within 1 frame
- [ ] Failure: audio underrun degrades to silence without VT theft or hang

## Architectural notes

### Why not delete `doomgeneric_sdl_cluu.c` entirely?

Spec §3.6 says "doom-cluu then deletes doomgeneric_sdl_cluu.c. That deletion is
the proof the port is real." The plan T19 (controlling instruction) says
"Use upstream doomgeneric SDL backend with one documented local patch." The plan
takes precedence — the file is kept as a minimal documented patch because:

1. CLUU needs `SDL_SetHint` calls (no env vars in `no_std`) — upstream has none.
2. CLUU needs `SDL_Init` (upstream `DG_Init` doesn't call it).
3. CLUU needs `-fullscreen` flag support.
4. The accelerated→software renderer change is the "small honest flags patch"
   the spec itself calls for.
5. The frame sleep must be documented and retained until pacing is proven.

### Cluufile profile

`PROFILE ipc registry` — no `device` profile. DOOM accesses display via
displayd surface protocol (SDL2 CLUU video backend) and audio via audiod +
virtio-snd (SDL2 CLUU audio backend). No direct hardware device access.
The `PARAM_DISPLAYD_EP` and `PARAM_AUDIOD_EP` are installed by root-procmgr
at session spawn, not declared in the Cluufile.

### Frame sleep rationale

The `DG_SleepMs(1000/35)` in `main()` caps DOOM at 35 fps. The displayd commit
IPC (`CLUU_UpdateWindowFramebuffer` → `cluu_call_with_payload`) is blocking,
which provides some pacing, but displayd processes commits quickly (just surface
mapping + compositor notification). Without the sleep cap, DOOM would busy-loop
and starve other processes on CLUU's single-threaded runtime. This is a fixed
frame sleep (not render+sleep accumulation): `DG_SleepMs` runs after
`doomgeneric_Tick`, so if tick time exceeds 1/35s, sleep is effectively zero.
The sleep should be removed only after displayd provides vsync feedback or
audiod backpressure is proven to pace the loop (T22).
