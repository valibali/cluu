# Getting Started

CLUU is a hobby microkernel + minimal POSIX-flavored userspace written in Rust.
It is seL4-inspired, pre-v1, and built over ~18 months of evenings and weekends.
It boots, lets you log in, gives you a shell, and admits what it can't do yet.

## What works today

- **Boots a 134 MB ISO under stock QEMU** with one command (`cargo xtask build` → `cargo xtask run`).
- **Login + multi-user** via `/etc/users.toml` (password-hashed, TPM-backed).
- **DIY shell** with `cd`, `pwd`, `ls`, `cat`, `echo`, `touch`, `ps`, `top`, `spawn`, `spawnbg`, `jobs`, `fg`, `bg`, `stop`, `kill`, `sudo`, `su`, `container`, `exit`, and ↑/↓ command history.
- **POSIX-ish utilities** — `/bin/mkdir`, `/bin/rm` (with `-r`), `/bin/cp`, `/bin/mv` — each shipping as its **own container** with a declared capability profile.
- **Job control** — Ctrl-C, fg/bg, jobs listing.
- **Virtual terminals** — Alt-F1 / Alt-F2 / Alt-F3 (text VTs) + Alt-F4 (compositor).
- **Framebuffer console** — text is rendered to the GPU framebuffer (not legacy VGA). Userspace programs can `framebuffer_acquire()` to grab the FB and write raw pixels.
- **A live `/proc` filesystem** — per-PID `stat`/`status`/`cmdline`.
- **`top`** reads `/proc` and gives you a live process list.
- **Graceful shutdown** — Ctrl-Alt-Del → reboot/poweroff.
- **Capability-based IPC** at ~1,200–1,600 cycles for a full call/reply.
- **A declarative container model** — every userspace binary has a `Cluufile` (think Dockerfile, but for a single binary) that defines its capability profile, mount policy, restart policy, and entrypoint. See [Container Encapsulation](../containers/index.html).
- **MicroPython** (`spawn micropython`) — runs as a container, executes scripts, can read files via the POSIX shim.
- **POSIX-ish C runtime** via a custom-patched newlib targeting `x86_64-cluu-elf`. C programs build with the standard toolchain and use stdio, malloc, pthreads, futexes, signals.
- **TUI compositor** with floating windows, rounded Unicode chrome, shared-memory cell-grid protocol.
- **cluuterm** — graphical terminal emulator running as a compositor window, hosting the shell via a pseudo-terminal (`/dev/pts/<id>`).
- **vi-like editor** (`edit`) — modal, TUI, running as a compositor window.

## What does NOT work yet (honest)

- **Pipes.** `cat foo | grep bar` doesn't execute as a real pipeline yet.
- **Redirection.** No `>`, `>>`, `<`.
- **Tab completion** is in progress (shell↔cluuterm IPC protocol exists).
- **In-line cursor editing.** Arrow keys do history (↑/↓) but ←/→ inside a typed line do nothing.
- **No network.** No driver, no socket layer, no DHCP.
- **No package manager**, no shell scripting beyond `cluu_lang`.

## Build

Tested on Debian 12 / Ubuntu 22.04 with KVM.

```bash
# Prerequisites
rustup toolchain install nightly
rustup component add rust-src --toolchain nightly
sudo apt install qemu-system-x86 ovmf e2fsprogs build-essential nasm

# Build
cargo xtask build    # ~5-10 min cold; ~30 s incremental
# Output: target/cluu.img (boot image) + target/userdisk.img (ext2)

# Run
cargo xtask run      # QEMU window opens
```

## Login

Default users are in `etc/users.toml`. The seeded accounts:

| User | Password | Profile | Notes |
|------|----------|---------|-------|
| `root` | _(empty)_ | `supervisor` | Godmode — sees all sessions |
| `alice` | _(empty)_ | `user` | Standard user, can `sudo` to `admin` |
| `guest` | _(empty)_ | `user` | No `sudo` |

## A 10-minute tour

```sh
cat /etc/welcome.txt          # what works, what doesn't
cat /etc/architecture.txt     # 200 words on how the OS is structured
ls /                          # top-level directories
ls /var/images                # all installed containers
cat /etc/users.toml           # the user table

# Container model demo:
container run hello           # runs the 'hello' container
ps                            # see processes
top                           # live process monitor — q to quit

# Mount-policy demo:
spawn mkdir /tmp/demo         # /bin/mkdir runs as a container
spawn mkdir /tmp/demo/inner   # inherits shell's /tmp
spawn rm -r /tmp/demo         # sees what mkdir created — across spawns

# MicroPython:
spawn micropython -c "print(2 ** 64)"
spawn micropython -c "open('/etc/welcome.txt').read()[:80]"

# History:
↑ ↑ ↑                         # walk back through commands
```

Use Alt-F1 / Alt-F2 / Alt-F3 to switch text VTs. Alt-F4 switches to the
compositor. `exit` logs out. Ctrl-Alt-Del shuts down.

## What's actually distinctive

### 1. Authority is structural, not conventional

The kernel knows **threads**, **capability tokens**, and **IPC**. That's it.
There is no process concept in the kernel, no filesystem, no scheduler policy,
no network stack. Processes are a userspace concept owned by `procmgr`. The
filesystem is a userspace service (`vfs`). The scheduler policy is a userspace
concern (the kernel scheduler is a simple priority bitmap; procmgr decides what
to spawn and with what priority).

