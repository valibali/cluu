# CLUU Process Isolation & Containerization Design

**Date:** 2026-02-20
**Scope:** Capability profiles, VFS views, container model, spawn protocol
**Status:** Implementation in progress — Phases A/B complete; Phase C baseline implemented
**Depends on:** IPC registry (docs/IPC_REGISTRY.md), kernel token system (rights.rs)

---

## 1. Guiding Principles

### 1.1 Capabilities as the Only Access Control

CLUU follows seL4's capability model. In traditional systems (Linux, Windows),
access control is a separate layer: the kernel checks UIDs, file permissions,
SELinux policies, or seccomp filters at every operation. The process holds a
file descriptor, but the kernel still decides at runtime whether the operation
is allowed.

In CLUU there is no such layer. A token (capability) **is** permission. If a
process holds a token with `IPC_SEND` rights to an endpoint, it can send. If
it doesn't hold the token, it can't. There is no runtime check beyond "does
this token exist and does it have the requested right bit set?" The kernel
doesn't know or care who the process is — it only sees tokens.

This means all security decisions happen at **distribution time**: when a parent
spawns a child, it chooses which tokens to hand over and with what rights. Once
the child is running, neither the parent nor the kernel can retroactively change
what the child can do (except by revoking the token entirely, which is a hard
kill — not a policy adjustment).

The consequence: **the spawn path is the entire security policy**. Everything
in this document is about making that spawn path systematic and composable.

### 1.2 The Three Rules

These are not guidelines — they are structural invariants enforced by the
kernel's `token_derive` syscall and procmgr's validation logic.

1. **Capabilities only narrow, never widen.** `token_derive(parent_token,
   rights, badge)` returns a new token whose rights are `parent_rights & rights`.
   The kernel rejects any attempt to set a bit that the parent doesn't have.
   This is not a policy check — it's arithmetic.

2. **VFS views only narrow, never widen.** A VFS view is a set of path prefixes.
   When procmgr tells VFS "this client can see /bin and /tmp", VFS records it.
   When the client later spawns a child with view "/bin", procmgr checks that
   "/bin" is a subset of the parent's view before telling VFS. VFS never accepts
   a view expansion from any source.

3. **Every spawn goes through procmgr.** The kernel's `thread_create` syscall
   requires a capability with the CREATE right. Only init and procmgr hold this
   capability. Init uses it once (to spawn the boot services) and then idles.
   After boot, procmgr is the sole process that can create new threads and
   address spaces. Any process wanting to spawn a child must ask procmgr via
   IPC, and procmgr applies the profile/view validation before proceeding.

### 1.3 Trust Chain

```
Kernel
  │ hands root token to init
  │
init (SUPERVISOR)
  │ spawns procmgr, console, tty, kbd, vtmgr, vfs
  │ derives tokens with specific rights for each
  │ idles forever after boot
  │
procmgr (SUPERVISOR)
  │ sole authority for all subsequent spawns
  │ validates profiles, derives tokens, creates views
  │
  ├─ tty → requests shell spawn (TTY_SPAWN_SHELL_LABEL)
  │   └─ shell (USER) → can spawn child programs
  │       └─ user program (USER or SANDBOXED)
  │
  └─ vtmgr → requests tty:N spawn (PROCMGR_SPAWN_SERVICE_LABEL)
      └─ tty:N (SERVICE) → requests shell:N spawn
```

The trust hierarchy is: kernel > init > procmgr > services > user programs.
Each level can only grant capabilities it already holds, so the hierarchy
cannot be inverted.

---

## 2. Capability Profiles

### 2.1 Concept

A **CapProfile** is a compact bitmask that answers the question: "what categories
of system interaction is this process allowed to perform?" It does not replace
kernel token rights — it sits above them as an intent declaration that procmgr
translates into concrete token derivations.

Without profiles, every spawn call would need to specify individual token rights
per slot: "give the child IPC_SEND on slot 6, SPACE_MAP on slot 5, no IRQ on
slots 9-15, ..." This is error-prone and doesn't compose well. Profiles provide
a vocabulary: "this is a USER process" means a well-defined set of rights that
can be narrowed by the caller.

Profiles are stored as a `u16` bitmask in the process's params (one of the 10
param slots in ProcessInfo). This makes them introspectable: a process can read
its own profile, and procmgr can look up any process's profile when validating
a spawn request.

The design is deliberately bitmask-based rather than role-based (enum). A role
system (SANDBOXED=0, USER=1, SERVICE=2, SUPERVISOR=3) creates a linear hierarchy
that's hard to extend. A bitmask system creates a lattice: `CAP_IPC | CAP_VFS`
is a valid profile that's neither USER nor SERVICE. This matters because real
services have diverse needs — a network daemon needs IPC, registry, VFS, and
network access but not device caps or spawn rights.

### 2.2 Bit Definitions

