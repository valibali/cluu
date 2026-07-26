# CLUU Multimedia Architecture — Design

**Date:** 2026-07-26
**Status:** Proposed (measurement-grounded) — binding decisions recorded in §7; performance claims cite T2 baseline (`.omo/evidence/task-2-cluu-multimedia-stack.md`). Implementation plan: `.omo/plans/cluu-multimedia-stack.md`.
**Scope:** display server, surface protocol, audio mixer, SDL2 port, virtio-gpu
**Kernel impact:** none — no new syscalls, no kernel changes required (compatible with the freeze through ~2026-10-21)

---

## 1. Problem

DOOM (freedoom via doomgeneric) drives ~35% **host** CPU at 640×400. The suspected
cause was the linear framebuffer and CPU scanout. Investigation identifies two
independent contributors: redundant copy passes in the guest (plus a shim that defeats
vectorization), and QEMU-side VGA display emulation that guest-side metrics cannot see.
See §2.7 — the host/guest split has not yet been measured and is the first task.

T2 baseline (`.omo/evidence/task-2-cluu-multimedia-stack.md`) measured steady-state vCPU
at 4–5% across all four harness states (idle TUI, quiet shell, DOOM windowed, DOOM
fullscreen) under `-display none`. DOOM frame cadence is 3.6–4.3 fps; the shim update
loop consumes 62–87M TSC cycles per frame. No fixed absolute guest CPU percentage is a
defensible acceptance target before the displayd refactor — performance gates are
**relative to the T2 baseline**, not absolute.

The broader problem: the multimedia layer grew bottom-up around one application at a
time (compositor cell grid → pixel regions → cluuamp → DOOM). There is no surface
abstraction, no damage model, no frame pacing, and no audio mixing. Each new port
requires new special-casing, which is exactly the friction that discourages third-party
ports.

### 1.1 Goals

- TUI remains the primary UX. Cell-grid rendering must not regress.
- Fast pixel surfaces, both windowed and fullscreen, for games/emulators/image viewers.
- Windowed pixel content costs at most one composite pass; fullscreen is promoted to
  direct scanout **opportunistically** (exact-size, backend-compatible) and otherwise
  costs one composite pass. Zero guest copies is not guaranteed.
- Multiple applications can produce audio simultaneously.
- A third-party developer can port an SDL2 application without touching CLUU internals.
- "Good enough and SOLID", not professional-grade. No 3D, no GPU acceleration.

### 1.2 Non-goals

- OpenGL / virgl / venus / any 3D acceleration.
- Hardware overlay planes, multi-monitor, HDR, colour management.
- Input routing redesign (vtmgr/inputd/compositor focus policy stays as-is).
- Font/glyph rendering changes.

---

## 2. Findings (measured against the code, 2026-07-26)

### 2.1 Hypotheses eliminated

**The framebuffer is not uncached.** `userspace/libcluu/src/posix/framebuffer.rs:73`
maps with `MAP_DEVICE_WC`; `kernel/src/mm/pat.rs` programs PAT[1] = WC;
`kernel/src/elf.rs:846` sets PWT-only (not PCD, which would select UC). Write-combining
is already in effect for the direct-FB path. "UC BAR writes" is not the cause.

### 2.2 The CPU metric is sound and under-reports

`top` computes `sys_cpu_pct` as the sum of per-process `cpu_ticks` deltas divided by
elapsed scheduler ticks (`userspace/top/src/main.rs:185-196`). Ticks are attributed to
the running thread from the timer IRQ (`kernel/src/sched/thread_manager.rs:1437-1443`),
the idle thread is excluded, and ticks are *dropped* when `THREAD_REPOSITORY.try_lock()`
fails. Therefore 35% is a lower bound on real busy time.

### 2.3 Root cause: four full-frame passes, including two redundant upscales

Windowed DOOM, per frame. Note the resolution chain: DOOM renders at its native 320×200
in 8bpp, and is upscaled **twice** before reaching the screen.

