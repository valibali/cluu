# Driver Framework

CLUU's driver model: `drivermgr` enumerates hardware, `drivermon` supervises
drivers. Drivers are capability-scoped binaries spawned with device params
and capability tokens instead of self-probing PCI. This chapter covers the
architecture, philosophy, Cluufile API, and a step-by-step guide to adding
a new peripheral driver.

---

## Philosophy

### No self-probing

In a traditional monolithic OS, a driver enumerates the bus itself — it
scans PCI, checks vendor/device IDs, and claims a device. CLUU inverts
this: **drivermgr** enumerates the bus, **drivermgr** decides which driver
matches, and **drivermgr** spawns the driver with the device's BDF, BARs,
and IRQ line already in its `ProcessInfo.params[]`. The driver never
touches PCI config space to find its device.

Why: a driver that self-probes needs PCI_ACCESS authority. A driver that
receives device params does not — it only needs the narrower rights for
its own MMIO/IRQ region. This is the capability-scoped authority model
applied to device drivers.

### Capability tokens, not runtime ACL

Drivers receive capability tokens at spawn time — derived from
root-procmgr's authority. A PCI driver gets a token with `PCI_ACCESS` +
`SPACE_MAP` rights (slot 10). An IRQ-driven driver gets a token with
`IRQ_HANDLE` + `IRQ_ACK` rights (slot 11). There is no runtime
permission check — if the driver has the token, it can use it. Authority
is declared in the Cluufile, minted at spawn, and never re-evaluated.

### Supervised lifecycle

Drivers don't just exit. If a driver crashes, `drivermon` catches the
exit notification (via procmgr) or the fault IPC (via the kernel) and
applies the declared restart policy: `always`, `on_fault`, or `never`.
Boot-critical drivers that exhaust their restart budget cause an init
panic — the system won't boot without them.

### Declarative config, not code

All driver bind rules are declared in Cluufiles (TOML manifests), not
hardcoded in drivermgr. A new driver adds a `containers/<name>/Cluufile`
with `DRIVER bind` directives; drivermgr discovers it by scanning
`/var/images/*/manifest.toml` at boot. No drivermgr code changes needed.

---

## Architecture

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

**drivermgr** — primordial service spawned by init. Scans PCI bus 0..255
and ACPI RSDP/FADT/DSDT tables at boot. Reads `[[driver.bind]]` sections
from container manifests. Matches devices to bind rules. Spawns matched
drivers via procmgr's `PROCMGR_SPAWN_SERVICE_LABEL`. Publishes the device
tree via `/proc/devices`.

**drivermon** — primordial service spawned by init. Receives exit
notifications from procmgr (`PROCMGR_REGISTER_EXIT_NOTIFY_LABEL`) and
fault IPC from the kernel (via `PROCMGR_SET_FAULT_EP_LABEL`). Applies
restart policy with a restart budget (N restarts per M seconds).
Maintains a `DriverRuntimeTable` keyed by PID.

**procmgr** — root-procmgr spawns drivers on behalf of drivermgr. Derives
capability tokens from its own authority. Wires the spawned driver's
`ProcessInfo` with device params (BDF, BARs, IRQ) and token slots.

---

## Cluufile API

A driver Cluufile uses `FROM driver` instead of `FROM minimal`. The
`DRIVER` directive declares sub-specs:

