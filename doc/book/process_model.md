# The Process Isolation Model

CLUU's process isolation rests on one premise: **authority is structural,
not conventional**. There is no runtime access-control layer. The kernel
knows threads, capability tokens, and IPC. Processes, filesystems, and
security policy all live in userspace, and the boundary between them is
decided once at spawn time, then enforced by arithmetic.

This chapter covers the three mechanisms that compose that boundary:
capability profiles, VFS views, and the container model. The
[Session Encapsulation](sessions/index.html) chapter extends this with user
identity and login; the [Process Management](procmgr/index.html) chapter
covers the spawn path that installs these structures.

## Guiding principles

### Capabilities as the only access control

Traditional kernels layer access control on top of every operation. A
process holds a file descriptor, and the kernel still checks UIDs,
permission bits, or MAC labels at runtime. The process *has* the
descriptor, but the kernel decides whether the operation is permitted.

CLU follows seL4's capability model instead. A token **is** permission. If
a process holds a token with `IPC_SEND` rights to an endpoint, it can send.
If it does not hold the token, it cannot. The kernel never asks "who is the
caller?", it only checks "does this token exist and does it have the
requested right bit set?"

All security decisions happen at **distribution time**: when a parent
spawns a child, it chooses which tokens to hand over and with what rights.
Once the child is running, neither parent nor kernel can retroactively
widen what the child can do (except by revoking the token entirely, which
is a hard kill, not a policy adjustment). The spawn path is the entire
security policy.

### The three rules

These are structural invariants enforced by the kernel's `token_derive`
syscall and procmgr's validation logic.

1. **Capabilities only narrow, never widen.** `token_derive(parent_token,
   rights, badge)` returns a new token whose rights are `parent_rights &
   rights`. The kernel rejects any attempt to set a bit the parent lacks.
   This is arithmetic, not a policy check.
2. **VFS views only narrow, never widen.** A VFS view is a set of path
   prefixes. When a parent spawns a child with a view of `/bin`, procmgr
   checks that `/bin` is a subset of the parent's view before telling VFS.
   VFS never accepts a view expansion.
3. **Every spawn goes through procmgr.** The kernel's `thread_create`
   syscall requires a capability with the CREATE right. Only init and
   procmgr hold this. Init uses it once to spawn the boot services, then
   idles. After boot, procmgr is the sole process that can create new
   threads and address spaces.

### The trust chain

Authority flows in one direction: kernel → init (SUPERVISOR) → procmgr
(SUPERVISOR) → services → user programs. Init receives the root token,
spawns procmgr and the boot services, derives tokens with specific rights
for each, then idles forever. Each level can only grant capabilities it
already holds, so the hierarchy cannot be inverted.

Sessions and VTs are **siblings, not parent-child**. VT death does not
cascade to the session, vtmgr respawns the VT and reattaches. Session
death (logout) cascades to user containers but leaves the VT alive to show
a new login prompt. This lifecycle separation is covered in the
[Session Encapsulation](sessions/index.html) chapter.

## Capability profiles

### Why a bitmask, not a role enum

A **CapProfile** is a compact bitmask that answers: "what categories of
system interaction is this process allowed to perform?" It does not replace
kernel token rights. It sits above them as an intent declaration that
procmgr translates into concrete token derivations.

The design is deliberately bitmask-based rather than role-based. A role
system (`SANDBOXED=0, USER=1, SERVICE=2, SUPERVISOR=3`) creates a linear
hierarchy that is hard to extend. A bitmask creates a lattice: `CAP_IPC |
CAP_VFS` is a valid profile that is neither USER nor SERVICE. Real services
have diverse needs, a network daemon needs IPC, registry, VFS, and network
access but not device caps or spawn rights.

Profiles are stored as a `u16` bitmask in the process's params (one of the
10 param slots in `ProcessInfo`). This makes them introspectable: a process
can read its own profile, and procmgr can look up any process's profile
when validating a spawn request.

### The eight capability bits