Display is **1920×1080** (framebuffer 8.3 MB). The DOOM pixel region is 160×50 cells
(`sdl2-shim/src/lib.rs:130-131`), which at GLYPH_W=8 / GLYPH_H=16 is 1280×800 — it fits on
screen, so nothing is clipped.

| # | Pass | Location | u32 touched |
|---|------|----------|----------------|
| 1 | palette LUT + ×2 upscale, 320×200 8bpp → 640×400 ARGB (`DG_ScreenBuffer`) | doomgeneric `i_video.c` | 256K |
| 2 | ×2 upscale, 640×400 → 1280×800 into the SHM pixel region | `sdl2-shim/src/lib.rs:338-384` | 512K volatile stores + 512K copy |
| 3 | SHM → compositor backbuffer | `compositor/src/render.rs:45-55` | 1024K |
| 4 | backbuffer → WC framebuffer | `compositor/src/render.rs:248-258` | 1024K |

≈ 3.3M u32 ≈ **13 MB per frame**, × 35 fps ≈ **460 MB/s** of guest memory traffic for a
320×200-class game.

Under displayd the chain collapses to a single pass: the client submits 640×400 (or
eventually native 320×200 8bpp) with a `src_rect`/`dst_rect` pair, and displayd performs
one integer nearest scale inside the composite pass. Neither application-side upscale
remains.

**Honest floor.** One composite pass costs one write per *destination* pixel. At a
1280×800 destination that is 4 MB/frame ≈ 143 MB/s at 35 fps — a ~3.4× reduction in guest
traffic, not a ~10× one. Cost scales with displayed area, so a smaller window is
proportionally cheaper, and fullscreen scanout promotion (§3.2) is the only path to zero.
The larger win at 1920×1080 is host-side (§2.7): virtio-gpu flushing a 1280×800 damage
rect instead of QEMU rescanning the whole 8.3 MB framebuffer.

### 2.4 Four architectural defects, all confirmed

1. **Per-pixel `write_volatile`** in the shim scaler (`sdl2-shim/src/lib.rs:324`, `:358`,
   `:380`). Volatile stores forbid LLVM from emitting SIMD or `memcpy`, forcing one store
   per pixel. Neither SHM (normal cached RAM) nor a WC mapping requires volatile
   semantics; a single fence at the end of the frame is sufficient.

2. **Damage is always full-window.** `SDL_RenderPresent` sends `WIN_DAMAGE` with
   `0xFFFF, 0xFFFF` (`sdl2-shim/src/lib.rs:203-208`), so `dirty_rect` always collapses to
   the whole pixel region. Partial update is structurally impossible.

3. **Presentation lives in the wrong SDL call.** All scaling and blitting happens inside
   `SDL_UpdateTexture`; `SDL_RenderCopy` is `{ 0 }` (`sdl2-shim/src/lib.rs:195`). Any
   application that calls `UpdateTexture` more than once per frame pays the full cost each
   time, and `RenderCopy`'s src/dst rects are silently ignored — which breaks most real
   SDL2 ports.

4. **Fullscreen bypasses the compositor entirely.** `VT_DEACTIVATE` plus
   `framebuffer_acquire` steals the BAR (`sdl2-shim/src/lib.rs:266-301`). This yields two
   independent blit paths to maintain, no overlays or hotkeys while fullscreen, and no
   windowed↔fullscreen transition without a mode teardown.

Additionally: `DG_SleepMs(1000/35)` is a *fixed* 28 ms sleep added on top of render time
(`doom-cluu/doomgeneric_sdl_cluu.c:213`). There is no vsync and no frame pacing anywhere
in the stack, so the actual frame rate is below 35 and jittery.

### 2.5 Audio gap

`virtio-snd` supports N sessions but performs **no mixing**
(`userspace/virtio-snd/src/`). Two applications producing audio simultaneously is
unsolved. DOOM performs its own 8-channel mix in `doom-cluu/i_cluu_sound.c`.

### 2.7 The 35% is HOST CPU — guest and host costs are conflated

The observed 35% was measured on the host (the QEMU process), not inside CLUU. Under
`-accel kvm -vga std -display gtk` (`xtask/src/main.rs:2308-2350`), host load has three
distinct components:

