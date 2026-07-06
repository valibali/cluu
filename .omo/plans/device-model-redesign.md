# Device Model Redesign — Design Plan

**Date:** 2026-07-06
**Status:** Design, pre-implementation.
**Scope:** Generalize devmgr from block-only to a general device registry; make `/dev` dynamic and devmgr-backed; add `/dev/input/*` nodes; preserve no-runtime-ACL visibility via spawn-time VFS views.
**Related:**
- `docs/superpowers/designs/2026-05-13-input-routing-single-source.md` (inputd extraction path)
- `.omo/plans/async-vfs-devmgr-full.md` (devmgr + BlockRegion cap foundation)
- `AGENTS.md` §2 (capability tokens), §3 (no runtime ACL), §5 (session encapsulation), §6 (root godmode), §7 (async runtime)

## Destination

A CLUU where:

1. **devmgr is the general device registry.** Drivers of all classes (block, char, input, framebuffer) register with devmgr at boot. devmgr mints per-device capability tokens and is the single authority broker for device access — the device analogue of procmgr for processes.
2. **`/dev` is dynamic.** VFS enumerates `/dev` nodes by querying devmgr's registry, not from a hardcoded list in `mount.rs`. Adding a device requires registering with devmgr, not editing VFS source.
3. **Input has a `/dev` path.** `/dev/input/kbd` and `/dev/input/mouse` exist as VFS nodes backed by the input driver via the async VFS runtime. Ordinary binaries with the right view can `read(2)` input events. The fast path (vtmgr → compositor) is preserved.
4. **Session-scoped device visibility is declarative.** Which `/dev` nodes a session sees is decided at spawn via the VFS view (the existing `VIEW_SCOPE_DEV` mechanism), not by a runtime permission check. Root session godmode (§6) sees all devices.
5. **A new `DeviceRegion` capability type** generalizes the `BlockRegion` pattern to non-block devices, with monotone scope narrowing and per-access-class rights.
6. **`inputd` is extracted** from vtmgr per the documented SOLID contract, becoming the input driver that registers with devmgr and serves `/dev/input/*`.

## Constraints

- `no_std` + `alloc` (kernel and userspace).
- No `as any`/`unwrap`/`panic` in new code (rust-best-practices skill).
- **No new syscalls** (AGENTS.md §2). New authority flows through `InvokeOp` variants on the existing token-dispatch path.
- **No runtime ACL** (§3). Device visibility is the VFS view, decided at spawn. No per-open or per-read permission check.
- **Root session godmode stays root-bound** (§6). Root's view includes all devices; this is the only sanctioned escape hatch.
- **Single-threaded servers.** devmgr and VFS use the async runtime (`libcluu::async_runtime`) for any IPC-bound device op.
- **Minimal kernel churn.** The kernel is near freeze. New cap type must be clean and small — one new `ObjectRef` variant, a few new `Rights` bits, one new `InvokeOp` arm family.
- Match repo style: `Result<T>`, `debug_print`, explicit `alloc`.

## Stopping Condition

All 6 destination items implemented; kernel boots; harness passes existing cases plus new cases for `/dev/input/*` reads, dynamic `/dev` enumeration, and session-scoped device visibility. `cat /dev/input/mouse` returns events in a session whose view grants it. A session without the `/dev/input` mount gets `NotFound`. Root session sees all `/dev` nodes. devmgr unit tests cover registration, cap minting, and envelope-scoped device listing.

---

## Current State (from 5 explore agents)

### devmgr — block-only, 201 lines, one-device ceiling

- Source: `userspace/devmgr/src/main.rs` (single file, 201 lines).
- Three IPC labels: `DEVMGR_REGISTER_LABEL` (0x500), `DEVMGR_GRANT_REGION_LABEL` (0x501), `DEVMGR_REVOKE_LABEL` (0x502). All block-specific.
- Holds `BTreeMap<u32, DeviceEntry { total_sectors, root_token }>`. Only `device_id=0` has a real root token (boot-granted); other IDs get `root_token=0` and `GRANT_REGION` returns `Error::NotFound`. Comment at lines 100-107: "Other devices get root_token = 0 until a kernel-side mint path exists for per-device root tokens."
- `handle_grant_region` hardcodes rights to `READ|WRITE` (line 148). No read-only path.
- **Not on the async runtime** — plain blocking `ipc_recv_any_with_sender` loop. AGENTS.md §7 lists devmgr under "future devmgr use it."
- Primordial (priority 188); death = init panic.

### BlockRegion cap — DISABLED (dormant infrastructure, future work)

- `ObjectRef::BlockRegion { device_id: u32, start_sector: u64, sector_count: u64 }` — type tag `0x0A` (`kernel/src/token/scope.rs:179-186`). Variant, `TokenDeriveScoped` bounds check, and userspace helpers (`token_get_info_block_region`, `verify_block_region`) remain in-tree.
- **Not minted at boot.** Root token is 0; devmgr's `GRANT_REGION` returns `NotFound` gracefully. Procmgr's `mint_session_block_region` call degrades to `None`.
- **No driver verifies tokens at the I/O boundary.** virtio-blk services any request that reaches its endpoint.
- **Why disabled:** VFS-level isolation (`VfsViewManager` + per-user views) is the active isolation mechanism and matches the §2/§3 philosophy (no runtime ACL, visibility declarative via caps). Block-level isolation is belt-and-suspenders, not required. Re-enable by uncommenting the mint in `bootstrap.rs` and calling `verify_block_region` in virtio-blk if defense-in-depth is ever needed.

### VFS `/dev` — hardcoded, 4-place edit per device

- `DeviceBackend` in `userspace/vfs/src/mount.rs:687-800`. `open` (lines 721-765) is a path → `DeviceType` match. `readdir` (lines 790-793) returns a static name array. These two must stay in sync manually.
- Hardcoded nodes: `null`, `zero`, `urandom`, `random`, `tty0..tty4`, `console`, `fb0`. No `/dev/input`, `/dev/mouse`, `/dev/kbd`, `/dev/disk`, `/dev/mem`, `/dev/cpu`, `/dev/acpi`.
- `DeviceType` enum (`userspace/vfs/src/fd_table.rs:44-72`). Every new device needs: a new enum variant + arms in 3 read paths (`read_file_chunk` main.rs:3700, `read_grant_device` main.rs:4584, async tty read main.rs:3573/3940) + 1 write path (`handle_write` main.rs:2613).
- `/dev/pts` is the **only** dynamic `/dev` subtree. `PtsBackend` (`userspace/vfs/src/pts.rs`) + `PtsRegistry` (heap-allocated, raw pointer to VfsServer) is the proven dynamic pattern. Registration via `PTS_REGISTER_LABEL` / `VFS_REGISTER_PTS_LABEL`; per-session overlay via `PtsBackend::for_session(sid)`.
- `fb0` is geometry-only: reads return a 40-byte header (magic, width, height, pitch, bpp, size, phys). No pixel data. Real framebuffer access is `framebuffer_acquire()` (separate kernel op).
- **Dead code:** `DeviceBackend::set_tty_endpoint` (mount.rs:704) is never called. `DeviceBackend.tty_endpoints` stays `[0;4]`. Live tty endpoints live on `VfsServer.tty_endpoints`, populated by registry grants (main.rs:1476).
- **Session-VFS `/dev/tty*` are broken:** session-VFS skips tty registry subscriptions (main.rs:375 gated by `!is_session`), so `tty_endpoints` stays `[0;4]` and tty opens resolve to endpoint 0.

