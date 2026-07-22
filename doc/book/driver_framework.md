# Driver Framework

CLUU's driver model: `drivermgr` enumerates hardware, `drivermon` supervises
drivers. Drivers are capability-scoped binaries spawned with device params
instead of self-probing PCI. This chapter covers the architecture, the
`FROM driver` + `DRIVER` Cluufile directives, bind rules, spawn modes, and
the two-phase boot for boot-critical virtio-blk.

## Architecture overview

```
          ┌─────────────┐
          │    init     │  spawns drivermgr, drivermon (primordial)
          └──────┬──────┘
                 │
       ┌─────────┴─────────┐
       │                   │
  ┌────▼─────┐      ┌─────▼──────┐
  │ drivermgr│      │  drivermon │
  │  (probe  │      │ (supervise)│
  │  + bind  │      │            │
  │  + spawn)│      │            │
  └────┬─────┘      └─────▲──────┘
       │                   │
       │ spawn ───────────►│ REGISTER
       │                   │ RESPAWN
       │                   │ REBIND
       │                   │
  ┌────▼──────────────────────────┐
  │     matched drivers           │
  │  (virtio-blk, virtio-net,     │
  │   virtio-snd, virtio-9p,      │
  │   usb-input, kbd, mouse)      │
  └───────────────────────────────┘
```

**drivermgr** — enumerates PCI + ACPI devices at boot, reads `[driver]`
sections from container manifests, matches devices to bind rules, and
spawns matched drivers via procmgr. Publishes the device tree via
`/proc/devices`.

**drivermon** — receives exit/fault notifications from procmgr and the
kernel, applies restart policy (restart budget, fallback chain, boot-critical
panic). Maintains a `DriverRuntimeTable` keyed by PID.

**Bind rules** — declared in Cluufiles via `DRIVER bind` directives,
emitted to `manifest.toml` as `[[driver.bind]]` sections. Each rule
specifies bus (pci/acpi), vendor/device IDs or class code, lifecycle
policy, and source path.

## Cluufile directives

A driver Cluufile uses `FROM driver` instead of `FROM minimal`. The
`DRIVER` directive declares bind/hardware/lifecycle/source sub-specs:

```dockerfile
FROM driver
PROFILE ipc vfs registry device
DRIVER bind bus=pci vendor=0x1af4 devices=[0x1001,0x1042] class=0x010000
DRIVER hardware dma
DRIVER lifecycle critical=true
DRIVER source initrd_path="sys/virtio-blk.elf"
BUILD "cargo build --release ..." target/.../virtio-blk.elf /sys/virtio-blk
ENTRYPOINT /sys/virtio-blk
```

### DRIVER sub-directives

| Sub | Keys | Purpose |
|---|---|---|
| `bind` | `bus` (pci/acpi), `vendor` (hex), `devices` (hex array), `class` (hex), `hid` (string) | Match hardware to driver |
| `hardware` | `dma` (flag), `mmio` (flag), `irq` (bool) | Hardware capabilities |
| `lifecycle` | `critical` (bool), `restart_policy` (always/onfault/never), `max_restarts` (int), `window_secs` (int) | Restart policy |
| `source` | `initrd_path` (string), `image_path` (string) | Where to load the binary |
| `envelope` | `fallback` (string), `priority` (int) | Fallback chain, match priority |

### Validation

- `FROM driver` requires at least one `DRIVER bind` directive.
- `FROM minimal` (or any non-driver base) rejects `DRIVER` directives.
- Unknown DRIVER sub-directives fail with a clear error.

## Spawn modes

`/etc/drivermgr.toml` controls drivermgr's behavior:

```toml
spawn_mode = "observe"  # default — log matches, don't spawn
```

| Mode | Behavior |
|---|---|
| `observe` | Log device matches, don't spawn. Existing init-spawned drivers self-probe. (D2 behavior) |
| `spawn` | Spawn matched drivers via procmgr with device params. Drivers use param-driven init. (D3 behavior) |
| `hybrid` | Spawn if `[driver]` section exists, else fall back to init SERVICE_LIST. |

## Device tree and /proc/devices

drivermgr builds a `DeviceTree` (BTreeMap<String, DeviceNode>) at boot by
scanning PCI bus 0..255 and discovering ACPI RSDP/FADT/MCFG tables. Each
device node has: path, bus, vendor/device IDs, class code, BDF, BARs, IRQ
line, state (Unbound/Bound/Degraded/Failed).

`/proc/devices` is an async procfs backend in VFS that IPCs drivermgr on
read:
- `cat /proc/devices` — lists all devices as text
- `cat /proc/devices/pci/00:04.0` — per-device detail (BDF, BARs, IRQ)
- `ls /proc/devices/pci/` — lists PCI device names

## Two-phase boot (D3.6)

Boot-critical virtio-blk loads from initrd in phase 1, before userdisk is
mounted. Other drivers load from userdisk in phase 2.

**Phase 1** (before VFS "mounted"):
1. drivermgr reads `/dev/initrd/sys/*.manifest.toml`
2. Spawns virtio-blk with `source.initrd_path = "sys/virtio-blk.elf"`
3. procmgr reads ELF from `/dev/initrd/sys/virtio-blk.elf` via VFS
4. virtio-blk registers with devmgr → VFS mounts userdisk → publishes "mounted"