1. **vCPU thread** — actual CLUU execution, i.e. the four-pass pipeline of §2.3. Runs at
   native speed under KVM, so guest cycles are host cycles roughly 1:1.
2. **KVM dirty-page logging on VGA VRAM.** QEMU sets `DIRTY_MEMORY_VGA` logging on the
   framebuffer region, which causes KVM to write-protect its pages. The first write to
   each page after a refresh scan traps to the host. At 1920×1080×4 = 8.3 MB that is
   **2025 pages**, so up to 2025 VM exits per refresh cycle. Pure overhead, entirely
   invisible to guest `top`.
3. **QEMU display emulation.** `vga_draw_graphic` converts dirty VRAM scanlines into a
   `DisplaySurface`, which GTK/cairo then uploads to the window. Roughly 8.3 MB of
   conversion plus 8.3 MB of upload at the display refresh rate ≈ **500 MB/s** of host
   work at 60 Hz — and this occurs regardless of how efficient the guest becomes, as long
   as pages keep being dirtied.

Full HD roughly doubles both host components relative to 1024×768, which is why the
host-side fix (virtio-gpu, §2.6) matters more at this resolution than the guest-side one.

Components 2 and 3 are attributable to the *emulated VGA device*, not to CLUU. Guest-side
`top` cannot observe them. **The split between (1) and (2)+(3) is unmeasured and is the
first task of Phase 0** (§4). It determines how much of the projected win in §3.2 is real
host savings versus guest-only savings.

### 2.6 Assessment of virtio-gpu

For **guest** CPU, virtio-gpu is not the primary fix. The framebuffer is already
WC-mapped, so virtio-gpu 2D (`RESOURCE_CREATE_2D` + `TRANSFER_TO_HOST_2D` +
`RESOURCE_FLUSH`) trades WC-BAR writes for a hypercall plus a host-side memcpy — marginal.

For **host** CPU, virtio-gpu is a strong candidate, because it removes components (2) and
(3) of §2.7:

- Guest writes land in a plain guest-RAM resource with **no dirty-page logging**, so the
  write-protect VM exits disappear entirely.
- `RESOURCE_FLUSH` carries an **explicit damage rect**, so QEMU converts only the changed
  region instead of rescanning the framebuffer.
- `VIRTIO_GPU_F_RESOURCE_BLOB` (if the host supports it) lets the host map the guest
  buffer directly, so `TRANSFER_TO_HOST_2D` can be elided. **Blob support is an optional
  host capability, not a guarantee** — the classic 2D path
  (`CREATE_2D` + `ATTACH_BACKING` + `TRANSFER_TO_HOST_2D` + `RESOURCE_FLUSH`) is the
  baseline and must work without blobs. No virgl/Venus/3D.
- Additionally: modeset, resolution changes, multi-scanout, hardware cursor.

**Ordering is nevertheless unchanged: displayd first.** virtio-gpu's benefit is
proportional to damage-rect quality, and damage today is always full-window
(`sdl2-shim/src/lib.rs:203-208`, §2.4 defect 2). Adopting virtio-gpu against the current
model would mean `RESOURCE_FLUSH(whole screen)` every frame, trading dirty-logging exits
for a full-screen host copy and capturing only a fraction of the available win. displayd
is what produces real damage rects, so it is the prerequisite that makes virtio-gpu pay.

---

## 3. Architecture

### 3.1 Process shape

```
                    ┌────────────────────────────────────────┐
                    │ displayd — owns fb / virtio-gpu        │
                    │ surface list: z-order, geometry,       │
                    │ damage, integer scale.                 │
                    │ ONE composite pass → scanout.          │
                    └──▲───────────▲──────────▲──────────────┘
       WM control ─────┘           │          │   surface protocol
                    │              │          │   (SHM buffers, damage, present/release)
            ┌───────┴──────┐  ┌────┴───┐  ┌───┴──────┐
            │ compositor   │  │ DOOM   │  │ imgview  │
            │ TUI surface  │  │ (SDL2) │  │ nesemu…  │
            │ + WM policy  │  └────────┘  └──────────┘
            └──────────────┘
```