### Input — IPC-only, no `/dev` node, vtmgr is routing oracle

- `kbd` (IRQ1) and `mouse` (IRQ12) are pure IPC forwarders. Both attach IRQ via `irq_attach`, decode raw bytes, forward decoded events to `vtmgr:input`.
- `vtmgr` is the input-routing oracle. `InputRouter` (`userspace/vtmgr/src/input_routing.rs:19`) holds `active: RoutingTargetKind` and forwards messages unmodified to `compositor:input` (VT4) or `tty:N:main` (VT0-3).
- `compositor` is the focus oracle within VT4. `forward_input_event` (`compositor/src/window_mgr.rs:402-419`) repackages as `COMP_INPUT_FORWARD_LABEL` to the focused window's `input_endpoint`.
- `cluuterm` is the terminal consumer (`cluuterm/src/input.rs:29-57`).
- **No `/dev/input/*` path exists.** Ordinary binaries cannot read input via `/dev`. The only input path is the kbd → vtmgr → {compositor|tty} → cluuterm chain.
- **Wire-format quirk:** `KBD_EVENT_LABEL` (value 1) is reused for two layouts: kernel→kbd raw scancode in `words[0]` (`kernel/src/devices/irq.rs:74`), and kbd→vtmgr decoded event with `words[0]=0`, scancode in `words[3]` (`kbd/src/protocol.rs:37-50`). Same label, two meanings.
- **Documented `inputd` extraction:** `docs/superpowers/designs/2026-05-13-input-routing-single-source.md` §7 defines the lift path: copy `input_routing.rs` verbatim, change registry name from `vtmgr:input` to `inputd:input`. Zero label changes, zero protocol changes.

### VFS views — declarative, no runtime ACL

- `VfsViewTable` (`userspace/vfs/src/view.rs:59`) stores views keyed by `client_tid`. `check_path_with_target` (view.rs:155) is called by every VFS op — returns `NotFound` when a path matches no mount rule (indistinguishable from "doesn't exist").
- `VFS_SET_VIEW_LABEL = 21` (`libcluu/src/ipc.rs:95`). Handler at `vfs/src/main.rs:1488`. Requires a view-mgr cap (type tag `0x09`) verified via `token_get_info` — this is a capability presentation, not an ACL query.
- `VIEW_SCOPE_*` bits (vfs/src/main.rs:127-134): `ROOT=0x01`, `DEV=0x02`, `VAR_IMAGES=0x04`, `HOME=0x08`, `TMP=0x10`.
- Profile → mount tables (`libcluu/src/vfs_view.rs`):
  - `USER_MOUNTS`: `/bin /lib /etc /tmp /home/root /dev/initrd /dev/pts /proc`
  - `DEVICE_MOUNTS`: `/bin /lib /dev /etc /tmp /dev/initrd /proc` (`/dev` writable, no `/home`, no `/dev/pts`)
  - `SUPERVISOR_MOUNTS`: `[("/", "/", true)]` (full root rw — godmode)
- Session scoping = (a) sid-scoped view-mgr cap (kernel-enforced, sub-minted with `VIEW_SCOPE_ROOT|VIEW_SCOPE_DEV` + sid at `root-procmgr/src/main.rs:4353`), (b) `PtsBackend::for_session(sid)`, (c) `PARAM_SESSION_VFS_EP` registry short-circuit (`libcluu/src/registry.rs:99`).
- Mount policy (`MOUNT /tmp inherit|private|readwrite|ro`) composes parent/child MemFs ownership at spawn via `memfs_cid` in the `ViewMount`. Declarative, encoded once, no I/O-time evaluation.

### Capability system — 47 InvokeOps, PCI_ACCESS conflation, no MMIO cap

- `Token { scope: OpaqueScope (128-bit), role: Rights (u32), issuer, expire_at, signature }`. HMAC-SHA256 over `scope||role||issuer||expire_at` with kernel secret (`kernel/src/token/signature.rs:36-54`). Userspace sees only `TokenHandle(usize)`.
- `ObjectRef` enum (`kernel/src/token/scope.rs:162-187`): 10 variants — Thread, Space, Endpoint, Irq, Clock, Frame, Notification, VfsViewManager, BlockRegion. No char-device, input-device, or general device variant.
- `Rights` bits (`kernel/src/token/rights.rs:39-112`): 21 bits used. `PCI_ACCESS` (bit 30) gates PCI config AND I/O ports — a conflation. No `DEVICE_IO`, `DEVICE_MMIO`, `DEVICE_DMA` right. Free bits: 10-15, 19-23, 31.
- `InvokeOp` enum (`kernel/src/token/mod.rs:368-457`): **47 variants** (not ~70 as AGENTS.md §2 claims — that number is stale). Device-relevant: `IrqAttach/Ack`, `PciConfigRead/Write`, `PortIn8/16/32`, `PortOut8/16/32`, `VirtToPhys`, `PmmAllocLarge`, `FrameAllocate/Free/GetPhys`.
- **No MMIO region cap.** Any space-holding token with `SPACE_MAP` can map any physical address as device memory (`MAP_DEVICE`/`MAP_DEVICE_WC` flags). No per-region device-memory authority.
- Pattern to add a cap scope: add `ObjectRef` variant + `ObjectType` + `check_object_type`/`resolve_token_object` arms + `invoke_token_get_info` arm + (optional) `invoke_token_derive_scoped` arm + userspace wrappers. No `syscall/mod.rs` changes.

---

## Design Decisions

### Decision 1: General devmgr (all device types), not block-only

**Decision:** devmgr becomes the general device registry and capability broker for all device classes.

**Rationale:**
- Matches the §2 pattern: procmgr brokers process authority, devmgr brokers device authority. One broker per resource class.
- `BlockRegion` is already the proven per-device cap pattern. Generalizing it to `DeviceRegion` is a small, mechanical kernel change (one new `ObjectRef` variant, same scoped-derivation discipline).
- A single registry lets VFS enumerate `/dev` dynamically without hardcoded device-specific knowledge. VFS stops being a device list; it becomes a device **frontend**.
- Avoids a second device service. A separate char-device broker would duplicate devmgr's registration, cap-minting, and lifecycle logic.

**devmgr's expanded role:**
1. **Registry:** drivers register devices at boot (block, char, input, framebuffer). devmgr stores `BTreeMap<DeviceId, DeviceEntry>` where `DeviceEntry` carries class, driver endpoint, region info, and the root cap for that device.
2. **Cap broker:** devmgr holds root `DeviceRegion` caps (minted by kernel at boot) and derives scoped sub-caps for clients at spawn time, exactly as it does today for `BlockRegion`.
3. **Enumeration oracle:** procmgr queries devmgr at spawn time — "which `/dev` nodes does this envelope's profile/Cluufile grant?" — and installs the corresponding VFS view. This is the bridge between the registry and the declarative visibility model.
4. **VFS backend:** VFS queries devmgr's registry to enumerate `/dev` and to resolve `/dev` node opens to driver endpoints.

