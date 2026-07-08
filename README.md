![logo](artwork/logo_v1.png)

# CLUU

A hobby microkernel and minimal POSIX-flavored userspace, built solo over ~18 months. Written in Rust. seL4-inspired. Pre-v1 — boots, lets you log in, gives you a shell, and admits what it can't do yet.

This repo is at a "Show & Tell" stage. The kernel is solid. Userspace is thin but real. Posting it now to break my own feedback drought; happy to hear what's broken.

---

## What works today

- Boots a 134 MB ISO under stock QEMU with one command.
- Login + multi-user (`/etc/users.toml`, password-hashed, TPM-backed).
- DIY shell with: `cd`, `pwd`, `ls`, `cat`, `echo`, `touch`, `ps`, `top`, `spawn`, `spawnbg`, `jobs`, `fg`, `bg`, `stop`, `kill`, `sudo`, `su`, `container`, `exit`, ↑/↓ command history.
- `/bin/mkdir`, `/bin/rm` (with `-r`), `/bin/cp`, `/bin/mv` — each shipping as its own container with a declared capability profile.
- Job control: Ctrl-C, fg/bg, jobs listing.
- Two virtual terminals (Alt-F1 / Alt-F2), TTY scrollback.
- **Framebuffer console** — text is rendered to the GPU framebuffer (not legacy VGA). Userspace programs can `framebuffer_acquire()` to grab the FB and write raw pixels (`fbprobe` demonstrates this). There's no compositor / windowing system yet; that's the v2 GUI work.
- A live `/proc` filesystem (per-PID `stat`/`status`/`cmdline`).
- `top` reads `/proc` and gives you a live process list.
- Graceful shutdown (Ctrl-Alt-Del → reboot/poweroff).
- Capability-based IPC at ~1,200-1,600 cycles for a full call/reply.
- A declarative container model: every userspace binary has a `Cluufile` (think Dockerfile, but for a single binary) that defines its capability profile, mount policy, restart policy, and entrypoint. See [`containers/`](containers/).
- **MicroPython** (`spawn micropython`): runs as a container, executes scripts, can read files via the POSIX shim. Limitations: REPL ergonomics are rough (no line editing inside the REPL prompt), no socket support, large scripts may bump into heap limits.
- **POSIX-ish C runtime** via a custom-patched newlib targeting `x86_64-cluu-elf`. C programs (see [`userspace/c-programs/`](userspace/c-programs/)) build with the standard toolchain and use stdio, malloc, pthreads, futexes, signals.

## What does NOT work yet (honest)

- **Pipes.** `cat foo | grep bar` doesn't execute as a real pipeline.
- **Redirection.** No `>`, `>>`, `<`.
- **Tab completion.**
- **In-line cursor editing.** Arrow keys do history (↑/↓) but ←/→ inside a typed line do nothing.
- **No editor.** Nothing TUI runs yet (Phase 2 will port `kilo`).
- **MicroPython is rough.** Runs and reads files, but REPL line editing is missing, no sockets, heap limits are tight. Phase 2 will tighten this.
- **No network.** No driver, no socket layer, no DHCP, no anything.
- **No package manager**, no shell scripting beyond what's in `userspace/shell/src/cluu_lang/`.

This is not a v1 release. It's a checkpoint. See [`doc/book/roadmap.md`](doc/book/roadmap.md) for the full plan and [`doc/book/architecture.md`](doc/book/architecture.md) for the structural map.

---

## Boot it

Tested on Debian 12 / Ubuntu 22.04 with KVM. Should work on any recent Linux with `qemu-system-x86_64` + KVM access.

### Prerequisites

```bash
# nightly Rust (we use unstable build-std features)
rustup toolchain install nightly
rustup component add rust-src --toolchain nightly

# system packages (Debian/Ubuntu names)
sudo apt install qemu-system-x86 ovmf e2fsprogs build-essential
```

### Build

```bash
git clone <this-repo> cluu
cd cluu
cargo xtask build    # ~5-10 min cold; ~30 s incremental
```

The output is `target/cluu.img` (boot image) and `target/userdisk.img` (ext2 with /etc, container images).

### Run

```bash
cargo xtask run
```

A QEMU window opens with two virtual terminals. After the boot log settles, you'll see a login prompt.

### Login

Default user table is in `etc/users.toml`. The seeded admin account is:

- username: `admin`
- password: `admin`

(Change it before posting CLUU on the public internet.)

---

## A 10-minute tour

After login, try these in order:

```sh
cat /etc/welcome.txt          # what works, what doesn't, in one screen
cat /etc/architecture.txt     # 200 words on how the OS is structured
ls /                          # top-level directories
ls /var/images                # all installed containers
cat /etc/users.toml           # the user table (hashed passwords)

# Container model demo:
container run hello           # runs the 'hello' container (its own view, profile)
ps                            # see processes
top                           # live process monitor — q to quit

# Mount-policy demo:
spawn mkdir /tmp/demo         # /bin/mkdir runs as a container, inherits shell's /tmp
spawn mkdir /tmp/demo/inner
spawn rm -r /tmp/demo         # /bin/rm sees what mkdir created — across spawns

# MicroPython, with caveats:
spawn micropython -c "print(2 ** 64)"
spawn micropython -c "open('/etc/welcome.txt').read()[:80]"

# History recall:
↑                             # recall last command
↑ ↑ ↑                         # walk further back
```