**displayd knows pixels only.** No cells, no glyphs, no text. The compositor rasterizes
its cell grid into its own surface exactly as `flush_grid_to_backbuf` does today — that
code survives unchanged; only its destination moves from `fb_ptr` to a surface buffer.

**The compositor is a privileged client.** It holds a WM capability on displayd that lets
it set geometry, z-order, and visibility of *other* surfaces. It is no longer in the pixel
path for other applications' content.

**Input routing is unchanged.** vtmgr/inputd deliver to the focused client; the compositor
decides focus. Out of scope for this design.

### 3.2 Copy-pass budget

| | today | after |
|---|---|---|
| windowed pixel surface | 4 passes | **1** (displayd composite, integer scale folded in) |
| fullscreen pixel surface | 2 passes + BAR steal | **1** composite, or **0** when promoted to direct scanout (opportunistic, exact-size + backend-compatible only) |
| TUI text | 1 pass (tiny dirty rects) | 2 passes (tiny dirty rects) — negligible |

At a 1280×800 destination on a 1920×1080 screen: **13 MB/frame → 4 MB/frame**, ≈460 MB/s →
≈143 MB/s at 35 fps. A ~3.4× reduction in guest traffic. Fullscreen promotion, when it
applies, takes it to zero guest copies; promotion is **not guaranteed** and depends on
the backend and exact surface/output dimensions. The remaining host-side cost is
addressed separately by virtio-gpu (§2.6, §2.7).

Fullscreen is displayd opportunistically pointing scanout at the client's buffer when
the backend supports it; otherwise it falls back to a composite pass. No `VT_DEACTIVATE`,
no BAR handoff. If anything needs to draw on top (compositor overlay, hotkey menu, status
line), displayd demotes back to a composite pass for that frame and re-promotes
afterwards. With a blob-capable virtio-gpu backend, promotion lets the host read the
client's buffer directly — zero guest copies for that frame. Blob support is opportunistic
and must not be assumed; the classic 2D path is always available.

### 3.3 Surface protocol

```
surface_create(w, h, format, n_buffers=2) -> {surface_id, buffer_tokens[n]}
surface_acquire(surface_id) -> buffer_idx        // blocks until a FREE buffer is available
surface_present(surface_id, buffer_idx, damage_rects[], src_rect, dst_rect)
                                                 // nonblocking commit; returns immediately
surface_destroy(surface_id)

// WM capability required:
surface_set_geometry(surface_id, x, y, w, h, z)
surface_set_visible(surface_id, bool)
```

**Double-buffered, server-owned.** displayd allocates and maps the backing memory for
every buffer and retains lifecycle ownership. Clients receive buffer tokens (frame
capabilities) they can map for writing, but the server controls layout, lifetime, and
reclamation. This is the same `space_map_range` + frame-token mechanism `PixelRegion` uses
today — no new syscall — but the allocation authority is displayd's, not the client's.

**`present` is a nonblocking commit.** It hands the named buffer to displayd and returns
immediately; displayd schedules the composite/scanout at the next frame boundary. A client
that renders faster than displayd composites is not throttled by `present` — it is
throttled by `acquire`, which **blocks** when no FREE buffer is available (both buffers
still queued or displayed). This provides frame pacing and backpressure for free, and
deletes `DG_SleepMs(1000/35)` along with the resulting jitter. displayd never waits on
clients; it composites whatever buffers are currently committed.

**Authority is per-session and per-surface.** A numeric `surface_id` alone grants nothing.
`surface_create` is reached through a per-session display:client endpoint whose token is
delivered at spawn via the envelope; each created surface returns its own buffer tokens,
and only the holder of those tokens can present to that surface. Window-management
operations live behind a separate `display:wm` endpoint (§3.1, §1.5 of the coder
contract). No runtime ACL, no `sender_tid` interrogation — if a client cannot name the
token, it cannot reach the operation. This preserves the CLUU session and capability
invariants (AGENTS.md §3, §5).