### Decision 2: Dynamic `/dev`, declarative-at-spawn visibility

**Decision:** `/dev` is dynamic (devmgr-backed enumeration in VFS), but device visibility is still decided at spawn via VFS views. No runtime ACL.

**Mechanism:**
- devmgr maintains the live device registry. VFS's `DeviceBackend` is replaced by a `DevRegistryBackend` that queries devmgr (via a shared registry structure, mirroring the `PtsRegistry` pattern) for `/dev` enumeration and open dispatch.
- At spawn, root-procmgr (or session-procmgr for session children) queries devmgr: `DEVMGR_LIST_FOR_ENVELOPE(profile, cluufile_devices)`. devmgr returns the list of `(path, device_id, rights)` the envelope allows. procmgr installs a VFS view with exactly those `/dev` paths mounted.
- VFS enforces by path-rewrite failure (`NotFound`), same as today. A session without `/dev/input` in its view literally cannot name the path — it's invisible, not "denied."
- **Hotplug:** new devices appear in devmgr's registry, but only become visible to a session via an explicit view update (session-procmgr can install a new view on its children, or the session can be restarted). No auto-injection into live views. This is the conservative, §3-compliant answer — it mirrors seL4's "cap distribution is explicit" model. For a hobby OS where devices are known at boot, static-at-spawn + explicit-view-update is sufficient.

**Why not fully dynamic (devices auto-appear in all sessions)?**
- That would require a runtime check ("is this session allowed to see this new device?") — a runtime ACL, violating §3.
- The spawn-time view is the policy. If a device arrives after spawn, the policy hasn't been decided for it yet. An explicit view update is the declarative way to decide.

### Decision 3: Hybrid input model — `/dev/input/*` nodes + preserved fast path

**Decision:** `/dev/input/kbd` and `/dev/input/mouse` exist as VFS nodes backed by `inputd` via the async VFS runtime. The vtmgr → compositor fast path is preserved.

**Layering:**
```
hardware IRQ
    │
    ▼
inputd (extracted from vtmgr, per §7 of input-routing design)
    │  raw event decode (kbd scancode, mouse packet)
    │  registers with devmgr as input devices
    │  serves /dev/input/kbd, /dev/input/mouse reads via async VFS backend
    │  ALSO publishes inputd:input for the fast path
    │
    ├──[read(2) on /dev/input/kbd]──> VFS async backend ──> inputd ──> InputEvent bytes
    │                                  (slow path: ordinary binaries with the right view)
    │
    └──[inputd:input broadcast]──> vtmgr (routing oracle) ──> compositor:input / tty:N
                                   (fast path: VT-aware routing, unchanged from today)
```

**Why hybrid (not pure file, not pure IPC)?**
- **Not pure file (a):** the driver is a separate process; reads cross IPC anyway. A pure file abstraction would just be IPC behind a file interface — which is exactly the hybrid.
- **Not pure IPC (b, current):** violates Unix expectations; ordinary binaries can't access input; no composability; a test tool or game can't read the mouse.
- **Hybrid (c):** `/dev/input/*` nodes exist for ordinary access (read(2) returns `InputEvent` bytes via async VFS backend → inputd IPC). The fast path (inputd → vtmgr → compositor) is preserved for latency-sensitive GUI input. Both paths coexist; the fast path doesn't go through VFS.

**inputd's dual role:**
- **Device server:** registers `/dev/input/kbd` and `/dev/input/mouse` with devmgr. VFS opens forward to inputd via `DeviceRegion` cap + async backend. Reads return serialized `InputEvent` structs.
- **Fast-path publisher:** publishes `inputd:input` via the registry (renamed from `vtmgr:input`). vtmgr subscribes and routes to compositor/tty, unchanged.

**Event unification:** introduce a shared `InputEvent` enum in `libcluu/src/input.rs`:
```rust
pub enum InputEvent {
    Key { ascii: Option<u8>, scancode: u8, modifiers: u8, extended: u8 },
    Mouse { dx: i32, dy: i32, buttons: u8 },
}
```
This replaces the dual-format `KBD_EVENT_LABEL` quirk. The `/dev/input/*` read path returns postcard-encoded `InputEvent` bytes. The fast path (inputd → vtmgr) uses the same struct over IPC.

### Decision 4: Session-scoped device visibility via spawn-time views

**Decision:** Which `/dev` nodes a session sees is decided at spawn via the VFS view, using the existing `VIEW_SCOPE_DEV` mechanism. No runtime check.

**Per-session `/dev` content (decided at spawn):**
| Session type | `/dev` content |
|---|---|
| Login session (USER) | `/dev/null`, `/dev/zero`, `/dev/urandom`, `/dev/pts` (session-scoped), `/dev/tty` (controlling tty if any) |
| Login session (ADMIN) | USER set + `/dev/console` |
| Root session (§6 godmode) | ALL devices (devmgr returns the full registry) |
| Device-driver container | `/dev` writable, device-specific nodes per Cluufile `DEVICE` declarations |
| System service | per Cluufile `DEVICE` declarations only |

**Enforcement:** the VFS view. `check_path_with_target` returns `NotFound` for any `/dev` path not in the view. This is the existing mechanism — no new enforcement code.

**The new piece:** devmgr must support `DEVMGR_LIST_FOR_ENVELOPE` — a query that procmgr calls at spawn to get the visible device set. devmgr filters based on:
1. Profile (USER/ADMIN/SUPERVISOR/DEVICE) → baseline device set.
2. Cluufile `DEVICE` declarations → explicit additions (e.g., `DEVICE /dev/input/mouse`).
3. Session scope → root session gets all; non-root gets only what the profile + Cluufile grant.

This is a spawn-time query, not a runtime check. The result is encoded in the `ViewMountList` and never re-evaluated.

### Decision 5: New `DeviceRegion` cap type (approach B from cap analysis)

**Decision:** Add `ObjectRef::DeviceRegion { device_id, region_kind, base, len }` + new `Rights` bits (`DEVICE_IO`, `DEVICE_MMIO`, `DEVICE_DMA`, `DEVICE_CONFIG`) + scoped derivation mirroring `BlockRegion`.

**Why approach B (full rights + scope matrix) over approach A (minimal new scope):**
- Approach A (one new scope, reuse existing ops) leaves the `PCI_ACCESS` conflation and the MMIO hole open. Any space-holding token can still map any physical address as device memory.
- Approach B is closer to the seL4 model CLUU imitates: authority = scope (which region) × rights (which access class), minted once at spawn, narrowed by derivation, kernel-enforced.
- The kernel churn is bounded: one `ObjectRef` variant, four `Rights` bits, one `invoke_token_derive_scoped` arm, one `invoke_token_get_info` arm. No new syscalls.

