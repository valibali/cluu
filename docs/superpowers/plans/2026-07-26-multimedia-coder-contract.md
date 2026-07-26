# CLUU Multimedia — Coder Contract

**Audience:** implementing agents (GLM-5.2 and peers) working any phase of
`docs/superpowers/specs/2026-07-26-multimedia-architecture-design.md`.

**Status:** binding and measurement-grounded. Performance gates are relative to the T2
baseline (`.omo/evidence/task-2-cluu-multimedia-stack.md`), not absolute CPU percentages.
Read this fully before touching code. Every task in every multimedia plan implicitly
includes this document.

You are a skilled developer who does not know this codebase. This document tells you the
rules that are not discoverable from the code in the time you have.

---

## 0. The one-paragraph orientation

CLUU is a hobby x86_64 **capability microkernel**. The kernel knows threads, address
spaces, endpoints, tokens, and typed frames — nothing else. There is no kernel concept of
a process; `procmgr` owns that. Userspace is C-capable via newlib
(`x86_64-cluu-elf`). Every authority is an unforgeable token, and a child's view of
authority narrows monotonically from its parent's — it can never widen. Drivers and
services are ordinary userspace processes described by a `Cluufile`.

---

## 1. Hard rules — violating any of these fails review automatically

### 1.1 No new syscalls

Every new verb goes through an existing IPC invoke op. If your design appears to need a
new syscall, the design is wrong; solve it in userspace. The kernel is frozen through
approximately 2026-10-21 and every kernel commit must name the userspace failure that
forced it.

Surfaces need no kernel work: they are SHM frames. **displayd allocates** the backing
frames (server-owned double buffers) and hands clients buffer tokens they map with
`syscall::space_map_range(..., MAP_FRAME_TOKEN)` — the same mechanism
`userspace/libcluu/src/pixel_region.rs:48-90` uses. The client maps and writes; the server
owns layout, lifetime, and reclamation. Copy that pattern, but with displayd as the
allocator, not the client.

### 1.2 `send_msg_with_payload` clobbers `words[0]`

Any message sent with a payload has `words[0]` overwritten by the transport. **Put real
data in `words[1..6]` only.** This has already caused a production bug (`top` showed the
wrong pid). If your message carries a payload, `words[0]` is scratch.

### 1.3 IRQ handlers must never take a blocking lock

If you touch interrupt context, use `try_lock` and tolerate the miss. See
`kernel/src/sched/thread_manager.rs:1430-1443` for the canonical comment explaining why a
blocking lock inside the timer ISR is a total kernel halt. This applies to the virtio-gpu
and audio phases.

### 1.4 No timeouts as deadlock guards

Never write `sleep(n)` or a timeout to "make the race go away". If two components can
deadlock, fix the protocol. Timeouts are acceptable only as genuine protocol deadlines
(for example a recv loop that must service a periodic tick), never as synchronisation.

### 1.5 Capability discipline

A capability is not an ACL check. Do not write `if sender_tid == owner_tid`. Authority is
possession of a token. If a client should not be able to invoke an operation, it must not
hold the token that names that operation's endpoint — the operation must be physically
unreachable, not conditionally refused.

Concretely for `displayd`: window-management operations (`set_geometry`, `set_visible`,
`set_z`) live behind a **separate endpoint** from client operations
(`surface_create`/`present`/`destroy`). The compositor holds both; ordinary clients hold
only the client endpoint. Do not implement this as a flag check on one endpoint.

**Per-session and per-surface.** The `display:client` endpoint token is delivered per
session at spawn (envelope-bound), not globally published. Each `surface_create` call
returns its own buffer tokens; only the holder of a surface's tokens can present to it.
A numeric `surface_id` alone grants nothing — the caller must already hold the session's
`display:client` token and the surface's buffer tokens. No runtime ACL, no `sender_tid`
check (AGENTS.md §3). Cross-session surface visibility is a privilege bound to root
godmode (AGENTS.md §6), not a default.

### 1.6 Build and test commands are fixed

Full clean build (after touching newlib, crt0, or syscall stubs):

```bash
rm -rf target/newlib-build target/sysroot/x86_64-cluu-elf && make clean \
  && cargo xtask build-newlib && cargo xtask build-syscalls \
  && cargo xtask build-crt0 && cargo xtask build
```

Normal build: `cargo xtask build`

