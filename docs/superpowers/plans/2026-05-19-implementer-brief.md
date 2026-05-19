# CLUU — Implementer Brief

**Audience:** External implementer (deepseek v4 pro or any agentic worker) executing the four implementation plans in `docs/superpowers/plans/2026-05-18-plan{1,2,3,4}-*.md`.

**Purpose:** Five-minute primer on what CLUU is, the principles you must honor, and the workflow conventions. Read this first.

---

## What CLUU is

A hobby x86_64 **capability microkernel** + a userspace stack of services that together form a usable hobby OS. Not Linux, not a research kernel, not a teaching kernel — a system the author actually uses.

**Boot path:** firmware (BootBoot) → kernel → init → procmgr → primordials (registry, timeserver, vfs, virtio-blk, tpmd) → autostart (compositor, vtmgr, kbd, login) → user logs in → cluuterm + shell.

**Current state (May 2026):**
- Kernel: frozen through ~2026-10-21 (see `docs/ROADMAP.md`). Frame typing redesign just landed.
- Userspace: actively evolving. Shell is DIY in pest + Rust. MicroPython REPL works. /bin/edit is a vim-flavored editor.
- Hardware: virtio-blk (modern, ~1 GB/s), framebuffer console (1280×720 BGRA), PS/2 keyboard. No mouse, no network, no audio yet.

**Target:** *usable hobby OS, TUI now, GUI 2027+*.

---

## Architecture in one diagram

```
                   ┌──────────────────────────┐
                   │      Kernel (Rust)       │
                   │  threads / spaces /      │
                   │  endpoints / tokens /    │
                   │  typed frames            │
                   └────────────┬─────────────┘
                                │  IPC syscalls
                                ▼
       ┌──────────────────────────────────────────────────┐
       │            Userspace services                    │
       │                                                  │
       │  procmgr (process lifecycle, sessions, autostart)│
       │  vfs     (filesystem, /dev/pts, mount policy)    │
       │  registry (named-endpoint lookup)                │
       │  timeserver, virtio-blk, tpmd                    │
       │  compositor, kbd, vtmgr, tty                     │
       │  login, shell, cluuterm                          │
       │  ... user programs (cat, grep, edit, mp, ...)    │
       └──────────────────────────────────────────────────┘
```

Every userspace process is a normal program with its address space, threads, endpoints, and tokens. There is **no kernel-side process struct**. Procmgr is the sole owner of process lifecycle.

---

## Principles you MUST honor

### 1. Microkernel discipline

> The kernel knows **threads, address spaces, endpoints, tokens, typed frames**. That is the entire model.

If a plan step would add a new syscall, push back — there's almost always a way to achieve the goal via existing invoke operations on tokens. Adding kernel-side process state, kernel-side view state, or kernel-side session state is **forbidden by the architecture**.

Reference: `[[unified-process-model-decision-2026-05-18]]` (memory).

### 2. Capability discipline (seL4-shape)