**New cap structure:**
```rust
// kernel/src/token/scope.rs
ObjectRef::DeviceRegion {
    device_id: u32,       // which device (0 = all-devices sentinel for root)
    region_kind: u8,      // 0=block, 1=char, 2=input, 3=framebuffer, 4=mmio, 5=ioport
    base: u64,            // region start (sector for block, addr for mmio, 0 for char/input)
    len: u64,             // region length (sector_count for block, size for mmio, 0 for char/input)
}  // type tag 0x0B
```

**New rights bits** (free slots 10-13):
```rust
// kernel/src/token/rights.rs
pub const DEVICE_IO: Rights = Rights(1 << 10);      // I/O port access
pub const DEVICE_MMIO: Rights = Rights(1 << 11);    // MMIO region mapping
pub const DEVICE_DMA: Rights = Rights(1 << 12);     // DMA bus-master
pub const DEVICE_CONFIG: Rights = Rights(1 << 13);  // PCI config space access
```

**Scoped derivation** (monotone, like BlockRegion):
- `child.base ≥ parent.base`, `child.base + child.len ≤ parent.base + parent.len`.
- `device_id` inherited from parent (cannot retarget).
- `region_kind` inherited (cannot change class).
- Rights narrowed via standard derive (subset only).

**Root mint at boot** (`kernel/src/bootstrap.rs`): one root `DeviceRegion` per device class discovered by the kernel, OR a single all-devices root that devmgr sub-mints per class. The latter is simpler — one root token, devmgr derives per-device + per-class children.

**Enforcement:** this is the hardening that closes the BlockRegion gap. `invoke_port_*` handlers gain a `DeviceRegion` scope check (the presented token must name the device + region). `invoke_space_map` with `MAP_DEVICE` gains a `DEVICE_MMIO` right + scope check. This is **deferred to a later hardening phase** (see Scope vs Deferred) — the initial redesign ships the cap type and devmgr minting, but does not wire kernel-side enforcement into every port/MMIO op. The cap exists as authority metadata; full enforcement is a separate, auditable change.

---

## seL4 / Fuchsia Comparison