```
Bit   Name              Meaning
────────────────────────────────────────────────────────────────────────
0     CAP_IPC           Create endpoints, send/receive messages.
                        Without this bit, the process can only use
                        pre-wired stdio tokens — it cannot discover
                        or connect to any service.

1     CAP_SPAWN         Request procmgr to create child processes.
                        Without this bit, spawn requests are rejected.
                        This is the "can this process multiply" gate.

2     CAP_REGISTRY      Register outputs and subscribe to services
                        via the IPC registry. Without this bit, the
                        process cannot participate in service discovery.
                        It can still use pre-wired endpoints (stdio).

3     CAP_VFS           Access the virtual filesystem. VFS checks this
                        bit before accepting any open/read/write/stat
                        request. The actual paths visible are further
                        constrained by the VFS view.

4     CAP_DEVICE        Hold device tokens: IRQ, PCI BAR mappings,
                        DMA buffers. Only drivers need this. Without
                        it, TOKEN_EXTRA slots 9-15 are left empty.

5     CAP_SPACE_GRANT   Grant memory pages to other address spaces.
                        Required for shared memory, shared rings, and
                        zero-copy IPC. Without it, the process's
                        SPACE token lacks SPACE_GRANT right.

6     CAP_NET           Access the network stack (future). Reserved
                        for when CLUU gains networking. Processes
                        without this bit cannot open sockets or send
                        packets.

7     CAP_ADMIN         System-wide administrative operations: reboot,
                        mount/unmount filesystems, modify global
                        configuration. Only init and procmgr should
                        have this in normal operation.

8-15  (reserved)        Available for future expansion without
                        changing the u16 storage format.
```

### 2.3 Built-in Profiles

These are named constants, not special cases. Any combination of bits is a
valid profile.

| Profile     | Bitmask     | Hex    | Capabilities                                  |
|-------------|-------------|--------|-----------------------------------------------|
| SANDBOXED   | `0b00000001` | `0x01` | IPC only                                     |
| USER        | `0b00001111` | `0x0F` | IPC, spawn, registry, VFS                    |
| SERVICE     | `0b00111111` | `0x3F` | IPC, spawn, registry, VFS, device, space     |
| SUPERVISOR  | `0b11111111` | `0xFF` | Everything                                   |

Examples of intermediate profiles:

| Use Case               | Bits                              | Hex    |
|------------------------|-----------------------------------|--------|
| Network daemon         | IPC + registry + VFS + net        | `0x4D` |
| Sandboxed with VFS     | IPC + VFS                         | `0x09` |
| Worker (no spawn)      | IPC + registry + VFS              | `0x0D` |
| Pure compute (no I/O)  | IPC only                          | `0x01` |

### 2.4 Rust Type

```rust
// userspace/libcluu/src/cap.rs

use bitflags::bitflags;

bitflags! {
    #[derive(Copy, Clone, Debug, PartialEq, Eq)]
    pub struct CapProfile: u16 {
        const IPC         = 1 << 0;
        const SPAWN       = 1 << 1;
        const REGISTRY    = 1 << 2;
        const VFS         = 1 << 3;
        const DEVICE      = 1 << 4;
        const SPACE_GRANT = 1 << 5;
        const NET         = 1 << 6;
        const ADMIN       = 1 << 7;
    }
}

impl CapProfile {
    pub const SANDBOXED:  Self = Self::IPC;
    pub const USER:       Self = Self::IPC.union(Self::SPAWN)
                                  .union(Self::REGISTRY).union(Self::VFS);
    pub const SERVICE:    Self = Self::USER.union(Self::DEVICE)
                                  .union(Self::SPACE_GRANT);
    pub const SUPERVISOR: Self = Self::SERVICE.union(Self::NET)
                                  .union(Self::ADMIN);

    /// Check whether `child` is a valid narrowing of `self`.
    pub fn can_grant(self, child: CapProfile) -> bool {
        (child.bits() & !self.bits()) == 0
    }
}
```

### 2.5 Profile-to-Rights Mapping

When procmgr spawns a process with a given profile, it derives tokens with
these kernel rights:

| Token Slot      | SANDBOXED               | USER                          | SERVICE                                    |
|-----------------|-------------------------|-------------------------------|--------------------------------------------|
| TOKEN_STDIN (0) | IPC_SEND, IPC_RECV      | IPC_SEND, IPC_RECV            | IPC_SEND, IPC_RECV                         |
| TOKEN_STDOUT(1) | IPC_SEND                | IPC_SEND                      | IPC_SEND                                   |
| TOKEN_STDERR(2) | IPC_SEND                | IPC_SEND                      | IPC_SEND                                   |
| TOKEN_STDLOG(3) | IPC_SEND                | IPC_SEND                      | IPC_SEND                                   |
| TOKEN_SELF (4)  | (empty)                 | CREATE, GRANT                 | CREATE, GRANT, THREAD_CONTROL              |
| TOKEN_SPACE (5) | (empty)                 | SPACE_MAP                     | SPACE_MAP, SPACE_GRANT, CREATE             |
| TOKEN_IPC (6)   | IPC_SEND, IPC_RECV      | CREATE, IPC_SEND/RECV/CALL, GRANT | CREATE, IPC_SEND/RECV/CALL, GRANT      |
| TOKEN_CLOCK (7) | (shared read)           | (shared read)                 | (shared read)                              |
| TOKEN_REGISTRY  | (empty)                 | (registry send endpoint)      | (registry send endpoint)                   |
| TOKEN_EXTRA 9+  | (empty)                 | (empty)                       | (device-specific: IRQ, PCI, etc.)          |