**Phase 2** (after VFS "mounted"):
1. drivermgr reads `/var/images/*/manifest.toml`
2. Spawns remaining matched drivers from userdisk

## Restart and fallback (D4)

drivermon supervises spawned drivers. On crash:
- Exit 0 + non-critical → Unbound (clean exit)
- Exit ≠ 0 or fault → restart (if within budget)
- Budget exhausted + boot-critical → init panic
- Budget exhausted + non-critical + fallback exists → REBIND to fallback
- Budget exhausted + non-critical + no fallback → mark Failed

Restart budget: N restarts per M seconds. Tracked per-device with
`visited_fallbacks` for cycle detection.

## ACPI enumeration (D6)

drivermgr walks the DSDT for PNP Device() objects, extracts _HID and _CRS
(I/O ports, IRQ). ACPI devices appear in the device tree as
`/acpi/<HID>` (e.g. `/acpi/PNP0303`). kbd and mouse bind via ACPI PNP IDs
instead of autostart.toml.

The DSDT parser handles: NameOp, DeviceOp, ResourceTemplate, IRQNoFlags,
IO, FixedIO. It does NOT execute arbitrary AML methods.

## Capability flow

```
init ──► drivermgr
  tokens: [endpoint, pci_token, view_mgr_token]
  rights: CapProfile::SERVICE (IPC, VFS, REGISTRY, DEVICE, SPACE_GRANT)

init ──► drivermon
  tokens: [endpoint]
  rights: CapProfile::SERVICE

init ──► devmgr
  tokens: [..., irq_handle_root_token (TOKEN_EXTRA_2)]
  rights: CapProfile::SERVICE + IRQ_HANDLE|IRQ_ACK|GRANT (via derived token)

drivermgr ──► devmgr MINT_IRQ_CAP(irq=N)
  → token_derive(irq_handle_root_token, IRQ_HANDLE|IRQ_ACK)
  → scoped IRQ token for the driver

drivermgr ──► procmgr SPAWN_SERVICE(image, manifest, tokens, params)
  → spawned driver gets: pci_token (shared), irq_token (scoped), device params

drivermgr ──► drivermon REGISTER(pid, device_path, driver_image, policy)
  → drivermon tracks the driver in RuntimeTable
```

## IPC labels

| Label | Value | Direction | Purpose |
|---|---|---|---|
| `DEVMGR_MINT_IRQ_CAP_LABEL` | 0x503 | drivermgr → devmgr | Mint scoped IRQ token |
| `DRIVERMGR_QUERY_DEVICES_LABEL` | 0x520 | VFS → drivermgr | List all devices |
| `DRIVERMGR_QUERY_DEVICE_LABEL` | 0x521 | VFS → drivermgr | Per-device detail |
| `DRIVERMON_REGISTER_LABEL` | 0x530 | drivermgr → drivermon | Register spawned driver |
| `DRIVERMON_RESPAWN_LABEL` | 0x531 | drivermgr → drivermon | Ack respawn |
| `DRIVERMON_REBIND_LABEL` | 0x532 | drivermgr → drivermon | Rebind to fallback |

## Files

| File | Purpose |
|---|---|
| `userspace/drivermgr/src/main.rs` | Orchestration: scan, bind, spawn, recv |
| `userspace/drivermgr/src/pci_scan.rs` | PCI bus enumeration |
| `userspace/drivermgr/src/acpi_scan.rs` | ACPI RSDP/FADT/MCFG discovery |
| `userspace/drivermgr/src/device_tree.rs` | DeviceNode, DeviceTree types |
| `userspace/drivermgr/src/bind_rules.rs` | BindRule, BindRuleTable, matching |
| `userspace/drivermgr/src/spawn.rs` | procmgr spawn path (D3.2) |
| `userspace/drivermon/src/main.rs` | Recv loop, supervision dispatch |
| `userspace/drivermon/src/handlers.rs` | REGISTER/RESPAWN/REBIND handlers |
| `userspace/drivermon/src/runtime_table.rs` | RuntimeEntry, DriverRuntimeTable |
| `userspace/vfs/src/devices_procfs.rs` | /proc/devices async backend |
| `tools/container-build/src/main.rs` | FROM driver + DRIVER parsing |

## Kernel limitation: IRQ token scoping

The kernel's `invoke_token_derive_scoped` does not support
`ObjectRef::Irq` — it only handles `VfsViewManager`, `BlockRegion`, and
`DeviceRegion`. Per-IRQ-line kernel-level scoping is not available.

**Workaround**: `devmgr MINT_IRQ_CAP` uses `token_derive` (non-scoped)
with `IRQ_HANDLE | IRQ_ACK` rights. The "scoping" is advisory — drivermgr
tracks the requested irq_line and passes it as a spawn param; the driver
passes it to `irq_attach(token, ep, irq_line)` which checks only the
`IRQ_HANDLE` right, not the obj_ref. This matches the existing pattern
for all per-driver IRQ tokens in `init/src/context.rs`.

To add kernel-enforced IRQ scoping, `invoke_token_derive_scoped` would
need an `ObjectRef::Irq` arm.
