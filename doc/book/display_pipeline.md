# Display Pipeline

CLUU keeps display ownership in userspace. `displayd` owns the hardware output;
the compositor owns the normal direct framebuffer grant and turns window damage
into display commits. Applications do not write the hardware framebuffer
directly unless an explicit exclusive-display handoff protocol exists.

## Windowed and fullscreen application paths

Windowed applications allocate a `PixelRegion` frame token, attach it to a
compositor window, render into the shared mapping, and send
`COMP_WIN_DAMAGE_LABEL`. The compositor maps the grant, copies changed pixels
into its direct framebuffer, and sends a damage-only commit to displayd. The
compositor remains the sole direct framebuffer owner.

Windowed xnes uses this pipeline with compositor chrome. Fullscreen xnes uses
an explicit `displayd` direct-display lease: displayd transfers the framebuffer
grant to xnes, compositor visual work is quiesced, and xnes submits only the
changed game rectangle. Keyboard events remain routed through the lease's input
endpoint, and audio remains independent through audiod's shared ring.

DOOM uses the same split through SDL's CLUU backend. Windowed DOOM attaches a
compositor `PixelRegion`; startup with `-fullscreen` acquires a direct lease,
accepts raw keyboard events on the lease endpoint, and releases the lease in
the ordered release/unmap/ack sequence. Runtime SDL fullscreen transitions are
rejected rather than pretending to change ownership. DOOM sound uses SDL's
CLUU audio driver, which submits PCM through audiod; music remains unsupported.
The raw Ctrl-Alt-X close chord is handled by the SDL backend, matching xnes,
so `SDL_QUIT` reaches Doom teardown and returns the display lease to the
compositor.

This path avoids xnes sending a full frame token to displayd. That older path
made displayd map, copy, composite, and flush every frame. Fire-and-forget
commits accumulated stale frames; synchronous commits bounded the queue but
blocked emulation and starved audio.

## Direct framebuffer ownership

The current compositor path has one direct framebuffer owner. Fullscreen
applications that become direct `displayd` clients use an explicit exclusive
lease/handoff protocol:

1. Quiesce compositor visual work, cursor, status row, and damage commits.
2. Keep compositor keyboard routing alive.
3. Atomically transfer direct framebuffer ownership to the application.
4. Accept damage-only commits only from current lease owner/generation.
5. Revoke or recover ownership on release or client death.
6. Reacquire the compositor grant and force a full repaint.

Lease ownership transitions are explicit and rollback-safe; a second direct
grant cannot silently supersede the current owner. Release and client-death
paths restore compositor ownership before normal windowed rendering resumes.

Direct fullscreen requires a displayd backend capable of granting the scanout
mapping. If that backend is unavailable, SDL reports fullscreen creation
failure instead of silently falling back to composited fake fullscreen.

## Performance rules

- Never enqueue unbounded fullscreen frame commits.
- Never use frame skipping to hide a slow display path.
- Keep emulation/audio cadence independent from display transfer latency.
- Use actual framebuffer pitch for direct rendering.
- Preserve aspect ratio by rendering the NES image into a centered fit
  rectangle, leaving letterbox bars in the direct target.

## Relevant implementation

- `userspace/xnes-cluu/src/main.rs`: xnes frame pacing, audio, input, and
  windowed rendering, and direct fullscreen lease lifecycle.
- `userspace/libcluu/src/pixel_region.rs`: client-side shared pixel grant.
- `userspace/compositor/src/window_mgr.rs`: fullscreen/no-chrome window and
  pixel-region mapping.
- `userspace/compositor/src/render.rs`: shared-pixel copy and damage commit.
- `userspace/displayd/src/main.rs`: display surface ownership and commit
  dispatch, including direct-display lease ownership.
- `userspace/displayd/src/virtio_gpu_backend.rs`: direct framebuffer grant and
  GPU transfer/flush.

## Manual validation

On 2026-08-03, fullscreen xnes was manually exercised in QEMU: frame rate and
audio were good, `Ctrl-Alt-X` returned ownership to the compositor, and the
windowed path also rendered with good frame rate and audio. Routine per-input
diagnostics were then removed from xnes; input handling is unchanged.

On 2026-08-04, fullscreen DOOM was manually exercised in QEMU: direct-framebuffer
commits succeeded, audio played, and Ctrl-Alt-X exited through SDL teardown and
returned ownership to the compositor.