The `(empty)` entries mean the token slot is set to 0. The process cannot use
that capability at all — there is nothing to derive from, nothing to send to.

### 2.6 ProcessInfo Storage

The profile is stored in a currently-unused params slot:

```rust
// userspace/libcluu/src/boot.rs
pub const PARAM_CAP_PROFILE: usize = 5;  // u16 CapProfile bitmask (low 16 bits of u64)
```

Slot 5 is free for most process types. Console uses slots 0-7 for FB params,
but PARAM_CAP_PROFILE at slot 5 overlaps with PARAM_CONSOLE_INSTANCE — this
is acceptable because console is a SERVICE and its profile is implied.

For processes where all 10 param slots are already spoken for, the profile
could alternatively be encoded in the `exit_cookie` high bits or a dedicated
ProcessInfo field (future expansion).

### 2.7 Service Profile Assignments

Every existing service gets a concrete profile assignment:

| Service     | Profile       | Rationale                                               |
|-------------|---------------|---------------------------------------------------------|
| init        | SUPERVISOR    | Bootstrap authority, holds root token                   |
| procmgr     | SUPERVISOR    | Sole spawn authority, derives all child tokens          |
| console     | SERVICE       | Needs device (framebuffer), space_grant (shared memory) |
| tty         | SERVICE       | Needs registry (to subscribe to console, spawn shell)   |
| kbd         | SERVICE       | Needs device (IRQ), registry (subscribe to tty/vtmgr)   |
| vtmgr       | SERVICE       | Needs registry, spawn (to request tty:N from procmgr)   |
| vfs         | SERVICE       | Needs device (virtio), space_grant (grant reads)        |
| shell       | USER          | Needs spawn (child processes), VFS (file access)         |
| user program| USER          | Inherits shell's profile or narrower                     |
| plugin      | SANDBOXED     | IPC only — communicates through parent's pipes           |

---

## 3. VFS Views

### 3.1 Concept

A traditional Unix filesystem is a single global namespace. Every process sees
the same `/`, the same `/etc/passwd`, the same `/home`. Access control is
layered on top via UIDs, file permission bits, and optional MAC systems.

CLUU inverts this. Each process sees a **private namespace** defined by its
VFS view. Two processes running simultaneously may see completely different
directory trees. Process A might see `/bin` and `/tmp`. Process B might see
`/bin`, `/lib`, and `/data`. Neither can discover that the other's paths exist.

A VFS view is not a chroot. A chroot changes the root but still exposes the
full subtree. A VFS view is an explicit allowlist of path prefixes with
per-prefix access modes. Paths outside the view don't return "permission denied"
— they return "not found" (`ENOENT`). From the process's perspective, those
paths simply don't exist.

This is enforced by the VFS service (a userspace process), not the kernel.
The kernel doesn't know about paths at all — it only sees IPC messages between
the client and VFS. VFS checks the client's view on every operation.

### 3.2 View Structure

```rust
// userspace/vfs/src/view.rs

/// A path prefix rule within a VFS view.
struct ViewMount {
    /// Real path in the backing filesystem (e.g., "/var/containers/42/data")
    src: String,
    /// Virtual path as seen by the client (e.g., "/data")
    dst: String,
    /// Access mode: read-only or read-write.
    writable: bool,
}

/// Per-client VFS view.
struct VfsView {
    /// IPC endpoint that identifies this client (used as lookup key).
    client_id: usize,
    /// Ordered list of mount rules. First match wins.
    mounts: Vec<ViewMount>,
}
```

### 3.3 Path Resolution

When a client sends `open("/data/config.txt", O_RDONLY)`:

```
1. VFS receives the IPC message, identifies client by sender endpoint.

2. VFS looks up the client's VfsView.
   If no view is registered → use the default view for the client's profile.
   If no profile is known → reject with EACCES.

3. VFS iterates the view's mount list looking for a matching prefix:
   - Mount { src: "/var/containers/42/data", dst: "/data", writable: true }
   - Does "/data/config.txt" start with "/data"?  Yes.
   - Rewrite: "/var/containers/42/data" + "/config.txt"
   - The real path is "/var/containers/42/data/config.txt".

4. VFS opens the real path in the backing filesystem.
   If the real path doesn't exist → ENOENT.
   If the operation is write and writable=false → EACCES.
   Otherwise → proceed normally.
```

If no mount matches the requested path, VFS returns `ENOENT`. The client
cannot distinguish "path not in my view" from "path doesn't exist on disk."

### 3.4 Default Views by Profile

These are the views procmgr instructs VFS to install when no explicit view
is provided in the spawn request.

| Profile     | Mounts                                                      |
|-------------|-------------------------------------------------------------|
| SANDBOXED   | (empty — no filesystem access at all)                       |
| USER        | `/bin` (ro), `/lib` (ro), `/tmp` (rw), `/home/<user>` (rw) |
| SERVICE     | `/bin` (ro), `/lib` (ro), `/dev` (rw), `/etc` (ro), `/tmp` (rw) |
| SUPERVISOR  | `/` (rw) — full access                                       |

The `<user>` in USER views is determined by the spawning shell's identity
(future: user session management). For now, all USER processes get `/home/root`.

