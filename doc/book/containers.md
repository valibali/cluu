# Container Encapsulation

CLUU's "container" model is the heart of its authority system. It is **not**
Docker. It is not namespace+cgroup isolation. It is **capability-scoped binary
encapsulation at spawn time**.

## What a CLUU container is

A CLUU container is a normal ELF binary that gets spawned with a **declarative
authority envelope** read from its `Cluufile` manifest. The envelope defines:

1. **Capability profile** — a bitmask of rights (`IPC`, `VFS`, `REGISTRY`,
   `ADMIN`, `DEVICE`, `SUPERVISOR`).
2. **Mount policy** — per-path rules: `inherit` (use parent's mount),
   `private` (fresh backend), `rw`/`readwrite` (writable), `ro`/`readonly`
   (read-only).
3. **Entrypoint** — the binary path.
4. **Optional preload** — libraries to load before the binary starts.

The kernel never inspects Cluufiles. `procmgr` is the authority broker: it
reads the manifest, builds the envelope, and applies it at spawn time.

## What a CLUU container is NOT

- **Not a Docker image.** There is no image bundle, no layered filesystem, no
  replicated rootfs. The binary is the same ELF whether it runs "containerized"
  or not.
- **Not a parallel runtime.** There is no separate runtime, no namespace
  recreation, no cgroup. The binary runs as a normal userspace thread.
- **Not a filesystem isolation boundary.** There is no chroot, no pivot_root,
  no mount namespace. VFS view scoping is the isolation mechanism, and it works
  by **not showing the child paths it shouldn't see** — not by hiding paths
  behind a namespace wall.

The `containers/` directory name is historical. The precise word is
*capability-scoped binary*.

## The Cluufile

Every container has a `Cluufile` in `containers/<name>/Cluufile`:

```dockerfile
FROM minimal
PROFILE ipc vfs registry
MOUNT /tmp inherit
BUILD "cargo build ..." target/x86_64-cluu-user/debug/rm.elf /bin/rm
ENTRYPOINT /bin/rm
PRELOAD
```

- `FROM minimal` — base image (always `minimal` today).
- `PROFILE ipc vfs registry` — capability profile bitmask.
- `MOUNT /tmp inherit` — mount policy for `/tmp` (inherit parent's mount).
- `BUILD "..." <output> <install-path>` — build command.
- `ENTRYPOINT /bin/rm` — binary path inside the container.
- `PRELOAD` — optional preload libraries.

125 containers exist today, including `mkdir`, `rm`, `cp`, `mv`, `cat`, `grep`,
`ls`, `ps`, `top`, `edit`, `shell`, `cluuterm`, `micropython`, `hello`, and
many more.

## The spawn sequence (encapsulation at spawn)

When procmgr spawns a container, it runs this sequence:

```text
1. Read manifest.toml (built from Cluufile by container-build)
2. Build the envelope:
   - capability profile → rights bitmask
   - mount policy → per-path view entries
   - entrypoint → binary path
3. Kernel: space_create (new address space)
4. Kernel: thread_create(START_SUSPENDED) — child is suspended
5. VFS: VFS_SET_VIEW — install the child's VFS view
6. Kernel: thread_resume — child starts running
```

**The suspend-bracket (steps 4–6) is load-bearing.** The child thread is
created suspended, the view is installed, and only then is the child resumed.
Without this, the child would see the parent's filesystem namespace — an
authority leak. The suspend-bracket is the structural fix for the view-install
race that a runtime ACL would otherwise paper over.

## Mount policy composition

At spawn time, procmgr composes three inputs into the effective per-path policy:

1. **Built-in defaults** — `/tmp` inherit, `/log` private.
2. **Cluufile's `[[mounts.policy]]` entries** — the container's declared
   mounts.
3. **`deny_inherit` flag** — when set, throws the whole list away because there
   is nothing to inherit from (e.g., top-level autostart with no parent
   container).

Before composing, procmgr validates the Cluufile's demands against the parent's
actual view so a child cannot ask for `rw` on a path the parent only has `ro`.

Mount keywords:
- `inherit` — use the parent's mount at this path (child sees the same backend).
- `private` — get a fresh backend (e.g., a new memfs instance).
- `rw` / `readwrite` — child's view is writable.
- `ro` / `readonly` — child's view is read-only.

## Monotone-narrowing view derivation

The child's VFS view is always a **narrower-or-equal subset** of the parent's.
`VfsViewTable::verify_monotone` checks:
- Same or more-specific path prefix.
- Rights ≤ parent's rights.

A child that asks for more than its parent has is **denied at spawn**. This is
the structural enforcement of the monotone-narrowing authority model at the VFS
layer.

Example: if the parent has `ro:/etc`, the child cannot ask for `rw:/etc`. If
the parent doesn't have `/home/alice` at all, the child cannot ask for any
access to `/home/alice`.

## Why this is better than runtime ACL

| Runtime ACL (traditional) | CLUU encapsulation at spawn |
|---------------------------|----------------------------|
| Process can see the path, then a policy check says no. | Process never sees the path — it's not in the view. |
| Policy can be widened at runtime by a privileged agent. | Authority is fixed at spawn; narrowing is structural. |
| "What can X do?" requires running code or consulting a policy engine. | "What can X do?" is answered by reading the static envelope and view. |
| TOCTOU windows between check and use. | No check at use time — the view is the authority. |
| Policy regression is a runtime security hole. | Authority regression requires a kernel bug in token derivation. |

## The profile system

Capability profiles are bitmasks:

| Profile | Rights |
|---------|--------|
| `IPC` | IPC send/recv/call |
| `VFS` | VFS open/read/write |
| `REGISTRY` | Registry register/subscribe |
| `ADMIN` | Procmgr spawn/kill |
| `DEVICE` | Device I/O, IRQ |
| `SUPERVISOR` | All rights (root) |

A Cluufile's `PROFILE` line sets which bits the spawned binary gets. `rm` gets
`ipc vfs registry` — it can talk to VFS and registry, but cannot spawn
processes or touch devices. `mkdir` gets the same. `shell` gets more (it needs
to spawn children).

## Cross-container visibility

Two containers in the same session see each other through procmgr's process
table — but only if their VFS views grant `/proc`. A container without `/proc`
in its view cannot see other processes at all. This is not a policy decision;
it's a structural consequence of view scoping.

Cross-session visibility is a privilege reserved for the root session. See
[Session Encapsulation](../sessions/index.html).
