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

Fullscreen xnes uses the same pipeline with `COMP_WIN_FLAG_FULLSCREEN` and
`COMP_WIN_FLAG_NO_CHROME`. The window is pinned to the output and compositor
cursor rendering is suppressed while it is focused. Keyboard events still pass
through the compositor input-capture endpoint, and audio remains independent
through audiod's shared ring.

This path avoids xnes sending a full frame token to displayd. That older path
made displayd map, copy, composite, and flush every frame. Fire-and-forget
commits accumulated stale frames; synchronous commits bounded the queue but
blocked emulation and starved audio.

## Direct framebuffer ownership

The current compositor path has one direct framebuffer owner. Do not grant the
same scanout mapping to xnes while compositor still renders: two writable
owners can race with GPU transfer and overwrite each other's pixels.

Future fullscreen applications that must become direct `displayd` clients need
an explicit exclusive lease/handoff protocol:

1. Quiesce compositor visual work, cursor, status row, and damage commits.
2. Keep compositor keyboard routing alive.
3. Atomically transfer direct framebuffer ownership to the application.
4. Accept damage-only commits only from current lease owner/generation.
5. Revoke or recover ownership on release or client death.
6. Reacquire the compositor grant and force a full repaint.

The existing `direct_fb_token` singleton is not sufficient for this handoff:
granting a second mapping can silently supersede the first owner. A future
lease must make ownership transitions explicit and rollback-safe.

## Performance rules

- Never enqueue unbounded fullscreen frame commits.
- Never use frame skipping to hide a slow display path.
- Keep emulation/audio cadence independent from display transfer latency.
- Use actual framebuffer pitch for direct rendering.
- Preserve aspect ratio by rendering the NES image into a centered fit
  rectangle, leaving letterbox bars in the direct target.

## Relevant implementation

- `userspace/xnes-cluu/src/main.rs`: xnes frame pacing, audio, input, and
  compositor fullscreen window setup.
- `userspace/libcluu/src/pixel_region.rs`: client-side shared pixel grant.
- `userspace/compositor/src/window_mgr.rs`: fullscreen/no-chrome window and
  pixel-region mapping.
- `userspace/compositor/src/render.rs`: shared-pixel copy and damage commit.
- `userspace/displayd/src/main.rs`: display surface ownership and commit
  dispatch.
- `userspace/displayd/src/virtio_gpu_backend.rs`: direct framebuffer grant and
  GPU transfer/flush.