### 3.5 View Communication Protocol

When procmgr spawns a process, it tells VFS the new client's view via a new
IPC message:

```
VFS_SET_VIEW_LABEL (new label, value TBD)
  words[0] = payload length
  words[1] = client_id (the new process's VFS endpoint token)
  words[2] = mount count

Payload:
  For each mount:
    u16 src_len LE
    u16 dst_len LE
    u8  flags       (bit 0 = writable)
    src_bytes       (no NUL)
    dst_bytes       (no NUL)
```

VFS stores the view and begins enforcing it immediately. If VFS receives a
request from a client_id with no registered view, it falls back to the
profile-based default (which requires VFS to also know the client's profile —
communicated in the same message or a preceding one).

### 3.6 View Narrowing

A parent with view `{/bin(ro), /lib(ro), /tmp(rw)}` can grant a child:
- `{/bin(ro), /tmp(rw)}` — subset of paths, same modes. **Allowed.**
- `{/bin(ro), /tmp(ro)}` — same paths, stricter mode. **Allowed.**
- `{/bin(ro)}` — fewer paths. **Allowed.**

A parent **cannot** grant:
- `{/bin(ro), /etc(ro)}` — `/etc` not in parent's view. **Rejected.**
- `{/bin(rw)}` — parent has `/bin` as ro, child requests rw. **Rejected.**

Procmgr validates this by checking: for every mount in the child's requested
view, there must exist a mount in the parent's view where `child.dst` is a
prefix of `parent.dst` and `child.writable <= parent.writable`.

---

## 4. Container Model

### 4.1 Concept: Every Process is a Container

In Docker, a "container" is a special thing. You have processes, and then you
have containers, and they're different. You `docker run` to start a container
and `./myapp` to start a process, and the two mechanisms are separate.

In CLUU there is no distinction. Every process, from the system shell to a
sandboxed plugin, is a container. The word "container" just means "the
isolation boundary around a process," and that boundary is always present:

```
Container = CapProfile + VfsView + PrivateStorage
```

- **CapProfile** determines what kernel operations the process can perform.
- **VfsView** determines what filesystem paths the process can see.
- **PrivateStorage** is an optional isolated directory for persistent data.

A shell running `/bin/ls` creates a container. It just happens to be a
container that inherits most of the shell's own context. A manifest-based
application creates a container with explicitly declared boundaries. Both
go through the same spawn path, the same validation, the same isolation
mechanisms.

This unification has a practical benefit: there is no "escape from container
to host" because there is no host. There are only containers with wider or
narrower views. The root container (procmgr) has the widest view, and
everything else is a narrowing of that.

### 4.2 Inherited Containers

When a parent spawns a child without providing an explicit manifest, the child
**inherits** the parent's context with optional narrowing:

```
Shell (USER, view={/bin,/lib,/tmp,/home/root})
  │
  │ spawn("/bin/ls", profile=USER, view=inherit)
  │
  └─ ls (USER, view={/bin,/lib,/tmp,/home/root})
       │ same profile, same view, ephemeral storage
       │ exits → storage cleaned up automatically
```

The parent can narrow:
- **Profile:** `spawn("/bin/worker", profile=SANDBOXED)` — child gets IPC only.
- **View:** `spawn("/bin/editor", view={/tmp})` — child sees only /tmp.
- **Both:** `spawn("/bin/plugin", profile=SANDBOXED, view={})` — no fs, IPC only.

If the parent specifies nothing, the child inherits everything. This is the
common case for shell commands. The parent doesn't need to think about isolation
unless it wants to restrict the child.

### 4.3 Image Containers

An image container is a self-describing package that declares its own
requirements. It is a tar archive with a mandatory manifest:

```
myapp.container.tar
├── manifest.toml       ← declares profile, VFS mounts, exec info
├── bin/
│   └── myapp           ← ELF binary
└── data/               ← optional seed data (copied to private storage)
    └── config.json
```

The manifest is the container's "birth certificate" — it declares what
capabilities and paths the container needs. Procmgr reads the manifest,
validates it against the caller's profile (no escalation), and creates the
process accordingly.

#### manifest.toml Schema

```toml
[container]
name = "myapp"                    # Human-readable name (required)
version = "1.0.0"                 # Semver (required)
description = "My application"    # Optional

[profile]
# Capability bits requested. Must be a subset of the caller's profile.
capabilities = ["ipc", "vfs", "registry"]

[vfs]
# Mount rules. "src" paths starting with "/" are absolute (backed by the
# real filesystem, validated against caller's view). "src" paths without
# "/" are relative to the container image (extracted to private storage).
mounts = [
    { src = "/lib",    dst = "/lib",    mode = "ro" },
    { src = "/bin",    dst = "/bin",    mode = "ro" },
    { src = "data/",   dst = "/data",   mode = "rw", seed = true },
]

[exec]
binary = "bin/myapp"              # Path within the container image
args = ["--config", "/data/config.json"]
env = ["APP_MODE=production"]     # Additional env vars (merged with defaults)

[storage]
persistent = true                 # Keep /data across container restarts
quota = "16M"                     # Maximum private storage size (future)
```

Capability names in `[profile].capabilities` map directly to CapProfile bit
names (lowercase): `"ipc"` → `CAP_IPC`, `"spawn"` → `CAP_SPAWN`, etc.

