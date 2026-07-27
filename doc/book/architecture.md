# System Architecture

CLUU is a microkernel OS with a strict kernel/userspace split. The kernel
provides three primitives; everything else is composed in userspace services
that communicate via IPC.

The arrow direction matters: userspace never talks to the kernel directly for
filesystem, network, or process operations. It talks to the relevant userspace
service via IPC, and that service uses syscalls only for primitive operations
like sending a message or mapping a page.

## The kernel/userspace boundary

```text
┌─────────────────────────────────────────────────────────┐
│                    KERNEL (ring 0)                        │
│                                                          │
│  ┌─────────┐  ┌───────────┐  ┌──────┐  ┌──────┐        │
│  │ scheduler│  │  memory   │  │ IPC  │  │ token│        │
│  │ (threads)│  │ (PMM/VMM) │  │(rend)│  │ (HMAC)│       │
│  └─────────┘  └───────────┘  └──────┘  └──────┘        │
│         │           │           │          │             │
│         └───────────┴─────┬─────┴──────────┘             │
│                           │                              │
│                    ┌──────┴──────┐                       │
│                    │  syscall    │  7 syscalls           │
│                    │  dispatch   │  + 52 InvokeOps       │
│                    └──────┬──────┘                       │
│                           │                              │
├───────────────────────────┼──────────────────────────────┤
│                    USERSPACE (ring 3)                     │
│                           │                              │
│          ┌────────────────┼────────────────┐             │
│          │                │                │             │
│    ┌─────┴─────┐   ┌──────┴──────┐  ┌──────┴──────┐     │
│    │   init    │   │ root-procmgr│  │   vfs       │     │
│    │  (PID 1)  │   │ (sessions)  │  │ (namespace) │     │
│    └─────┬─────┘   └──────┬──────┘  └──────┬──────┘     │
│          │                │                │             │
│          │         ┌──────┴──────┐  ┌──────┴──────┐     │
│          │         │ session-    │  │ session-    │     │
│          │         │ procmgr     │  │ vfs         │     │
│          │         │ (per login) │  │ (per login) │     │
│          │         └──────┬──────┘  └──────┬──────┘     │
│          │                │                │             │
│    ┌─────┴─────────────────┴────────────────┴─────┐     │
│    │  shell, edit, cluuterm, compositor, kbd,     │     │
│    │  tty, console, vtmgr, ext2, virtio-blk,      │     │
│    │  devmgr, registry, tpmd, timeserver, ...     │     │
│    └───────────────────────────────────────────────┘     │
└──────────────────────────────────────────────────────────┘
```

## Kernel subsystems

Source under `kernel/src/`. Seven subsystems, each with a narrow charter.