Headless test: `python -m cluu_harness` (the Python gen2 harness; `scripts/harness_run.sh`
was retired and deleted). Baseline QA cases for multimedia work:
`python -m cluu_harness --case l2_baseline_idle_tui --no-build`,
`l2_baseline_quiet_shell`, `l2_baseline_doom_windowed`, `l2_baseline_doom_fullscreen`.
These four cases are the T2 baseline reference
(`.omo/evidence/task-2-cluu-multimedia-stack.md`) and the performance gates are relative
to them.

Set `HARNESS_FORCE_BUILD=1` after changing anything in the build environment. Use exactly
one minimal marker per test. If a harness run flakes, retry it **once** — if it fails
twice it is a real failure, not flake.

---

## 2. SOLID, as it applies to this codebase

These are not abstract principles here; each one maps to a specific decision you will face.

### 2.1 Single Responsibility — split by responsibility, not by layer

The existing compositor is the positive example. `userspace/compositor/src/` splits into
`protocol.rs` (wire format), `compose.rs` (which cell wins), `render.rs` (pixels move),
`state.rs` (what exists), `window_mgr.rs` (policy). Follow that shape.

For `displayd` this means four distinct responsibilities that must not be merged:

| Module | Responsibility | Must NOT know about |
|---|---|---|
| `protocol.rs` | wire encode/decode only | pixels, hardware, geometry policy |
| `scene.rs` | what surfaces exist, where, in what order, what is damaged | how a pixel is written |
| `compose.rs` | given a scene and a target buffer, move pixels | IPC, hardware, tokens |
| `backend/` | how a buffer reaches the display | surfaces, z-order, clients |

**Test:** if you cannot unit-test `scene.rs` with no framebuffer, no IPC, and no QEMU, the
boundary is wrong. `scene.rs` and the rect/clip math must be testable with plain
`cargo test` on the host.

**Warning sign:** `window_mgr.rs` is already 905 lines and `state.rs` 479. When a file in
your work passes ~400 lines, that is a signal it has taken on a second responsibility.
Split it then, not later.

### 2.2 Open/Closed — the Backend trait is the whole point

Phase 4 adds virtio-gpu. If adding it requires editing `compose.rs` or `scene.rs`, Phase 1
was built wrong. The only correct diff for Phase 4 is: one new file implementing
`Backend`, plus one line selecting it.

```rust
pub trait Backend {
    fn size(&self) -> (u32, u32);
    fn scanout_buffer(&mut self) -> &mut [u32];
    fn flush(&mut self, rects: &[Rect]);
    fn try_direct_scanout(&mut self, buf: &SurfaceBuffer) -> bool;
    fn set_mode(&mut self, w: u32, h: u32) -> Result<()>;
}
```

Write `linear_fb` against this trait in Phase 1 even though it is the only implementation.
Resist the urge to "simplify" by calling the framebuffer directly — that shortcut is
precisely the cost Phase 4 pays.

### 2.3 Liskov Substitution — degrade honestly, never lie

`try_direct_scanout` returns `bool`. `linear_fb` returns `false` unconditionally;
`virtio_gpu` returns `true` when it can promote. Callers must handle `false` on every
path. Do not add `if backend_is_virtio_gpu` anywhere — that is the substitution violation.

Same rule for `set_mode`: `linear_fb` returns `Err(Unsupported)`. Callers handle the error;
they do not interrogate the backend's identity.

### 2.4 Interface Segregation — two endpoints, not one endpoint with a flag

Stated in §1.5 as a capability rule; it is also an ISP rule. A DOOM process must not be
able to name `set_geometry` at all. Two endpoints:

- `display:client` — `surface_create`, `surface_present`, `surface_destroy`
- `display:wm` — `surface_set_geometry`, `surface_set_visible`, `surface_set_z`

Registry names are separate, message label ranges are separate, dispatch functions are
separate.

### 2.5 Dependency Inversion — compose depends on abstractions

`compose.rs` takes `&Scene` and `&mut dyn Backend`. It never imports the framebuffer
module, never reads a registry, never sends IPC. This is what makes it testable against an
in-memory `Vec<u32>` backend, which is how you will actually test the compositing maths.

Write that in-memory test backend in Phase 1. It is not throwaway — it is your entire
regression suite for blending, clipping, and scaling.

---

## 3. Performance rules specific to this work

These exist because the current code violates all of them; see spec §2.3–§2.4.

1. **Never `write_volatile` a pixel.** Neither shared memory nor a write-combining
   mapping requires volatile semantics. Volatile stores forbid LLVM from emitting SIMD or
   `memcpy`, costing one store per pixel. Write normal slices; emit one `sfence` before
   `Backend::flush`.