### 4.4 Private Storage

Each container gets an isolated storage directory. The container sees it as
a local path (e.g., `/data`), but the real backing path is scoped by
container ID:

```
/var/containers/
├── c-00001/                     ← container ID
│   ├── data/                    ← persistent (rw), survives restart
│   ├── tmp/                     ← ephemeral, cleared on restart
│   └── log/                     ← append-only log sink
├── c-00002/
│   ├── data/
│   ├── tmp/
│   └── log/
...
```

The VFS view maps these transparently:

```
Container c-00001 sees:
  /data    → /var/containers/c-00001/data  (rw)
  /tmp     → /var/containers/c-00001/tmp   (rw)
  /log     → /var/containers/c-00001/log   (append)
```

The container cannot see `/var/containers/` itself, nor any other container's
storage. It cannot discover that other containers exist.

#### Storage Lifecycle

```
Container created (spawn):
  1. Procmgr assigns container ID (monotonic counter: c-00001, c-00002, ...)
  2. VFS creates /var/containers/<id>/{data,tmp,log} directories
  3. If image container with seed=true mounts:
     - Extract seed data from tar into /var/containers/<id>/data/
  4. VFS view maps virtual paths to real paths

Container running:
  - Process reads/writes through VFS view
  - Writes to /data persist on disk
  - Writes to /tmp are ephemeral
  - Writes to /log are append-only (no truncate, no delete)

Container stopped (exit):
  - /tmp is deleted
  - /data persists (if storage.persistent=true in manifest)
  - /log persists
  - VFS view entry is removed

Container destroyed (explicit):
  - All of /var/containers/<id>/ is deleted
  - Container ID is freed for reuse (or retired permanently)
```

#### Storage Backend

Currently CLUU uses initrd (ram-backed) for boot and ext2 (via virtio-blk)
for persistent storage. Private container storage lives on ext2. If no ext2
partition is mounted, persistent=true is downgraded to ephemeral with a
warning logged.

### 4.5 Nested Containers

Containers can spawn child containers. The three rules apply recursively:

```
procmgr (SUPERVISOR, view=/)
  │
  ├─ vtmgr (SERVICE, view={/bin,/lib,/dev,/etc,/tmp})
  │    spawns tty:1 via procmgr
  │
  ├─ tty:1 (SERVICE, view={/bin,/lib,/dev,/etc,/tmp})
  │    spawns shell:1 via procmgr
  │
  └─ shell:1 (USER, view={/bin,/lib,/tmp,/home/root})
       │
       ├─ runs "ls" (USER, view=inherited)
       │
       ├─ runs "container run editor.container"
       │    │ manifest requests: profile=[ipc,vfs,registry], mounts=[/lib,/data]
       │    │ shell has USER=[ipc,spawn,registry,vfs]
       │    │ [ipc,vfs,registry] ⊆ [ipc,spawn,registry,vfs] → allowed
       │    │ /lib is in shell's view → allowed
       │    │ /data comes from image seed → allowed (private storage)
       │    │
       │    └─ editor (USER-subset, view={/lib(ro),/data(rw)})
       │         │ cannot spawn (no CAP_SPAWN)
       │         │ cannot see /bin, /tmp, /home
       │         │ has its own /data backed by private storage
       │         │
       │         └─ (cannot spawn children — no CAP_SPAWN bit)
       │
       └─ runs "sandbox plugin.wasm"
            └─ plugin (SANDBOXED, view={})
                 no filesystem, no registry, no spawn
                 communicates only through stdin/stdout pipes
```

#### Nesting Validation

For every spawn, procmgr checks:

```
fn validate_spawn(caller: &Process, request: &SpawnRequest) -> Result<()> {
    // Rule 1: capabilities only narrow
    if !caller.profile.can_grant(request.profile) {
        return Err(Error::PermissionDenied);
    }

    // Rule 2: VFS view only narrows
    for mount in &request.view.mounts {
        if !caller.view.contains_prefix(mount.dst) {
            return Err(Error::PermissionDenied);
        }
        if mount.writable && !caller.view.is_writable(mount.dst) {
            return Err(Error::PermissionDenied);
        }
    }

    // Rule 3: we're in procmgr, so this is always satisfied
    Ok(())
}
```

---

## 5. Spawn Protocol

All process creation goes through procmgr. There are two spawn labels:
one for system services (privileged, initrd-only) and one for user programs
(general purpose, supports argv/env/fd actions).

### 5.1 Service Spawn (PROCMGR_SPAWN_SERVICE_LABEL = 20)

Used by system services (vtmgr, etc.) to spawn other system services from
initrd. This is the "simple" spawn path — no argv, no env, no exit tracking.
The spawned process gets a ProcessInfo with tokens derived per the requested
mode but no PID and no exit notification.

```
Message header:
  label    = 20 (PROCMGR_SPAWN_SERVICE_LABEL)
  words[0] = payload length (for parse_message compatibility)
  words[1] = scheduling priority (0-255, higher = more important)
  words[2] = TOKEN_EXTRA_0 mode:
               0 = none (slot 9 left empty)
               1 = listen endpoint (IPC_RECV only)
               2 = grantable endpoint (IPC_RECV + IPC_SEND + IPC_CALL + GRANT)
  words[3] = param override count (0-10)
  words[4] = (reserved, 0)
  words[5] = (reserved, 0)

Payload:
  path\0                                    NUL-terminated initrd path
  [u16 param_index LE + u64 value LE] × N  10 bytes per param override
```