**Damage is a rect list**, capped at a small N (8) per present; overflow degrades to the
bounding box. displayd composites the union of damage across all surfaces for a frame.

**`src_rect`/`dst_rect` express integer nearest-neighbour scaling, performed by displayd
inside the single composite pass.** Emulators submit native resolution (320×240, 256×240,
160×144) and never scale themselves. This is the most important API decision for making
ports feel good — it removes an entire pass from every application.

**Formats:** `XRGB8888` (opaque; fast path is `copy_nonoverlapping` per row) and
`ARGB8888` (alpha-blended). `RGB565` and `INDEX8` + palette are deferred until an actual
consumer needs them.

### 3.4 displayd internals

- `Output` — a scanout: `{width, height, pitch, format, backend}`.
- `Backend` trait, two implementations sharing one interface:
  - `linear_fb` — WC-mapped BAR, as today.
  - `virtio_gpu` — classic 2D (CREATE_2D, ATTACH_BACKING, SET_SCANOUT, TRANSFER_TO_HOST_2D, RESOURCE_FLUSH); additionally implements `try_direct_scanout` for opportunistic zero-copy promotion when backend supports it. Blob resources are not a baseline feature.

  ```
  trait Backend {
      fn scanout_buffer(&mut self) -> &mut [u32];
      fn flush(&mut self, rects: &[Rect]);
      fn try_direct_scanout(&mut self, buf: &SurfaceBuffer) -> bool;  // false = must composite
      fn set_mode(&mut self, w: u32, h: u32) -> Result<()>;           // linear_fb: unsupported
  }
  ```

- **Composite pass:** for each damage rect, walk surfaces back-to-front (painter's
  algorithm), clip to the rect, blit rows. Opaque unscaled rows use
  `copy_nonoverlapping`; scaled rows use an integer step; `ARGB8888` rows blend. Fully
  occluded regions are skipped. No volatile stores anywhere; one `sfence` before
  `flush()`.

Adding the virtio-gpu backend is therefore additive, not a refactor. linear-fb remains as
a fallback permanently.

### 3.5 audiod — mixer server

```
virtio-snd (thin driver, exactly ONE session: audiod's)
    ▲ AUDIO_SUBMIT_PCM / AUDIO_COMPLETE
    │
 audiod  ── resample → mix (saturating i32→i16) → device period
    ▲ SHM ring per stream
    │
 clients: DOOM (via SDL2), cluuamp, notification beeps
```

```
stream_open(rate, channels, format, ring_bytes) -> {stream_id, ring_token, ctl_ep}
stream_write_advance(stream_id, frames)      // client bumps write ptr, fire-and-forget
stream_set_volume(stream_id, q8_8)
stream_pause(stream_id, bool)
stream_drain(stream_id)
stream_close(stream_id)
    -> (async) stream_low_water(stream_id)   // wake client to refill
```

**Pull model, driven by `AUDIO_COMPLETE`.** On each period completion audiod pulls
`PERIOD_FRAMES` from every active stream, resamples to the device rate (linear
interpolation is sufficient), mixes with saturation, and submits. A stream that underruns
contributes silence; there is no global glitch.

**virtio-snd stays a driver.** Mixing, volume, and resampling are policy and live in
audiod. This matches how every other CLUU subsystem is split.

**Latency:** `PERIOD_BYTES` is currently 4096 (`virtio-snd/src/session.rs:30`) = 1024
stereo s16 frames = 23 ms at 44.1 kHz. That is too chunky for game SFX. **Initial target:
2048 bytes (512 frames, 11.6 ms)** with a correspondingly larger `RING_SLOTS`. A measured
experiment at **1024 bytes (256 frames, 5.8 ms)** follows once audiod is driving the
device; QEMU's virtio-sound may not sustain it, and 2048 is the fallback if not. The T2
baseline (`.omo/evidence/task-2-cluu-multimedia-stack.md`) did not exercise audio periods —
this decision is a forward-looking binding, not a measured result, and the 1024-byte
experiment is owned by the audiod phase.

cluuamp migrates off its direct virtio-snd session. DOOM may keep its own 8-channel mix in
`i_cluu_sound.c` and present audiod with a single stream — either arrangement works.