```dockerfile
FROM driver
PROFILE ipc vfs registry device
DRIVER bind bus=pci vendor=0x1af4 devices=[0x1001,0x1042] class=0x010000
DRIVER tokens slot=10 rights=0x43010028
DRIVER tokens slot=11 rights=0x30000000
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
| `tokens` | `slot` (int 9–15), `rights` (hex bitmask) | Capability token request — procmgr derives from its own authority |
| `hardware` | `dma` (flag), `mmio` (flag), `irq` (bool) | Hardware capabilities |
| `lifecycle` | `critical` (bool), `restart_policy` (always/onfault/never), `max_restarts` (int), `window_secs` (int) | Restart policy |
| `source` | `initrd_path` (string), `image_path` (string) | Where to load the binary |
| `envelope` | `fallback` (string), `priority` (int) | Fallback chain, match priority |

### Token slots

Token slots map directly to `ProcessInfo.tokens[]` indices:

| Slot | Constant | Typical use |
|------|----------|-------------|
| 9 | `TOKEN_EXTRA_0` | Listen/grantable endpoint (set by `ENDPOINT listen`/`grantable`) |
| 10 | `TOKEN_EXTRA_1` | PCI access token (rights include `PCI_ACCESS`, `SPACE_MAP`) |
| 11 | `TOKEN_EXTRA_2` | IRQ handle token (rights include `IRQ_HANDLE`, `IRQ_ACK`) |
| 13–15 | `TOKEN_EXTRA_4..6` | Additional tokens |

procmgr derives each requested token from its own `self.token` with the
specified rights bitmask. The driver accesses them via
`info.tokens[TOKEN_EXTRA_1]`, etc.

### Common rights bitmasks

| Hex | Rights | Use |
|-----|--------|-----|
| `0x43010028` | `SPACE_MAP | SPACE_GRANT | IPC_SEND | IPC_RECV | IPC_CALL | PCI_ACCESS` | PCI driver: MMIO + IPC |
| `0x30000000` | `IRQ_HANDLE | IRQ_ACK` | IRQ handler: interrupt attach + ack |

### Validation

- `FROM driver` requires at least one `DRIVER bind` directive.
- `FROM minimal` (or any non-driver base) rejects `DRIVER` directives.
- Unknown DRIVER sub-directives fail with a clear error.

---

## Device matching

drivermgr matches devices to bind rules at boot. The matching algorithm:

1. For each PCI device in the tree (bus 0..255):
   - Check all bind rules with `bus = "pci"`.
   - If `vendor` is set, must match the device's vendor ID.
   - If `devices` is set, the device ID must be in the list.
   - If `class` is set, must match the device's class code.
   - A rule with no vendor/devices/class matches any PCI device (wildcard).
   - Highest priority rule wins (lower number = higher priority).

2. For each ACPI device in the tree (from DSDT/SSDT PNP scan):
   - Check all bind rules with `bus = "acpi"`.
   - If `hid` is set, must match the device's `_HID` (e.g. `PNP0303`).
   - Highest priority rule wins.

### Example: PCI class match (USB EHCI)

```dockerfile
DRIVER bind bus=pci class=0x0c0320
```

Matches any PCI device with class code 0x0c0320 (USB 2.0 EHCI host
controller). No vendor/device ID check — any EHCI controller matches.

### Example: PCI vendor+device match (virtio-blk)

```dockerfile
DRIVER bind bus=pci vendor=0x1af4 devices=[0x1001,0x1042] class=0x010000
```

Matches Red Hat (0x1af4) virtio block devices (0x1001 transitional,
0x1042 modern) with class 0x010000 (block storage).

### Example: ACPI PNP match (keyboard)

```dockerfile
DRIVER bind bus=acpi hid=PNP0303
```

Matches the ACPI keyboard device with _HID PNP0303 (IBM Enhanced
101/102-key).

---

## Driver spawn flow

When drivermgr matches a device to a bind rule:

1. **Build param overrides** — packs device info into `ProcessInfo.params[]`:
   - `PARAM_DEVICE_PATH` (slot 19): packed BDF as `(bus<<16 | dev<<8 | func)`
   - `PARAM_PCI_BDF` (slot 20): same packed BDF
   - `PARAM_PCI_BAR0..5` (slots 21–26): the 6 PCI base address registers
   - `PARAM_IRQ_LINE` (slot 27): the IRQ line assigned to the device
   - `PARAM_DMA_BASE` / `PARAM_DMA_PAGES`: DMA region (if `hardware dma`)

2. **Build token requests** — from `[[driver.tokens]]` sections:
   - Each request is `(slot, rights)` — e.g. `(10, 0x43010028)` for PCI access
   - procmgr derives each token from its own authority with the requested rights

3. **Send PROCMGR_SPAWN_SERVICE_LABEL** to procmgr:
   - Payload: `initrd_path\0` + param overrides + token requests
   - `words[0]` = payload length, `words[1]` = priority, `words[2]` = token mode,
     `words[3]` = param count, `words[4]` = cap profile

4. **procmgr spawns the driver**:
   - Loads ELF from initrd (`/dev/initrd/sys/<name>.elf`)
   - Creates address space, maps segments + stack
   - Derives capability tokens for each requested slot
   - Writes `ProcessInfo` with params + tokens to the spawn page
   - `thread_create(START_SUSPENDED)` → `VFS_SET_VIEW` → `thread_resume`

5. **drivermgr registers with drivermon**:
   - Sends `DRIVERMON_REGISTER_LABEL` with PID, device path, restart policy
   - Registers exit-notify and fault endpoint with procmgr

The driver wakes up in `main()` with all device info already in
`process_info().params[]` and all capability tokens in
`process_info().tokens[]`.

---

## Driver-side API

A driver reads its device params and tokens at startup:

```rust
use libcluu::boot::{process_info, TOKEN_EXTRA_0, TOKEN_EXTRA_1, TOKEN_EXTRA_2, TOKEN_SPACE};