| Subsystem | Owns | Does not own |
|---|---|---|
| `architecture/x86_64/` | GDT, IDT, TSS, MSR setup, SYSCALL/SYSRET, CPU features, SMAP/SMEP | Higher-level scheduling, IPC semantics |
| `mm/` | Buddy allocator (PMM), page tables, demand paging, heap, frame registry, address spaces | Anything user-visible (mmap is a thin syscall wrapper around space-map) |
| `sched/` | `Thread` struct, scheduler queues, priorities, FPU/SSE save/restore, suspend/resume | The notion of process (procmgr's job) |
| `token/` | `Token` (cap-handle), `ObjectRef`, HMAC issuance + verification, rights mask, scope, expiration | Persisting tokens across reboot (none of that exists) |
| `ipc/` | `Endpoint`, `Message`, send/recv/call/reply, notifications, rendezvous, payload transfer | Pretty-printing payloads (raw byte slices) |
| `syscall/` | The small syscall table, fast-path SYSCALL entry, ABI marshaling | Anything user-policy (capabilities decide what is allowed, not the syscall itself) |
| `devices/` | IRQ vector dispatch to userspace driver endpoints, PCI scan | Per-device protocol (drivers live in userspace) |

Size signal: 63 `.rs` files, roughly 50 KLOC of Rust plus assembly, zero lines
of in-kernel drivers. All drivers are userspace.

See [The Kernel](../kernel/index.html) for the per-subsystem deep dive.

## The three kernel primitives

### Threads

The kernel scheduler is an O(1) priority bitmap. `ThreadManager` is the
singleton the rest of the kernel calls for `ThreadCreate`, `ThreadResume`,
`ThreadSetPriority`, etc. Threads carry IPC bookkeeping (`CallReplyInfo`) and
fault state inline, because IPC and fault delivery are synchronous and tied to
the running thread.

The kernel does **not** have a process concept. `sched::process` and
`sched::process_manager` were retired in favor of a unified thread model.
Processes are a userspace concept owned by procmgr.

### Capability tokens

Authority flows through HMAC-signed capability tokens. A token binds five
fields under one HMAC-SHA256 signature keyed by a kernel secret:

- **Scope**: opaque identifier preventing cross-scope reuse.
- **Rights bitmask**: which operations the token authorizes.
- **Object reference**: which kernel object the token points at.
- **Expiration timestamp**: kernel-monotonic; revocation is instant.
- **HMAC-SHA256 signature**: covers the above; kernel-only key.

Tokens are unforgeable. Userspace can pass them around freely (through IPC
payloads, for example), but any tampering is caught by signature verification
on the next syscall. Userspace names kernel objects only by presenting a token;
the kernel verifies signature and rights on every operation.

See [Capability Tokens](../capability_tokens/index.html) for the full model and
the rights matrix.

### IPC

Synchronous rendezvous-based message passing. Sender blocks until receiver is
ready and vice versa. No buffered queue inside the kernel. Buffer transfer
supports three modes: `Copy` (safe but slow), `Grant` (transfer page
ownership, zero-copy), `Map` (shared mapping, zero-copy).

The fast path carries up to six register-passed words (`MessageTag` +
`Message` words) without touching memory.

Three message shapes flow through token-protected endpoints:

- **Oneway (`send`)**: fire and forget. Sender deposits a message; receiver
  picks it up with `recv`.
- **Synchronous (`call` / `reply`)**: caller blocks until the server replies.
  `recv` injects a kernel-minted `ReplyId` so the server can reply without
  holding the caller's token. Reply-tokens are kernel-injected, not minted by
  userspace, which closes a whole class of forgery attacks.
- **Async notify**: signal-shaped. Sender sets a bit on the endpoint and wakes
  any receiver already blocked in `recv`. No payload.

Performance, measured in the kernel audit: a full call/reply round-trip is
about 1,200 to 1,600 cycles. Direct delivery (recipient already in `recv`) is
roughly 7x faster than the queued path.

## The syscall surface

7 syscalls: `Send(0)`, `Recv(1)`, `Call(2)`, `Reply(3)`, `Yield(4)`,
`Invoke(5)`, `DebugPrint(255)`.

Argument convention is register-only: RAX holds the number, RDI/RSI/RDX/R10/R8/R9
hold `arg1..arg6`, RAX returns the result or negative errno.

52 `InvokeOp` variants are dispatched through `sys_invoke`: thread management,
address-space management, token derivation, IRQ handling, endpoint creation,
PCI config, I/O ports, clock, frame allocation, notifications.

The design rule: when CLUU needs a new thing the kernel can do, the answer is
almost never "add a syscall." It is "add an `InvokeOp` on the existing
token-dispatch path." New userspace features compose from the existing invoke
surface, not from new syscall entries.

## Userspace service topology

### Boot-critical services (spawned by init)

| Service | Role |
|---------|------|
| `init` | PID 1. Spawns boot-critical services, monitors primordial exits. |
| `registry` | Name → endpoint mapping. Central broker for service discovery. |
| `root-procmgr` | System-scope process manager. Owns all sessions, mints session caps. |
| `vfs` | Virtual filesystem. Owns global namespace, mounts, per-session views. |
| `virtio-blk` | Block device driver (virtio-blk-pci). |
| `ext2` | ext2 filesystem driver (plugged into VFS). |
| `devmgr` | Device manager. Registers block/char devices, brokers device caps. |
| `tpmd` | TPM 2.0 daemon. PCR, seal/unseal, AIK/Quote for login + measured boot. |
| `timeserver` | Clock service. Periodic tick subscriptions. |

### Per-session services (spawned by root-procmgr per login)

| Service | Role |
|---------|------|
| `session-procmgr` | Per-session process manager. Owns session children, pipes, signals, process groups. |
| `session-vfs` | Per-session VFS instance. View layered on root-VFS backends. |
| `shell` | DIY shell. Pest grammar, Rust executor. |
| `cluuterm` | Graphical terminal emulator (compositor window). |
| `login` | Login binary. |

### Terminal stack

| Service | Role |
|---------|------|
| `kbd` | PS/2 keyboard driver. Scancode set 2, HU QWERTZ layout. |
| `mouse` | PS/2 mouse driver. 3-byte packet reassembly. |
| `vtmgr` | VT manager. Manages text VTs (Alt-F1/F2/F3) + VT4 (compositor). |
| `console` | Framebuffer text renderer. Glyph atlas, SIMD blit, double-buffering. |
| `tty` | Legacy text-VT terminal service. Cooked mode, line discipline. |
| `compositor` | TUI window compositor. Owns VT4. Floating windows, SHM cell-grid. |

Data flow through the terminal stack. Each box is its own container with its
own capability profile; they communicate over token-protected endpoints and
know each other only by name, resolved via the registry:

```text
  kbd ──scancodes──→ tty ──whole lines──→ shell ──spawn──→ container
                       ↑                                     │
                       └──────── echo / output ──────────────┤
                       │                                     │
                       console ──render glyphs──→ framebuffer │
                       ↑                                     │
                  vtmgr (focus: VT0/VT1)                    stdout
```

See [Terminal Stack](../terminal/index.html) for the per-service detail.

### Multimedia services

| Service | Role |
|---------|------|
| `displayd` | Display daemon. Surface protocol, compositor backend, linear-fb / virtio-gpu backends. Session-scoped via `PARAM_DISPLAYD_EP`. |
| `audiod` | Audio daemon. N-stream mixer (i32 accumulation, single saturation), linear resampling, per-session streams via `PARAM_AUDIOD_EP`. Sole virtio-snd client. |
| `compositor` | TUI window compositor. Runs as a displayd client — composites cell-grid windows and flushes to displayd surfaces. |
| `sdl2` | Pinned SDL2 2.30.0 with CLUU video/events/audio backends. SDL2 apps (DOOM, cluuamp) go through displayd + audiod, not direct hardware. |

**Measured behavior** (T22, 2026-07-27):
- Linear-fb backend: WORKS. `DISPLAYD_READY 1920 1080 7680 linear_fb` on every boot.
- Virtio-gpu backend: CANNOT BOOT. Three independent blockers (BOOTBOOT panic with `-vga none`, kernel hang with `QEMU_EXTRA_ARGS`, T11 driver no IPC dispatch). See T13 evidence.
- displayd self-test: PASS. `DISPLAYD_SELFTEST_OK` verifies create/destroy/damage/quota lifecycle.
- audiod unit tests: 29/29 PASS (ring 7, resample 8, mixer 10, session 4).
- DOOM (T19 SDL2 migration): PAGE_FAULT during DG_Init. Pre-existing regression — see gotchas.
- Display backend selection: `DisplayBackend` enum delegates to `LinearFbBackend` or `VirtioGpuBackend`. Virtio-gpu probe times out (500ms) → falls back to linear-fb.

### Utilities (each its own container)

`mkdir`, `rm`, `cp`, `mv`, `cat`, `grep`, `head`, `tail`, `wc`, `ls`, `ps`,
`touch`, `top`, `basename`, `date`, `dirname`, `env`, `kill`, `printf`,
`sleep`, `which`, `sort`, `uniq`, `cut`, `tr`, `find`, `du`, `stat`, `edit`,
`micropython`, `doom`, `cluuamp`, `mp3player`, `imgview`.

Each ships with a `Cluufile` declaring its capability profile and mount policy.
See [Container Encapsulation](../containers/index.html).

Counts: 27 top-level userspace crates, 114 `.rs` files (excluding newlib and
tests).

## IPC flow

Every userspace service communicates via IPC through capability-token-protected
endpoints. The registry service maps names to endpoints (e.g., `"vfs"` → thread
ID 5). `subscribe_output("vfs", "main")` resolves the endpoint via the registry
and returns a grant endpoint the caller can `send`/`call` to.

```text
  caller                    registry                    vfs
    │                          │                         │
    │── SUBSCRIBE "vfs","main" ─────────────────────────→│
    │←────────────────────────── grant endpoint ─────────│
    │                                                    │
    │── CALL(vfs_ep, VFS_OPEN, "/etc/passwd") ──────────→│
    │←────────────────────────── fd ────────────────────│
```

Authority to target an endpoint is proved by presenting a capability token.
The kernel does not maintain a per-thread ACL.

## Container model

A CLUU "container" is not a Docker-style image bundle. There is no parallel
runtime, no namespace plus cgroup recreation, no replicated rootfs, no shipped
image. A CLUU binary is spawned with a declarative authority envelope read from
its `Cluufile` manifest. The kernel never inspects Cluufiles; procmgr is the
authority broker. The repo directory is named `containers/` for historical
reasons. The precise term is *capability-scoped binary*.

A Cluufile declares a `PROFILE` (capability bitmask), per-path `MOUNT` policy,
`ENTRYPOINT`, and optional `PRELOAD`:

```text
containers/rm/Cluufile:
    FROM minimal
    PROFILE ipc vfs registry
    MOUNT /tmp inherit
    BUILD "cargo build ..." target/.../rm.elf /bin/rm
    ENTRYPOINT /bin/rm
```

The build tool (`tools/container-build`) parses Cluufiles and emits per-container
`manifest.toml`. At spawn time, procmgr reads the manifest and constructs the
process's authority and view.

### Spawn sequence

```text
  Cluufile ──container-build──→ manifest.toml
                                    │
  ELF binary ──map_elf via VFS──→ Address space
                                    │
  procmgr ──space_create────────→ Address space
  procmgr ──thread_create(SUSPENDED)──→ Thread
  procmgr ──set_view + memfs_cid──→ VFS
  procmgr ──thread_resume──→ Thread ──runs──→ /bin/<name>
```

The thread is created suspended. Procmgr sends `set_view` to VFS so the view
lands in VFS's inbox, then procmgr resumes the thread. VFS processes its inbox
in send order, which closes the race where a freshly created thread could reach
VFS before its view did. The suspend-bracket pattern is the kernel
`THREAD_CREATE_START_SUSPENDED` flag plus procmgr's `install_view_and_run`
helper.

### Profile bits and default mounts

A Cluufile `PROFILE` line maps to a `CapProfile` bitmask. The bitmask drives
which default mounts the container sees:

| Profile bit | Mount set |
|---|---|
| `VFS` (USER_MOUNTS) | `/bin /lib /tmp /home/root /dev/initrd /proc` |
| `DEVICE` | USER plus `/dev` plus `/etc` |
| `ADMIN` | USER plus `/etc` plus `/home/root` |
| `SUPERVISOR` | just `/` |

Defined in `userspace/libcluu/src/vfs_view.rs`.

See [Container Encapsulation](../containers/index.html) for the full model.

## Mount policy

Containers declare per-path mount inheritance in their Cluufile. Four policies:

| `MOUNT` directive | Effect |
|---|---|
| `inherit` | Default. Use the parent container's MemFs for this path. |
| `private` | Fresh per-container MemFs. Shell opts into this for `/tmp`. |
| `readwrite` | Writable view of the parent's mount. |
| `ro` | Read-only view of the parent's mount. |

`inherit` is what makes shell pipelines work. `mkdir /tmp/x; rm /tmp/x` across
two spawned containers shares the same `/tmp/x` because both inherit the
shell's MemFs:

```text
  init ──spawn (MOUNT /tmp private)──→ shell
                                        │ set_view: /tmp → MemFs(shell_cid)
                                        │ shell's /tmp is its own MemFs
                                        │
  shell ──spawn (MOUNT /tmp inherit)──→ mkdir
                                         │ set_view: /tmp → MemFs(shell_cid)
                                         │ mkdir's /tmp = shell's /tmp
                                         │ mkdir /tmp/x
                                         │
  shell ──spawn (MOUNT /tmp inherit)──→ rm
                                         │ set_view: /tmp → MemFs(shell_cid)
                                         │ rm /tmp/x  ✓  (same MemFs)
```

Implementation: the `MOUNT` directive parser lives in
`tools/container-build/src/main.rs`. Per-path policy resolution is in
`userspace/procmgr/src/mount_policy.rs`. The wire-level extension is a per-mount
`memfs_cid` field in `VFS_SET_VIEW` (see `userspace/libcluu/src/ipc.rs`,
`VFS_SET_VIEW_LABEL`), which pins MemFs ownership per mount.

See [Virtual Filesystem](../vfs/index.html) for the view-derivation model.

## Boot sequence

1. **Firmware** (OVMF/UEFI) loads the boot image.
2. **Kernel entry** (`_start` in `kernel/src/main.rs`): naked assembly. Reads
   APIC ID, parks non-BSP cores, switches to kernel stack, jumps to `kstart`.
3. **`kstart`**: UART → logger → GDT → PIC → IDT → PS/2 aux → SMAP/SMEP →
   Spectre V2 → syscall MSRs → IPC fast-path toggles → MM init → heap init →
   frame table → crypto/token init → TSC calibration → APIC timer (250 Hz) →
   `bootstrap::init` (creates init thread) → `ThreadManager::start`.
4. **`init`** (PID 1): reads boot snapshot + boot manifest from initrd, launches
   `SERVICE_LIST` (registry, timeserver, devmgr, root-procmgr, vfs, virtio-blk,
   tpmd), extends TPM PCRs for measured boot, then monitors primordial exits.
5. **Root-procmgr**: reads `etc/system.toml` `[[service]]` entries and starts
   system services (console, vtmgr, inputd, compositor). drivermgr (started
   by init) scans PCI + ACPI buses and spawns matched drivers (kbd, mouse,
   virtio-blk, usb-input, virtio-9p, virtio-snd) from initrd. Presents login
   prompt.
6. **Login**: user authenticates → root-procmgr spawns `session-procmgr` +
   `session-vfs` for the session → spawns shell/cluuterm.

Time budget: firmware to login prompt is roughly 10 to 15 seconds in QEMU with
KVM, dominated by initrd parse, ext2 fsck, and service spawn order. The kernel
itself is ready in well under a second.

See [Boot Flow](../boot/index.html) for the full sequence.

## Build system

`cargo xtask` orchestrates the build:

```text
cargo xtask build          # Build everything (kernel + userspace + disk image)
cargo xtask run            # Run in QEMU
cargo xtask run --build    # Build then run
cargo xtask test           # Run tests
cargo xtask docs           # Build documentation (rustdoc)
cargo xtask clean          # Clean artifacts
cargo xtask doctor         # Verify host tools
```

The build pipeline:
1. Build dependencies: klibcluu, libcluu, newlib, syscalls, crt0.
2. Build kernel.
3. Build init primordials (init, registry, timeserver, devmgr, root-procmgr,
   vfs, virtio-blk, tpmd).
4. Build containers (each from its Cluufile).
5. Package: initrd (kernel + primordials), userdisk (ext2 with containers),
   disk image (bootable).

### Test surface

47 integration cases across IPC, security, l2 shell, performance, container
lifecycle, and kernel-primitive smoke tests (44 currently passing). The
Python gen2 harness (`python/cluu_harness`) covers a representative subset and
is the structured-results driver.

Unit tests for pure-logic modules use `rustc --test` directly because most
userspace crates are `#![no_std]`, so per-package `cargo test` does not work:

```text
rustc --edition 2021 --test userspace/tty/src/line_discipline.rs -o /tmp/t && /tmp/t
rustc --edition 2021 --test userspace/procmgr/src/mount_policy.rs -o /tmp/t && /tmp/t
```