2. **Prefer `copy_from_slice` / `copy_nonoverlapping` over per-pixel loops.** The opaque
   unscaled row case must be a single row-length copy. Only blended or scaled rows may
   loop, and those loops must not contain a division — precompute the integer step.

3. **Never scale in the application.** Scaling is `displayd`'s job, expressed as
   `src_rect` → `dst_rect` and folded into the one composite pass. Any application-side
   upscale is a redundant full-frame pass.

4. **Damage must be real.** `0xFFFF, 0xFFFF` is banned. If you genuinely do not know what
   changed, submit the true bounding box, and add a comment saying why. Full-screen damage
   defeats the entire architecture and specifically destroys the virtio-gpu win in Phase 4.

5. **Cost scales with destination area, not source area.** A 320×200 game scaled to
   1280×800 costs 1M pixel writes, not 64K. When choosing default window sizes, know what
   you are paying.

6. **No busy-wait, no fixed-sleep frame pacing.** Pacing comes from blocking on
   `surface_acquire` (blocks when no FREE buffer is available). `surface_present` is a
   nonblocking commit — it returns immediately and displayd schedules the composite.
   `DG_SleepMs(1000/35)` in the current DOOM loop is exactly the anti-pattern being
   removed.

---

## 4. Process and service conventions

### 4.1 Adding a new service

A service needs, at minimum:

1. `userspace/<name>/Cargo.toml` + `src/main.rs`
2. `containers/<name>/Cluufile`
3. Registration in the workspace `Cargo.toml` members list
4. A registry name via `libcluu::registry::init(...)` and
   `registry::register_output(...)`

Copy `containers/compositor/Cluufile` for a display-adjacent service:

```
FROM minimal
PROFILE ipc registry device space_grant
PRELOAD
ENDPOINT grantable
DEVICE framebuffer
BUILD "cargo build --manifest-path userspace/<name>/Cargo.toml --target triplets/x86_64-cluu-user.json -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem" target/x86_64-cluu-user/debug/<name>.elf /bin/<name>
ENTRYPOINT /bin/<name>
RESTART always
```

`PROFILE` grants authority classes. `DEVICE framebuffer` is what permits the framebuffer
mapping — a service without it physically cannot map the display.

### 4.2 The main loop shape

Services are event-driven and block in `ipc_recv_any_with_sender` with a long timeout,
looping on `Timeout`. See `userspace/compositor/src/main.rs:223-240`. Do not poll. Do not
spin. Do not use a short timeout as a substitute for a wakeup.

### 4.3 Registry and READY

Defer the READY notification until the dispatch loop is actually running and has entered
`recv_any` — sending it earlier races with clients that immediately connect. The
compositor documents this at `userspace/compositor/src/main.rs:160-168`. Same pattern for
`displayd`.

### 4.4 Registry does not re-Grant on entry replacement

If a replaceable service name is re-registered, existing subscribers are not automatically
re-granted. A client that must survive a service restart has to re-Grant itself. Account
for this if `displayd` is `RESTART always`.

---

## 5. Testing

### 5.1 Two test layers, both required

**Host unit tests** for pure logic — rect intersection, damage union, z-order occlusion,
scale-step arithmetic, blend maths. These run with plain `cargo test` and are where you do
TDD. Make this possible by declaring the crate:

```rust
#![cfg_attr(not(test), no_std)]
```

so `cargo test` can link the std test harness while the shipped binary stays `no_std`.
Verify this compiles both ways before building on it.

**Marker probe containers** for integration — a `containers/<probe>/Cluufile` plus a
userspace binary that prints a single `PASS`/`FAIL` marker, registered in
`python/cluu_harness/markers.py`. One minimal marker per test. Existing examples:
`containers/l3_session_create_destroy/`, `containers/fbprobe/`.

### 5.2 Visual verification is not optional

Compositing bugs pass unit tests and look wrong. After any change to `compose.rs` or a
backend, boot the system and capture the framebuffer (or run a harness baseline case with
a visual marker) and actually look at the output. State in your report what you saw.
There is no `scripts/fb_dump.sh` — if you need a screenshot, use the QEMU monitor
`screendump` command or the harness visual-marker path.

### 5.3 Report honestly

If a test fails, say so and paste the output. If you skipped a step, say which and why. Do
not report "done" for work that is partially done — report what is complete, what is not,
and what blocked it. A truthful partial result is more useful than a confident wrong one.

