# CLUU Process Isolation & Containerization Design

**Date:** 2026-02-20
**Scope:** Capability profiles, VFS views, container model, spawn protocol
**Status:** Implementation in progress — Phases A–E mostly complete; Phases F–H in design
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
  │ reads /etc/users.toml for authentication
  │
  ├─ Tier 2 autostart (from /etc/autostart.toml)
  │   ├─ vtmgr → manages VT lifecycle
  │   │   └─ VT containers (IPC+REGISTRY): tty:N (terminal driver)
  │   ├─ kbd, console, ...
  │   │
  │   └─ (VT containers are I/O adapters, NOT session parents)
  │
  └─ Sessions (top-level, parent_container_id=0)
      │ spawned by procmgr after authentication
      │ attached to VT via IPC wiring (not lifecycle dependency)
      │
      Session container (USER/ADMIN, per user record)
        shell:N (entrypoint = user's shell)
        ├─ user program (USER or narrower)
        ├─ container run editor → nested container
        ├─ sudo cmd → elevated container (ADMIN)
        └─ su bob → nested session (Bob's view)
```

**Lifecycle separation:** VTs and sessions are siblings, not parent-child.
VT death does not cascade to the session — vtmgr respawns the VT and
reattaches to the surviving session. Session death (logout) cascades to
user containers but leaves the VT alive to show a new login prompt.

**Current state (pre-Phase G):** vtmgr still uses PROCMGR_SPAWN_SERVICE_LABEL
to spawn bare tty:N services. tty:N then independently requests shell:N via
TTY_SPAWN_SHELL_LABEL. Phase G migrates to VT containers; Phase H adds
the session/login model.

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

| Profile     | Bitmask      | Hex    | Capabilities                                  |
|-------------|--------------|--------|-----------------------------------------------|
| SANDBOXED   | `0b00000001` | `0x01` | IPC only                                      |
| USER        | `0b00001111` | `0x0F` | IPC, spawn, registry, VFS                     |
| ADMIN       | `0b10001111` | `0x8F` | USER + admin (system management)              |
| SERVICE     | `0b00111111` | `0x3F` | IPC, spawn, registry, VFS, device, space      |
| SUPERVISOR  | `0b11111111` | `0xFF` | Everything                                    |

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
    pub const SANDBOXED:       Self = Self::IPC;
    pub const USER:            Self = Self::IPC.union(Self::SPAWN)
                                      .union(Self::REGISTRY).union(Self::VFS);
    pub const PROFILE_ADMIN:   Self = Self::USER.union(Self::ADMIN);
    pub const SERVICE:         Self = Self::USER.union(Self::DEVICE)
                                      .union(Self::SPACE_GRANT);
    pub const SUPERVISOR:      Self = Self::SERVICE.union(Self::NET)
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

Every service gets its **minimal** profile — only the capability bits it
actually needs. Most services were historically over-privileged with the
full SERVICE bitmask.

| Service     | Profile       | Hex    | Bits Needed                             | Rationale                                  |
|-------------|---------------|--------|-----------------------------------------|--------------------------------------------|
| init        | SUPERVISOR    | `0xFF` | All                                     | Bootstrap authority, holds root token      |
| procmgr     | SUPERVISOR    | `0xFF` | All                                     | Sole spawn authority, derives all tokens   |
| vfs         | Custom        | `0x25` | IPC + REGISTRY + SPACE_GRANT            | Serve clients, advertise, zero-copy grants |
| virtio-blk  | Custom        | `0x37` | IPC + REGISTRY + DEVICE + SPACE_GRANT   | PCI BAR, DMA, shared memory with VFS       |
| console     | Custom        | `0x17` | IPC + REGISTRY + DEVICE                 | Framebuffer mapping, advertise endpoints   |
| kbd         | Custom        | `0x17` | IPC + REGISTRY + DEVICE                 | IRQ attachment, subscribe to tty endpoints |
| vtmgr       | Custom        | `0x0F` | IPC + SPAWN + REGISTRY + VFS            | Must can_grant(USER) for VT containers (Phase G) |
| registry    | Custom        | `0x05` | IPC + REGISTRY                          | Process subscriptions, self-register       |
| timeserver  | Custom        | `0x05` | IPC + REGISTRY                          | Serve time queries                         |
| VT (tty)    | Custom        | `0x05` | IPC + REGISTRY                          | Terminal driver only; session is separate (Phase H) |
| session     | USER or ADMIN | `0x0F`/`0x8F` | Per user record                  | User's login session container (see 4.9)   |
| shell       | USER          | `0x0F` | IPC + SPAWN + REGISTRY + VFS            | Standalone shell container                 |
| user program| USER          | `0x0F` | Inherits shell's profile or narrower    | —                                          |
| plugin      | SANDBOXED     | `0x01` | IPC only                                | Communicates through parent's pipes        |

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
| ADMIN       | USER + `/etc` (ro), `/var/log` (ro), `/var/services` (rw)   |
| SERVICE     | `/bin` (ro), `/lib` (ro), `/dev` (rw), `/etc` (ro), `/tmp` (rw) |
| SUPERVISOR  | `/` (rw) — full access                                       |

The `<user>` in USER and ADMIN views is determined by the session identity
(see section 4.9). For now, all USER processes get `/home/root`.

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

### 4.1 Containers as the Universal Isolation Boundary

In Docker, a "container" is a special thing separate from processes. You
`docker run` to start a container and `./myapp` to start a process, and
the two mechanisms are distinct. You can run processes outside containers.

In CLUU, **no process can exist outside a container**. Containers are not
a special mode — they are the universal isolation boundary. Every process
runs within one. Procmgr enforces this: any spawn request from a process
with `container_id == 0` is rejected.

A container is an isolation boundary that one or more processes share:

```
Container = ContainerId + CapProfile + VfsView + PrivateStorage
```

- **ContainerId** scopes the container's private storage and lifecycle.
- **CapProfile** determines what kernel operations processes can perform.
- **VfsView** determines what filesystem paths processes can see.
- **PrivateStorage** is an optional isolated directory for persistent data.

A container can hold multiple processes. The **entrypoint** is the process
spawned when the container starts (from the manifest). Other processes
are spawned within the container at runtime and share its boundary:

```
Container (container_id=5, image=vt)
├── tty     (entrypoint — defines container lifecycle)
├── shell   (spawned by tty, shares container_id=5)
├── ls      (spawned by shell, shares container_id=5)
└── cat     (spawned by shell, shares container_id=5)
```

All processes within a container share the same VFS view, the same
private storage, and the same container_id. They may have different
(narrowed) profiles — a parent can restrict a child's capabilities
without creating a new container.

Container lifecycle is tied to the entrypoint. When the entrypoint
exits, the container is destroyed and all remaining processes within
it are cleaned up.

This model has a structural security property: **there is no "host"
to escape to**. There are only containers with wider or narrower views.
The widest container (procmgr, SUPERVISOR) sees everything. Every other
container is a narrowing of that. A container escape would require
widening a VFS view or escalating a CapProfile, both of which are
prevented by the kernel's `token_derive` arithmetic.

### 4.2 Intra-Container Binary Spawn

A container image can bundle multiple binaries (via BUILD or COPY directives).
The entrypoint is spawned automatically when the container starts. Other
binaries can be spawned by any process within the container using the normal
`PROCMGR_SPAWN_LABEL` path.

**Enforcement rule:** Only processes that are already inside a container
(container_id ≠ 0) and have CAP_SPAWN can use `PROCMGR_SPAWN_LABEL`. Bare
binary spawn outside a container context is rejected. This ensures every
process runs within a container boundary.

The child **inherits** the parent's container context:

```
VT Container (container_id=5, image=vt)
  │
  tty:1 (entrypoint, USER, view={/bin→/var/images/vt/bin, ...})
  │
  │ spawn("/bin/shell", profile=USER, view=inherit)
  │
  └─ shell:1 (USER, view=inherited from tty:1)
       │ same container_id=5, same view, same profile
       │
       │ spawn("/bin/ls", profile=USER, view=inherit)
       │
       └─ ls (USER, view=inherited from shell:1)
            │ same container_id=5
            │ exits → cleaned up, container continues
```

The parent can narrow:
- **Profile:** `spawn("/bin/worker", profile=SANDBOXED)` — child gets IPC only.
- **View:** `spawn("/bin/editor", view={/tmp})` — child sees only /tmp.
- **Both:** `spawn("/bin/plugin", profile=SANDBOXED, view={})` — no fs, IPC only.

If the parent specifies nothing, the child inherits everything. This is the
common case for shell commands.

**View-aware binary loading:** When procmgr receives a spawn request for
`/bin/ls`, it must resolve the path through the **caller's** VFS view, not
procmgr's own SUPERVISOR view. The caller's view maps `/bin` →
`/var/images/vt/bin/`, so procmgr loads `/var/images/vt/bin/ls` from the
real filesystem. This ensures:
- Containers can only spawn binaries visible in their view.
- No hardcoded paths in procmgr — binary location is determined by the
  container image layout.
- Different containers can have different `/bin` contents.

### 4.3 Image Containers

An image container is a self-describing package that declares its own
requirements. It is built from a **Cluufile** (a Dockerfile-like declarative
build file) and stored as a pre-extracted directory on the ext2 disk.

#### Cluufile

The Cluufile is the source of truth for building container images. It uses
a line-oriented, keyword-driven syntax inspired by Dockerfile:

```dockerfile
# containers/myapp/Cluufile
FROM base

PROFILE ipc vfs
ENTRYPOINT /bin/myapp --config /data/config.json

COPY target/x86_64-cluu-user/debug/myapp.elf /bin/myapp
COPY config/default.json /data/config.json

PERSISTENT /data
ENV APP_MODE=production
```

| Directive     | Syntax                          | Required | Description                                                  |
|---------------|---------------------------------|----------|--------------------------------------------------------------|
| `FROM`        | `FROM base`                     | Yes      | Base layer. `base` = shared sysroot (/bin, /lib).            |
| `PROFILE`     | `PROFILE cap1 cap2 ...`         | Yes      | Space-separated CapProfile bit names.                        |
| `ENTRYPOINT`  | `ENTRYPOINT /path arg1 arg2`    | Yes      | Binary path (container-relative) + arguments.                |
| `COPY`        | `COPY <host-path> <ctr-path>`   | No       | Copy file from build host into image. Host path is relative to Cluufile dir. |
| `PERSISTENT`  | `PERSISTENT /path`              | No       | Directory that survives container restart.                   |
| `ENV`         | `ENV KEY=VALUE`                 | No       | Environment variable for the container process.              |

Parsing rules: one directive per line, `#` comments, `FROM` must be first
non-comment directive. Multiple `COPY`, `ENV`, `PERSISTENT` allowed. Only
one `FROM`, `PROFILE`, `ENTRYPOINT`. Unknown directives are errors.

Capability names map directly to CapProfile bits (lowercase): `ipc`, `vfs`,
`spawn`, `registry`, `device`, `space_grant`, `net`, `admin`. Unknown names
are rejected (fail-fast, not silently ignored).

#### Build Pipeline

`cargo xtask container-build <cluufile-path>`:

1. Parse Cluufile directives.
2. Resolve `FROM base`: merge the sysroot's `/bin` and `/lib` into the
   image directory. This makes each image self-contained — no runtime
   overlay or layer fallback needed.
3. Process `COPY` directives: copy specified files into their container paths.
4. Generate `manifest.toml` from `PROFILE`, `ENTRYPOINT`, `ENV`,
   `PERSISTENT` directives.
5. Output: `target/containers/<name>/` with manifest.toml + merged files.
6. During `cargo xtask build`, copy to `/var/images/<name>/` on the ext2
   userdisk image.

#### Image Layout on Disk

```
/var/images/
└── myapp/
    ├── manifest.toml       ← runtime manifest (generated from Cluufile)
    ├── bin/
    │   ├── myapp           ← COPY'd binary
    │   ├── ls              ← merged from base sysroot
    │   └── shell           ← merged from base sysroot
    ├── lib/
    │   └── ...             ← merged from base sysroot
    └── data/
        └── config.json     ← COPY'd seed data
```

`FROM base` does not copy base files at runtime. It merges them at build
time. Each image is self-contained under `/var/images/<name>/`. This trades
disk space for simplicity — no runtime overlay, COW, or whiteouts needed.

#### manifest.toml (Generated)

The manifest is generated from the Cluufile by xtask. Procmgr reads it at
runtime. It is intentionally simple (flat TOML, no nested structures) to
keep the no_std TOML parser minimal.

```toml
# Auto-generated from Cluufile — do not edit
[container]
name = "myapp"

[profile]
capabilities = ["ipc", "vfs"]

[exec]
binary = "/bin/myapp"
args = ["--config", "/data/config.json"]

[storage]
persistent_dirs = ["/data"]

[[env]]
key = "APP_MODE"
value = "production"
```

#### Container Run Flow

When a user runs `container run myapp`:

1. Shell sends `PROCMGR_CONTAINER_RUN_LABEL` (24) with image name as payload.
2. Procmgr reads `/var/images/myapp/manifest.toml` via VFS.
3. Procmgr validates: `requested_profile ⊆ caller.profile` (can_grant).
4. Allocates container_id via `next_container_id()`.
5. Creates container dirs: `/var/containers/c-N/{data,tmp,log}` (Phase D).
6. Copies seed data from `/var/images/myapp/data/` to `/var/containers/c-N/data/`
   (only on first run or if target is empty).
7. Builds view mounts (first-match-wins ordering):
   ```
   /tmp   → /var/containers/c-N/tmp       (rw)   ← writable ephemeral
   /data  → /var/containers/c-N/data      (rw)   ← writable persistent
   /log   → /var/containers/c-N/log       (rw)   ← writable log
   /bin   → /var/images/myapp/bin         (ro)   ← merged base + image
   /lib   → /var/images/myapp/lib         (ro)   ← merged base + image
   ```
8. Spawns thread from `/var/images/myapp/bin/myapp` (procmgr loads ELF
   via its SUPERVISOR view — child never loads its own binary).
9. Registers VFS view with container_id (child can now access VFS).
10. Tracks instance: pid, container_id, image name.

**Critical ordering:** Steps 5-6 (create dirs, copy seed) MUST precede
step 8 (thread_create). If seed copy happens after spawn, there's a race
where the child accesses `/data` before seed files exist.

#### Image Security Properties

- Images stored at `/var/images/` on ext2, outside USER view — containers
  cannot see or modify their own base images.
- VFS enforces read-only on image mounts (writable=false in ViewMount).
- Manifest capabilities are validated as subset of caller's profile.
- Procmgr loads the binary, not the container — no path traversal risk.
- Unknown capability names in manifest are rejected, not silently ignored.

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

There are two ways a process spawns another process. They use different
IPC labels, different validation, and different wiring:

| | Intra-container spawn | Container run |
|---|---|---|
| IPC label | PROCMGR_SPAWN_LABEL (2) | PROCMGR_CONTAINER_RUN_LABEL (24) |
| Binary source | Caller's VFS view (`/bin/ls`) | Image manifest (`/var/images/foo/bin/foo`) |
| Container_id | Inherited from parent | Fresh (new container) |
| Profile | Inherited (or narrowed) | From manifest (validated ⊆ caller) |
| VFS view | Inherited (or narrowed) | Launcher's view + image_dirs (see below) |
| Tokens | Profile-based + FDAC | Manifest-based (endpoints, devices, params) |
| Private storage | Shared with parent container | Own container storage |
| Use case | `ls`, `cat`, shell→helper | `container run editor`, vtmgr→vt |

#### Container Run View Construction

When a process inside a container runs `container run editor`, the child
container's VFS view is built by **combining the launcher's view with the
container image**. The principle: if the child has CAP_VFS, it sees what
the launcher sees, except that image-provided directories (`/bin`, `/lib`)
are replaced by the container's own.

**Rule: image dirs override, everything else passes through.**

```
Alice's session view (launcher):
  /bin          → /var/images/vt/bin          (ro)
  /lib          → /var/images/vt/lib          (ro)
  /home/alice   → /home/alice                 (rw)
  /usr          → /usr                        (ro)
  /tmp          → /var/containers/c-5/tmp     (rw)

Alice runs: container run editor

Editor container view (child):
  /bin          → /var/images/editor/bin      (ro)  ← image overrides
  /lib          → /var/images/editor/lib      (ro)  ← image overrides
  /home/alice   → /home/alice                 (rw)  ← passed through
  /usr          → /usr                        (ro)  ← passed through
  /tmp          → /var/containers/c-8/tmp     (rw)  ← container-scoped
```

The editor can open any file Alice can access — `/home/alice/docs/report.txt`,
`/usr/share/data/fonts.dat`, etc. The "Open File" dialog works naturally.
The only difference from a bare binary: `/bin` and `/lib` come from the
editor's image instead of Alice's session container.

**Why this respects "views only narrow":**

The child's user-visible paths (everything except image dirs and /tmp) are
a subset of the launcher's paths. The image dirs (`/bin`, `/lib`) point to
different real paths but are read-only and image-scoped — they provide the
container's own binaries, not escalated access.

**Containers without CAP_VFS** (sandboxed) get no paths at all. No image
dirs, no user dirs, nothing. They communicate only through stdio pipes.

**Containers that want restricted access** can opt out of specific paths
in their manifest:

```toml
# Restrict: only image dirs + private storage, no user paths
[mounts]
deny_inherit = true

# Or selectively deny specific paths
[mounts]
deny = ["/home"]
```

By default, user-visible paths pass through. Restriction is opt-in by
the container author. This matches user expectations: **if you can see it
in your shell, you can see it in any app you launch** — unless that app
explicitly declares it doesn't need your data.

Future work: a sophisticated ownership model can refine which paths are
(ro) vs (rw) based on user identity, file ownership, and ACLs. For now,
the launcher's view modes are inherited as-is.

#### Container Run Examples

```
Alice's session (container_id=5, USER)
  view: /bin(ro), /lib(ro), /home/alice(rw), /usr(ro), /tmp(rw)
  │
  shell (entrypoint)
  │
  ├─ container run editor                       NEW container_id=8
  │    manifest: profile=[ipc,vfs,registry]
  │    USER can_grant([ipc,vfs,registry]) → allowed
  │    view: /bin(ro,image), /lib(ro,image),
  │          /home/alice(rw), /usr(ro), /tmp(rw,scoped)
  │    │
  │    └─ editor sees all Alice's files + own binaries
  │
  ├─ container run sandbox                      NEW container_id=9
  │    manifest: profile=[ipc]
  │    no CAP_VFS → empty view
  │    │
  │    └─ sandbox sees nothing, communicates via pipes
  │
  └─ container run game                         NEW container_id=10
       manifest: profile=[ipc,vfs], deny_inherit=true
       view: /bin(ro,image), /lib(ro,image), /tmp(rw,scoped)
       │
       └─ game has VFS but only sees its own binaries + temp
            no access to /home/alice, /usr — manifest opted out
```

#### Nesting Validation

For every container run, procmgr checks:

```
const MAX_NESTING_DEPTH: u32 = 8;

fn validate_container_run(
    caller: &Process,
    manifest: &Manifest,
) -> Result<ContainerView> {
    // Rule 0: nesting depth limit
    let caller_depth = container_depth(caller.container_id);
    if caller_depth >= MAX_NESTING_DEPTH {
        return Err(Error::NestingLimitExceeded);
    }

    // Rule 1: capabilities only narrow
    if !caller.profile.can_grant(manifest.profile) {
        return Err(Error::PermissionDenied);
    }

    // Rule 2: build child view from launcher's view + image
    let container_id = allocate_container_id();
    let mut child_view = Vec::new();

    // Image dirs override launcher's paths (read-only, from image)
    for dir in &manifest.image_dirs {
        child_view.push(ViewMount {
            src: format!("/var/images/{}/{}", manifest.name, dir),
            dst: format!("/{}", dir),
            writable: false,
        });
    }

    // Container-scoped /tmp (always ephemeral)
    child_view.push(ViewMount {
        src: format!("/var/containers/c-{}/tmp", container_id),
        dst: "/tmp".into(),
        writable: true,
    });

    // Pass through launcher's user-visible paths (unless manifest denies)
    if manifest.profile.contains(CAP_VFS) && !manifest.deny_inherit {
        for mount in &caller.view.mounts {
            let already_overridden = child_view.iter()
                .any(|m| m.dst == mount.dst);
            if !already_overridden && !manifest.denied(mount.dst) {
                child_view.push(mount.clone());
            }
        }
    }

    // Rule 3: record parent relationship for cascading cleanup
    // Detached containers become top-level (parent=0), skipping cascade.
    let parent = if manifest.detach { 0 } else { caller.container_id };
    register_container(ContainerInfo {
        container_id,
        parent_container_id: parent,
        // ...
    });

    Ok(child_view)
}
```

For intra-container binary spawns (`PROCMGR_SPAWN_LABEL`), the simpler
inherited-view rule applies:

```
fn validate_intra_spawn(caller: &Process, request: &SpawnRequest) -> Result<()> {
    // Rule 1: capabilities only narrow
    if !caller.profile.can_grant(request.profile) {
        return Err(Error::PermissionDenied);
    }

    // Rule 2: VFS view only narrows (subset of parent's view)
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

#### Nested Container Lifecycle

When a process inside container A spawns container B via `container run`,
container B is a **child container** of A. The question: what happens to B
when A dies?

**Default: cascading cleanup.**

When container A's entrypoint exits, procmgr destroys container A and all
its processes (section 4.1). As part of destruction, procmgr also destroys
all child containers that A spawned. This cascades recursively:

```
Container A (entrypoint exits)
├── process 1 (killed)
├── process 2 (killed)
├── Container B (child — destroyed)
│   ├── process 3 (killed)
│   └── Container C (grandchild — destroyed)
│       └── process 4 (killed)
└── Container D (child — destroyed)
    └── process 5 (killed)
```

Cascading cleanup is the right default because:

1. **View validity.** Container B's view is derived from A's view. If A's
   view is torn down, B's view references paths that may no longer be valid
   (e.g., A's private storage mounts passed through to B). Orphaning B
   would leave it with dangling view entries.

2. **Resource accounting.** A's resource quotas (future) cover its children.
   If A dies but B survives, B's resources are unaccounted — no parent to
   charge them to.

3. **User expectation.** If Alice's session container dies, all containers
   she launched should die too. Orphaned editors and games with no session
   would be confusing.

4. **Simplicity.** No orphan tracking, no re-parenting, no zombie containers.
   The container tree is a strict hierarchy that cleans up top-down.

**Implementation in procmgr:**

Procmgr already tracks `container_id` per process. For nested containers,
it additionally tracks `parent_container_id` per container:

```rust
struct ContainerInfo {
    container_id: u64,
    parent_container_id: u64,    // 0 for Tier 2 (autostart) containers
    image_name: String,
    entrypoint_pid: u32,
    // ...
}
```

When container A is destroyed:
1. Kill all processes with `container_id == A`.
2. Find all containers with `parent_container_id == A`.
3. For each child container, recursively destroy it (step 1-3).
4. Clean up A's private storage.

**Exception: detached containers.**

Some containers need to outlive their launcher — background services,
daemons, long-running compute jobs. The manifest can declare this:

```toml
[lifecycle]
detach = true
```

When `detach = true`, procmgr sets `parent_container_id = 0` at spawn
time. The container becomes top-level, identical to a Tier 2 autostart
container. No re-parenting logic, no ancestor tree walking — the
container is simply not part of any parent's cleanup scope.

A detached container dies only when:
- It is explicitly stopped (`container stop <name>`).
- Its own entrypoint exits.
- The system shuts down.

Detach requires CAP_SPAWN in the launcher (which all USER processes
have). If a stronger gate is needed later, a dedicated CAP_DETACH bit
can be added using the reserved profile bits (8-15).

**Container stop semantics:**

`container stop <name>` sends a destroy request to procmgr. Procmgr
destroys the named container with the same cascading logic — all child
containers are destroyed recursively. There is no "stop only this
container but keep its children" operation; the tree is always cleaned
up as a unit.

### 4.6 VT Containers

A Virtual Terminal (VT) is a single container that colocates the tty process
and the shell process. The container profile is USER (`0x0F`).

```dockerfile
# containers/vt/Cluufile
FROM base
PROFILE ipc spawn registry vfs
BUILD "cargo build --release -p tty" target/x86_64-cluu-user/release/tty /bin/tty
BUILD "cargo build --release -p shell" target/x86_64-cluu-user/release/shell /bin/shell
ENTRYPOINT /bin/tty
PARAM tty_instance
PRIORITY 205
ENDPOINT grantable
```

The ENTRYPOINT is tty. tty spawns shell internally (via procmgr, same as
today). Both processes share the container's VFS view, private storage, and
container_id. Child processes spawned by the shell also inherit the same
container_id.

Container lifecycle = ENTRYPOINT lifecycle: the VT container lives as long
as tty lives. If shell exits, tty can respawn it. If tty exits, the
container is destroyed and all children are cleaned up.

A standalone shell container also exists for non-VT use cases:

```dockerfile
# containers/shell/Cluufile
FROM base
PROFILE ipc spawn registry vfs
BUILD "cargo build --release -p shell" target/x86_64-cluu-user/release/shell /bin/shell
ENTRYPOINT /bin/shell
```

When overlay/layering is introduced, the standalone shell becomes
lightweight via shared base layers with the VT container.

### 4.7 Two-Tier Boot Model

Services are divided into two tiers based on the bootstrap ordering
constraint: container-run requires procmgr + VFS + ext2, but ext2 requires
virtio-blk, which must be spawned first.

**Tier 1: Primordial services (init-spawned from initrd)**

These provide the infrastructure that containers depend on. They cannot go
through the container-run flow because the required infrastructure doesn't
exist yet when they start.

```
init → registry → timeserver → procmgr → vfs → virtio-blk
```

Init spawns these 5 directly using the root token, same as today. They
still have CapProfiles, VFS views, and private storage — they ARE
containers in every functional sense. They just aren't spawned from a
Cluufile.

**Tier 2: System service containers (procmgr-spawned from ext2)**

Once the primordials are running and ext2 is mounted, all subsequent
services are real image containers with Cluufiles:

```
procmgr reads /etc/autostart.toml
procmgr → container run kbd
procmgr → container run console (instance=0)
procmgr → container run vtmgr
procmgr → container run vt (instance=0)
```

Init sends `BOOT_PHASE2_LABEL` to procmgr after all primordials are up.
Procmgr reads `/etc/autostart.toml` from ext2 and starts Tier 2 services.

```toml
# /etc/autostart.toml
[[service]]
name = "kbd"

[[service]]
name = "console"
params = { console_instance = 0 }

[[service]]
name = "vtmgr"

[[service]]
name = "vt"
params = { tty_instance = 0 }
```

On-demand VT creation (Ctrl+Alt+Fn) goes through the same path:
vtmgr sends `container run vt` with `params = {tty_instance = N}`.

### 4.8 Three-Tier Wiring Model

Process wiring — how a newly spawned process gets its tokens, endpoints,
params, and service connections — happens at three distinct tiers depending
on how the process was created.

#### Tier 1: Manifest Wiring (Container Entrypoint)

Applies to: the entrypoint binary of an image container (the process
created by `container run`).

Source of truth: `manifest.toml` (generated from Cluufile).

The manifest declares everything the entrypoint needs:

| Resource             | Manifest directive      | Example                        |
|----------------------|------------------------|--------------------------------|
| Capability profile   | `[profile] capabilities` | `["ipc", "spawn", "registry", "vfs"]` |
| Grantable endpoint   | `[tokens] endpoint_mode` | `"grantable"` → TOKEN_EXTRA_0  |
| Device tokens        | `[hardware] devices`    | `["irq"]` → TOKEN_EXTRA_1      |
| Boot parameters      | `[params] slots`        | `["tty_instance"]` → PARAM slot |
| Priority             | `[scheduling] priority` | `205`                          |
| VFS view             | Derived from profile + image_dirs | `/bin` → `/var/images/vt/bin` |

Procmgr reads the manifest, derives tokens per the profile, creates
endpoints per `endpoint_mode`, maps device tokens, sets params from the
caller's overrides, and registers the VFS view. The entrypoint starts
with everything it needs — no runtime discovery required for its core
function.

#### Tier 2: Self-Wiring (Intra-Container Secondary Binaries)

Applies to: binaries spawned within a container via `PROCMGR_SPAWN_LABEL`
(e.g., tty spawns shell, shell spawns ls).

Source of truth: inherited context + runtime service discovery.

The child inherits its parent's container context:

| Resource             | Source                           |
|----------------------|----------------------------------|
| Capability profile   | Inherited from parent (or narrowed) |
| TOKEN_IPC            | Derived from profile (CREATE + GRANT if CAP_IPC) |
| TOKEN_EXTRA slots    | Empty (0) — no manifest-level endpoints |
| Boot parameters      | Default (zeros) — no param overrides |
| VFS view             | Inherited from parent            |
| Container_id         | Inherited from parent            |
| Stdio tokens         | Wired to parent's tty (or via FDAC) |

The key capability is TOKEN_IPC with CREATE + GRANT rights. This lets
the child **self-wire** at runtime:

1. Create its own endpoints via `endpoint_create(TOKEN_IPC)`.
2. Register with the IPC registry (if CAP_REGISTRY is set).
3. Subscribe to other services and receive grants.

Example: shell starts with no special endpoints. It creates an endpoint,
registers as "shell" with the registry, subscribes to "tty:N", and
receives tty's endpoint via a registry grant. All wiring happens at
runtime through standard IPC — no procmgr involvement after spawn.

#### Tier 3: FDAC (Explicit Parent-to-Child Wiring)

Applies to: any spawn where the parent needs to pre-wire specific
connections to the child.

Source of truth: the parent's spawn request payload (FDAC block).

FDAC (File Descriptor Action Context) lets the parent set up the child's
stdio or extra token slots at spawn time:

```
Parent creates pipe: pipe() → (read_end, write_end)
Parent spawns child with FDAC: spawn("/bin/helper", fdac={
    fd=0 (stdin) → read_end,
    fd=1 (stdout) → write_end,
})
```

This is used for:
- Shell pipe chains: `cat file | grep pattern` — shell creates pipes
  and wires stdin/stdout of each stage via FDAC.
- Explicit parent→child channels where registry discovery is too slow
  or inappropriate (e.g., sandboxed plugins that can't use the registry).

#### Summary: When Each Tier Applies

This model applies to **any** container, not just VT containers. A
container image can bundle an arbitrary number of binaries. The entrypoint
is the process that defines the container lifecycle; all other binaries
are spawned within the container at runtime.

```
container run myapp                       → Tier 1 (manifest)
  └─ myapp (entrypoint)                     endpoints, devices, params from manifest
       │
       ├─ spawn("/bin/worker")            → Tier 2 (self-wiring)
       │    │                                TOKEN_IPC → create endpoints
       │    │                                registry → discover services
       │    │
       │    └─ spawn("/bin/helper")       → Tier 2 (self-wiring)
       │                                     inherits container, runs, exits
       │
       ├─ spawn("/bin/plugin", SANDBOXED) → Tier 2 (narrowed profile)
       │                                     no registry, no VFS, no spawn
       │                                     only stdio pipes from FDAC
       │
       └─ cat file | grep pattern         → Tier 3 (FDAC)
            cat stdout → pipe → grep stdin
```

Examples of multi-binary containers:

| Container | Entrypoint | Bundled binaries | Wiring pattern |
|-----------|-----------|------------------|----------------|
| vt        | /bin/tty  | /bin/shell       | tty gets grantable EP (manifest), shell self-wires via registry |
| webserver | /bin/httpd | /bin/cgi-handler | httpd gets network EP (manifest), cgi-handler gets FDAC pipes |
| editor    | /bin/edit  | /bin/spellcheck  | edit gets VFS view (manifest), spellcheck inherits + narrows |
| mp        | /bin/micropython | (none)    | Single-binary container, only tier 1 wiring |

No hardcoded paths or special procmgr handlers are needed. The entrypoint
gets manifest wiring, secondary binaries self-wire via the registry, and
explicit connections use FDAC.

### 4.9 Users and Sessions

#### Motivation

The container model (sections 4.1–4.8) defines profiles, views, lifecycle,
and nesting — but it's missing the answer to "whose view?" When Alice logs
in, something must determine that she gets `/home/alice` (rw) and not
`/home/bob`. That something is the user/session model.

Without it, all USER processes get `/home/root` (section 3.4's current
default). With it, user identity drives view construction at login time.

#### What Is a User?

A user is a named entry in `/etc/users.toml` that maps to a set of
session defaults. There are no UIDs — CLUU does not have Unix-style file
permission checks. The VFS view IS the access control. If Alice's view
doesn't include `/home/bob`, she cannot access it, regardless of what
the ext2 inode metadata says.

```toml
# /etc/users.toml

[user.alice]
home = "/home/alice"
shell = "/bin/shell"
profile = "user"           # default session profile at login
escalate = "admin"         # maximum profile via sudo (optional)

[user.bob]
home = "/home/bob"
shell = "/bin/shell"
profile = "user"
# no escalate = sudo rejected unconditionally

[user.root]
home = "/root"
shell = "/bin/shell"
profile = "admin"          # login session starts as ADMIN
escalate = "supervisor"    # can sudo to SUPERVISOR if needed
```

Fields:
- **home**: absolute path to the user's home directory.
- **shell**: path to the shell binary (resolved through session view).
- **profile**: the CapProfile for the login session container.
- **escalate** (optional): the ceiling profile for `sudo`. If absent,
  escalation is rejected outright.

Procmgr reads `/etc/users.toml` at boot (or on demand). Only procmgr
(SUPERVISOR) can read this file — it's not in any USER or ADMIN view.

#### What Is a Session?

A session is a **top-level container** that binds user identity to the
container model.

```
Session = Container + UserIdentity + VT Attachment
```

It has a container_id, a profile (from user record), a VFS view (built
from user record + profile defaults), and private storage. Its entrypoint
is the user's shell. All containers the user launches are children of the
session container — they inherit the session's view and cascade on logout.

The session container is spawned by procmgr after successful authentication.
It has `parent_container_id = 0` — it is top-level, not a child of the VT
container. The VT is an I/O adapter attached to the session via IPC wiring,
not a lifecycle parent.

#### VT–Session Attachment

Sessions and VTs are **siblings, not parent-child**. They are connected
by IPC wiring, not by container lifecycle.

```
procmgr
  │
  ├─ vtmgr (Tier 2 autostart)
  │   ├─ VT:0 (tty:0, I/O adapter)  ──IPC──┐
  │   ├─ VT:1 (tty:1, I/O adapter)  ──IPC──┤
  │   └─ VT:2 (tty:2, I/O adapter)  ──IPC──┤
  │                                         │
  └─ Sessions (top-level, parent=0)         │
      ├─ Session:alice (USER) ◄─────────────┘ (attached to VT:0)
      │   ├─ shell
      │   ├─ editor (nested container)
      │   └─ ...
      ├─ Session:bob (USER) ◄─── (attached to VT:1)
      └─ (VT:2 has no session — showing login prompt)
```

The attachment is an IPC endpoint pair: tty holds a send token to the
session's shell stdin, and the session's shell holds a send token to
tty's output. These are established when procmgr spawns the session and
tells tty "wire to this endpoint."

**Attachment lifecycle:**

| Event | VT | Session | Attachment |
|-------|-----|---------|------------|
| Login succeeds | Running | Created (parent=0) | Established |
| User works | Running | Running | Active |
| tty crashes | Dies | **Survives** | Broken → vtmgr respawns VT, procmgr reattaches |
| User logs out | Running | Dies (cascades children) | Broken → tty returns to login |
| VT switch away | Deactivated | Running | Paused (session in background) |
| VT switch back | Activated | Running | Resumed |
| System shutdown | Killed | Killed | Torn down |

This is analogous to tmux/screen: the session persists independently of
the terminal. The terminal is a replaceable I/O front-end.

**Reattachment after VT crash:**

1. tty:1 crashes.
2. Procmgr detects VT container death (entrypoint exit notification).
3. Procmgr notifies vtmgr: "VT:1 died."
4. vtmgr spawns a new VT:1 container (new tty:1 process).
5. Procmgr reattaches the new tty:1 to Alice's session (same IPC wiring).
6. Alice's shell and programs continue uninterrupted. Output resumes on screen.

The session didn't die. The user lost a few seconds of display, not their work.

#### Login Flow

```
1. vtmgr → container run vt (instance=1)

2. VT container starts (profile=0x05: IPC+REGISTRY)
     tty:1 (entrypoint)
       - registers as tty:1
       - subscribes to console:0/vt:1
       - displays login prompt

3. tty:1 reads username + password from keyboard
     tty:1 → PROCMGR_SESSION_LOGIN_LABEL(username, password, vt_instance=1)

4. procmgr receives login request:
     - looks up username in /etc/users.toml
     - verifies password (hash comparison)
     - if invalid → replies with error, tty shows "login failed"
     - if valid:

5. procmgr builds session:
     - profile = user_record.profile (e.g., USER 0x0F)
     - view = profile default view (section 3.4) with <user> resolved:
         /bin (ro), /lib (ro), /tmp (rw,scoped), /home/alice (rw)
     - container_id = fresh
     - parent_container_id = 0 (top-level session)
     - entrypoint = user_record.shell

6. procmgr spawns session container:
     - derives tokens per profile
     - registers VFS view with user-specific home
     - wires shell stdin/stdout to tty:1 (IPC attachment)
     - records session→VT attachment in session table
     - replies to tty:1 with session info

7. tty:1 switches to terminal mode (pure keyboard/display bridge)
     shell runs inside session container
     user can spawn programs, containers, etc.

8. shell exits → session container destroyed (cascading)
     all child containers killed
     procmgr detects session death, notifies tty:1
     tty:1 returns to login prompt (step 2)
```

Key properties:
- **tty never holds user credentials.** It forwards them to procmgr via
  IPC. The VT container has no VFS access — it can't read `/etc/users.toml`.
- **Views only narrow.** Procmgr (SUPERVISOR) narrows to the user's view.
  The VT container's view never widens.
- **VT survives logout.** The VT container (tty) is a separate container
  from the session. When the session dies, tty returns to login.
- **Session survives VT crash.** Session is top-level (parent=0). VT death
  breaks the I/O attachment but doesn't cascade to the session.

#### VT Container Profile Change

With sessions as separate containers, the VT container no longer needs
USER capabilities. tty needs IPC (talk to console, procmgr) and REGISTRY
(register as tty:N, subscribe to console). It does not need SPAWN (procmgr
spawns the session) or VFS (tty doesn't access files).

| Before (pre-sessions) | After (with sessions) |
|---|---|
| VT container: USER (0x0F) = IPC+SPAWN+REGISTRY+VFS | VT container: 0x05 = IPC+REGISTRY |
| tty + shell in same container | tty in VT container, shell in session container |
| VT container has VFS access | VT container has no VFS access |

This is a security improvement — the terminal driver runs with minimal
capabilities.

#### Escalation: sudo

`sudo` does not widen the current session. It creates a new container
with an elevated profile, derived from procmgr's SUPERVISOR authority.

```
Alice's session (USER = 0x0F)
  shell
    $ sudo reboot

    shell prompts for password
    shell → PROCMGR_ESCALATE_LABEL(password, "/bin/reboot")

    procmgr:
      1. verify password against alice's record
      2. check alice.escalate = "admin" → authorized
      3. build elevated container:
           profile = ADMIN (0x8F)
           view = ADMIN default (section 3.4) with alice's home:
             /bin(ro), /lib(ro), /home/alice(rw), /usr(ro),
             /etc(ro), /var/log(ro), /var/services(rw), /tmp(rw,scoped)
           command = "/bin/reboot"
           parent = Alice's session (cascading)
      4. spawn container, runs command, exits
```

The elevated container's view is derived from the escalation profile's
default view template (section 3.4), NOT from the current session's view.
This means `sudo cat /etc/shadow` works because ADMIN's default view
includes `/etc` — even though Alice's USER view doesn't.

Alice's session remains USER throughout. The elevated container is a child
that cascades on logout.

**sudo -s (elevated shell):** Same mechanism, command = shell binary.
Creates an interactive elevated session. `exit` returns to the USER shell.

#### Identity Switch: su

`su bob` creates a nested session with Bob's identity, inside the current
session's container tree.

```
Alice's session (USER, view includes /home/alice)
  shell
    $ su bob

    shell prompts for Bob's password
    shell → PROCMGR_SESSION_LOGIN_LABEL("bob", password)

    procmgr:
      1. verify Bob's password
      2. build session from Bob's user record:
           profile = bob.profile (USER)
           view = USER default with bob's home:
             /bin(ro), /lib(ro), /home/bob(rw), /usr(ro), /tmp(rw,scoped)
           parent = Alice's session (cascading)
      3. spawn Bob's session container
```

Bob's view is derived from procmgr's SUPERVISOR authority and Bob's user
record — NOT narrowed from Alice's view. This is why `su` works even
though Alice's view doesn't include `/home/bob`. Procmgr is the one
creating the container, and procmgr has SUPERVISOR.

If Alice logs out, Bob's nested session cascades (destroyed). This is
correct — Alice's VT session is the lifecycle root. Bob was operating
within Alice's terminal.

#### Security Properties

| Operation | Credential check | Profile derivation | View derivation |
|-----------|-----------------|-------------------|-----------------|
| login     | password → procmgr | user_record.profile | profile default + user home |
| su        | target's password → procmgr | target's user_record.profile | target's profile default + target home |
| sudo      | own password → procmgr | user_record.escalate | escalation profile default + own home |

All three operations:
- Require password verification by procmgr.
- Create a NEW container (never widen existing).
- Derive views from procmgr's SUPERVISOR authority (not caller's view).
- Cascade on parent session death.

What users CANNOT do:
- Escalate beyond their `escalate` ceiling (procmgr rejects).
- `su` without the target's password (procmgr rejects).
- Access paths outside their session view (VFS rejects).
- Forge a session (only procmgr can create session containers).

### 4.10 Restart Policies

#### The Problem

Services can crash. When `console` crashes, the display goes black. When
`vfs` crashes, no process can read or write files. When `virtio-blk`
crashes, the disk is gone. These are system-critical services that act
as drivers — the system cannot function without them.

The current model ("entrypoint exits → container destroyed") is correct
for user containers (editor crashes, clean up, done). But for system
services, destruction without restart means a single crash takes down
the functionality permanently.

#### Restart Tiers

Not all containers need the same restart behavior. The policy is
determined by container type and manifest declaration:

| Tier | Containers | Restart policy | Rationale |
|------|-----------|---------------|-----------|
| **Primordial** | procmgr, vfs, registry, virtio-blk, ext2 | **Kernel panic** | These ARE the infrastructure. If they die, the system is unrecoverable. Restarting them would leave dangling state in every client. |
| **System service** | console, kbd, vtmgr, timeserver | **Auto-restart** | Stateless or recoverable. Clients reconnect via registry. |
| **VT** | tty:N containers | **Auto-restart** (vtmgr) | Session survives VT crash. vtmgr respawns VT, reattaches. |
| **Session** | User login sessions | **No restart** | Session death = logout. Intentional. |
| **User** | Containers spawned from session | **No restart** | Parent (session) decides whether to respawn. |

#### Primordial Failure

If a Tier 1 primordial dies, the system is in an inconsistent state:
- VFS death: every process's file operations fail. Views, mounts, open
  file handles — all gone.
- Procmgr death: no new processes can be spawned. No container lifecycle
  management.
- Registry death: service discovery stops. New subscriptions fail.

Restarting these would require every client to re-establish connections,
re-register, re-open files — effectively a full reboot but worse because
the kernel state is stale. The correct response is a kernel panic with
a diagnostic message.

Init can detect primordial death (it spawned them, it holds their exit
notification tokens). If any primordial exits, init triggers a kernel
panic.

#### System Service Auto-Restart

Tier 2 autostart services (console, kbd, vtmgr, timeserver) are
stateless or self-recovering. When one crashes:

1. Procmgr detects container death (entrypoint exit notification).
2. Procmgr checks the container's restart policy.
3. If `restart = "always"` or `restart = "on-failure"`:
   - Wait for backoff delay (exponential: 100ms, 200ms, 400ms, ...,
     max 10s).
   - Re-run the container from the same image.
   - Reset backoff on successful startup (running for >30s).
4. If crash count exceeds `max_restarts` (default 5 within 60s):
   - Log error, stop restarting.
   - Notify admin session (if any) via IPC.

Registry handles reconnection: when console restarts, it re-registers
its outputs. Clients (tty, vtmgr) that subscribed before get new GRANT
events and update their endpoints. The existing registry subscription
protocol already supports this — a grant for an already-subscribed output
replaces the old endpoint token.

#### Manifest Declaration

```toml
# containers/console/Cluufile (system service)
FROM base
PROFILE ipc registry device
ENTRYPOINT /bin/console
RESTART always
MAX_RESTARTS 5

# containers/editor/Cluufile (user container)
FROM base
PROFILE ipc spawn registry vfs
ENTRYPOINT /bin/editor
# no RESTART directive → default is "never" for user containers
```

The restart policy is stored in `manifest.toml`:

```toml
[lifecycle]
restart = "always"     # "always" | "on-failure" | "never"
max_restarts = 5       # within restart_window seconds
restart_window = 60    # seconds
```

| restart value | When it restarts |
|---------------|-----------------|
| `never`       | Container dies, stays dead. Default for user containers. |
| `on-failure`  | Restarts if entrypoint exits with non-zero status. |
| `always`      | Restarts regardless of exit status. For system services. |

#### VT Restart (Special Case)

VT containers have `restart = "always"` but the restart is managed by
vtmgr, not directly by procmgr. vtmgr tracks VT state (created, spawned,
active) and handles VT-specific concerns like reattaching sessions.

When a VT container dies:
1. Procmgr notifies vtmgr (exit notification on the container).
2. vtmgr clears the `vt_spawned` bit for that VT index.
3. If the VT is currently active, vtmgr immediately respawns it.
4. If the VT is inactive (user is on a different VT), vtmgr respawns
   on next switch to that VT (lazy respawn).
5. Procmgr reattaches the surviving session to the new VT (section 4.9).

### 4.11 Graceful Shutdown

#### Shutdown Sequence

The system shuts down in reverse boot order. Procmgr orchestrates
the entire sequence after receiving a shutdown request (from an ADMIN
session's `reboot` command or a hardware event).

```
1. Notify sessions
     procmgr sends SHUTDOWN_NOTIFY to all active sessions
     sessions have a grace period (default 5s) to save state
     after grace period, procmgr forcibly kills remaining sessions

2. Kill user containers
     all session containers destroyed (cascading kills children)
     user's work is done

3. Stop Tier 2 services (reverse autostart order)
     container stop vtmgr
     container stop console
     container stop kbd
     container stop timeserver
     each service gets a SHUTDOWN_NOTIFY + grace period (1s)
     then forcibly killed

4. Unmount filesystems
     procmgr tells VFS to flush and unmount ext2
     VFS tells ext2 to sync
     ext2 tells virtio-blk to flush

5. Stop Tier 1 primordials (reverse init order)
     stop ext2
     stop virtio-blk
     stop vfs
     stop registry
     stop procmgr (self-stop)

6. init receives "all stopped" or timeout
     init calls kernel shutdown/reboot syscall
```

#### Shutdown Signals

Processes learn about shutdown through two mechanisms:

**SHUTDOWN_NOTIFY (IPC message):** Procmgr sends this to each container's
entrypoint endpoint. The message carries a grace period value. Well-behaved
processes save state and exit cleanly within the grace period.

**Forcible kill (kernel thread_destroy):** After the grace period expires,
procmgr destroys the process's thread and address space. This is a hard
kill — no signal handler, no cleanup, just gone.

There are no Unix-style signals (SIGTERM, SIGKILL) in CLUU. The IPC
message IS the signal. If a process doesn't handle it (e.g., no listener
on that endpoint), the grace period elapses and it gets killed.

#### Grace Periods

| Container type | Grace period | Rationale |
|---------------|-------------|-----------|
| Session | 5s | User may have unsaved work. Shells can prompt. |
| User container | 2s | Inherited from session shutdown cascade. |
| Tier 2 service | 1s | Stateless services; flush buffers and exit. |
| Tier 1 primordial | 0s (immediate) | Shutdown is past the point of no return. |

#### Shutdown Triggers

| Trigger | Who sends it | IPC label |
|---------|-------------|-----------|
| `reboot` command | ADMIN session shell | PROCMGR_SHUTDOWN_LABEL |
| `poweroff` command | ADMIN session shell | PROCMGR_SHUTDOWN_LABEL |
| Hardware power button | kbd (ACPI event) | PROCMGR_SHUTDOWN_LABEL |
| Ctrl+Alt+Del | kbd | PROCMGR_SHUTDOWN_LABEL |

All shutdown triggers require the sender to have CAP_ADMIN in their
profile. A USER session cannot trigger shutdown — only ADMIN or
SUPERVISOR can.

### 4.12 Container Addressing

#### The Problem

Alice runs `container run editor`. She runs it again. Now there are two
editor containers. `container stop editor` — which one?

With multi-user sessions, the problem multiplies: Alice has an editor,
Bob has an editor. `container list` shows both. Alice must be able to
address her containers without seeing Bob's.

#### Addressing Scheme

Every running container has three identifiers:

```
container_id = 42          # system-wide unique (monotonic, assigned by procmgr)
image_name   = "editor"    # from manifest (same for all instances of this image)
instance     = "editor.2"  # scoped name (image_name + instance counter)
```

The **instance name** is the user-facing identifier. It's constructed as:

```
first instance:    "editor"       (no suffix)
second instance:   "editor.2"
third instance:    "editor.3"
```

Instance counters are **per-session** for user containers and
**system-wide** for Tier 2 services. Alice's editors are numbered
independently of Bob's editors.

#### Scoping Rules

Users only see containers they own (in their session tree):

```
Alice's session (container_id=10)
  ├─ editor     (container_id=42, instance="editor")
  └─ editor.2   (container_id=57, instance="editor.2")

Bob's session (container_id=11)
  └─ editor     (container_id=63, instance="editor")
```

Alice types `container list`:
```
INSTANCE    IMAGE     STATUS    CONTAINER_ID
editor      editor    running   42
editor.2    editor    running   57
```

Alice types `container stop editor` → stops container_id=42.
Alice types `container stop editor.2` → stops container_id=57.

Bob's containers are invisible to Alice. Bob has his own `editor`
instance that doesn't collide with Alice's.

#### System Service Addressing

Tier 2 autostart containers and VT containers are system-scoped.
They're visible to ADMIN sessions but not to USER sessions:

```
System containers:
  console     (container_id=3)
  kbd         (container_id=4)
  vtmgr       (container_id=5)
  vt          (container_id=6, PARAM tty_instance=0)
  vt.2        (container_id=20, PARAM tty_instance=1)
```

An admin can `container stop vt.2` to kill a specific VT. Regular
users cannot.

#### Addressing by container_id

For unambiguous addressing, the numeric container_id always works:

```
container stop @42     # "@" prefix = container_id
```

This is useful in scripts and for admin operations where instance
names might be ambiguous across sessions.

#### Implementation

Procmgr maintains a container table:

```rust
struct ContainerEntry {
    container_id: u64,
    parent_container_id: u64,
    session_id: u64,            // 0 for system containers
    image_name: String,
    instance_name: String,      // "editor", "editor.2", etc.
    instance_counter: u32,      // per-session counter per image_name
    entrypoint_pid: u32,
    restart_policy: RestartPolicy,
    // ...
}
```

`container list` queries procmgr with the caller's session_id. Procmgr
filters to containers where `session_id` matches (or `session_id == 0`
for system containers if the caller has CAP_ADMIN).

`container stop <name>` resolves the instance name within the caller's
session scope, then destroys the matching container.

### 4.13 VT Screen State Management

#### The Problem

VT switching, session reattach, and multi-user operation all depend on
one thing: **the screen looks correct after any transition**. Currently
it doesn't — cursor lands in the wrong position, screen looks garbled
on VT switch. With the session model (4.9), the stakes are higher:
users working on multiple VTs simultaneously, sessions surviving VT
crashes, future session detach/reattach.

#### Screen State Inventory

The complete screen state for one VT:

```
Per-VT screen state:
  cells[rows × cols]          character grid (CP437 codepoints)
  fg_cells[rows × cols]       foreground color per cell (BGRA32)
  bg_cells[rows × cols]       background color per cell (BGRA32)
  cursor_x, cursor_y          cursor position
  cursor_visible              cursor visibility flag
  current_fg, current_bg      current drawing colors (for next write)
  esc_state                   ANSI parser state machine position
  esc_params[4]               accumulated CSI parameters
  esc_param_count             parameter count
  scroll_top, scroll_bottom   scroll region (future: DECSTBM)
  scrollback[history_lines]   scrollback buffer (future)
```

All of this must be preserved across VT switches and restored correctly
on reactivation. A single missing or corrupted field causes garbled
output.

#### Current Architecture (What's Broken)

Console owns all VT screen state in a context-switch model:

```
Console process
  ├─ active registers (cursor, cells, colors, parser state)
  ├─ vt_buffers[0] = None (VT 0 state lives in active registers)
  ├─ vt_buffers[1] = Some(VtBuffer { ... })
  ├─ vt_buffers[2] = Some(VtBuffer { ... })
  └─ vt_buffers[3] = Some(VtBuffer { ... })
```

**Bug 1: Context-switch trick for inactive writes.**
`write_to_vt()` temporarily swaps in a target VT's state to process
writes, then swaps back. If any IPC arrives during this window (another
write, a VT switch command), the active registers contain the WRONG VT's
state. The result: cursor position from VT 1 gets saved to VT 0's buffer,
or characters land in the wrong VT's grid.

Fix: **eliminate the context-switch trick**. Each VT buffer must be a
self-contained object that can process writes independently, without
touching the active registers.

**Bug 2: Split deactivate/activate.**
vtmgr sends `CONSOLE_DEACTIVATE_LABEL(old)` then `CONSOLE_ACTIVATE_LABEL(new)`
as two separate IPC messages. Between them, console is in an undefined state:
`active = false` but no new VT is loaded. Writes arriving in this window
may go to the wrong buffer or be dropped.

Fix: **single atomic switch message**. Replace the two-message protocol
with `CONSOLE_SWITCH_VT_LABEL(old_vt, new_vt)` — one message, one
handler, atomic save-load-repaint.

**Bug 3: Output loss during inactive period.**
tty's `console_output_queue` is 16KB. If more output arrives while the VT
is inactive (e.g., a compile running on a background VT), the oldest bytes
are silently dropped. When the VT is reactivated, the screen is missing
content — characters, ANSI sequences, or partial escape sequences are gone.

Fix: **backpressure, not dropping**. When the output queue is full, tty
should stop accepting writes from the shell (block the write call) rather
than dropping data. Alternatively, the console's per-VT buffer should
absorb writes without any tty-side queue limit — the bottleneck should be
at the console, not the tty.

#### Target Architecture

**Principle: each VT buffer is an independent virtual screen that processes
writes directly, without context-switching.**

```
Console process
  ├─ vt_screens[0]: VtScreen { cells, cursor, parser, ... }
  ├─ vt_screens[1]: VtScreen { cells, cursor, parser, ... }
  ├─ vt_screens[2]: VtScreen { cells, cursor, parser, ... }
  ├─ vt_screens[3]: VtScreen { cells, cursor, parser, ... }
  ├─ active_vt: usize
  └─ framebuffer backend
```

`VtScreen` is a self-contained virtual terminal:
- Has its own `write_bytes()` method that updates cells, cursor, colors.
- Has its own ANSI parser state.
- Can process writes regardless of which VT is active.
- Does NOT touch the framebuffer — it only updates the cell grid.

The renderer is a separate concern:
- On each frame (or after batch write), the renderer reads
  `vt_screens[active_vt]` and renders dirty cells to the framebuffer.
- Inactive VTs' writes update their VtScreen but trigger no rendering.
- VT switch: change `active_vt`, do a full `repaint_all()` from the
  new VtScreen's cell grid.

This eliminates the context-switch trick entirely. Each VtScreen is
always self-consistent. Writes to inactive VTs are just method calls
on the right VtScreen object — no register swapping.

#### Atomic VT Switch Protocol

Replace the two-message deactivate/activate with a single message:

```
CONSOLE_SWITCH_VT_LABEL (new label)
  words[0] = old_vt_index
  words[1] = new_vt_index
```

Console handler:
1. Mark `vt_screens[old_vt].active = false`.
2. Set `active_vt = new_vt`.
3. Mark `vt_screens[new_vt].active = true`.
4. `repaint_all()` from `vt_screens[new_vt]`.
5. `backend.flush()`.

No intermediate state. No window where `active = false` and no VT is
loaded. The switch is a single atomic operation.

vtmgr changes:
```rust
// Before (two messages, race window):
send(console, CONSOLE_DEACTIVATE_LABEL, [old_vt, ...]);
send(console, CONSOLE_ACTIVATE_LABEL, [new_vt, ...]);

// After (one message, atomic):
send(console, CONSOLE_SWITCH_VT_LABEL, [old_vt, new_vt, ...]);
```

#### Output Flow Control for Inactive VTs

When a VT is inactive, output from tty still needs to reach the console's
VtScreen buffer. The question is: what happens when output arrives faster
than console can process it?

**Current (broken):** tty queues output, drops oldest bytes on overflow.
Screen state becomes inconsistent.

**Target: credit-based flow control, all the way through.**

```
Session shell → tty → console
                 │        │
                 │        └─ VtScreen buffer (unlimited within reason)
                 │
                 └─ credit system (existing, but needs adjustment)
```

For inactive VTs:
- Console processes writes into the VtScreen immediately (no rendering,
  so this is fast — just cell grid updates).
- Console does NOT send credit refills for inactive VTs at a lower rate.
  The bottleneck is cell-grid write speed, which is fast.
- tty's 16KB output queue is a transport buffer, not a screen buffer.
  It should never be the place where screen content lives or is dropped.

For active VTs:
- Same as current: credit-based flow control, rendering on dirty flush.

If a background VT generates extreme output (e.g., `cat /dev/urandom`),
console can impose a per-VT memory limit on cell processing rate. But
the cell grid is fixed-size (rows × cols), so unbounded input just
overwrites the grid — it doesn't grow memory. The only risk is CPU
time spent processing ANSI sequences for invisible VTs.

#### Session Reattach: Screen Recovery

When a VT crashes and respawns (section 4.9, VT–Session Attachment),
the new tty reattaches to the session. But the session's programs have
been running — their output went to the console's VtScreen buffer.
The screen content is there. The issue is wiring the new tty to the
right VtScreen.

**If console is alive (normal VT crash):**
1. New tty:N starts, registers with registry.
2. Console already has VtScreen[N] with all content intact.
3. New tty subscribes to console:0/vt:N — gets the write endpoint.
4. Procmgr reattaches session to new tty.
5. tty switches to terminal mode.
6. Console renders VtScreen[N] — screen shows exactly what was there.

No data loss. The session's output was going to console the entire time
(console routes by VT index, not by tty instance). The new tty is just
a new keyboard/display bridge.

**If console crashes (rare):**
1. Console restarts (section 4.10, auto-restart).
2. All VtScreens are gone (they were in console's memory).
3. Console starts with empty VtScreens.
4. Procmgr sends SCREEN_REDRAW_NOTIFY to all sessions via their tty.
5. tty forwards this as a terminal resize event (like SIGWINCH).
6. Well-behaved programs (shell, editors, TUI apps) redraw themselves.
7. Programs that don't handle redraw show a blank screen until they
   produce output.

This is the same behavior as resizing a terminal window in Linux — every
program gets SIGWINCH and redraws. It's not perfect (stateless programs
like `cat` don't redraw past output), but it's the accepted behavior
in all terminal systems.

#### Scrollback Buffer (Future)

The current implementation has no scrollback — scrolling destroys content.
A scrollback buffer preserves lines that scroll off the top:

```
VtScreen {
    viewport[rows × cols]         visible screen (current)
    scrollback[max_lines × cols]  history ring buffer
    scroll_offset: usize          0 = at bottom, >0 = scrolled up
}
```

- When a line scrolls off the top, it's appended to the scrollback ring.
- Shift+PageUp/PageDown adjusts `scroll_offset`.
- Console renders viewport or scrollback slice depending on offset.
- Memory-bounded: `max_lines` configurable per-VT (default 1000 lines).

This is not needed for correctness but significantly improves usability
for multi-VT multi-user operation. Deferred to after the core fixes.

#### Summary: Screen State Guarantees

| Scenario | Screen state | Recovery mechanism |
|----------|-------------|-------------------|
| VT switch (Ctrl+Alt+Fn) | Fully preserved in VtScreen | Atomic switch + full repaint |
| Session continues on inactive VT | Output processed into VtScreen | No data loss; repaint on switch |
| tty crash, session survives | VtScreen intact in console | New tty reattaches; repaint |
| Console crash, sessions survive | VtScreens lost | SCREEN_REDRAW_NOTIFY → programs redraw |
| Session logout | VtScreen cleared | tty shows login prompt |
| System shutdown | VtScreens discarded | N/A |

---

## 5. Spawn Protocol

All process creation goes through procmgr. There are three spawn labels:
one for system services (privileged, initrd-only), one for intra-container
binary spawns (user programs with argv/env/fd), and one for container
image spawns (manifest-driven).

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

### 6.3 VT Switch Flow (Current — Pre Phase G)

This is the CURRENT implementation. See Phase G section 7.G.3 for the
target flow using container run.

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

### Phase D: Private Storage — COMPLETE

Per-container isolated storage areas, managed by VFS.

| #  | Task                                              | Status    |
|----|---------------------------------------------------|-----------|
| D1 | Container ID generation in procmgr (monotonic)    | done      |
| D2 | VFS: create `/var/containers/<id>/` on spawn      | done      |
| D3 | VFS: auto-add private storage mounts to view      | done      |
| D4 | VFS: clean tmp/ on container exit                 | done      |
| D5 | VFS: delete all of `<id>/` on destroy             | done      |
| D6 | Procmgr: track container_id in process bookkeeping | done     |
| D7 | Build + test: container storage isolation          | done      |

### Phase E: Container Images — MOSTLY COMPLETE

Cluufile-based container packaging with extract-on-install and merge-at-build.

| #   | Task                                              | Status  | Depends |
|-----|---------------------------------------------------|---------|---------|
| E1  | Cluufile parser (standalone `tools/container-build/`) | done | —       |
| E2  | manifest.toml generator                           | done    | E1      |
| E3  | `container-build` command (merge base + COPY)     | done    | E2      |
| E4  | Userdisk integration (copy images to /var/images/) | done   | E3      |
| E5  | Minimal TOML parser (no_std) in libcluu           | done    | —       |
| E6  | Procmgr: container run handler (manifest + spawn) | done    | E4, E5  |
| E7  | VFS hardlink support (FS_LINK + VFS_LINK)         | done    | —       |
| E8  | Procmgr: seed persistent data via hardlinks       | done    | E6, E7  |
| E9  | Shell builtin: `container run <name>`             | done    | E6      |
| E10 | Shell builtin: `container list`                   | done    | E6      |
| E11 | Shell builtin: `container stop <name>`            | done    | E6      |
| E12 | Extract container-build from xtask to `tools/`    | done    | E3      |
| E13 | Integration test: build + run container            | done    | E9      |

Critical path: E1 → E2 → E3 → E4 → E6 → E9 → E13
Parallel tracks: E5 (TOML parser), E7 (hardlink support), E12 (crate extraction)

Additional completed work (post-Phase E):

| Task                                                 | Status | Commit  |
|------------------------------------------------------|--------|---------|
| Ephemeral MemFs container root filesystem            | done   | 007e364 |
| image_dirs auto-discovery in container-build         | done   | 9060b60 |
| Per-image /bin isolation via view mount overrides    | done   | 9060b60 |
| Security: FDAC endpoint validation (token_derive)    | done   | aee31ee |
| Security: Registry grant includes service name       | done   | 8b8ee2b |
| Security: Console per-VT endpoints (confused-deputy) | done   | c286d3c |
| TTY race condition fix (fire-and-forget FG handoff)  | done   | ba6d592 |
| Console async credits (non-blocking backpressure)    | done   | (E fix) |

### Phase F: Nested Containers

Container-from-container spawn with profile narrowing, view construction
(image dirs override + launcher passthrough), cascading lifecycle, and
detach support. Implements sections 4.5 and the nested lifecycle model.

| #   | Task                                                   | Status  | Depends |
|-----|--------------------------------------------------------|---------|---------|
| F1  | Procmgr: `parent_container_id` tracking in ContainerInfo | done    | —     |
| F2  | Procmgr: validate_container_run (profile ⊆ caller)    | done    | —       |
| F3  | Procmgr: container run view construction (image dirs override + passthrough) | done    | F2 |
| F4  | Procmgr: manifest `deny_inherit` and `deny` list in view builder | done    | F3 |
| F5  | Procmgr: cascading container cleanup (recursive child destroy) | done    | F1 |
| F6  | Procmgr: detach support (parent_container_id=0 when manifest.detach) | done    | F1 |
| F7  | container-build: emit `deny_inherit`/`deny`/`detach` to manifest.toml | done    | — |
| F8  | Test: USER spawns nested container with narrowed profile | pending | F2, F3 |
| F9  | Test: escalation attempt (child profile > parent) rejected | pending | F2 |
| F10 | Test: view passthrough — child sees launcher's /home  | pending | F3     |
| F11 | Test: deny_inherit — child only sees image dirs + /tmp | pending | F4     |
| F12 | Test: cascading cleanup — parent death kills children  | pending | F5     |
| F13 | Test: detached container survives parent death          | pending | F6     |

Critical path: F1 → F5 (lifecycle), F2 → F3 → F4 (view construction)
Parallel tracks: F6 (detach), F7 (container-build), tests depend on their feature tasks

### Phase G: VT Container Migration

Migrate vtmgr from spawning bare `sys/tty` services via
PROCMGR_SPAWN_SERVICE_LABEL to spawning VT containers via
PROCMGR_CONTAINER_RUN_LABEL. This is the transition from section 6.3's
current architecture to section 4.6's target architecture.

#### G.1 Design Issues

**Issue 1: Container run parameter passing**

The container run handler (`handle_container_run`) reads manifest param
slot names (e.g., `slots = ["tty_instance"]`) but does not accept parameter
VALUES from the caller. The comment says "wire format deferred to vtmgr
migration." The autostart path hardcodes `PARAM_TTY_INSTANCE=0` for the
"vt" image instead.

Proposed wire format extension for PROCMGR_CONTAINER_RUN_LABEL:

```
Message header:
  words[0] = (unused by handler)
  words[1] = notify_endpoint (existing)
  words[2] = fdac_offset (existing, 0=none)
  words[3] = param_offset (NEW, byte offset in payload, 0=none)
  words[4] = param_count (NEW, number of param override entries)

Payload:
  image_name\0                             NUL-terminated image name
  [FDAC data at fdac_offset]               (optional, existing)
  [param overrides at param_offset]        (NEW, optional)

Param override entry (10 bytes each):
  u16 param_index LE                       ProcessInfo.params[] index
  u64 param_value LE                       Value to set
```

This reuses the same (u16 index + u64 value) format as
PROCMGR_SPAWN_SERVICE_LABEL's param overrides, so the caller (vtmgr)
directly specifies the param index (PARAM_TTY_INSTANCE) and value.

**Issue 2: vtmgr profile escalation**

vtmgr currently has profile `0x05` (IPC + REGISTRY). The VT container
manifest declares USER profile (`0x0F` = IPC + SPAWN + REGISTRY + VFS).
Procmgr's `handle_container_run` validates:

```rust
if !caller_profile.can_grant(requested_profile) { return Err(PermissionDenied); }
```

`0x05.can_grant(0x0F)` = false (vtmgr lacks SPAWN + VFS bits).

Resolution: vtmgr's profile must be at least `0x0F` (USER) so it can
`can_grant` the VT container's USER profile. vtmgr doesn't USE spawn/VFS
for itself, but it must HOLD those bits to delegate them. This is correct
per the capability model — you can only grant what you have.

Update in `init/src/services.rs`: change vtmgr from `0x05` to `0x0F`.

**Issue 3: Fire-and-forget vs call semantics**

vtmgr currently fire-and-forgets spawn requests (send, not call). The
container run handler uses `extract_reply_id` and skips the reply if no
token is present, so fire-and-forget works. vtmgr doesn't need the reply
(pid, container_id) — it only needs the container to start. The bitmask
tracking (`vt_spawned`) is sufficient.

**Issue 4: Endpoint routing**

vtmgr currently subscribes to `procmgr/spawn`. Procmgr routes by MESSAGE
LABEL, not by endpoint — both PROCMGR_SPAWN_SERVICE_LABEL and
PROCMGR_CONTAINER_RUN_LABEL arrive on the same endpoint and are
distinguished by the label field. vtmgr can reuse its existing
`procmgr_spawn_endpoint` for container run messages.

#### G.2 Implementation Tasks

| #   | Task                                                  | Status  | Depends |
|-----|-------------------------------------------------------|---------|---------|
| G0  | View-aware binary loading in procmgr spawn handler    | pending | —       |
| G1  | Extend container run wire format with param overrides | pending | —       |
| G2  | Procmgr: resolve param slots → indices, apply values | pending | G1      |
| G3  | vtmgr: replace spawn_tty with spawn_vt_container     | pending | G1      |
| G4  | vtmgr: rename fields (tty_spawned → vt_spawned)      | pending | G3      |
| G5  | init: update vtmgr profile from 0x05 to 0x0F         | pending | —       |
| G6  | Remove TTY_SPAWN_SHELL_LABEL + SERVICE_PATH from procmgr | pending | G0, G3 |
| G7  | Build + boot test: VT switch spawns container         | pending | G0-G6   |

G0 is a prerequisite for all intra-container binary spawns (not just VT).
It makes `handle_spawn_message` resolve the requested binary path through
the caller's VFS view before loading. Without it, `/bin/shell` resolves
to the userdisk root instead of the container's `/var/images/vt/bin/`.

#### G.3 VT Switch Flow (Target — Post Phase G)

```
1. User presses Ctrl+Alt+F2.

2. kbd decodes scancode, sends VTMGR_SWITCH_VT_LABEL(1) to vtmgr.

3. vtmgr.switch_vt(1):
   - Checks vt_spawned bitmask: is bit 1 set?
     - No → calls spawn_vt_container(1):
       Sends PROCMGR_CONTAINER_RUN_LABEL to procmgr with:
         payload = "vt\0" + param override (PARAM_TTY_INSTANCE=1)
         words[3] = param_offset, words[4] = 1
       Sets vt_spawned |= (1 << 1).
   - Checks vt_created bitmask: is bit 1 set?
     - No → sends CONSOLE_CREATE_VT_LABEL(1) to console.
       Sets vt_created |= (1 << 1).
   - Sends CONSOLE_DEACTIVATE_LABEL(old) to console.
   - Sends CONSOLE_ACTIVATE_LABEL(1) to console.
   - Updates active_vt = 1.

4. procmgr receives PROCMGR_CONTAINER_RUN_LABEL:
   - Reads /var/images/vt/manifest.toml.
   - Validates: caller (vtmgr, 0x0F) can_grant(USER=0x0F) → yes.
   - Reads param slots ["tty_instance"] from manifest.
   - Reads 1 param override from payload: (PARAM_TTY_INSTANCE, 1).
   - Creates grantable endpoint for TOKEN_EXTRA_0.
   - Spawns /var/images/vt/bin/tty with PARAM_TTY_INSTANCE=1.
   - Registers USER VFS view with container_id.

5. tty:1 starts, registers as "tty:1", subscribes to console:0/vt:1.
   tty:1 requests shell spawn via TTY_SPAWN_SHELL_LABEL (or internally
   via container-scoped procmgr spawn once G6 is complete).

6. kbd discovers tty:1 via registry, routes keystrokes to it.
```

Note: G6 (removing TTY_SPAWN_SHELL_LABEL) is optional for the initial
migration. The VT container image includes `/bin/shell`, and tty already
requests shell spawn via procmgr. This works within the container context
because tty inherits the container's SPAWN capability. The dedicated
TTY_SPAWN_SHELL_LABEL handler in procmgr can be removed later when tty
switches to standard `posix_spawn("/bin/shell")`.

### Phase H: Users and Sessions

User identity, authentication, session containers, and privilege
escalation. Implements section 4.9. Depends on Phase G (VT container
migration) because the login flow requires the VT container to be a
separate container from the session.

| #   | Task                                                   | Status  | Depends |
|-----|--------------------------------------------------------|---------|---------|
| H1  | Define /etc/users.toml format and add to userdisk      | pending | —       |
| H2  | Procmgr: parse /etc/users.toml at boot                | pending | H1      |
| H3  | Add PROCMGR_SESSION_LOGIN_LABEL IPC handler            | pending | H2      |
| H4  | Procmgr: build session view from user record + profile | pending | H3      |
| H5  | Procmgr: spawn session container (parent=0, top-level) | pending | H4      |
| H6  | Procmgr: VT–session attachment (wire shell to tty)     | pending | H5      |
| H7  | Procmgr: session table (track session→VT attachments)  | pending | H5      |
| H8  | Procmgr: VT crash recovery (reattach session to new VT)| pending | H6, H7  |
| H9  | tty: login prompt mode (read username/password, send to procmgr) | pending | H3 |
| H10 | tty: switch between login mode and terminal mode       | pending | H9      |
| H11 | tty: handle session death notification (return to login)| pending | H10     |
| H12 | VT container: reduce profile from 0x0F to 0x05        | pending | H5, G7  |
| H13 | Add PROCMGR_ESCALATE_LABEL handler (sudo)              | pending | H2      |
| H14 | Shell builtin: `sudo` (prompt + escalation request)    | pending | H13     |
| H15 | Shell builtin: `su` (prompt + session login request)   | pending | H3      |
| H16 | Add ADMIN profile constant to cap.rs                   | pending | —       |
| H17 | ADMIN default view template in procmgr                 | pending | H16     |
| H18 | Test: login → session with correct view (parent=0)     | pending | H5      |
| H19 | Test: VT crash → session survives, reattaches          | pending | H8      |
| H20 | Test: logout → cascades children, VT shows login       | pending | H5, H11 |
| H21 | Test: sudo creates elevated container                  | pending | H13     |
| H22 | Test: su creates nested session with target's view     | pending | H15     |
| H23 | Test: escalation beyond ceiling rejected               | pending | H13     |

Critical path: H1 → H2 → H3 → H4 → H5 → H6 → H8 (login + reattach)
Parallel tracks: H9-H11 (tty login mode), H13-H14 (sudo), H16-H17 (ADMIN profile)

### Phase I: Restart Policies

Container restart management for system services. Implements section 4.10.
Can be done independently of Phase H (sessions).

| #   | Task                                                   | Status  | Depends |
|-----|--------------------------------------------------------|---------|---------|
| I1  | Cluufile: RESTART and MAX_RESTARTS directives          | pending | —       |
| I2  | container-build: emit restart policy to manifest.toml  | pending | I1      |
| I3  | Procmgr: restart policy struct + manifest parsing      | pending | I2      |
| I4  | Procmgr: entrypoint exit detection + restart logic     | pending | I3      |
| I5  | Procmgr: exponential backoff timer for restarts        | pending | I4      |
| I6  | Procmgr: crash loop detection (max_restarts/window)    | pending | I4      |
| I7  | init: panic on primordial death (Tier 1 failure)       | pending | —       |
| I8  | vtmgr: VT-specific restart (lazy respawn on inactive)  | pending | I4, G7  |
| I9  | Registry: handle service re-registration after restart | pending | I4      |
| I10 | Test: system service crash → auto-restart              | pending | I4      |
| I11 | Test: crash loop → stops restarting after max          | pending | I6      |
| I12 | Test: primordial death → kernel panic                  | pending | I7      |

Critical path: I1 → I2 → I3 → I4 → I5/I6 (restart core)
Parallel tracks: I7 (init panic), I8 (vtmgr), I9 (registry reconnect)

### Phase J: Graceful Shutdown

Orderly system shutdown with grace periods. Implements section 4.11.
Depends on Phase I (restart policies must be disabled during shutdown).

| #   | Task                                                   | Status  | Depends |
|-----|--------------------------------------------------------|---------|---------|
| J1  | Add PROCMGR_SHUTDOWN_LABEL IPC handler                 | pending | —       |
| J2  | Add SHUTDOWN_NOTIFY IPC label for container notification| pending | —      |
| J3  | Procmgr: session shutdown (notify + grace + kill)      | pending | J1, J2  |
| J4  | Procmgr: Tier 2 service shutdown (reverse order)       | pending | J3      |
| J5  | VFS: flush + unmount on shutdown                       | pending | J4      |
| J6  | Procmgr: Tier 1 primordial shutdown sequence           | pending | J5      |
| J7  | init: kernel shutdown/reboot syscall                   | pending | J6      |
| J8  | Procmgr: disable restart policies during shutdown      | pending | J1, I4  |
| J9  | kbd: Ctrl+Alt+Del → PROCMGR_SHUTDOWN_LABEL             | pending | J1      |
| J10 | Shell builtin: `reboot` and `poweroff` (require ADMIN) | pending | J1      |
| J11 | Test: graceful shutdown sequence completes              | pending | J7      |
| J12 | Test: Ctrl+Alt+Del triggers shutdown                   | pending | J9      |

Critical path: J1 → J3 → J4 → J5 → J6 → J7 (full shutdown chain)

### Phase K: Container Addressing

Per-session instance naming and scoped container management. Implements
section 4.12. Can be done independently; improves UX for multi-instance
and multi-user scenarios.

| #   | Task                                                   | Status  | Depends |
|-----|--------------------------------------------------------|---------|---------|
| K1  | Procmgr: ContainerEntry with session_id + instance_name| pending | —      |
| K2  | Procmgr: per-session instance counter per image_name   | pending | K1      |
| K3  | Procmgr: instance name generation (editor, editor.2)   | pending | K2      |
| K4  | container list: filter by caller's session_id          | pending | K1      |
| K5  | container stop: resolve instance name within session   | pending | K3      |
| K6  | container stop @N: resolve by container_id             | pending | K1      |
| K7  | ADMIN visibility: system containers in container list  | pending | K4      |
| K8  | Test: two instances of same image get distinct names   | pending | K3      |
| K9  | Test: Alice's containers invisible to Bob              | pending | K4      |
| K10 | Test: container stop resolves correct instance         | pending | K5      |

Critical path: K1 → K2 → K3 → K5 (naming + resolution)

### Phase L: VT Screen State Hardening

Fix VT switching bugs and harden screen state management for multi-user
operation. Implements section 4.13. Can start immediately — the core
renderer fixes (L1-L5) have no dependencies on other phases.

| #   | Task                                                   | Status  | Depends |
|-----|--------------------------------------------------------|---------|---------|
| L1  | Refactor: VtScreen as self-contained object (own write_bytes, parser) | done | — |
| L2  | Eliminate context-switch trick for inactive VT writes  | done    | L1      |
| L3  | Atomic VT switch: CONSOLE_SWITCH_VT_LABEL (single msg) | done   | L1     |
| L4  | vtmgr: replace deactivate+activate with switch message | done    | L3      |
| L5  | Console: repaint_all correctness (cursor position, colors) | pending | L1  |
| L6  | Output flow control: console processes inactive VT writes into VtScreen directly | pending | L1 |
| L7  | Remove tty output queue overflow/drop behavior         | pending | L6      |
| L8  | SCREEN_REDRAW_NOTIFY: console crash → tty → session programs | pending | I4  |
| L9  | tty: forward SCREEN_REDRAW as terminal resize event   | pending | L8      |
| L10 | Scrollback buffer: ring buffer per VtScreen            | pending | L1      |
| L11 | kbd: Shift+PageUp/PageDown for scrollback navigation   | pending | L10     |
| L12 | Test: VT switch preserves screen content + cursor      | pending | L3, L5  |
| L13 | Test: background VT output not lost on switch back     | pending | L6      |
| L14 | Test: console crash → sessions redraw                  | pending | L8      |

Critical path: L1 → L2/L3/L5/L6 (renderer refactor enables all fixes)
High priority: L1-L5 fix the current garbled screen bug
Lower priority: L8-L9 (reattach), L10-L11 (scrollback)

---

## 8. File Map

Files created or modified across all phases:

| File                               | Phases    | Purpose                              |
|------------------------------------|-----------|--------------------------------------|
| `userspace/libcluu/src/cap.rs`     | B         | CapProfile bitflags + helpers        |
| `userspace/libcluu/src/lib.rs`     | B, E      | Export cap, toml modules             |
| `userspace/libcluu/src/boot.rs`    | B         | PARAM_CAP_PROFILE constant           |
| `userspace/libcluu/src/ipc.rs`     | A,C,D,E   | send_msg_with_payload, VFS_SET_VIEW, container cleanup, container run/list labels |
| `userspace/libcluu/src/toml.rs`    | E         | Minimal no_std TOML parser           |
| `userspace/libcluu/src/fs/client.rs` | E       | VfsClient::link() for hardlinks      |
| `userspace/libcluu/src/registry.rs`| Security  | service_name in GRANT_DELIVER        |
| `userspace/procmgr/src/main.rs`    | A,B,D,E,G | Profile-gated spawn, container IDs, container run, FDAC validation, image_dirs |
| `userspace/vtmgr/src/context.rs`   | A, G      | VT lifecycle, container run migration |
| `userspace/vtmgr/src/main.rs`      | A         | Clean unused imports                 |
| `userspace/console/src/context.rs` | Security  | Per-VT endpoints                     |
| `userspace/console/src/main.rs`    | Security  | Per-VT endpoint dispatch             |
| `userspace/kbd/src/context.rs`     | Security  | VT slot by service_name              |
| `userspace/tty/src/context.rs`     | Security  | Subscribe to vt:N endpoint           |
| `userspace/vfs/src/main.rs`        | C, D, E   | View enforcement, storage lifecycle, VFS_LINK |
| `userspace/vfs/src/mount.rs`       | E         | MountTable::link() → ext2 FS_LINK, MemFs |
| `userspace/vfs/src/view.rs`        | C         | VfsView struct + path filter logic   |
| `userspace/init/src/services.rs`   | B, G      | Profile assignments per service      |
| `userspace/shell/src/commands.rs`   | E        | Container shell builtins             |
| `tools/container-build/`          | E         | Standalone container image builder (Cluufile parser, manifest gen) |
| `xtask/src/main.rs`               | D, E      | Userdisk build, /var/images/ integration |
| `containers/*/Cluufile`           | E         | Container build definitions          |
| `docs/PROCESS_ISOLATION_DESIGN.md` | all       | This document (updated per phase)    |

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
| Console confused-deputy          | Per-VT endpoints: endpoint index IS VT identity |
| Multi-VT service spoofing        | Registry GRANT_DELIVER includes service_name   |
| FDAC handle injection            | token_derive probe validates endpoint ownership |
| Privilege escalation via sudo    | Escalation ceiling in user record; procmgr enforces |
| Cross-user home access           | Session view scoped to user's home; no /home/* wildcard |
| Session forgery                  | Only procmgr can create session containers      |
| Credential theft via tty         | tty has no VFS access (0x05); creds forwarded via IPC |
| VT crash kills user work         | Session is top-level (parent=0); VT crash → reattach |
| Service crash takes down system  | Auto-restart with backoff; primordial death → panic |
| Unauthorized shutdown            | PROCMGR_SHUTDOWN_LABEL requires CAP_ADMIN |
| Cross-user container visibility  | container list scoped by session_id |

### What This Design Does NOT Prevent (Yet)

| Attack                    | Status                                          |
|---------------------------|-------------------------------------------------|
| Resource exhaustion (RAM) | No per-process memory quotas yet                |
| CPU starvation            | Priority-based scheduling only, no hard limits  |
| Covert channels (timing)  | Not addressed — common in capability systems   |
| Storage quota enforcement | Planned (manifest.toml `quota` field) but not implemented |