**Current policy checks:**
- Path must start with `sys/` (initrd system binaries only).
- Param index must be 0-9.
- Token mode must be 0, 1, or 2.
- The spawn endpoint itself is capability-gated (caller must hold procmgr's
  `spawn` output token).

**After CapProfile integration (Phase B):**
- Caller must have `CAP_SPAWN` in their profile.
- A `words[4] = requested_profile` field will be added.
- Requested profile must be a subset of caller's profile.

### 5.2 User Spawn (PROCMGR_SPAWN_LABEL = 2)

Used by shells and user programs. Supports argv, env, fd actions (pipes),
exit notification, and PID tracking. Sent via `ipc_call` (synchronous —
caller blocks until procmgr replies with PID).

```
Message header (via ipc_call):
  label    = 2 (PROCMGR_SPAWN_LABEL)
  words[0] = payload length
  words[1] = argc
  words[2] = fdac_offset (byte offset of fd actions in payload, 0=none)
  words[3] = notify_endpoint (for exit notification, 0=none)
  words[4] = envc (environment variable count)
  words[5] = env_payload_offset (byte offset of env data in payload)

Payload:
  path\0                    NUL-terminated absolute path (e.g., "/bin/ls")
  argv[0]\0 argv[1]\0 ...  NUL-terminated argument strings
  env[0]\0 env[1]\0 ...    NUL-terminated "KEY=VALUE" strings
  FDAC block                fd actions (pipe redirections, see below)

Reply:
  words[0] = error code (0=success, nonzero=errno)
  words[1] = PID
  words[2] = exit cookie (for waitpid)
  words[3] = child stdin send token (for foreground I/O routing)
```

**After CapProfile integration (Phase B):**
- A new `words[?]` field (or payload extension) will carry the requested
  CapProfile bitmask and optional VFS view overrides.

### 5.3 Wire Example: vtmgr Spawning tty:1

```
vtmgr builds:
  path = "sys/tty\0"                   (8 bytes)
  param[0]: index=0 (PARAM_TTY_INSTANCE), value=1   (10 bytes)
  total payload = 18 bytes

  Message:
    label    = 20
    words[0] = 18     (payload length)
    words[1] = 205    (priority)
    words[2] = 2      (grantable endpoint)
    words[3] = 1      (1 param override)

  vtmgr calls send_msg_with_payload(procmgr_spawn_ep, &msg, &payload)

procmgr receives:
  parse_message extracts 18 bytes of payload
  reads words[1]=205, words[2]=2, words[3]=1
  parses path "sys/tty" from payload
  parses param override: index=0, value=1
  validates: path starts with "sys/", param index < 10, token mode ≤ 2
  spawns tty with PARAM_TTY_INSTANCE=1, priority=205, grantable endpoint
```

---

## 6. Current Architecture (vtmgr Migration)

The vtmgr migration extracted VT lifecycle management from init (which is
dead post-boot) and kbd (which was doing too much) into a dedicated
coordinator service.

### 6.1 Service Topology

```
┌─────────┐    IRQ     ┌──────┐  VTMGR_SWITCH_VT   ┌───────┐
│  kernel  │──────────→│  kbd  │──────────────────→│ vtmgr │
└─────────┘            └──────┘                    └───────┘
                          │                           │  │  │
                   KBD_EVENT to                       │  │  │
                   active tty                         │  │  │
                          │          CONSOLE_CREATE_VT│  │  │PROCMGR_SPAWN_SERVICE
                          ▼                           │  │  │
                    ┌──────────┐  CONSOLE_WRITE_VT    │  │  ▼
                    │  tty:N   │─────────────────→┌───────┐  ┌─────────┐
                    └──────────┘                  │console│  │ procmgr │
                          │                       └───────┘  └─────────┘
                   TTY_SPAWN_SHELL                    ▲
                          │           CONSOLE_ACTIVATE│
                          ▼           CONSOLE_DEACTIVATE
                    ┌──────────┐          │
                    │ shell:N  │     from vtmgr
                    └──────────┘
```

### 6.2 IPC Label Table

| Constant                   | Value | Direction          | Wire Format                         |
|---------------------------|-------|--------------------|-------------------------------------|
| VTMGR_SWITCH_VT_LABEL     | 15    | kbd → vtmgr        | words[0]=target VT index            |
| VTMGR_SPAWN_TTY_LABEL     | 16    | (unused, reserved) | —                                   |
| CONSOLE_CREATE_VT_LABEL   | 17    | vtmgr → console    | words[0]=VT index                   |
| CONSOLE_WRITE_VT_LABEL    | 18    | tty → console      | words[0]=len, words[1]=VT index     |
| CONSOLE_WRITE_VT_SYNC_LABEL| 19   | tty → console      | words[0]=len, words[1]=VT index     |
| PROCMGR_SPAWN_SERVICE_LABEL| 20   | any → procmgr      | See section 5.1                     |