### 3.6 SDL2 — upstream port with a CLUU backend

SDL 2.30.x, built static, `--disable-shared`, all subsystems disabled except video, audio,
events, timer, thread. The **exact SDL revision is pinned in T14** — this spec does not
hardcode a revision; T14 selects and vendors the pinned revision with a small documented
patch series.

Backend files to write (logical responsibilities — T14 determines the final file count
and may add platform-config, libc, or thread-gap shims):

| Path | Responsibility |
|---|---|
| `src/video/cluu/SDL_cluuvideo.c` | `VideoBootStrap`, `VideoInit`, display mode list from displayd |
| `src/video/cluu/SDL_cluuwindow.c` | `CreateSDLWindow` → `surface_create`; fullscreen → scanout promotion |
| `src/video/cluu/SDL_cluuframebuffer.c` | `CreateWindowFramebuffer` / `UpdateWindowFramebuffer` → `surface_present` + damage |
| `src/video/cluu/SDL_cluuevents.c` | `PumpEvents` → `SDL_SendKeyboardKey` / `SDL_SendMouseMotion` from compositor input forward |
| `src/audio/cluu/SDL_cluuaudio.c` | `SDL_AudioDriverImpl` → audiod |

**Key leverage: no renderer is written.** Implementing `CreateWindowFramebuffer` is
sufficient — SDL's built-in software renderer sits on top and provides correct
`SDL_RenderCopy` with src/dst rects, `SDL_Texture`, blend modes, `SDL_Surface`, and
`SDL_BlitScaled` for free. `SDL_RENDERER_ACCELERATED` falls back to software
automatically; DOOM needs a small honest flags patch (it requests accelerated, gets
software, and that is the correct outcome). The five logical responsibilities above are
the CLUU backend surface — **the total SDL scope (file count, patch series, libc/thread
gaps) is determined in T14, not fixed here.**

`src/timer/unix` and `src/thread/pthread` are expected to build unmodified against
`userspace/libcluu/src/posix/pthread.rs` (create/join/mutex/cond all present) and
`clock_gettime`. Expect to stub `SDL_GetBasePath`, `dlopen`, and `sem_timedwait`.

**Transitional shim.** `userspace/sdl2-shim/` is not deleted immediately. It is **frozen**
(no new features, bug fixes only) at the start of the SDL port and **deleted in T19** once
the upstream SDL2 port runs DOOM via stock `doomgeneric_sdl.c`. `doom-cluu` then deletes
`doomgeneric_sdl_cluu.c`. That deletion is the proof the port is real.

---

## 4. Phase 0 — confirm the diagnosis before building

The 35% is host-side (§2.7). Guest and host costs must be separated before any of §3 is
built, because they have different fixes: guest cost is addressed by displayd, host cost
by virtio-gpu.

**T2 already produced the `-display none` baseline**
(`.omo/evidence/task-2-cluu-multimedia-stack.md`): steady-state vCPU is 4–5% across all
four harness cases, DOOM frame cadence is 3.6–4.3 fps, and the shim update loop dominates
guest cycles (62–87M TSC per frame). That baseline is the reference for every performance
gate in this spec. The remaining Phase 0 work is the `-display gtk` host-thread split
below, which T2 did not cover (it pinned `-display none` for reproducibility).

**Step 1 — split host CPU (minutes, no code).** QEMU names its threads, so:

```
top -H -p $(pidof qemu-system-x86_64)
```

Record `qemu:vcpu0` versus the main/display thread in four states — the same four the T2
harness already covers: `l2_baseline_idle_tui`, `l2_baseline_quiet_shell`,
`l2_baseline_doom_windowed`, `l2_baseline_doom_fullscreen` (run via
`python -m cluu_harness --case <name>`). Re-run once with `-display none` (T2 already has
this) and once with `-display gtk` for comparison. If host load collapses under
`-display none`, component (3) of §2.7 dominates and virtio-gpu is the high-value fix. If
`vcpu0` stays high regardless, the guest pipeline dominates and displayd is.