fn run() -> Result<()> {
    let info = process_info();

    // PCI device identity (packed BDF from PARAM_DEVICE_PATH)
    let packed = info.params[PARAM_DEVICE_PATH];
    let bus = ((packed >> 16) & 0xFF) as u8;
    let device = ((packed >> 8) & 0xFF) as u8;
    let function = (packed & 0xFF) as u8;

    // Capability tokens (from DRIVER tokens in Cluufile)
    let listen_endpoint = info.tokens[TOKEN_EXTRA_0];  // ENDPOINT listen
    let pci_token = info.tokens[TOKEN_EXTRA_1];         // slot=10, PCI access
    let irq_token = info.tokens[TOKEN_EXTRA_2];          // slot=11, IRQ handle
    let space_token = info.tokens[TOKEN_SPACE];          // address space

    // Initialize registry
    registry::init("my-driver")?;
    registry::register_default_outputs()?;

    // For a PCI driver: use pci_token to access config space + enable device
    let pci_device = find_virtio_device_with_params(pci_token, &[0x1042], &[0x1042], &info.params)?;
    enable_device(pci_token, &pci_device)?;

    // For an IRQ driver: attach IRQ handler using irq_token
    let irq_line = info.params[PARAM_IRQ_LINE] as usize;
    irq_attach(irq_token, listen_endpoint, irq_line)?;

    // Main IPC loop
    let mut buf = [0u8; 4096];
    loop {
        let tokens = [listen_endpoint, registry::control_endpoint()];
        match ipc_recv_any_with_sender(&tokens, &mut buf, u64::MAX) {
            Ok((idx, len, sender_tid)) => { /* handle IPC */ }
            Err(_) => continue,
        }
    }
}
```

### Param slots

| Slot | Constant | Content |
|------|----------|---------|
| 19 | `PARAM_DEVICE_PATH` | Packed BDF: `(bus<<16 \| dev<<8 \| func)` |
| 20 | `PARAM_PCI_BDF` | Same packed BDF |
| 21–26 | `PARAM_PCI_BAR0..5` | PCI base address registers |
| 27 | `PARAM_IRQ_LINE` | IRQ line number |
| 28–29 | `PARAM_DMA_BASE`, `PARAM_DMA_PAGES` | DMA region |

### Token slots

| Slot | Constant | Source | Content |
|------|----------|--------|---------|
| 0–3 | `TOKEN_STDIN..STDLOG` | procmgr | stdin/stdout/stderr/stdlog endpoints |
| 5 | `TOKEN_SPACE` | procmgr | Address space token (SPACE_MAP right) |
| 6 | `TOKEN_IPC` | procmgr | IPC token (SEND/RECV/CALL rights) |
| 8 | `TOKEN_REGISTRY` | procmgr | Registry endpoint |
| 9 | `TOKEN_EXTRA_0` | `ENDPOINT listen`/`grantable` | Listen or grantable endpoint |
| 10 | `TOKEN_EXTRA_1` | `DRIVER tokens slot=10` | PCI access token |
| 11 | `TOKEN_EXTRA_2` | `DRIVER tokens slot=11` | IRQ handle token |

---

## Boot sequence

```
init
 ├─ spawn drivermgr (primordial, from initrd)
 ├─ spawn drivermon (primordial, from initrd)
 ├─ spawn VFS (primordial, from initrd)
 ├─ spawn root-procmgr (primordial, from initrd)
 │
 │  drivermgr:
 │    1. Scan PCI bus 0..255 → DeviceTree
 │    2. Scan ACPI RSDP → FADT → DSDT → PNP devices → DeviceTree
 │    3. Read bind rules from /var/images/*/manifest.toml
 │    4. For each matched device:
 │       a. Build param overrides (BDF, BARs, IRQ)
 │       b. Build token requests (from [[driver.tokens]])
 │       c. PROCMGR_SPAWN_SERVICE_LABEL → procmgr spawns driver
 │       d. DRIVERMON_REGISTER_LABEL → drivermon supervises
 │
 │  procmgr:
 │    1. Read /etc/system.toml [[service]] from VFS
 │    2. autostart_container() for each service (console, vtmgr, etc.)
 │    3. Present login prompt
 │
 │  VFS:
 │    1. Read /etc/system.toml [[mount]] from initrd (like fstab)
 │    2. Mount / (blkdev, required), /host (hostfs, optional)
 │    3. Mount /proc, /proc/devices, /dev (internal backends)
 │
 └─ Login flow (user authenticates → session-procmgr + session-vfs)
```

### Two-phase boot

Boot-critical drivers (e.g. virtio-blk, which backs the root filesystem)
load from initrd in phase 1, before userdisk is mounted. Other drivers
also load from initrd (all driver ELFs are packed into the initrd).

**Phase 1** (before VFS "mounted"):
1. drivermgr reads bind rules from `/var/images/*/manifest.toml` via VFS
2. Spawns matched drivers with `source.initrd_path = "sys/<name>.elf"`
3. procmgr loads ELF from `/dev/initrd/sys/<name>.elf`
4. virtio-blk registers with VFS → VFS mounts userdisk → publishes "mounted"

**Phase 2** (after VFS "mounted"):
1. procmgr reads `system.toml` [[service]] entries
2. Starts system services (console, vtmgr, inputd, compositor)

---

## Restart and fallback

drivermon supervises spawned drivers. On crash:

| Condition | Action |
|-----------|--------|
| Exit 0 + non-critical | Unbound (clean exit) |
| Exit ≠ 0 or fault | Restart (if within budget) |
| Budget exhausted + boot-critical | init panic |
| Budget exhausted + non-critical + fallback | REBIND to fallback driver |
| Budget exhausted + non-critical + no fallback | Mark Failed |

Restart budget: N restarts per M seconds. Tracked per-device with
`visited_fallbacks` for cycle detection.

---

## /proc/devices

drivermgr publishes the device tree as an async procfs backend in VFS:

```sh
cat /proc/devices           # list all devices
cat /proc/devices/pci/00:04.0   # per-device detail (BDF, BARs, IRQ)
ls /proc/devices/pci/        # list PCI device names
ls /proc/devices/acpi/       # list ACPI device names
```

Each device node has: path, bus, vendor/device IDs, class code, BDF,
BARs, IRQ line, and state (`Unbound`/`Bound`/`Degraded`/`Failed`).

---

## ACPI enumeration

drivermgr walks the DSDT for PNP `Device()` objects, extracts `_HID` and
`_CRS` (I/O ports, IRQ). ACPI devices appear in the device tree as
`/acpi/<HID>` (e.g. `/acpi/PNP0303`).

The DSDT parser handles: `NameOp`, `DeviceOp`, `ResourceTemplate`,
`IRQNoFlags`, `IO`, `FixedIO`. It also scans SSDT tables. It does NOT
execute arbitrary AML methods.

EISA IDs are big-endian 32-bit values within AML DWORDs (which are
otherwise little-endian). See `gotchas.md#cluu-eisa-id-big-endian`.