Authority flows through **HMAC-signed capability tokens**. The kernel verifies
the signature on every operation. Possession of a valid token *is* authority —
there is no runtime ACL, no per-call identity check, no "who is the caller"
interrogation. If a binary can name a token, it can use it; if it cannot, it
never sees the endpoint. See [Capability Tokens](../capability_tokens/index.html).

### 2. Seven syscalls, fifty-two invoke ops

The syscall surface is deliberately tiny: `Send`, `Recv`, `Call`, `Reply`,
`Yield`, `Invoke`, `DebugPrint`. New userspace features almost never need a new
syscall — they add an `InvokeOp` on the existing `Invoke` dispatch path. There
are 52 invoke ops today (thread management, address-space management, token
derivation, IRQ handling, endpoint creation, PCI config, I/O ports, clock,
frame allocation, notifications). This keeps the kernel attack surface bounded
while letting userspace grow without kernel changes.

### 3. Container = capability-scoped binary, not Docker image

A CLUU "container" is **not** a Docker-style image bundle. There is no parallel
runtime, no namespace+cgroup recreation, no replicated rootfs. A CLUU container
is a **capability-scoped binary**: a normal ELF that gets spawned with a
declarative authority envelope read from its `Cluufile` manifest. The kernel
never inspects Cluufiles — `procmgr` reads the manifest and applies the envelope
at spawn time. **Encapsulation at spawn**, not containerization in the Docker
sense. See [Container Encapsulation](../containers/index.html).

### 4. Session encapsulation with root godmode

Each login gets its own `session-procmgr` (owns the session's children, exit
cookies, signals, pipes, process groups) and its own `session-vfs` (owns the
session's VFS view layered on top of root-VFS backends). A session binary sees
only its own session's processes. The **root session** is the sole exception:
root's session-procmgr can observe and affect processes across the whole system.
This is the only sanctioned escape hatch, and it is bound to the root identity,
not to a capability that can be forwarded. See [Session Encapsulation](../sessions/index.html).

### 5. Monotone-narrowing authority derivation

When a process spawns a child, the child's authority is always a **strict
subset** of the parent's. Capability tokens are derived with narrowed rights
and shorter expiry. VFS views are derived with narrowed paths and rights. A
child that asks for more than its parent has is denied at spawn. This is
enforced structurally — `verify_monotone` on the view table, `Token::derive`
on the token system — not by runtime policy checks. Authority can only shrink
as you descend the spawn tree.

### 6. The suspend-bracket

Spawn is not instantaneous. Between `thread_create(START_SUSPENDED)` and
`thread_resume`, procmgr installs the child's VFS view via `VFS_SET_VIEW`. If
the child started running before its view was installed, it would see the
parent's filesystem namespace — a authority leak. The suspend-bracket closes
this race: the child thread is created suspended, the view is installed, and
only then is the child resumed. This is the structural fix for the
view-install race that a runtime ACL would otherwise paper over.

### 7. Async runtime as deadlock-avoidance

VFS and session-procmgr are single-threaded servers. If VFS makes an IPC call
to procmgr (to resolve a `/proc` entry), and procmgr simultaneously makes an
IPC call to VFS (to install a view), both block forever. CLUU's async runtime
(`libcluu::async_runtime`) is the canonical structural fix: VFS dispatches
IPC-bound backend operations through `dispatch_async()`, so a single-threaded
server can have multiple outstanding downstream calls without blocking itself.

## Repository layout

Canonical structure and naming rules. Generated outputs and downloaded sources
are never tracked in git.

### Top-level

| Path | Contents |
|------|----------|
| `kernel/` | Microkernel crate (`x86_64-cluu-kernel` target). |
| `klibcluu/` | Shared kernel-side utility crate (compiled for both kernel and userspace). |
| `userspace/` | Userspace services and support crates. |
| `tests/kernel/` | Kernel test crate (hosted test harness for kernel modules). |
| `xtask/` | Build orchestration and developer workflows (`cargo xtask`). |
| `tools/` | Third-party and local build tools (e.g. `mkbootimg`, `container-build`). |
| `doc/` | Rendered rustdoc book (this document). Source markdown in `doc/book/`, crate root in `doc/src/lib.rs`. |
| `scripts/` | Automation scripts used by xtask and CI (e.g. `harness_run.sh`). |
| `external/` | Source download cache roots. Only `external/sources.env` is tracked. |
| `target/`, `tmp/` | Generated outputs and build caches. Never tracked. |

### Userspace layout

- `userspace/libcluu/` — shared userspace API crate (IPC wrappers, POSIX shim, async runtime, capability helpers).
- `userspace/libcluu_syscalls/` — syscall static library for C/newlib programs.
- `userspace/c-programs/` — C probe and sample programs used for integration checks.
- Service crates stay one-directory-per-service (e.g. `userspace/shell/`, `userspace/tty/`, `userspace/procmgr/`).

### Naming rules

- Directory names use `kebab-case` (hyphens), not `snake_case`.
- Avoid ambiguous names like `misc`, `temp`, `stuff`.
- Keep third-party and tooling directories explicit (`tools/`, `external/`).
- Keep root focused on project entry points: `README.md`, `Cargo.toml`, `Makefile`, `LICENSE`, `AGENTS.md`.

### Cleanliness rules

- Generated binaries and objects must not be tracked in git.
- Downloaded sources must not be tracked in git.
- `make clean` should reset build artifacts to a repo-clean state for tracked files.
- CI enforces repository hygiene checks.