### 6.3 VT Switch Flow (Detailed)

```
1. User presses Ctrl+Alt+F2.

2. kbd (IRQ handler) decodes scancode sequence:
   - Recognizes Ctrl+Alt+F2 as VT switch to index 1.
   - Calls ctx.switch_vt(1).

3. kbd.switch_vt(1):
   - Checks: 1 < VT_COUNT && 1 != active_vt.
   - Sends VTMGR_SWITCH_VT_LABEL with words[0]=1 to vtmgr.
   - Updates local active_vt = 1 (so keystrokes route to tty:1 immediately).

4. vtmgr receives VTMGR_SWITCH_VT_LABEL:
   - Checks vt_created bitmask: is bit 1 set?
     - No → calls create_vt(1):
       Sends CONSOLE_CREATE_VT_LABEL(1) to console.
       Sets vt_created |= (1 << 1).
   - Checks tty_spawned bitmask: is bit 1 set?
     - No → calls spawn_tty(1):
       Sends PROCMGR_SPAWN_SERVICE_LABEL to procmgr with path="sys/tty",
       PARAM_TTY_INSTANCE=1, priority=205, grantable endpoint.
       Sets tty_spawned |= (1 << 1).
   - Sends CONSOLE_DEACTIVATE_LABEL(0) to console.
   - Sends CONSOLE_ACTIVATE_LABEL(1) to console.
   - Updates active_vt = 1.

5. console receives CONSOLE_CREATE_VT_LABEL(1):
   - Allocates VtBuffer for index 1 (cell grid, fg/bg arrays, cursor state).

6. console receives CONSOLE_DEACTIVATE_LABEL(0):
   - Saves VT 0 state (cells, cursor) to VtBuffer[0].

7. console receives CONSOLE_ACTIVATE_LABEL(1):
   - Loads VT 1 state from VtBuffer[1] into active rendering fields.
   - Sets active=true, triggers full repaint from cell grid.

8. procmgr receives PROCMGR_SPAWN_SERVICE_LABEL:
   - Loads sys/tty from initrd, creates address space, maps ELF segments.
   - Creates grantable endpoint for TOKEN_EXTRA_0.
   - Sets params[PARAM_TTY_INSTANCE] = 1.
   - Starts thread at priority 205.

9. tty:1 starts up:
   - Reads PARAM_TTY_INSTANCE=1 from ProcessInfo.
   - Registers as "tty:1" with the IPC registry.
   - Subscribes to "console:0" write endpoint.
   - Sends TTY_SPAWN_SHELL_LABEL to procmgr to spawn shell:1.

10. kbd discovers tty:1 via registry subscription:
    - Registry grants tty:1's "main" endpoint to kbd.
    - kbd stores it in tty_endpoints[1].
    - Subsequent keystrokes are sent to tty:1 (since active_vt=1).

11. VT 1 is now fully operational with its own shell.
```

---

## 7. Implementation Phases

### Phase A: Stabilize vtmgr + Fix Build — COMPLETE

Fix compilation errors from the interrupted refactor. Get the codebase back
to a green build with passing harness tests.

| #  | Task                                              | Status    |
|----|---------------------------------------------------|-----------|
| A1 | Add `send_msg_with_payload` to ipc.rs             | done      |
| A2 | Fix vtmgr imports (`send`, `send_msg_with_payload`) | done   |
| A3 | Fix spawn protocol word layout (words[1..3])      | done      |
| A4 | Clean unused imports in vtmgr/main.rs             | done      |
| A5 | Remove unused VTMGR_SPAWN_TTY_LABEL (16)         | done      |
| A6 | Verify clean build (`cargo xtask build`)          | done      |
| A7 | Run harness tests (m1_recv, m2_token_audit)       | done      |

### Phase B: CapProfile Foundation — COMPLETE

Introduce the profile bitmask type and integrate it into procmgr's spawn
path. No VFS changes yet — profiles only gate token rights at spawn time.

| #  | Task                                              | Status    |
|----|---------------------------------------------------|-----------|
| B1 | Create `libcluu/src/cap.rs` with CapProfile bitflags | done   |
| B2 | Add `PARAM_CAP_PROFILE` constant to boot.rs      | done      |
| B3 | Add `profile_to_rights()` in procmgr              | done      |
| B4 | Store caller profile in procmgr bookkeeping       | done      |
| B5 | Validate profile in `handle_service_spawn`        | done      |
| B6 | Validate profile in `handle_spawn_message`        | done      |
| B7 | Derive token rights based on profile               | done      |
| B8 | Write child's profile into PARAM_CAP_PROFILE      | done      |
| B9 | Assign profiles to init-spawned services           | done      |
| B10 | Build + harness verification (all 4 pass)         | done      |

### Phase C: VFS Views — COMPLETE

Add per-client path filtering to VFS. Processes receive a scoped view at
spawn time, and VFS enforces it on every file operation.

