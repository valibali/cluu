# CLUU Process Isolation & Containerization Design

**Date:** 2026-02-20
**Scope:** Capability profiles, VFS views, container model, spawn protocol
**Status:** Implementation in progress — Phases A–D complete; Phase E in design
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
| vtmgr       | Custom        | `0x05` | IPC + REGISTRY                          | Coordinate VTs via IPC, no direct spawn    |
| registry    | Custom        | `0x05` | IPC + REGISTRY                          | Process subscriptions, self-register       |
| timeserver  | Custom        | `0x05` | IPC + REGISTRY                          | Serve time queries                         |
| VT (tty+shell) | USER       | `0x0F` | IPC + SPAWN + REGISTRY + VFS            | Terminal + shell colocated (see 4.6)       |
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

### Phase E: Container Images

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
| E8  | Procmgr: seed persistent data via hardlinks       | pending | E6, E7  |
| E9  | Shell builtin: `container run <name>`             | done    | E6      |
| E10 | Shell builtin: `container list`                   | done    | E6      |
| E11 | Shell builtin: `container stop <name>`            | done    | E6      |
| E12 | Extract container-build from xtask to `tools/`    | done    | E3      |
| E13 | Integration test: build + run container            | pending | E9      |

Critical path: E1 → E2 → E3 → E4 → E6 → E9 → E13
Parallel tracks: E5 (TOML parser), E7 (hardlink support), E12 (crate extraction)

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
| `userspace/libcluu/src/lib.rs`     | B, E    | Export cap, toml modules             |
| `userspace/libcluu/src/boot.rs`    | B       | PARAM_CAP_PROFILE constant           |
| `userspace/libcluu/src/ipc.rs`     | A, C, D, E | send_msg_with_payload, VFS_SET_VIEW, container cleanup, container run/list labels |
| `userspace/libcluu/src/toml.rs`    | E       | Minimal no_std TOML parser           |
| `userspace/libcluu/src/fs/client.rs` | E     | VfsClient::link() for hardlinks      |
| `userspace/procmgr/src/main.rs`    | A,B,D,E | Profile-gated spawn, container IDs, container run |
| `userspace/vtmgr/src/context.rs`   | A       | Fix spawn protocol                   |
| `userspace/vtmgr/src/main.rs`      | A       | Clean unused imports                 |
| `userspace/vfs/src/main.rs`        | C, D, E | View enforcement, storage lifecycle, VFS_LINK |
| `userspace/vfs/src/mount.rs`       | E       | MountTable::link() → ext2 FS_LINK   |
| `userspace/vfs/src/view.rs`        | C       | VfsView struct + path filter logic   |
| `userspace/init/src/services.rs`   | B       | Profile assignments per service      |
| `userspace/shell/src/commands.rs`   | E       | Container shell builtins             |
| `tools/container-build/`          | E       | Standalone container image builder (Cluufile parser, manifest gen) |
| `xtask/src/main.rs`               | D, E    | Userdisk build, /var/images/ integration |
| `containers/*/Cluufile`           | E       | Container build definitions          |
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