---

## Interactive commands

Shell builtins exist for manual use post-login:

| Command | IPC label | Purpose |
|---------|-----------|---------|
| `probe <bus>` | `DRIVERMGR_PROBE_LABEL` | Trigger bus re-scan + driver spawn |
| `mount <path> <service>` | `VFS_MOUNT_LABEL` | Mount a service endpoint at a VFS path |
| `start <image>` | `PROCMGR_START_IMAGE_LABEL` | Spawn a container by image name |
| `wait <service>` | registry subscribe | Block until a service registers |

These use the same IPC paths as the boot-time machinery but go through
the shell for interactive use.

---

## Step-by-step: adding a new driver

### 1. Write the driver binary

Create `userspace/<name>/Cargo.toml` and `userspace/<name>/src/main.rs`:

```rust
#![no_std]
#![no_main]
extern crate alloc;

use libcluu::boot::{process_info, TOKEN_EXTRA_0, TOKEN_EXTRA_1, TOKEN_EXTRA_2};

fn run() -> Result<()> {
    let info = process_info();
    let pci_token = info.tokens[TOKEN_EXTRA_1];
    let irq_token = info.tokens[TOKEN_EXTRA_2];
    let endpoint = info.tokens[TOKEN_EXTRA_0];

    registry::init("my-driver")?;
    registry::register_default_outputs()?;

    // ... initialize hardware, attach IRQ, enter IPC loop ...
    Ok(())
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() { Ok(_) => 0, Err(_) => -1 }
}
```