| #  | Task                                              | Status    |
|----|---------------------------------------------------|-----------|
| C1 | Define VfsView + ViewMount structs in vfs/src/view.rs | done    |
| C2 | Add per-client view storage (BTreeMap) in VFS     | done      |
| C3 | Add VFS_SET_VIEW_LABEL IPC handler                | done      |
| C4 | Path rewriting in VFS open/read/write/stat/readdir | done     |
| C5 | Default view generation from CapProfile            | done      |
| C6 | Procmgr: send VFS_SET_VIEW after spawn            | done      |
| C7 | View narrowing validation in procmgr              | done      |
| C8 | Inherit parent view when child specifies none     | done      |
| C9 | Build + test: USER process cannot see /dev         | done      |

Latest verification snapshot (2026-02-22):
- `cargo xtask clean-full && cargo xtask build` passed.
- Harness regressions passed for `m4_deny_paths`, `m4_registry_deny_paths`, and `l2_owner_deny`.

### Phase D: Private Storage

Per-container isolated storage areas, managed by VFS.

| #  | Task                                              | Status    |
|----|---------------------------------------------------|-----------|
| D1 | Container ID generation in procmgr (monotonic)    | done      |
| D2 | VFS: create `/var/containers/<id>/` on spawn      | done      |
| D3 | VFS: auto-add private storage mounts to view      | done      |
| D4 | VFS: clean tmp/ on container exit                 | done      |
| D5 | VFS: delete all of `<id>/` on destroy             | done      |
| D6 | Procmgr: track container_id in process bookkeeping | done     |
| D7 | Build + test: two processes have isolated /data    | pending   |

### Phase E: Container Images

Manifest-based container packaging and loading from tar archives.

| #  | Task                                              | Status    |
|----|---------------------------------------------------|-----------|
| E1 | Define manifest.toml schema (finalize section 4.3) | pending  |
| E2 | Add TOML parser to libcluu (minimal, no_std)      | pending   |
| E3 | Manifest parsing in procmgr                       | pending   |
| E4 | Extract binary from container tar                 | pending   |
| E5 | Extract seed data to private storage              | pending   |
| E6 | Profile + view from manifest (validated as always) | pending  |
| E7 | Shell builtin: `container run <path>`             | pending   |
| E8 | Shell builtin: `container list`                   | pending   |
| E9 | Shell builtin: `container stop <id>`              | pending   |
| E10 | Build + test: run a container image from shell   | pending   |

### Phase F: Nested Containers

Recursive spawn with profile and view narrowing, end-to-end.

| #  | Task                                              | Status    |
|----|---------------------------------------------------|-----------|
| F1 | Profile narrowing: child ⊆ parent validation     | pending   |
| F2 | View narrowing: child mounts ⊆ parent mounts     | pending   |
| F3 | Nested private storage scoping                    | pending   |
| F4 | Test: SERVICE spawns USER spawns SANDBOXED        | pending   |
| F5 | Test: escalation attempts are rejected            | pending   |
| F6 | Test: view escape attempts return ENOENT          | pending   |

---

## 8. File Map

Files created or modified across all phases:

| File                               | Phases  | Purpose                              |
|------------------------------------|---------|--------------------------------------|
| `userspace/libcluu/src/cap.rs`     | B       | CapProfile bitflags + helpers        |
| `userspace/libcluu/src/lib.rs`     | B       | Export cap module                    |
| `userspace/libcluu/src/boot.rs`    | B       | PARAM_CAP_PROFILE constant           |
| `userspace/libcluu/src/ipc.rs`     | A, C    | send_msg_with_payload, VFS_SET_VIEW  |
| `userspace/procmgr/src/main.rs`    | A,B,D,E | Profile-gated spawn, container IDs   |
| `userspace/vtmgr/src/context.rs`   | A       | Fix spawn protocol                   |
| `userspace/vtmgr/src/main.rs`      | A       | Clean unused imports                 |
| `userspace/vfs/src/main.rs`        | C, D    | View enforcement, storage lifecycle  |
| `userspace/vfs/src/view.rs`        | C       | VfsView struct + path filter logic   |
| `userspace/init/src/services.rs`   | B       | Profile assignments per service      |
| `userspace/shell/src/main.rs`      | E       | Container shell builtins             |
| `docs/PROCESS_ISOLATION_DESIGN.md` | all     | This document (updated per phase)    |

---

## 9. Security Properties

### What This Design Prevents

| Attack                           | Prevention Mechanism                          |
|----------------------------------|-----------------------------------------------|
| Privilege escalation via spawn   | Profile bitmask: child ⊆ parent (arithmetic)  |
| Filesystem escape (path traversal)| VFS view: paths outside view → ENOENT         |
| Cross-container data access      | Private storage: each container has unique ID  |
| Unauthorized device access       | CAP_DEVICE bit: no bit → no device tokens      |
| Service impersonation            | Registry gated by CAP_REGISTRY bit             |
| Fork bomb                        | CAP_SPAWN bit: no bit → cannot spawn           |
| Token forgery                    | Kernel: token_derive enforces right narrowing  |

### What This Design Does NOT Prevent (Yet)

| Attack                    | Status                                          |
|---------------------------|-------------------------------------------------|
| Resource exhaustion (RAM) | No per-process memory quotas yet                |
| CPU starvation            | Priority-based scheduling only, no hard limits  |
| Covert channels (timing)  | Not addressed — common in capability systems   |
| Storage quota enforcement | Planned (manifest.toml `quota` field) but not implemented |