| Bit | Name              | Meaning                                                              |
|-----|-------------------|----------------------------------------------------------------------|
| 0   | `CAP_IPC`         | Create endpoints, send/receive. Without it, only pre-wired stdio works. |
| 1   | `CAP_SPAWN`       | Request procmgr to create children. The "can this process multiply" gate. |
| 2   | `CAP_REGISTRY`    | Register outputs and subscribe to services via the IPC registry.     |
| 3   | `CAP_VFS`         | Access the virtual filesystem. VFS checks this before any file op.   |
| 4   | `CAP_DEVICE`      | Hold device tokens: IRQ, PCI BAR mappings, DMA buffers. Drivers only. |
| 5   | `CAP_SPACE_GRANT` | Grant memory pages to other address spaces. Required for shared memory. |
| 6   | `CAP_NET`         | Access the network stack (future, reserved).                         |
| 7   | `CAP_ADMIN`       | System-wide administrative operations: reboot, mount, global config. |
| 8-15 | (reserved)       | Available for future expansion without changing the `u16` format.    |

### Built-in profiles

These are named constants, not special cases. Any combination of bits is a
valid profile.

| Profile    | Hex    | Capabilities                                  |
|------------|--------|-----------------------------------------------|
| SANDBOXED  | `0x01` | IPC only                                      |
| USER       | `0x0F` | IPC, spawn, registry, VFS                     |
| ADMIN      | `0x8F` | USER plus admin                               |
| SERVICE    | `0x3F` | IPC, spawn, registry, VFS, device, space grant |
| SUPERVISOR | `0xFF` | Everything                                    |

The `can_grant` helper checks whether a child profile is a valid narrowing
of a parent: `(child.bits() & !self.bits()) == 0`. This is the same
arithmetic the kernel uses for `token_derive`, applied at the profile layer.

### Profile-to-rights mapping

When procmgr spawns a process with a given profile, it derives kernel
tokens with rights determined by the profile. A SANDBOXED process gets
stdio tokens with `IPC_SEND` and `IPC_RECV`, but its `TOKEN_SELF`,
`TOKEN_SPACE`, `TOKEN_IPC`, and `TOKEN_EXTRA` slots are empty (zero). It
cannot use those capabilities, there is nothing to derive from.

A USER process gets `TOKEN_IPC` with `CREATE`, `IPC_SEND`, `IPC_RECV`,
`IPC_CALL`, and `GRANT` rights, letting it self-wire at runtime: create
endpoints, register with the IPC registry, subscribe to services. A SERVICE
process gets the same plus `TOKEN_SPACE` with `SPACE_GRANT` and
device-specific `TOKEN_EXTRA` slots. Empty slots mean "no token here",
stronger than a permission denial, the capability does not exist.

### Service profile assignments

Every service gets its **minimal** profile, only the bits it actually
needs. Most services were historically over-privileged with the full
SERVICE bitmask. The current assignments:

| Service    | Profile       | Bits needed                             |
|------------|---------------|-----------------------------------------|
| init       | SUPERVISOR    | All (bootstrap authority)               |
| procmgr    | SUPERVISOR    | All (sole spawn authority)              |
| vfs        | `0x25`        | IPC, REGISTRY, SPACE_GRANT              |
| virtio-blk | `0x37`        | IPC, REGISTRY, DEVICE, SPACE_GRANT      |
| console    | `0x17`        | IPC, REGISTRY, DEVICE                   |
| kbd        | `0x17`        | IPC, REGISTRY, DEVICE                   |
| vtmgr      | `0x0F`        | IPC, SPAWN, REGISTRY, VFS (must `can_grant` USER for VT containers) |
| registry   | `0x05`        | IPC, REGISTRY                           |
| VT (tty)   | `0x05`        | IPC, REGISTRY (terminal driver only; session is separate) |
| session    | USER or ADMIN | Per user record                         |
| shell      | USER          | IPC, SPAWN, REGISTRY, VFS               |
| plugin     | SANDBOXED     | IPC only (communicates through parent's pipes) |

## VFS views

### Private namespaces, not chroot

A traditional Unix filesystem is a single global namespace. Every process
sees the same `/`, the same `/etc/passwd`, the same `/home`. Access control
is layered on top via UIDs, file permission bits, and optional MAC systems.

CLUU inverts this. Each process sees a **private namespace** defined by its
VFS view. Two processes running simultaneously may see completely different
directory trees. Process A might see `/bin` and `/tmp`. Process B might see
`/bin`, `/lib`, and `/data`. Neither can discover that the other's paths
exist.

A VFS view is not a chroot. A chroot changes the root but still exposes the
full subtree. A VFS view is an explicit allowlist of path prefixes with
per-prefix access modes. Paths outside the view return `ENOENT`, "not
found", not "permission denied." From the process's perspective, those
paths simply do not exist.

This is enforced by the VFS service (a userspace process), not the kernel.
The kernel does not know about paths. It only sees IPC messages between the
client and VFS. VFS checks the client's view on every operation.

### View structure and path resolution

A view is an ordered list of mount rules. Each rule has a source path (the
real backing path on disk), a destination path (what the client sees), and a
writable flag. First match wins.

When a client sends `open("/data/config.txt", O_RDONLY)`, VFS receives the
IPC, identifies the client by sender endpoint, looks up the client's view
(falling back to the profile default if none registered), iterates the mount
list for a matching prefix, rewrites the path, and opens the real path in
the backing filesystem. Missing path returns `ENOENT`. Write on a read-only
mount returns `EACCES`. If no mount matches, VFS returns `ENOENT`, the
client cannot distinguish "path not in my view" from "path does not exist
on disk."

### Default views by profile

When no explicit view is provided in the spawn request, procmgr instructs
VFS to install the default for the profile:

| Profile    | Mounts                                                         |
|------------|----------------------------------------------------------------|
| SANDBOXED  | (empty, no filesystem access at all)                          |
| USER       | `/bin` (ro), `/lib` (ro), `/tmp` (rw), `/home/<user>` (rw)    |
| ADMIN      | USER plus `/etc` (ro), `/var/log` (ro), `/var/services` (rw)  |
| SERVICE    | `/bin` (ro), `/lib` (ro), `/dev` (rw), `/etc` (ro), `/tmp` (rw) |
| SUPERVISOR | `/` (rw), full access                                         |

The `<user>` in USER and ADMIN views is determined by the session identity.
A session-scoped `/proc` mount routes to the session-procmgr, not the
root-procmgr, so a session binary only sees its own session's processes.

### View narrowing

A parent with view `{/bin(ro), /lib(ro), /tmp(rw)}` can grant a child
`{/bin(ro), /tmp(rw)}` (subset), `{/bin(ro), /tmp(ro)}` (stricter mode),
or `{/bin(ro)}` (fewer paths). All allowed. A parent **cannot** grant
`{/bin(ro), /etc(ro)}` (`/etc` not in parent's view) or `{/bin(rw)}`
(parent has `/bin` as ro, child requests rw). Both rejected.

Procmgr validates: for every mount in the child's requested view, there
must exist a mount in the parent's view where `child.dst` is a prefix of
`parent.dst` and `child.writable` is no greater than `parent.writable`.

## The container model

### Containers as the universal isolation boundary

In Docker, a "container" is a special thing separate from processes. You
`docker run` to start a container and `./myapp` to start a process. You can
run processes outside containers.

In CLUU, **no process can exist outside a container**. Containers are not
a special mode. They are the universal isolation boundary. Procmgr enforces
this: any spawn request from a process with `container_id == 0` is rejected.

```text
Container = ContainerId + CapProfile + VfsView + PrivateStorage
```

A container can hold multiple processes. The **entrypoint** is the process
spawned when the container starts. Other processes are spawned within the
container at runtime and share its boundary, same VFS view, same private
storage, same `container_id`. They may have different (narrowed) profiles.
Container lifecycle is tied to the entrypoint: when the entrypoint exits,
the container is destroyed and all remaining processes are cleaned up.

This has a structural security property: **there is no "host" to escape
to**. There are only containers with wider or narrower views. The widest
container (procmgr, SUPERVISOR) sees everything. Every other container is a
narrowing of that. A container escape would require widening a VFS view or
escalating a CapProfile, both prevented by `token_derive` arithmetic.

### Image containers

An image container is a self-describing package built from a **Cluufile**
(a Dockerfile-like declarative build file) and stored as a pre-extracted
directory on the ext2 disk. The build pipeline parses the Cluufile, resolves
`FROM base` by merging the sysroot's `/bin` and `/lib` into the image
directory, processes `COPY` directives, and generates a `manifest.toml`.
Each image is self-contained under `/var/images/<name>/`, no runtime
overlay, COW, or whiteouts needed.

Procmgr reads the manifest at runtime. It is intentionally simple (flat
TOML, no nested structures) to keep the `no_std` TOML parser minimal.
Unknown capability names are rejected, not silently ignored.

Image security: images are stored at `/var/images/` on ext2, outside the
USER view, containers cannot see or modify their own base images. VFS
enforces read-only on image mounts. Manifest capabilities are validated as
a subset of the caller's profile. Procmgr loads the binary, not the
container, no path traversal risk.

### Private storage

Each container gets an isolated storage directory. The container sees it as
a local path (e.g., `/data`), but the real backing path is scoped by
container ID:

```text
/var/containers/
├── c-00001/
│   ├── data/    persistent (rw), survives restart
│   ├── tmp/     ephemeral, cleared on restart
│   └── log/     append-only log sink
├── c-00002/
│   └── ...
```

The VFS view maps these transparently. The container cannot see
`/var/containers/` itself, nor any other container's storage. It cannot
discover that other containers exist. Storage lifecycle follows the
container: directories created at spawn, `/tmp` cleared on restart, `/data`
persists if the manifest declares it, the entire tree deleted on explicit
destroy.

### Nested containers

There are two ways a process spawns another process:

- **Intra-container spawn** (`PROCMGR_SPAWN_LABEL`): the child inherits the
  parent's `container_id`, view, and profile (or a narrowing). Binary source
  is the caller's VFS view. Used for `ls`, `cat`, shell-to-helper.
- **Container run** (`PROCMGR_CONTAINER_RUN_LABEL`): the child gets a fresh
  `container_id`, a profile from the manifest (validated as subset of
  caller), and a view combining the launcher's view with the container
  image. Used for `container run editor`, vtmgr spawning VTs.

When a process runs `container run editor`, the child's view is built by
combining the launcher's view with the container image. **Image dirs
override, everything else passes through.** If the child has `CAP_VFS`, it
sees what the launcher sees, except that image-provided directories
(`/bin`, `/lib`) are replaced by the container's own. The child's
user-visible paths are a subset of the launcher's paths, so "views only
narrow" holds.

Containers without `CAP_VFS` (sandboxed) get no paths at all, they
communicate only through stdio pipes. Containers that want restricted
access can opt out via `deny_inherit = true` or a `deny = ["/home"]` list
in their manifest. By default, user-visible paths pass through, restriction
is opt-in by the container author.

Nesting is bounded by `MAX_NESTING_DEPTH = 8`. The default lifecycle is
**cascading cleanup**: when a parent container's entrypoint exits, procmgr
destroys it and all child containers recursively. There is no "stop only
this container but keep its children" operation. This preserves view
validity, simplifies resource accounting, matches user expectations, and
avoids zombie containers.

The exception is **detached containers**. A manifest can declare
`detach = true`, which sets `parent_container_id = 0` at spawn. The
container becomes top-level. A detached container dies only when explicitly
stopped, when its own entrypoint exits, or at system shutdown.

### VT containers

A Virtual Terminal (VT) is a single container that colocates the tty
process and the shell process. The container profile is USER (`0x0F`). The
ENTRYPOINT is tty, which spawns shell internally. Both processes share the
container's VFS view, private storage, and `container_id`.

Container lifecycle equals entrypoint lifecycle: the VT container lives as
long as tty lives. If shell exits, tty can respawn it. If tty exits, the
container is destroyed and all children are cleaned up.

With sessions as separate containers, the VT container no longer needs USER
capabilities. tty needs IPC and REGISTRY only, not SPAWN (procmgr spawns
the session) or VFS (tty does not access files). The VT container profile
dropped from `0x0F` to `0x05`, a security improvement: the terminal driver
runs with minimal capabilities.

### Container addressing

Every running container has three identifiers: `container_id` (system-wide
unique, monotonic, assigned by procmgr), `image_name` (from the manifest,
same for all instances), and `instance` (scoped name: image_name plus
counter, `"editor"`, `"editor.2"`, `"editor.3"`). Instance counters are
**per-session** for user containers and **system-wide** for Tier 2
services. Alice's editors are numbered independently of Bob's.

Users only see containers they own (in their session tree). Bob's
containers are invisible to Alice. Tier 2 autostart containers and VT
containers are system-scoped, visible to ADMIN sessions but not USER
sessions. For unambiguous addressing, the numeric `container_id` always
works: `container stop @42`.

## Security properties

### What this design prevents

| Attack                              | Prevention mechanism                                  |
|-------------------------------------|-------------------------------------------------------|
| Privilege escalation via spawn      | Profile bitmask: child is subset of parent (arithmetic) |
| Filesystem escape (path traversal)  | VFS view: paths outside view return `ENOENT`          |
| Cross-container data access         | Private storage: each container has unique ID          |
| Unauthorized device access          | `CAP_DEVICE` bit: no bit, no device tokens             |
| Service impersonation               | Registry gated by `CAP_REGISTRY` bit                   |
| Fork bomb                           | `CAP_SPAWN` bit: no bit, cannot spawn                  |
| Token forgery                       | Kernel: `token_derive` enforces right narrowing        |
| Console confused-deputy             | Per-VT endpoints: endpoint index is VT identity        |
| FDAC handle injection               | `token_derive` probe validates endpoint ownership      |
| Privilege escalation via sudo       | Escalation ceiling in user record; procmgr enforces    |
| Cross-user home access              | Session view scoped to user's home; no `/home/*` glob  |
| Session forgery                     | Only procmgr can create session containers             |
| Credential theft via tty            | tty has no VFS access (`0x05`); creds forwarded via IPC |
| VT crash kills user work            | Session is top-level (`parent=0`); reattach on VT crash |
| Service crash takes down system     | Auto-restart with backoff; primordial death → panic    |
| Unauthorized shutdown               | `PROCMGR_SHUTDOWN_LABEL` requires `CAP_ADMIN`          |
| Cross-user `/proc` snooping         | ProcfsBackend session-based filtering via procmgr      |

### What this design does not prevent yet

| Attack                       | Status                                           |
|------------------------------|--------------------------------------------------|
| Resource exhaustion (RAM)    | No per-process memory quotas yet                 |
| CPU starvation               | Priority-based scheduling only, no hard limits   |
| Covert channels (timing)     | Not addressed (common in capability systems)     |
| Storage quota enforcement    | Planned (`manifest.toml` `quota` field), not impl |

## Frame typing + unified process model (2026-05-18)

Two flaws met in the wrong place and broke compositor RSV at runtime.

**Flaw 1 — no kernel-side frame ownership.** PMM was a pure buddy allocator
with bitmap + intrusive free lists. It did not know what a physical frame
was being used for. `frame_registry` covered only user-visible tokens
(FrameAllocate) and SpaceGrant shares — never intermediate page tables
(PT/PD/PDPT/PML4) and never plain user leaves. `teardown_user_pages`
discovered PT/PD/PDPT addresses by walking the live PML4; there was no
global record of "this phys is S's PDPT for the 0x400000–0x5fffff window".
Two paths could end up holding the same phys with no interlock. Concrete
failure: same frame freed once as login's user leaf and again as
user-compositor's PDPT, with no alloc in between — the frame had two
owners; the first to free poisoned the second's table.

**Flaw 2 — kernel duplicated procmgr's job.** Three parallel models of
"userspace process" existed: (1) `sched/process.rs::Process` +
`PROCESS_MANAGER` (only used for init's primordials), (2)
`mm/space_repository.rs` storing `AddressSpace` per `AddressSpaceId`
(used by everything procmgr spawns via `invoke_space_create`), (3)
procmgr's own `pid → space_token / cookie / container` tables.
Lifecycle gates lived in different places: kernel `space_destroy` ↔
`Process::drop` ↔ procmgr cascade-kill ↔ vfs `container_cleanup`. None
of them knew about the typed state of the frames being torn down.

The redesign makes the kernel know exactly **five things** about
userspace: threads, address spaces, endpoints, tokens, and typed frames
(Untyped / PageTable / UserData / Grant / Device / KernelHeap). No
`Process` struct. No primordial registry. No process-state mirror.
Procmgr (userspace) owns pid allocation, parent/child trees, containers,
session/login state, exit notification fanout, restart policy,
cascading kill, name → space_token mapping. Key decisions:

- **Frame-type model**: `FrameMeta { tag: FrameTag, refcount: u16,
  owner: u16, extra: u8 }` per-frame array (~6 bytes/frame; 8 GB RAM =
  ~12 MB static kernel BSS). A frame's type is set by **retype**, never
  by direct write. `pmm::free_frame` accepts only frames whose state is
  `Untyped` with `refcount == 0`. Retype back to Untyped requires
  `refcount == 0` and removal from all containing tables.
- **Refcount semantics**: PageTable refcount = number of child
  PT/PD/PDPT/PML4 entries pointing to it (seL4 "vstore" semantics).
  UserData refcount = number of leaf PTEs pointing to it (typically 1;
  ≥2 for grants / MAP_SHARE_PHYS). Once any UserData refcount hits 2,
  the frame is retyped UserData → Grant atomically. Grant stays alive
  until refcount == 0 (simpler than reverse-retype on drop to 1).
- **Unified teardown**: only one path — a thread (or procmgr on behalf
  of a dying process) invokes `space_destroy(space_token)`. The kernel
  removes from `space_repository`, walks the PML4 calling `dec_ref` on
  each user-half PT/PD/PDPT, `dec_ref` on each leaf PTE's user
  data/grant frame, `dec_ref` on the PML4 itself. `dec_ref` automatically
  retypes Untyped + `pmm::free_frame_untyped` when count hits zero. The
  teardown loop never directly calls `pmm::free_*`. No more
  `freed: BTreeSet`. No alias possible because shared frames have
  refcount ≥ 2 and stay alive until everyone is done.
- **What leaves the kernel**: `sched/process.rs` (`Process`,
  `ProcessId`, `ProcessState`, `ProcessType`, `Process::Drop`),
  `sched/process_manager.rs` (`PROCESS_MANAGER`, `spawn_user`,
  `spawn_kernel`, `reap`), `sched/spawn.rs`. **What stays**: `Thread` +
  `THREAD_MANAGER`, `AddressSpace` + `space_repository`, `Token` table +
  `OpaqueScope` + revocation, IPC endpoints.
- **Migration in 4 phases**: Phase 1 lands `FrameMeta` + `retype_*` API,
  behaviorally identical (refcount advisory). Phase 2 enforces refcount
  semantics + routes SHARED_PHYS/SpaceGrant through Grant (the
  load-bearing semantic change — the 2026-05-18 alias trace stops
  happening). Phase 3 retires kernel-side `Process` (delete
  `sched/process.rs`, `process_manager.rs`, `PROCESS_MANAGER`,
  `Process::Drop` teardown). Phase 4 removes temporary soft-fails in PMM
  + decides Grant → UserData reverse-retype (recommend keeping Grant
  alive until refcount = 0).