### 2. Create the Cluufile

Create `containers/<name>/Cluufile`:

```dockerfile
FROM driver
PROFILE ipc vfs registry device
DRIVER bind bus=pci vendor=0x1234 devices=[0x5678]
DRIVER tokens slot=10 rights=0x43010028
DRIVER tokens slot=11 rights=0x30000000
DRIVER source initrd_path="sys/<name>.elf"
BUILD "cargo build --release --manifest-path userspace/<name>/Cargo.toml --target triplets/x86_64-cluu-user.json -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem" target/x86_64-cluu-user/release/<name>.elf /sys/<name>
ENTRYPOINT /sys/<name>
```

### 3. Register in xtask

Add the driver to `sys_programs` and `driver_programs` arrays in
`xtask/src/main.rs` so its ELF + manifest are packed into the initrd:

```rust
let sys_programs = [ /* ... */ "<name>" ];
let driver_programs = [ /* ... */ "<name>" ];
```

### 4. Add to the workspace

Add the crate to `Cargo.toml` workspace members and to
`userspace/init/src/services.rs` if it needs special init-time wiring
(most drivers don't — drivermgr handles it).

### 5. Build and test

```sh
cargo xtask build
cargo xtask run          # or: python -m cluu_harness --case l2_login --no-build
```

Check the serial log for:
```
drivermgr: found /pci/00:XX.X vendor=0x1234 device=0x5678
drivermgr: device /pci/00:XX.X matched driver <name> (priority 100)
drivermgr: spawned <name> for /pci/00:XX.X (initrd=sys/<name>.elf) pid=N
```

### 6. Verify interactively

After login, check the device tree and manually probe:

```sh
cat /proc/devices            # device should show as Bound
probe pci                    # re-scan if needed
```

---

## IPC labels

| Label | Value | Direction | Purpose |
|---|---|---|---|
| `DEVMGR_MINT_IRQ_CAP_LABEL` | 0x503 | drivermgr → devmgr | Mint scoped IRQ token |
| `DRIVERMGR_QUERY_DEVICES_LABEL` | 0x520 | VFS → drivermgr | List all devices |
| `DRIVERMGR_QUERY_DEVICE_LABEL` | 0x521 | VFS → drivermgr | Per-device detail |
| `DRIVERMGR_RESPAWN_DEVICE_LABEL` | 0x523 | drivermon → drivermgr | Request respawn |
| `DRIVERMGR_DEVICE_STATE_LABEL` | 0x524 | drivermon → drivermgr | Notify state change |
| `DRIVERMGR_PROBE_LABEL` | 0x525 | shell → drivermgr | Bus re-scan + spawn |
| `DRIVERMON_REGISTER_LABEL` | 0x530 | drivermgr → drivermon | Register spawned driver |
| `PROCMGR_SPAWN_SERVICE_LABEL` | 20 | drivermgr → procmgr | Spawn driver from initrd |
| `PROCMGR_START_IMAGE_LABEL` | 0x54 | shell → procmgr | Spawn container by name |
| `PROCMGR_REGISTER_EXIT_NOTIFY_LABEL` | 0x52 | drivermgr → procmgr | Exit notification routing |
| `PROCMGR_SET_FAULT_EP_LABEL` | 0x53 | drivermgr → procmgr | Fault IPC routing |
| `VFS_MOUNT_LABEL` | 0x79 | service → VFS | Self-register mount |

---

## Files

| File | Purpose |
|---|---|
| `userspace/drivermgr/src/main.rs` | Orchestration: scan, bind, spawn, recv |
| `userspace/drivermgr/src/pci_scan.rs` | PCI bus enumeration |
| `userspace/drivermgr/src/acpi_scan.rs` | ACPI RSDP/FADT/DSDT/SSDT discovery |
| `userspace/drivermgr/src/device_tree.rs` | DeviceNode, DeviceTree types |
| `userspace/drivermgr/src/bind_rules.rs` | BindRule, BindRuleTable, matching |
| `userspace/drivermgr/src/spawn.rs` | procmgr spawn path + token passing |
| `userspace/drivermon/src/main.rs` | Recv loop, supervision dispatch |
| `userspace/drivermon/src/handlers.rs` | REGISTER/RESPAWN/REBIND handlers |
| `userspace/drivermon/src/runtime_table.rs` | RuntimeEntry, DriverRuntimeTable |
| `userspace/vfs/src/devices_procfs.rs` | /proc/devices async backend |
| `tools/container-build/src/main.rs` | FROM driver + DRIVER parsing |
| `etc/drivermgr.toml` | spawn_mode config |
| `etc/system.toml` | [[mount]] + [[service]] boot config |

---

## Kernel limitation: IRQ token scoping

The kernel's `invoke_token_derive_scoped` does not support
`ObjectRef::Irq` — it only handles `VfsViewManager`, `BlockRegion`, and
`DeviceRegion`. Per-IRQ-line kernel-level scoping is not available.

**Workaround**: procmgr derives IRQ tokens with `IRQ_HANDLE | IRQ_ACK`
rights (non-scoped). The "scoping" is advisory — drivermgr passes the
IRQ line as a spawn param; the driver passes it to
`irq_attach(token, ep, irq_line)` which checks only the `IRQ_HANDLE`
right, not the obj_ref.

To add kernel-enforced IRQ scoping, `invoke_token_derive_scoped` would
need an `ObjectRef::Irq` arm.