Optional sharpening: `perf kvm stat` or `perf stat -e kvm:kvm_page_fault` on the QEMU pid
quantifies component (2), the dirty-logging exits, directly.

**Step 2 — attribute guest CPU.** `BENCH_COMP_BLIT` (`compositor/src/render.rs:99-114`) is
the existing hook; generalize it.

1. Per-process `cpu_ticks` deltas from `top` for doom, compositor, virtio-snd, vtmgr.
2. rdtsc brackets, reported on COM2 every 100 frames, around:
   - `doomgeneric_Tick` (DOOM's own render, pass 1)
   - the `SDL_UpdateTexture` scale loop (`sdl2-shim/src/lib.rs:338`, pass 2)
   - `flush_pixel_regions_to_backbuf` (pass 3)
   - `flush_backbuf_to_fb` (pass 4)
3. Same four baselines as Step 1, so host and guest numbers correspond.

**Step 3 — control experiment.** Remove `write_volatile` from the shim scaler
(`sdl2-shim/src/lib.rs:358`, `:380`) and shrink the pixel region from 160×50 cells to
80×25 (= 640×400, eliminating the second upscale entirely). Rebuild, re-measure both host
and guest. This tests the copy-count diagnosis for roughly an hour of work, and the
region-size change alone should cut passes 2–4 by 4×.

Cost: approximately one day total. If the numbers indicate something other than
copy-bound guest behaviour plus VGA-emulation host behaviour, this design changes — and
that is discovered for one day of effort rather than three weeks.

**Exit gate:** a table of host-thread CPU and guest per-process CPU across the four T2
harness cases (`l2_baseline_idle_tui`, `l2_baseline_quiet_shell`,
`l2_baseline_doom_windowed`, `l2_baseline_doom_fullscreen`), with the fraction
attributable to the guest pipeline versus VGA emulation stated explicitly. The T2
`-display none` baseline is the reference column; the new `-display gtk` column is the
delta. If VGA emulation dominates by a wide margin, promote virtio-gpu from Phase 4 to
run concurrently with Phase 1 (§5).

---

## 5. Sequencing

| Phase | Work | Exit gate |
|---|---|---|
| **0** | Measurement (§4) — host/guest split first | Host-vs-guest attribution table; copy-bound confirmed |
| **1** | `displayd` + surface protocol; linear-fb backend; compositor becomes a client holding the WM cap; existing shim retargeted to surfaces | DOOM windowed CPU at target; TUI visually unchanged |
| **2** | Upstream SDL2 port (exact revision pinned in T14); `sdl2-shim` frozen (bug fixes only); shim + `doomgeneric_sdl_cluu.c` deleted in T19 after stock `doomgeneric_sdl.c` validates | Stock doomgeneric builds and runs against CLUU backend |
| **3** | `audiod` + SDL2 audio backend; cluuamp migrated | DOOM music + SFX + cluuamp simultaneously |
| **4** | virtio-gpu backend behind the `Backend` trait, classic 2D with opportunistic direct scanout; linear-fb retained as fallback | Fullscreen ≤ 1 copy; direct scanout when backend-compatible; modeset works |
| **5** | Port a NES emulator | API validated without modification |

Phase 5 is a test, not a victory lap. If porting an emulator requires changing displayd or
the SDL backend, the API is wrong, and that is discovered cheaply.

---

## 6. Risks

**Phase 1 is the substantial one.** `compositor/src/window_mgr.rs` (905 lines) and
`compositor/src/state.rs` (479 lines) both assume the compositor owns `fb_ptr`. Separating
"produce the TUI surface" from "tell displayd where windows go" is the real work; the wire
protocol is the easy part. Expect `state.rs` to split into TUI-surface state and
WM-policy state.

**Kernel freeze.** Nothing here requires kernel changes. Surfaces are SHM via the existing
`space_map_range` plus frame-token path, exactly as `PixelRegion` works today. virtio-gpu
is PCI plus virtqueues, following the existing virtio driver pattern. If any phase appears
to need a new syscall, that is a design error to be resolved in userspace.

**Audio latency target may be unreachable.** The initial period is 2048 bytes (11.6 ms).
The 1024-byte (5.8 ms) experiment depends on QEMU's virtio-sound implementation keeping
up. Fallback is 2048 bytes — which is the initial default, not a degradation. Measure in
the audiod phase before committing to 1024.

**Blocking `acquire` and deadlock.** A client that never calls `present` must not stall
displayd. displayd never waits on clients; it composites whatever buffers are currently
committed. Only the *client* blocks in `acquire`, waiting for a FREE buffer. A client that
holds a buffer and never presents it will block on its next `acquire` — that is correct
backpressure, not a deadlock, because displayd reclaims buffers on its own frame boundary
regardless of client behaviour.

---

## 7. Decisions recorded

| Decision | Choice | Rationale |
|---|---|---|
| Display ownership | `displayd` created now as sole hardware owner; compositor becomes a session-aware WM/TUI client | Cleanest layering; removes the dual blit path; accepts lifecycle complexity for a clean long-term boundary |
| Surface buffers | Server-owned double buffers (displayd allocates/maps backing, retains lifecycle ownership) | Backend controls layout/lifetime; safe queued/displayed ownership; same frame-token mechanism as `PixelRegion`, no new syscall |
| Presentation | `present` = nonblocking commit (returns immediately, displayd schedules display); `acquire` = blocking (blocks when no FREE buffer) | Correct double-buffer lifecycle; free backpressure; deletes fixed-sleep jitter; displayd never waits on clients |
| Authority | Per-session creation/WM endpoints and per-surface capabilities; no global numeric-ID authority or runtime ACL | Preserves CLUU session and capability invariants (AGENTS.md §3, §5) |
| Scaling | displayd performs integer nearest scale | Removes a pass from every emulator/game port |
| virtio-gpu | Classic 2D only (`CREATE_2D`, `ATTACH_BACKING`, `SET_SCANOUT`, `TRANSFER_TO_HOST_2D`, `RESOURCE_FLUSH`); blobs/virgl/Venus/3D deferred; direct scanout opportunistic, not guaranteed | Portable and spec-defined; blob/direct-scanout are optional host capabilities, not promises |
| SDL2 | Port upstream with a CLUU backend; exact revision pinned in T14; transitional `sdl2-shim` frozen then deleted in T19 | Real API semantics; scope (file count, patch series) determined in T14, not fixed here |
| Audio | `audiod` mixer server; virtio-snd stays thin; initial/default 2048-byte periods, measured 1024-byte experiment (2048 is the fallback if 1024 shows regressions) | Policy out of drivers; enables simultaneous audio; 1024 is an experiment, 2048 is the initial default and fallback |
| Performance gates | Relative to T2 baseline (`.omo/evidence/task-2-cluu-multimedia-stack.md`), not an absolute CPU percentage | Host/QEMU-dependent; a fixed percentage target is not defensible before measurement |
| Compositor scope | WM policy stays in compositor (holds `display:wm` cap) | One composite pass; preserves the existing process split |

**Corrections recorded (2026-07-26, T3):** the prior "Approved" status was premature.
The following claims were removed or qualified because they were not measurement-grounded:
a fixed absolute guest CPU budget (replaced by relative-to-T2 gates); the guaranteed
blob-elision / transfer-is-skipped claim (requalified as opportunistic host capability);
the fixed SDL file-count scope claim (scope is T14's call); the blocking-commit
presentation model (replaced by nonblocking commit + blocking acquire); client-side
buffer allocation (replaced by server-owned double buffers). The 35%-host-CPU
identification (§2.7) and the four-pass pipeline diagnosis (§2.3) stand — T2 confirmed
the vCPU baseline and frame cadence under `-display none`.

**Correction recorded (§2.7):** an earlier reading of this problem treated the 35% as
guest CPU and concluded virtio-gpu was low-value. With the figure identified as host CPU
(§2.7), virtio-gpu's value is substantially higher — it eliminates dirty-page write-protect
exits and full-framebuffer rescans, neither of which any guest-side change can address.
The sequencing is unchanged only because damage rects are its prerequisite.