---

## 6. Things that will waste your day if nobody tells you

- **fd numbering differs from Unix.** fd 0–3 are stdin, stdout, stderr, **stdlog**. Not
  0–2.
- **`GLYPH_W = 8`, `GLYPH_H = 16`.** Compositor geometry is in cells; a "160×50" window is
  1280×800 pixels. Check your units.
- **The display is 1920×1080**, framebuffer 8.3 MB. Do not assume 1024×768.
- **The framebuffer is already write-combining.** `MAP_DEVICE_WC` selects PAT[1];
  `kernel/src/mm/pat.rs` programs it. Do not "fix" caching — it is not broken. Note that
  setting `PCD` would select UC and make things dramatically worse.
- **`MAP_SHARE_PHYS` cache invalidation is currently disabled** at five reset sites,
  pending refcount-aware invalidation. Do not rely on shared-phys pages being invalidated.
- **VFS cache regions currently collide with the pthread stack region** at the 256 MB
  layout. If you allocate large VAs, check `userspace/libcluu/src/boot.rs` constants first.
- **The compositor maps the framebuffer through `/dev/fb0` mmap**, not
  `framebuffer_acquire`. Two paths exist; know which one you are in.

---

## 7. Commit discipline

- Commit frequently, one logical change per commit.
- Conventional prefixes as used in this repo: `feat:`, `fix:`, `docs:`, `refactor:`.
- Plans and specs are always committed.
- Do not commit on `master`. Work happens on `develop` or a feature branch.
- If `git status` shows work-in-progress older than three days on `develop`, stop and
  raise it rather than piling on.

---

## 8. Binding decisions (T3, 2026-07-26)

These are frozen for all implementation tasks (T4–T22). They mirror
`docs/superpowers/specs/2026-07-26-multimedia-architecture-design.md` §7. If anything in
this contract contradicts the spec, the spec wins.

| Decision | Binding |
|---|---|
| Display process | `displayd` created now as sole hardware owner; compositor becomes a session-aware WM/TUI client |
| Surface buffers | Server-owned double buffers — displayd allocates/maps backing, retains lifecycle ownership |
| Presentation | `surface_present` = nonblocking commit (returns immediately); `surface_acquire` = blocking (blocks when no FREE buffer). displayd never waits on clients |
| Authority | Per-session `display:client` / `display:wm` endpoints; per-surface buffer tokens; no global numeric-ID authority, no runtime ACL |
| virtio-gpu | Classic 2D only: `CREATE_2D`, `ATTACH_BACKING`, `SET_SCANOUT`, `TRANSFER_TO_HOST_2D`, `RESOURCE_FLUSH`. No blobs/virgl/Venus/3D as guaranteed features. Direct scanout is opportunistic (`try_direct_scanout` returns `bool`), not promised |
| Fullscreen | One composite pass expected; direct scanout promoted only when exact-size and backend-compatible. Zero guest copies is not guaranteed |
| SDL2 | Upstream port with CLUU backend. **Exact SDL revision pinned in T14** — do not hardcode a revision now. Total file count and patch series are T14's scope, not fixed here |
| Transitional shim | `userspace/sdl2-shim/` frozen (bug fixes only) at SDL port start; deleted in T19 after stock `doomgeneric_sdl.c` validates. `doomgeneric_sdl_cluu.c` deleted in T19 |
| Audio | `audiod` mixer server; virtio-snd stays thin. Initial/default period 2048 bytes (11.6 ms); measured 1024-byte (5.8 ms) experiment in the audiod phase. 2048 is the initial default and fallback if 1024-byte experiment shows regressions |
| Performance gates | Relative to T2 baseline (`.omo/evidence/task-2-cluu-multimedia-stack.md`), not an absolute CPU percentage. A fixed percentage target is not defensible |
| QA commands | `python -m cluu_harness --case <name>`. Baseline cases: `l2_baseline_idle_tui`, `l2_baseline_quiet_shell`, `l2_baseline_doom_windowed`, `l2_baseline_doom_fullscreen`. `scripts/fb_dump.sh` and `scripts/harness_run.sh` do not exist |

---

## 9. When to stop and ask

Ask rather than guess when:

- A task appears to require a new syscall.
- A task appears to require widening a capability view.
- The spec and the code disagree about an interface.
- A performance result contradicts the spec's projections by more than ~2×.

Do not ask about ordinary implementation choices — naming, local structure, which loop
form to use. Make the call, note it, move on.