Every authority is a **token**. Tokens are unforgeable kernel objects with narrow rights. Children get caps via *narrow-derive only* (rights ⊆ parent's). Caller's parent-view → child's view is monotone-narrowing.

If you find yourself wanting "ambient authority" ("is the caller a shell?"), stop. The right answer is "does the caller hold the cap with this right?".

Reference: `[[spawn-cap-composable]]`, `[[vfs-view-caps-monotone]]`.

### 3. No timeouts as deadlock guards

> When a userspace IPC waiter needs to unblock on the death of its peer, use the kernel's capability-revocation primitive — NOT a time-bounded recv.

`recv_with_timeout(N ms)` in any code path is a smell. Timeouts convert hangs (debuggable) into spurious failures (cascading kills, double-frees, ghost bugs). If a producer dies, its endpoint cap revokes → blocked recv returns a concrete error.

Acceptable exceptions:
- UX disambiguation (e.g., 25 ms ESC-vs-CSI in `userspace/edit/`).
- NOT acceptable: defensive bounds on RPC calls, "shouldn't be that slow" guards.

The 2 s `COMPOSITOR_READY_LABEL` wait is the longest-standing violation; plan 3 task 9 deletes it.

Reference: `feedback_no_timeouts` (memory).

### 4. Procmgr-stateless where feasible

> Default to in-process state passed inline on spawn, not procmgr-side mirrors.

If you're about to add a procmgr-side table to mirror caller state (cwd, env, umask) — push back. Carry it on the envelope. Procmgr stores only what it must to make routing/lifecycle decisions without the live process.

### 5. POSIX compat via libcluu shims

CLUU has its own ABI. POSIX is a *compatibility surface* implemented in `userspace/libcluu/src/posix/`. Newlib programs (MicroPython, ported C utilities) link against it. CLUU-native programs (shell, cluuterm, login, edit) use the libcluu native API directly (or the shim for convenience).

Never add POSIX behavior inside the kernel. Never add a new syscall for a POSIX feature.

### 6. Frame typing (just-landed redesign)

Every frame is typed: `Untyped` / `UserData` / `PageTable` / `Grant` / `Device` / `KernelHeap` / `BootReserved`. PMM hands out *untyped* frames; caller must retype before use. Every map inc_refs, every unmap dec_refs.

If a plan step allocates a frame, follow the typed-frame contract. If it shares a frame across address spaces (MAP_SHARE_PHYS), inc_ref both sides.

Reference: `[[frame-typing-redesign-landed-2026-05-18]]` (memory).

### 7. fd 0-3 (not 0-2)

CLUU's standard fds: stdin (0), stdout (1), stderr (2), **stdlog (3)**. Differs from Unix. When wiring FdInherit entries, account for the fourth fd.

### 8. Shell is DIY

Shell is built on a pest grammar + Rust executor. Do not propose porting bash/dash/ash. Extend the pest grammar instead. Long-term goal: xonsh-hybrid.

Reference: `[[shell-diy-pest]]`, `[[xonsh-inspiration]]` (memory).

---

## Source-tree shape

```
/home/vlb2bp/git/cluu/
├── kernel/                  Rust no-std kernel
├── klibcluu/                kernel-side helper crate
├── userspace/
│   ├── libcluu/             userspace runtime (syscalls, IPC, fd table, posix shims)
│   │   ├── src/ipc.rs       IPC label constants + helpers
│   │   ├── src/posix/       newlib-facing C ABI shims
│   │   ├── src/tty_core/    line discipline (spec 2 expands)
│   │   ├── src/fs/          VFS client
│   │   └── src/...
│   ├── libcluu_syscalls/    raw syscall wrappers (klibcluu duplicate for std build)
│   ├── newlib/              CLUU-target newlib + crt0.S
│   ├── init/                kernel-spawned bootstrap; primordial monitor
│   ├── procmgr/             process lifecycle + autostart + sessions
│   ├── registry/            named-endpoint directory
│   ├── timeserver/          monotonic time
│   ├── vfs/                 filesystem + mount policy
│   ├── ramfs/, ext2/        backends
│   ├── virtio-blk/          block driver
│   ├── virtio-core/         shared virtio plumbing
│   ├── tpmd/                TPM (currently stub)
│   ├── compositor/          framebuffer compositor (spec 4 formalizes)
│   ├── compdemo/            demo client
│   ├── console/             text console fallback
│   ├── kbd/                 keyboard driver
│   ├── vtmgr/               VT switching
│   ├── tty/                 text-VT terminal (spec 2 unifies w/ cluuterm)
│   ├── cluuterm/            graphical terminal (spec 2/4 work)
│   ├── shell/               DIY shell, pest-based
│   ├── login/               login window (spec 3 rewrites)
│   ├── edit/                vim-flavored editor
│   ├── cat/, grep/, head/, tail/, wc/, ls/, ps/, ...   coreutils-style
│   ├── micropython/         MicroPython port
│   ├── probes/              tiny test binaries (harness markers)
│   └── tests/               kernel & userspace tests
├── xtask/                   build orchestration
├── scripts/                 harness_run.sh, fb_dump.sh, perf_ratchet
├── target/                  cargo build outputs (gitignored)
├── docs/                    architecture + plans + specs
├── etc/                     /etc files burned into rootfs
└── var/images/              per-image manifests (Cluufiles)
```

After plan 1 lands, expect a new `userspace/cluu_proto/` member (shared wire-protocol crate).

---

## Build / test workflow

### Building

Standard:
```bash
cargo xtask build
```

Clean rebuild (touch newlib):
```bash
rm -rf target/newlib-build target/sysroot/x86_64-cluu-elf
make clean
cargo xtask build-newlib
cargo xtask build-syscalls
cargo xtask build-crt0
cargo xtask build
```

### Lint

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

### Boot smoke test

```bash
bash scripts/harness_run.sh
```

Runs the OS in headless QEMU; captures COM2 serial; expects `compositor: ready` in `serial.log`.

### Harness markers

Per-feature tests landing as small probe binaries. To run a marker:

```bash
HARNESS_FORCE_BUILD=1 MARKER_MODE=<marker_name> bash scripts/harness_run.sh
grep "<marker_name>:" serial.log
```

Expect `<marker_name>: PASS` on success.

`HARNESS_FORCE_BUILD=1` is essential after any code change — without it the harness reuses a cached build.

### Interactive testing

`CLUU_SHELL_AUTOSTART_CMD=<binary>` autostarts a binary in place of the login shell (used by probe markers). Without it, the harness reaches the login window and waits.

### Visual smoke

```bash
bash scripts/fb_dump.sh
```

Captures framebuffer to a PNG for visual inspection. Useful after compositor / window-protocol work (plan 4).

---

## Plans you'll be implementing

Four detailed plans in `docs/superpowers/plans/`:

| # | File | Tasks | Spec |
|---|---|---|---|
| 1 | `2026-05-18-plan1-unified-spawn-protocol.md` | 20 | unified spawn |
| 2 | `2026-05-18-plan2-terminal-pty-unification.md` | 13 | terminal + PTY |
| 3 | `2026-05-18-plan3-session-lifecycle.md` | 12 | session lifecycle |
| 4 | `2026-05-18-plan4-window-protocol.md` | 13 | window protocol |

Specs (background context, not the work item) are in `docs/superpowers/specs/2026-05-18-*.md`.

**Dependency chain (read this carefully):**

- Plan 1 tasks 1-4 (cluu_proto crate + libcluu/procmgr integration) unlock plans 2/3/4.
- Plan 1 task 9 (`PROCMGR_SPAWN_UNIFIED` dispatch) unlocks plan 3.
- Plan 3 task 5 (compositor `SESSION_ENDED` subscriber) unlocks plan 4 task 10.
- Plans 2, 3, 4 may land in parallel within those constraints.

Each plan's task list is self-contained: file paths, complete code blocks, verification commands with expected output. Follow the order; each task ends with a commit.

---

## Critical do's and don'ts

### DO

- Read the referenced spec before starting a plan (each plan header points at its spec).
- Commit after every task. Don't squash. Frequent small commits make per-task revert trivial.
- Run `bash scripts/harness_run.sh` between tasks. Per-task gate is "boot reaches `compositor: ready`".
- Write tests inline where applicable. Cluu_proto types have round-trip tests; line_discipline has pure-function tests.
- Reuse existing helpers. If a plan step asks for a function that already exists in `main.rs`, extract it (`pub(crate) fn`) rather than rewriting.
- Honor cap-revocation. If a verb call could outlive its peer, ensure the peer's death produces a concrete error, not a hang.
- Use `git grep` aggressively. Every plan has grep-proof checkpoints.

### DON'T

- **Don't add new syscalls.** If a plan step seems to need one, you've misread the plan; re-read.
- **Don't add timeouts.** Every `recv_with_timeout` or `call_with_timeout` you introduce is a regression.
- **Don't refactor outside the plan's scope.** If a related file is messy, leave it; file a follow-up. Plans deliberately don't expand to "fix surrounding code".
- **Don't skip a step's verification.** Each task's exact verification command is the gate. If it fails, fix before moving on.
- **Don't use `--no-verify` on commit hooks.** If a hook fails, investigate.
- **Don't write new docs unless a plan task asks for it.** All needed docs already exist in `docs/`.
- **Don't propose porting Linux software when CLUU equivalents exist.** Don't propose bash/dash/ash for shell. Don't propose Linux frame-typing/sessions/PTS for CLUU's own.

---

## Glossary

- **Cluufile**: per-image manifest at `/var/images/<image>/manifest.toml`. Declares ENTRYPOINT, RESTART policy, mount overlays, capability rights. Procmgr enforces at spawn.
- **Envelope**: serialized request payload. Spec 1 introduces `SpawnEnvelope`; specs 2-4 use similar postcard envelopes.
- **Image**: a process-kind. One Cluufile = one entrypoint = one image. 1:1 image:binary.
- **Primordial**: a kernel-spawned service that must exist before user spawns work (registry, timeserver, procmgr, vfs, virtio-blk, tpmd).
- **Session**: a procmgr-owned typed object grouping processes belonging to one logged-in user-instance. Has a leader; leader exit cascades destroy.
- **View**: a procmgr-owned VFS narrowing. Each process sees its parent's view minus what its Cluufile MOUNTs filtered out.
- **FdInherit**: the sole fd-wiring mechanism on the spawn wire. Lists (child_fd, source, rights) per inherited fd. No POSIX `adddup2` survives onto the wire.
- **Surface**: a compositor-owned window object. Has buffers, damage, session_id, focus state.
- **PTS**: pseudo-terminal slave. Lives at `/dev/pts/<n>` per session (after spec 2).
- **Postcard**: zero-copy `no_std`-friendly Rust serialization format. Used for every IPC envelope in plans 1-4.
- **ABI_VERSION**: `1` across all four wire formats. Procmgr rejects mismatches with `Internal(EBADABI)`.

---

## Reference memory entries (cross-session knowledge)

Cluu's full project memory lives at
`/home/vlb2bp/.claude/projects/-home-vlb2bp-git-cluu/memory/`. Notable
entries that bear on the four-plan work:

- `feedback_no_timeouts.md` — the law.
- `project_unified_process_model_decision_2026_05_18.md` — architectural call.
- `project_frame_typing_redesign_landed_2026_05_18.md` — typed frames.
- `feedback_procmgr_stateless.md`, `feedback_vfs_view_caps_monotone.md`.
- `project_spec_quartet_2026_05_18.md` — overview of the four-spec work.

You don't need to read these to execute the plans. They're context if a plan step's *why* is unclear.

---

## When in doubt

1. Re-read the plan task's "Goal" line.
2. Re-read the referenced spec section.
3. If the spec is also unclear, the *user-facing behavior* in the spec's acceptance section is the contract.
4. If still unclear: stop, document the ambiguity, ask. Don't guess and don't half-implement.

Commit small, commit often. Each task's "Step 5: Commit" is non-negotiable. Build green between tasks.

Welcome to CLUU.