Use Alt-F1 / Alt-F2 to switch virtual terminals. `exit` logs out. Ctrl-Alt-Del shuts the system down cleanly.

---

## What's actually distinctive

**Native authority encapsulation per binary.** Every userspace executable, including `mkdir` and `rm`, ships with a declarative manifest. A binary's authority — capability profile, VFS view, mount policy, restart policy, entrypoint — is fully described by a [`Cluufile`](containers/rm/Cluufile):

```
FROM minimal
PROFILE ipc vfs registry
MOUNT /tmp inherit
BUILD "cargo build ..." target/x86_64-cluu-user/debug/rm.elf /bin/rm
ENTRYPOINT /bin/rm
```

Profiles are capability bitmasks (IPC, VFS, REGISTRY, ADMIN, DEVICE, SUPERVISOR). Mount policy controls how the binary's `/tmp`, `/log`, etc. interact with its parent's view.

> *A note on the word "container."* Early writeups called these "containers" because the Cluufile is Dockerfile-shaped — that's misleading. A CLUU "container" is **not** a Docker-style image bundle: there's no parallel runtime, no namespace + cgroup recreation, no replicated rootfs. A CLUU binary is spawned with a declarative authority envelope read from its Cluufile manifest, and the kernel doesn't know about Cluufiles at all — procmgr reads the manifest and applies the envelope at spawn time. **Encapsulation at spawn**, not containerization in the Docker sense. The directory is named `containers/` for historical reasons; the precise word is *capability-scoped binary*.

**Capability-based IPC, no syscall sprawl.** The kernel exposes a tiny syscall surface. New userspace features almost never need a new syscall — they go through capability-token invoke ops. The kernel knows threads; processes are a userspace concept that procmgr maintains.

**No new audits.** This isn't a research artifact and there's no paper. The kernel was audited at 9/10 internally; freeze begins now (see ROADMAP §3) until userspace catches up.

For diagrams and the architectural map, see [`doc/book/architecture.md`](doc/book/architecture.md). For the long-form per-subsystem deep dive, see [`doc/book/kernel.md`](doc/book/kernel.md).

---

## Layout

```
kernel/                       # x86_64 microkernel (Rust, no_std)
userspace/
├── libcluu/                  # userspace runtime + capability/IPC wrappers + POSIX shim
├── newlib/                   # custom-patched newlib for fd 0..3 (stdin/out/err/log)
├── init/                     # primordial userspace, spawns the boot services
├── procmgr/                  # the process manager (containers, restart policy, /proc)
├── vfs/                      # virtual filesystem service
├── shell/                    # DIY shell (pest grammar, Rust executor)
├── tty/, kbd/, console/      # terminal stack
├── mkdir/, rm/, cp/, mv/     # POSIX-ish utilities, each its own container
├── *probe/                   # test/demo containers used by the harness
└── ...
containers/                   # one Cluufile per container image
scripts/                      # build + harness drivers
doc/book/                    # design specs, roadmap, internals (rendered book)
```

## Tests

```bash
cargo xtask build

# Python gen2 harness (event-driven, structured results)
cd python
pip install -e '.[dev]'
python -m cluu_harness --list              # list registered cases
python -m cluu_harness --case l2_login --no-build   # run one case
python -m cluu_harness --no-build          # run all cases
pytest -m smoke                            # no-QEMU unit tests
pytest -m slow                             # QEMU-booting cases
```

The Python harness covers a representative subset of cases (11 so far). Remaining cases are being ported from the retired bash harness.

Unit tests for pure-logic modules use `rustc --test` directly because most userspace crates are `#![no_std]`:

```bash
rustc --edition 2021 --test userspace/tty/src/line_discipline.rs -o /tmp/t && /tmp/t
rustc --edition 2021 --test userspace/procmgr/src/mount_policy.rs -o /tmp/t && /tmp/t
```

## Roadmap

- **Phase 1** (in progress): pipes, redirection, line editing polish, history persistence, tab completion. Phase 1 exit is "the shell feels like a shell."
- **Phase 2**: MicroPython REPL + one TUI editor (probably `kilo`).
- **Phase 3 (revised)**: ship v1 with what Phases 0-2 give us. Don't gate v1 on network.
- **Phase 4 / v1.1**: SpaceDestroy, leak audit, virtio-net, DHCP, sockets, wget.

The full plan: [`doc/book/roadmap.md`](doc/book/roadmap.md).

## Status

- 18 months solo, built in evenings and weekends.
- Kernel: ~50K LOC Rust + assembly. Userspace: ~30K LOC Rust + ~20K C/asm of newlib.
- Develops on `develop`, releases (eventually) on `master`.
- License: see [`LICENSE`](LICENSE) (MIT).

If you boot it and find something broken, please open an issue. If you want to read about how it's built, [`doc/book/kernel.md`](doc/book/kernel.md) is the long form. If you just want to know what's coming next, [`doc/book/roadmap.md`](doc/book/roadmap.md).