| Aspect | seL4 | Fuchsia | CLUU (this design) |
|---|---|---|---|
| Device abstraction | None at kernel level. Device = cap to MMIO region + cap to IRQ. | `devfs` at `/dev`, dynamic. Devices in `devhost` processes. | `/dev` dynamic, devmgr-backed. Devices in driver processes. |
| Authority model | Capability to device memory (Untyped/frame cap to MMIO phys) + IRQ cap. No `/dev`. | Zircon handle to directory (`/dev` or subdirectory). Namespace = authority. | Capability token (`DeviceRegion`) + VFS view. View = authority. |
| Visibility scoping | Cap distribution at boot (initial thread's CSpace). No runtime ACL. | Component namespace — each component gets only the directories it declared (`use` manifest). Decided at component start. | VFS view at spawn. `VIEW_SCOPE_DEV` + sid-scoped view-mgr cap. Decided at spawn. No runtime ACL. |
| Enumeration | None — caps are pre-distributed. Bootloader/platform hands untypeds to initial task. | Dynamic — `devcoordinator` discovers devices, drivers bind, `/dev` populates. | Dynamic — drivers register with devmgr, VFS enumerates from registry. But visibility per-session is spawn-time. |
| Input | Pure IPC — driver holds IRQ cap, clients hold endpoint caps. | `/dev/class/input/*` event nodes + Scenic input pipeline. | Hybrid — `/dev/input/*` nodes + `inputd:input` fast path. |
| Hotplug | Explicit cap minting — no auto-appearance. | Dynamic — devices appear in `/dev`, components see them if in namespace. | Explicit view update — devices appear in devmgr, but session visibility requires view update (§3 compliance). |

**CLUU is closer to Fuchsia** (VFS views = Fuchsia namespaces; dynamic `/dev` = Fuchsia devfs; devmgr = Fuchsia devcoordinator). But CLUU's §3 (no runtime ACL) is **stricter** than Fuchsia (which has some runtime policy in the component framework) and matches seL4's cap-only model. The device design preserves this: authority is the view + the cap, both decided at spawn, neither re-evaluated at access time.

**Key difference from seL4:** CLUU has a VFS layer, so `/dev` makes sense. seL4 has no filesystem at the kernel level — `/dev` would be a pure userspace construct. CLUU's `/dev` is a userspace construct too (VFS is a userspace service), but it's the canonical path-based interface, backed by caps.

---

## Architecture (Target State)

```
┌────────────────── KERNEL ──────────────────┐
│ ObjectRef::BlockRegion (existing)           │
│ ObjectRef::DeviceRegion (NEW, tag 0x0B)     │
│ Rights::DEVICE_IO/MMIO/DMA/CONFIG (NEW)     │
│ invoke_token_derive_scoped: DeviceRegion arm│
│ Boot: mint root DeviceRegion → devmgr       │
└───────────────────┬────────────────────────┘
                    │ TOKEN_EXTRA_1
                    ▼
┌────────────────── devmgr (general) ──────────────────┐
│ BTreeMap<DeviceId, DeviceEntry {                       │
│   class: Block|Char|Input|Framebuffer,                 │
│   driver_endpoint: usize,                              │
│   root_cap: TokenHandle,                               │
│   path: String,  // "/dev/disk/0", "/dev/input/kbd"    │
│ }>                                                     │
│                                                        │
│ IPC labels:                                            │
│   DEVMGR_REGISTER_LABEL (0x500) — block (existing)     │
│   DEVMGR_REGISTER_CHAR (0x510) — char/input/fb (NEW)   │
│   DEVMGR_GRANT_REGION (0x501) — block cap derive       │
│   DEVMGR_GRANT_DEVICE (0x511) — general cap derive(NEW)│
│   DEVMGR_LIST_FOR_ENVELOPE (0x512) — spawn query (NEW) │
│   DEVMGR_REVOKE (0x502) — revoke (existing)            │
│                                                        │
│ Async runtime (§7 — devmgr joins VFS/procmgr)          │
└─────┬──────────────────────────────┬───────────────────┘
      │ DevRegistry (shared)         │ DEVMGR_LIST_FOR_ENVELOPE
      ▼                              ▼
┌─────────── VFS ───────────┐  ┌────────── procmgr ──────────┐
│ DevRegistryBackend (NEW)  │  │ At spawn:                    │
│   replaces DeviceBackend  │  │   query devmgr for visible   │
│   queries DevRegistry     │  │   /dev set for profile +     │
│   for /dev enumeration    │  │   Cluufile DEVICE decls      │
│   + open dispatch         │  │   install VFS view with      │
│                           │  │   exactly those /dev paths   │
│ /dev/null,zero,urandom    │  │   (VIEW_SCOPE_DEV)           │
│   (in-process, sync)      │  └──────────────────────────────┘
│ /dev/pts (PtsBackend)     │
│ /dev/tty* (async → tty)   │
│ /dev/fb0 (geometry)       │
│ /dev/input/* (async → inputd) NEW
│ /dev/disk/* (async → virtio-blk) NEW
└───────────────────────────┘

┌─────────── inputd (extracted from vtmgr) ────────────┐
│ Owns: IRQ1 (kbd), IRQ12 (mouse), raw decode           │
│ Registers with devmgr: /dev/input/kbd, /dev/input/mouse│
│ Serves: /dev/input/* reads via async VFS backend       │
│ Publishes: inputd:input (renamed from vtmgr:input)    │
└─────────────────────┬─────────────────────────────────┘
                      │ inputd:input
                      ▼
┌─────────── vtmgr (shrunk) ────────────┐
│ VT lifecycle + input routing oracle   │
│ Subscribes to inputd:input            │
│ Routes to compositor:input / tty:N    │
│ (unchanged from today, new reg name)  │
└───────────────────────────────────────┘
```

---

## Changes Needed

### Kernel (minimal, near-freeze-safe)

| File | Change |
|---|---|
| `kernel/src/token/scope.rs:163` | Add `ObjectRef::DeviceRegion { device_id, region_kind, base, len }` variant. Add type tag `0x0B` in `from_object_ref` (mirror BlockRegion at lines 108-113). |
| `kernel/src/token/rights.rs` | Add `DEVICE_IO` (bit 10), `DEVICE_MMIO` (bit 11), `DEVICE_DMA` (bit 12), `DEVICE_CONFIG` (bit 13). Add Debug arms. Add `device_full()` constructor. |
| `kernel/src/token/table.rs:722` | Add `ObjectType::DeviceRegion`. Add arms to `check_object_type` + `resolve_token_object` (mirror BlockRegion). |
| `kernel/src/syscall/handlers.rs:2748` | Add `DeviceRegion` arm to `invoke_token_get_info` (pack device_id, region_kind, base, len into return usize). |
| `kernel/src/syscall/handlers.rs:2785` | Add `DeviceRegion` arm to `invoke_token_derive_scoped` — monotone bounds narrowing (mirror BlockRegion at 2802-2820). |
| `kernel/src/bootstrap.rs:188` | Mint root `DeviceRegion` token (all-devices sentinel, `device_id=0`, full span, rights `READ|WRITE|GRANT|DEVICE_*`). Hand to devmgr via `TOKEN_EXTRA_1` (alongside or replacing the BlockRegion root — BlockRegion can be derived from a DeviceRegion with `region_kind=0`, OR keep both for backward compat). |
| `userspace/libcluu/src/syscall.rs` | Add `token_get_info_device_region` + `token_derive_scoped_device_region` + `verify_device_region` wrappers (mirror BlockRegion wrappers at lines 1111-1159). |

**Deferred kernel hardening** (not in this plan, separate audit):
- `invoke_port_*` handlers gain `DeviceRegion` scope check.
- `invoke_space_map` with `MAP_DEVICE` gains `DEVICE_MMIO` right + scope check.
- `virtio-blk` gains `verify_block_region` / `verify_device_region` call at I/O boundary.

### devmgr (expand from block-only to general)

| File | Change |
|---|---|
| `userspace/devmgr/src/main.rs` | Expand `DeviceEntry` to carry `class: DeviceClass`, `driver_endpoint`, `path: String`. Replace `BTreeMap<u32, DeviceEntry>` with `BTreeMap<DeviceId, DeviceEntry>`. |
| `userspace/devmgr/src/main.rs` | Add `DEVMGR_REGISTER_CHAR` (0x510) handler — char/input/fb drivers register with path + endpoint + region info. devmgr stores entry, assigns `DeviceId`. |
| `userspace/devmgr/src/main.rs` | Add `DEVMGR_GRANT_DEVICE` (0x511) handler — VFS or procmgr requests a scoped `DeviceRegion` cap for a specific device + rights. devmgr derives from root. |
| `userspace/devmgr/src/main.rs` | Add `DEVMGR_LIST_FOR_ENVELOPE` (0x512) handler — procmgr queries visible devices for a profile + Cluufile DEVICE list. Returns postcard-encoded `Vec<(path, device_id, rights)>`. |
| `userspace/devmgr/src/main.rs` | **Move to async runtime.** Convert `ipc_recv_any_with_sender` loop to async `Runtime` + completion queue (mirror VFS main loop at `vfs/src/main.rs:348`). This is the "future devmgr" from §7. |
| `userspace/devmgr/src/registry.rs` (NEW) | `DevRegistry` struct — shared device table, queryable by VFS. Mirror `PtsRegistry` pattern (heap-allocated, raw pointer handed to VFS backend). |
| `userspace/libcluu/src/ipc.rs` | Add new label constants: `DEVMGR_REGISTER_CHAR=0x510`, `DEVMGR_GRANT_DEVICE=0x511`, `DEVMGR_LIST_FOR_ENVELOPE=0x512`. |

### VFS (replace hardcoded `/dev` with dynamic backend)

| File | Change |
|---|---|
| `userspace/vfs/src/mount.rs:687` | Replace `DeviceBackend` with `DevRegistryBackend`. `open` queries `DevRegistry` for the path → returns `OpenFile::Device(DeviceFile { device_id, cap, class })`. `readdir` lists registry entries. In-process devices (null/zero/urandom) stay as a fast-path fallback. |
| `userspace/vfs/src/fd_table.rs:44` | Replace `DeviceType` enum with a `(DeviceId, DeviceClass, TokenHandle)` tuple or a trait-object. New devices no longer need an enum variant — dispatch is by `DeviceClass` → async backend → driver IPC. |
| `userspace/vfs/src/main.rs:3700,4584,3573,3940,2613` | Consolidate the 3 read paths + 1 write path into a single async dispatch: `DeviceClass::Block → virtio-blk`, `Char → driver IPC`, `Input → inputd`, `Framebuffer → geometry header`. The async runtime is already wired (`Runtime::new` at main.rs:348). |
| `userspace/vfs/src/main.rs:903` | `DevRegistryBackend` gets `for_session(sid)` treatment like `PtsBackend::for_session` — session-VFS only sees devices in the session's view. |
| `userspace/vfs/src/dev_registry.rs` (NEW) | `DevRegistry` consumer side — receives device list from devmgr (via IPC at boot + incremental updates), exposes query API to `DevRegistryBackend`. Mirror `PtsRegistry` ownership pattern. |
| `userspace/vfs/src/main.rs:375` | Fix session-VFS `/dev/tty*` — either subscribe to tty registry in session mode too (if sessions should see tty), or explicitly document that sessions don't get `/dev/tty*` (current behavior, now intentional). |

### Input (extract inputd, add `/dev/input/*`)

| File | Change |
|---|---|
| `userspace/inputd/` (NEW) | Extract `input_routing.rs` from vtmgr verbatim (per design doc §7). Add raw decode (kbd scancode, mouse packet) — move from kbd/mouse services OR keep kbd/mouse as pure IRQ→inputd forwarders and put decode in inputd. **Chosen:** keep kbd/mouse as thin IRQ forwarders (they already are); inputd does decode + registration + `/dev/input` serving + `inputd:input` publish. |
| `userspace/inputd/src/main.rs` (NEW) | Main loop: recv raw bytes from kbd/mouse via IPC, decode → `InputEvent`, register `/dev/input/kbd` + `/dev/input/mouse` with devmgr, serve read requests via async VFS backend, publish `inputd:input` for vtmgr fast path. |
| `userspace/libcluu/src/input.rs` (NEW) | `InputEvent` enum (`Key { ascii, scancode, modifiers, extended }`, `Mouse { dx, dy, buttons }`). Postcard encode/decode. Shared by `/dev/input/*` read path and `inputd:input` fast path. |
| `userspace/vtmgr/src/input_routing.rs` | Remove (moved to inputd). vtmgr shrinks to VT lifecycle only. vtmgr subscribes to `inputd:input` instead of being the publisher. |
| `userspace/vtmgr/src/context.rs:75` | Change registry output from `input` (self-published) to subscribing to `inputd:input`. |
| `userspace/kbd/src/context.rs:71` | Change subscription from `vtmgr:input` to `inputd:input`. |
| `userspace/mouse/src/context.rs:42` | Change subscription from `vtmgr:input` to `inputd:input`. |
| `userspace/libcluu/src/ipc.rs` | Fix `KBD_EVENT_LABEL` dual-format quirk — split into `KBD_RAW_LABEL` (kernel→driver, raw byte) and `KBD_EVENT_LABEL` (driver→router, decoded event). Or keep `KBD_EVENT_LABEL` for decoded and use a distinct label for raw. |
| `userspace/init/src/services.rs` | Add `inputd` to `PRIMORDIAL_SERVICES` + `SERVICE_LIST`. Spawn order: after kbd/mouse (they forward to inputd), before vtmgr (it subscribes to inputd). |

### procmgr (spawn-time device view construction)

| File | Change |
|---|---|
| `userspace/root-procmgr/src/main.rs:5612` | In `handle_container_run` view-building, after profile defaults, call `DEVMGR_LIST_FOR_ENVELOPE(profile, cluufile_devices)` → get visible `/dev` set → prepend to `ViewMountList` with correct `memfs_cid` (0 = DevRegistryBackend). |
| `userspace/root-procmgr/src/main.rs:4353` | Session-VFS sub-mint already uses `VIEW_SCOPE_DEV` — verify the sid-scoped cap allows the new dynamic `/dev` mount. May need no change (the cap already grants DEV scope). |
| `userspace/session-procmgr/src/elf_spawn.rs:461` | Session children get `PARAM_SESSION_VFS_EP` → their `/dev` resolution goes to session-VFS → `DevRegistryBackend::for_session(sid)`. Verify the session-VFS has the devmgr registry pointer. |
| `userspace/libcluu/src/vfs_view.rs:8` | Update `DEVICE_MOUNTS` — device-driver containers get `/dev` writable, but the specific nodes come from Cluufile `DEVICE` declarations, not a static list. |
| `containers/*/Cluufile` | Add `DEVICE /dev/input/mouse` etc. declarations where needed. Most containers don't need device access. |

### Root session godmode (§6)

| File | Change |
|---|---|
| `userspace/root-procmgr/src/main.rs` | Root session's `DEVMGR_LIST_FOR_ENVELOPE` query returns ALL devices (devmgr recognizes root identity, not a forwardable cap). This is the §6 escape hatch — bound to root, not to a capability. |

### Docs

| File | Change |
|---|---|
| `AGENTS.md §2` | Update "~70 invoke ops" → actual count + note DeviceRegion added. |
| `AGENTS.md §7` | Note devmgr is now on the async runtime. |
| `docs/ARCHITECTURE.md` | Update device model section — devmgr is general registry, `/dev` is dynamic, inputd serves `/dev/input`. |

---

## Phases

### Phase 0: Kernel cap type (no behavior change yet)

Add `ObjectRef::DeviceRegion`, `Rights::DEVICE_*`, `invoke_token_derive_scoped` arm, `invoke_token_get_info` arm, boot root mint, userspace wrappers. No callers yet — the cap type exists but isn't used. All existing tests pass unchanged.

**Test:** kernel unit test for `DeviceRegion` scoped derivation (monotone bounds, device_id inheritance, region_kind inheritance). Mirror the BlockRegion test pattern.

### Phase 1: devmgr general registry + async runtime

Expand devmgr to general registry. Add `DEVMGR_REGISTER_CHAR`, `DEVMGR_GRANT_DEVICE`, `DEVMGR_LIST_FOR_ENVELOPE`. Move to async runtime. Existing block registration still works (unchanged labels). devmgr can now accept char/input/fb registrations but no driver registers yet.

**Test:** devmgr unit test — register a fake char device, query `LIST_FOR_ENVELOPE`, verify it appears. Block devices still register and grant correctly (regression).

### Phase 2: VFS dynamic `/dev` backend

Replace `DeviceBackend` with `DevRegistryBackend`. VFS queries devmgr's registry for `/dev` enumeration. Existing hardcoded devices (null/zero/urandom) become in-process fast-path entries in the backend. tty/fb/pts unchanged. `DeviceType` enum replaced with `(DeviceId, DeviceClass, TokenHandle)`.

**Test:** harness case — `ls /dev` shows the same nodes as before (regression). `cat /dev/null` still works. `cat /dev/zero | head` still works. `ls /dev/pts` still works.

### Phase 3: inputd extraction + `/dev/input/*`

Extract `inputd` from vtmgr. kbd/mouse forward to `inputd:input`. inputd registers `/dev/input/kbd` + `/dev/input/mouse` with devmgr. VFS serves reads via async backend → inputd IPC. vtmgr subscribes to `inputd:input` (renamed from `vtmgr:input`). Fix `KBD_EVENT_LABEL` dual-format quirk.

**Test:** harness case — `cat /dev/input/mouse` in a session with the right view returns mouse event bytes when the mouse moves. VT switching still works (Ctrl+Alt+F1..F5). Compositor still receives input (fast path preserved). `top` still works during input.

### Phase 4: procmgr spawn-time device views

root-procmgr calls `DEVMGR_LIST_FOR_ENVELOPE` at spawn, installs `/dev` view. Cluufile `DEVICE` declarations parsed. Session-VFS gets `DevRegistryBackend::for_session(sid)`. Root session godmode sees all devices.

**Test:** harness case — a USER session `ls /dev` shows only `null zero urandom pts tty` (no `input`, no `disk`, no `fb0`). A session with `DEVICE /dev/input/mouse` in its Cluufile sees `/dev/input/mouse`. Root session `ls /dev` shows everything. A session without `/dev/input` in its view gets `NotFound` on `cat /dev/input/mouse`.

### Phase 5: Docs + cleanup

Update AGENTS.md §2 (invoke op count), §7 (devmgr async), ARCHITECTURE.md (device model). Remove dead `DeviceBackend::set_tty_endpoint`. Consolidate tty endpoint source-of-truth. Update `docs/superpowers/designs/2026-05-13-input-routing-single-source.md` — mark §7 extraction as done.

**Test:** full harness suite passes. `cargo xtask build` clean. No new clippy warnings.

---

## Scope vs Deferred

### In scope

- General devmgr registry (block + char + input + framebuffer).
- `DeviceRegion` cap type (kernel + userspace wrappers).
- Dynamic `/dev` in VFS (devmgr-backed `DevRegistryBackend`).
- `/dev/input/kbd` + `/dev/input/mouse` nodes via inputd.
- `inputd` extraction from vtmgr (per documented SOLID contract).
- Session-scoped `/dev` visibility via spawn-time views.
- Cluufile `DEVICE` declarations.
- Root session godmode → all devices.
- Fix `KBD_EVENT_LABEL` dual-format quirk.
- Fix dead `DeviceBackend::set_tty_endpoint`.
- devmgr on async runtime.

### Deferred (explicitly out of scope)

- **Kernel-side DeviceRegion enforcement** on `invoke_port_*` / `invoke_space_map` — the cap exists as authority metadata but port/MMIO ops don't yet check it. Separate hardening audit.
- **virtio-blk BlockRegion verification** at I/O boundary — the existing gap. Separate fix.
- **Hotplug notifications** — static-at-spawn + explicit view-update for now. Devices don't auto-appear in live sessions.
- **`/dev/mem`, `/dev/cpu`, `/dev/acpi`** — not needed for a hobby OS.
- **DMA bus-master cap enforcement** — current `SPACE_MAP` + `VirtToPhys` + `FrameAllocate` path works; hardening is separate.
- **Driver crash recovery / device hot-replug** — out of scope.
- **USB device tree** — out of scope.
- **Per-device ioctl whitelisting** — out of scope.
- **MMIO region cap enforcement in kernel** — any space token can still map any phys as device memory. Separate hardening task.
- **Multiple block devices** — devmgr's one-device ceiling (only `device_id=0` has a root token) requires a kernel per-device root mint path. Separate task.
- **Per-session `/dev/tty*` fix** — session-VFS tty endpoints are broken today (no registry subscriptions in session mode). Decide: fix (sessions get tty) or document (sessions don't get tty). Either way, separate from this redesign.

---

## Risks

- **Risk:** devmgr async migration introduces deadlock regressions.
  **Mitigation:** devmgr is primordial (death = panic). Test under load — `top` during spawn, `cat /dev/input/mouse` during VFS load. The async runtime is proven in VFS; follow the same pattern.

- **Risk:** `DevRegistryBackend` raw-pointer-to-VfsServer pattern (mirroring `PtsRegistry`) is `unsafe Send+Sync`.
  **Mitigation:** single-threaded VFS — the unsafety is justified by the same argument as `PtsBackend`. Document the invariant. No new threading.

- **Risk:** inputd extraction breaks the input fast path (latency regression for compositor).
  **Mitigation:** the fast path is inputd → vtmgr → compositor, same hop count as today (kbd → vtmgr → compositor becomes inputd → vtmgr → compositor; kbd → inputd is the same as kbd → vtmgr). Measure input latency before/after with a harness marker.

- **Risk:** `DEVMGR_LIST_FOR_ENVELOPE` at spawn adds latency to every spawn.
  **Mitigation:** devmgr query is a single IPC round-trip. Cache the result per-profile if it becomes measurable. Most spawns don't need device access (empty device list → fast path).

- **Risk:** `DeviceType` enum removal is a large refactor touching 4 dispatch paths in VFS main.rs.
  **Mitigation:** Phase 2 is isolated to VFS. Lock behavior with regression tests first (harness cases for null/zero/urandom/pts/tty/fb0), then refactor. The async dispatch consolidation reduces 4 paths to 1, which is a net simplification.

- **Risk:** `DeviceRegion` cap type added but not enforced — "caps exist but nobody checks" repeats the BlockRegion gap.
  **Mitigation:** explicitly deferred (see Scope vs Deferred). The cap type is authority metadata for devmgr minting and view construction. Kernel enforcement is a separate, auditable change that closes the gap for both BlockRegion and DeviceRegion together.

- **Risk:** Cluufile `DEVICE` declarations add complexity to manifest parsing.
  **Mitigation:** `DEVICE /dev/input/mouse` is one line per device. Most containers have zero `DEVICE` lines. Parse in the existing `cluufile_mount_policies` path (`root-procmgr/src/main.rs:5433`).

---

## QA / Acceptance Criteria (all agent-executable)

### Phase 0 — Kernel cap type
- **Test:** `rustc --edition 2021 --test kernel/src/token/scope.rs -o /tmp/t && /tmp/t` — DeviceRegion scoped derivation: child bounds within parent, device_id inherited, region_kind inherited, rights narrowed. Expected: all assertions pass.
- **Test:** `cargo xtask build` — clean build, no warnings.

### Phase 1 — devmgr general registry
- **Test:** devmgr unit test — register fake char device via `DEVMGR_REGISTER_CHAR`, query `DEVMGR_LIST_FOR_ENVELOPE(USER_PROFILE, [])`, verify device appears in result. Query with `ADMIN_PROFILE`, verify block devices appear.
- **Test:** regression — `DEVMGR_GRANT_REGION` still works for block devices (existing harness cases pass).

### Phase 2 — VFS dynamic `/dev`
- **Test:** harness `l2_dev_nodes` — `ls /dev` output contains: `null`, `zero`, `urandom`, `pts`, `tty0`, `tty1`, `tty2`, `tty3`, `tty4`, `console`, `fb0`. Exact match (no more, no less — inputd not registered yet).
- **Test:** harness `l2_cat_dev_null` — `cat /dev/null` exits 0, no output.
- **Test:** harness `l2_cat_dev_zero` — `cat /dev/zero | head -c 4 | xxd` returns `00000000`.
- **Test:** harness `l2_pts_basic` — `ls /dev/pts` works, existing PTS cases pass.

### Phase 3 — inputd + `/dev/input/*`
- **Test:** harness `l2_dev_input_mouse` — in a session with `DEVICE /dev/input/mouse` in its Cluufile, `cat /dev/input/mouse &` then send mouse movement via QEMU monitor `sendkey` / `mouse_move` → serial output shows `InputEvent::Mouse` bytes (postcard-encoded: variant tag 1, dx, dy, buttons).
- **Test:** harness `l2_dev_input_kbd` — in a session with `DEVICE /dev/input/kbd`, `cat /dev/input/kbd &` then `sendkey a` → serial output shows `InputEvent::Key` bytes (variant tag 0, ascii='a', scancode, modifiers).
- **Test:** harness `l2_vt_switch_regression` — Ctrl+Alt+F1/F2/F3/F4 still switches VTs (inputd → vtmgr fast path preserved). Serial markers for VT activate/deactivate appear.
- **Test:** harness `l2_compositor_input` — compositor still receives mouse + kbd (GUI fast path). `fbprobe` or compositor demo responds to input.
- **Test:** harness `l2_top_during_input` — `top` runs while mouse moves + keys pressed → no hang, no deadlock.

### Phase 4 — Session-scoped device visibility
- **Test:** harness `l2_session_dev_visibility` — USER session `ls /dev` → output contains `null zero urandom pts tty` and does NOT contain `input disk fb0`. Exact assertion via serial marker.
- **Test:** harness `l2_session_dev_input_denied` — USER session `cat /dev/input/mouse` → `cat: /dev/input/mouse: No such file or directory` (NotFound, not PermissionDenied — indistinguishable from nonexistent).
- **Test:** harness `l2_cluufile_device_grant` — container with `DEVICE /dev/input/mouse` in Cluufile → `cat /dev/input/mouse` succeeds (returns event bytes on input).
- **Test:** harness `l2_root_dev_godmode` — root session `ls /dev` → output contains ALL devices including `input`, `disk`, `fb0`. Root can `cat /dev/input/mouse`.
- **Test:** harness `l2_session_dev_pts_scope` — session A's `/dev/pts` shows only session A's PTS entries, not session B's. Open PTS in session A, `ls /dev/pts` in session B → session A's PTS not listed.

### Phase 5 — Docs + cleanup
- **Test:** `cargo xtask build` — clean, no warnings, no clippy issues.
- **Test:** full harness suite `python -m cluu_harness --no-build` — all cases pass.
- **Test:** `grep -r "set_tty_endpoint" userspace/` → no results (dead code removed).
- **Test:** `grep -r "DeviceType" userspace/vfs/src/` → no results (enum removed, replaced by tuple).

---

## Open Questions (resolve before Phase 3)

1. **inputd decode location:** should kbd/mouse stay as thin IRQ→inputd forwarders (raw bytes over IPC), or should decode move into inputd (kbd/mouse become pure IRQ stubs)? **Lean:** keep kbd/mouse as thin forwarders (they already are); inputd does decode. This centralizes decode logic and makes inputd the single input source.

2. **BlockRegion vs DeviceRegion coexistence:** should BlockRegion become a `DeviceRegion` with `region_kind=0`, or stay a separate type? **Lean:** keep both for now (backward compat). devmgr mints BlockRegion for block devices, DeviceRegion for everything else. A future cleanup can unify them. Adding `region_kind` to DeviceRegion makes unification mechanical later.

3. **Session-VFS `/dev/tty*`:** fix (sessions get controlling tty) or document (sessions don't get tty)? **Lean:** document as intentional — sessions use PTS, not raw VTs. If a session needs a tty, it gets `/dev/tty` (controlling tty) not `/dev/tty0..4` (raw VTs). This matches the "session = PTS-based" model.

4. **DevRegistry push vs pull:** does devmgr push device list updates to VFS (pub-sub), or does VFS pull on each `/dev` enumeration? **Lean:** pull for simplicity (VFS queries devmgr at readdir time, cached). Push is an optimization for hotplug, which is deferred. The `PtsRegistry` uses push (register/unregister IPC) because PTS entries change frequently; devices are more stable.

---

## Directives for Prometheus (the implementer)

### Core Directives
- MUST: Follow the phase order — Phase 0 (kernel cap) before Phase 1 (devmgr) before Phase 2 (VFS) before Phase 3 (inputd) before Phase 4 (procmgr views). Each phase is independently testable.
- MUST: Preserve AGENTS.md §3 (no runtime ACL) — all device visibility is the VFS view, decided at spawn. No per-open or per-read permission check anywhere in the new code.
- MUST: Preserve AGENTS.md §6 (root godmode) — root's `DEVMGR_LIST_FOR_ENVELOPE` returns all devices. This is bound to root identity, not a forwardable cap.
- MUST: Use the async runtime (`libcluu::async_runtime`) for all IPC-bound device ops in devmgr and VFS's `DevRegistryBackend`. Sync `MountBackend` stays for in-process devices (null/zero/urandom).
- MUST: Mirror the `PtsRegistry` + `PtsBackend` pattern for `DevRegistry` + `DevRegistryBackend` — it's the proven dynamic-`/dev`-subtree pattern. Heap-allocated registry, raw pointer to VfsServer, `for_session(sid)` variant.
- MUST: Mirror the `BlockRegion` pattern for `DeviceRegion` — same `invoke_token_derive_scoped` monotone bounds, same `token_get_info` packing, same userspace wrapper shape.
- MUST: Lock behavior with regression tests BEFORE refactoring. Phase 2 (DeviceType removal) and Phase 3 (inputd extraction) are refactors — capture current behavior in harness cases first, then change.
- MUST NOT: Add runtime device permission checks. Visibility = view. Authority = cap. Both decided at spawn.
- MUST NOT: Auto-inject hotplugged devices into live session views. New devices require an explicit view update.
- MUST NOT: Change the `VFS_SET_VIEW` wire format or the view-mgr cap scope model without consulting `docs/superpowers/specs/2026-05-26-vfs-view-cap-delegation.md`.
- MUST NOT: Break the input fast path (inputd → vtmgr → compositor). Measure latency before/after.
- PATTERN: Follow `userspace/vfs/src/pts.rs` (`PtsRegistry` + `PtsBackend`) for `DevRegistry` + `DevRegistryBackend`.
- PATTERN: Follow `kernel/src/token/scope.rs:179-186` (`BlockRegion`) for `DeviceRegion`.
- PATTERN: Follow `userspace/devmgr/src/main.rs:125-189` (`handle_grant_region`) for `handle_grant_device`.
- TOOL: Use `codegraph_explore` before editing any file to see the blast radius.
- TOOL: Use the harness (`python -m cluu_harness`) for all QA. No manual testing.

### QA/Acceptance Criteria Directives
- MUST: Write acceptance criteria as harness cases (Python, QEMU-backed) with exact serial markers — see QA section above for specific case names and assertions.
- MUST: Include exact expected outputs — e.g. `ls /dev` output must match an exact set, not "contains null".
- MUST: Every phase has a regression test that captures pre-change behavior BEFORE the change.
- MUST: QA scenarios include BOTH happy-path (device visible with right view) AND failure/edge-case (device invisible without view → NotFound, not PermissionDenied).
- MUST: QA scenarios use specific data — `sendkey a` not "press a key"; `cat /dev/input/mouse` not "read the mouse".
- MUST NOT: Create criteria requiring "user manually tests input" — use QEMU `sendkey`/`mouse_move` via monitor socket.
- MUST NOT: Create criteria requiring "user visually confirms VT switch" — use serial markers for `COMP_VT_ACTIVATE`/`CONSOLE_DEACTIVATE`.
- MUST NOT: Write vague QA scenarios ("verify /dev works", "test input") — exact commands + exact serial output assertions.

---

## Recommended Approach

Implement in 5 phases, each independently testable. Start with the kernel cap type (Phase 0) — it's additive and breaks nothing. Then devmgr generalization (Phase 1), VFS dynamic backend (Phase 2), inputd extraction (Phase 3), and finally procmgr spawn-time views (Phase 4). The hardest refactor is Phase 2 (DeviceType enum removal) — lock behavior with regression tests first. The most architecturally significant is Phase 4 (spawn-time device views) — it's where the no-runtime-ACL invariant is tested. Defer kernel-side cap enforcement to a separate hardening audit; this plan ships the cap type and the registry, not the per-op checks.
