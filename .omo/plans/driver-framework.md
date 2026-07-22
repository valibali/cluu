# Driver Framework — Implementation Plan

> Design: `doc/book/driver_framework_brainstorm.md` (all 10 forks decided,
> Oracle-reviewed, sub-brainstorm resolved).
>
> This plan is the executable breakdown. 6 phases (D1-D6), each with
> atomic tasks, dependencies, exit criteria, and risk notes.

## Context

CLUU has ~15 driver binaries, each self-probing PCI at boot, spawned by
init from a hardcoded `SERVICE_LIST`. No central enumeration, no bind
rules, no restart, no fallback, no exception handling. The `acpi` crate
exists but nothing consumes it. `driver-framework` crate has a `DriverProbe`
trait with zero implementors.

This plan introduces `drivermgr` (probe + bind + spawn) and `drivermon`
(supervise: restart + fallback + fault handling) as new primordial
services, extends `container-build` with `FROM driver` + `DRIVER`
directives, and migrates drivers from self-probe to drivermgr-spawned over
6 phases.

## Phase D1 — drivermgr + drivermon skeleton (non-disruptive)

**Goal:** Two new primordial services exist, boot, scan PCI + ACPI, publish
device tree to `/proc/devices`. Neither spawns drivers. Existing drivers
keep self-probing. Zero behavior change.

**Exit criteria:**
- `ls /proc/devices` lists all PCI devices with vendor/device/class
- `cat /proc/devices/pci/00:04.0` shows per-device detail (BDF, BARs, IRQ)
- Serial log shows `drivermgr: ready` and `drivermon: ready`
- All existing harness cases pass (no regressions)

### Tasks

**D1.1 — Create drivermgr crate skeleton**
- `userspace/drivermgr/Cargo.toml` — deps: libcluu, cluu-acpi, cluu-driver-framework, spin
- `userspace/drivermgr/src/main.rs` — `fn main() -> i32`, registry init, register_output("main", ep), recv loop
- `userspace/drivermgr/src/device_tree.rs` — `DeviceNode` struct, `DeviceTree: BTreeMap<String, DeviceNode>`
- Add to `userspace/init/src/services.rs` SERVICE_LIST (after vfs, before virtio-blk), priority 190, CapProfile::SERVICE
- Add to `xtask/src/main.rs` sys_programs list
- Add to `xtask/src/main.rs` manifest_rights_mask
- **Verify:** boots, `drivermgr: ready` on serial

**D1.2 — PCI scan in drivermgr**
- `userspace/drivermgr/src/pci_scan.rs` — loop bus 0..255, dev 0..32, fn 0..8 via `libcluu::pci::read_ids` + `config_read_u32`
- Reuse `driver-framework::pci::enumerate` (already scans 0..8 bus — extend to 255 or keep 0..8 for QEMU)
- Build DeviceTree entries: path `/pci/XX:YY.Z`, vendor_id, device_id, class_code, bars, irq_line
- **Verify:** serial log shows `drivermgr: found /pci/00:04.0 vendor=1af4 device=1042`

**D1.3 — ACPI RSDP/FADT/MCFG discovery in drivermgr**
- Link `cluu_acpi` crate
- `userspace/drivermgr/src/acpi_scan.rs` — `find_rsdp(space_token)` → `find_fadt_from_rsdp` → log FADT pm1a_cnt_blk
- Parse MCFG for ECAM base (log it, don't use yet)
- No DSDT walk yet (D6)
- **Verify:** serial log shows `drivermgr: ACPI RSDP at <addr>, FADT pm1a=0x<port>`

**D1.4 — /proc/devices procfs entry**
- New procfs backend in VFS: `userspace/vfs/src/backends/devices_procfs.rs`
- On read of `/proc/devices`: IPC call to `drivermgr:query` → list all DeviceNodes as text
- On read of `/proc/devices/pci/XX:YY.Z`: IPC call to `drivermgr:query_device(path)` → per-device detail
- Register procfs entry in VFS mount table
- **Verify:** `cat /proc/devices` from shell shows PCI device list

**D1.5 — Create drivermon crate skeleton**
- `userspace/drivermon/Cargo.toml` — deps: libcluu, cluu-wire, spin
- `userspace/drivermon/src/main.rs` — registry init, register_output("main", ep), recv loop
- `userspace/drivermon/src/runtime_table.rs` — `RuntimeEntry` struct, `DriverRuntimeTable: BTreeMap<u32, RuntimeEntry>`
- Add to SERVICE_LIST (after drivermgr), priority 189, CapProfile::SERVICE
- Add to xtask sys_programs + manifest_rights_mask
- **Verify:** boots, `drivermon: ready` on serial

**D1.6 — drivermon exit-notify endpoint**
- drivermon creates a notify endpoint at boot
- Registers it with registry as `drivermon:notify`
- (No drivers register with it yet — D3 wires this)
- **Verify:** endpoint exists, no crashes

**Dependencies:** None. D1 is standalone.

**Risk:** Low. All new code, no existing behavior changed. If drivermgr/drivermon crash, they're not primordial yet (can make them primordial in D3).

---

## Phase D2 — Cluufile DRIVER directives + bind rules (non-disruptive)

**Goal:** `container-build` parses `FROM driver` + `DRIVER` directives, emits `[driver]` section in manifest.toml. drivermgr reads `[driver]` sections at boot, builds BindRuleTable, logs matches. Still doesn't spawn. Zero behavior change.

**Exit criteria:**
- `container-build` accepts `FROM driver` and `DRIVER` directives
- `manifest.toml` for virtio-blk contains `[driver.bind]` section
- Serial log shows `drivermgr: device /pci/00:04.0 matched driver virtio-blk`
- All existing harness cases pass

### Tasks

**D2.1 — Extend container-build with FROM driver + DRIVER parsing**
- `tools/container-build/src/main.rs`: add `DRIVER` directive parser (one per line: `DRIVER <sub> <key>=<val> ...`)
- Store in `Cluufile.driver: Option<DriverSpec>` (None if no DRIVER directives)
- Validate: `FROM driver` requires at least one `DRIVER` directive; `FROM minimal` rejects `DRIVER`
- Validate: `DRIVER bind` is required for `FROM driver`
- **Verify:** `container-build containers/rm/Cluufile` succeeds (FROM minimal, no DRIVER); create a synthetic test Cluufile with `FROM driver` and no `DRIVER` directives → fails with error; create a synthetic test Cluufile with `FROM minimal` and a `DRIVER` directive → fails with error. Defer virtio-blk verify to D2.4.

**D2.2 — Emit [driver] section in manifest.toml**
- `generate_manifest_toml`: if `cluufile.driver.is_some()`, emit `[driver.bind]`, `[driver.hardware]`, `[driver.lifecycle]`, `[driver.source]`, `[driver.envelope]` sections
- **Verify:** `cat target/containers/virtio-blk/manifest.toml` shows `[driver.bind]` with vendor=0x1af4

**D2.3 — Create Cluufiles for init-spawned drivers**
- 4 drivers (virtio-blk, virtio-net, virtio-snd, usb-input) are spawned by init from SERVICE_LIST with hardcoded rights — they have NO Cluufiles today
- Use `containers/virtio-9p/Cluufile` as the pattern (it's the only virtio driver with a Cluufile)
- Derive PROFILE from the rights bitmask in `userspace/init/src/services.rs` (VIRTIOBLK_RIGHTS, VIRTIONET_RIGHTS, etc.)
- Derive BUILD/ENTRYPOINT from `userspace/<name>/Cargo.toml` + xtask build commands
- Create:
  - `containers/virtio-blk/Cluufile` — FROM driver, PROFILE from VIRTIOBLK_RIGHTS
  - `containers/virtio-net/Cluufile` — FROM driver, PROFILE from VIRTIONET_RIGHTS
  - `containers/virtio-snd/Cluufile` — FROM driver, PROFILE from VIRTIO_SND_RIGHTS
  - `containers/usb-input/Cluufile` — FROM driver, PROFILE from USB_INPUT_RIGHTS
- **Verify:** `container-build` succeeds on all 4 new Cluufiles, produces manifest.toml in `target/containers/<name>/`

**D2.4 — Add DRIVER directives to all driver Cluufiles**
- Modify existing Cluufiles (virtio-9p, kbd, mouse) AND the 4 created in D2.3:
  - `containers/virtio-blk/Cluufile`: DRIVER pci vendor=0x1af4 devices=[0x1001,0x1042] class=0x010000, dma, lifecycle critical=true, source initrd_path="sys/virtio-blk.elf"
  - `containers/virtio-net/Cluufile`: DRIVER pci vendor=0x1af4 devices=[0x1041]
  - `containers/virtio-snd/Cluufile`: DRIVER pci vendor=0x1af4 devices=[0x1059]
  - `containers/virtio-9p/Cluufile`: DRIVER pci vendor=0x1af4 devices=[0x1009]
  - `containers/usb-input/Cluufile`: DRIVER pci class=0x0c0320
  - `containers/kbd/Cluufile`: DRIVER acpi hid=PNP0303
  - `containers/mouse/Cluufile`: DRIVER acpi hid=PNP0F13
- **Verify:** all build with `cargo xtask build`, `target/containers/virtio-blk/manifest.toml` contains `[driver.bind]` section

**D2.5 — drivermgr reads [driver] sections, builds BindRuleTable**
- `userspace/drivermgr/src/bind_rules.rs` — `BindRule` struct, `BindRuleTable: Vec<BindRule>`
- Phase 2 boot (after VFS "mounted"): `readdir("/var/images/")` → for each, open manifest.toml → parse → check for `[driver]` section → if present, add to BindRuleTable
- Sort by priority (high to low)
- **Verify:** serial log shows `drivermgr: loaded N bind rules`

**D2.6 — drivermgr matches devices to bind rules (observe mode)**
- After building DeviceTree + BindRuleTable, walk matches
- Log: `drivermgr: device /pci/00:04.0 matched driver virtio-blk (priority 180)`
- Log unmatched devices: `drivermgr: device /pci/00:05.0 no matching driver`
- Do NOT spawn. Just observe.
- **Verify:** serial log shows matches for all present PCI devices

**D2.7 — Add drivermgr + drivermon to PRIMORDIAL_SERVICES**
- `userspace/init/src/services.rs`: add "drivermgr" and "drivermon" to PRIMORDIAL_SERVICES
- init panics if either dies (they're now critical for observability)
- **Verify:** kill drivermgr → init panics (test in QEMU)

**Dependencies:** D1 complete (drivermgr + drivermon exist and boot).

**Risk:** Low. Cluufile changes are additive (FROM minimal still works). No spawn behavior changed. Risk: container-build parser bugs → validate with all 163 existing Cluufiles.

---

## Phase D3 — devmgr cap minting + drivermgr spawn (disruptive, opt-in)

**Goal:** drivermgr spawns matched drivers via procmgr, passing device params. Drivers gain "param-driven init" path. Toggle via `/etc/drivermgr.toml` `spawn_mode`. Boot-critical virtio-blk loads from initrd.

**Exit criteria:**
- Boot with `spawn_mode = "spawn"`: all drivers come up via drivermgr
- Boot with `spawn_mode = "observe"`: existing behavior (drivers self-probe)
- virtio-blk loads from initrd (phase 1), others from userdisk (phase 2)
- All existing harness cases pass in observe mode; spawn mode passes boot-to-login

### Tasks

**D3.1 — devmgr MINT_IRQ_CAP label**
- `userspace/devmgr/src/handlers.rs`: `handle_mint_irq_cap` — TokenDeriveScoped on irq_handle_root_token, return scoped IRQ_HANDLE token
- `userspace/libcluu/src/ipc.rs`: add `DEVMGR_MINT_IRQ_CAP_LABEL = 0x503`
- init derives `irq_handle_root_token` from `root_token` (init/src/context.rs), wires to devmgr TOKEN_EXTRA_2
- **Verify:** drivermgr can call devmgr MINT_IRQ_CAP(irq=11) → get token

**D3.2 — drivermgr spawn path**
- `userspace/drivermgr/src/spawn.rs`: call procmgr:spawn with envelope + device params
- Params: PARAM_DEVICE_PATH, PARAM_PCI_BDF, PARAM_PCI_BAR0..5, PARAM_IRQ_LINE, PARAM_DMA_BASE, PARAM_DMA_PAGES
- Tokens: TOKEN_EXTRA_1 = shared pci_access_token (derived by init, passed to drivermgr), TOKEN_EXTRA_2 = scoped irq_token (from devmgr)
- Register drivermon's notify_ep and fault_ep in the spawn envelope
- After spawn, tell drivermon: REGISTER(pid, device_path, driver_image, policy, fallback)
- **Verify:** drivermgr can spawn a test driver (e.g. devprobe) with params

**D3.3 — drivermon REGISTER/RESPAWN/REBIND IPC labels**
- `userspace/drivermon/src/handlers.rs`: handle REGISTER (add RuntimeEntry), handle RESPAWN (drivermgr→drivermon ack), handle REBIND
- `cluu_wire`: add DRIVERMON_REGISTER_LABEL, DRIVERMON_RESPAWN_LABEL, DRIVERMON_REBIND_LABEL
- **Verify:** drivermgr calls REGISTER → drivermon has entry in RuntimeTable

**D3.4 — /etc/drivermgr.toml config file**
- New config: `spawn_mode = "observe" | "spawn" | "hybrid"`
- drivermgr reads at boot (phase 2, after VFS "mounted")
- observe: log matches, don't spawn (D2 behavior)
- spawn: spawn matched drivers (D3 behavior)
- hybrid: spawn if [driver] section exists, else fall back to init SERVICE_LIST
- **Verify:** boot with observe → no drivermgr spawns. Boot with spawn → drivermgr spawns.

**D3.5 — Driver param-driven init path**
- For each driver that gets drivermgr-spawned, add param-driven init:
  - Read PARAM_DEVICE_PATH from process_info().params
  - If present: query drivermgr for DeviceNode, claim, use params for BDF/BARs/IRQ
  - If absent: fall back to self-probe (existing path)
- Drivers to migrate: virtio-blk, virtio-net, virtio-snd, virtio-9p, usb-input
- **Verify:** virtio-blk boots via drivermgr spawn in spawn mode, self-probes in observe mode

**D3.6 — Two-phase boot: virtio-blk from initrd**
- Prerequisite: D2.3 + D2.4 completed — virtio-blk has a Cluufile, container-build produces `target/containers/virtio-blk/manifest.toml` with `[driver]` section
- xtask: copy `target/containers/virtio-blk/manifest.toml` → `sys/virtio-blk.manifest.toml` in initrd (as file, not spawned)
- xtask: copy `target/containers/virtio-blk/bin/virtio-blk.elf` → `sys/virtio-blk.elf` in initrd (as file, not spawned)
- drivermgr phase 1: readdir("/dev/initrd/sys/"), find *.manifest.toml, parse [driver] section
- drivermgr phase 1: spawn virtio-blk with source.initrd_path = "sys/virtio-blk.elf"
- procmgr:spawn reads ELF from /dev/initrd/sys/virtio-blk.elf via VFS
- After virtio-blk registers with devmgr, VFS mounts userdisk, publishes "mounted"
- drivermgr phase 2: readdir("/var/images/"), load remaining [driver] sections, spawn rest
- **Verify:** boot to login in spawn mode, virtio-blk loaded from initrd

**D3.7 — Remove self-probe from migrated drivers (spawn mode only)**
- In spawn mode, drivers no longer call pci::find_virtio_device or XhciController::probe
- They receive BDF + BARs + IRQ from params
- Keep self-probe path for observe/hybrid mode (fallback)
- **Verify:** serial log shows `virtio-blk: init from params BDF=00:04.0` (no PCI scan)

**Dependencies:** D2 complete (bind rules + [driver] sections). devmgr MINT_IRQ_CAP.

**Risk:** HIGH. This is the disruptive phase. If drivermgr spawn fails, system doesn't boot. Mitigation: observe/hybrid toggle lets us fall back to today's behavior. Test incrementally: one driver at a time.

---

## Phase D4 — restart + fallback (additive)

**Goal:** drivermon supervises drivers. Crash → restart per policy. Budget exhausted → try fallback. Boot-critical failure → init panic.

**Exit criteria:**
- Kill a non-critical driver → drivermon restarts it (if policy allows)
- Kill a driver 4x in 30s → drivermon tries fallback (if configured) or marks Failed
- Kill a boot-critical driver beyond time budget → init panic
- `/proc/devices` shows device state (Bound/Degraded/Failed)

### Tasks

**D4.1 — drivermon exit notification handling**
- drivermon receives PROC_EXIT_LABEL from procmgr (notify_ep registered at spawn in D3.2)
- Look up RuntimeEntry by cookie (not PID — race guard per Oracle)
- Apply F7.B+D policy: exit 0 + non-critical → Unbound; exit ≠ 0 or fault → restart
- **Verify:** kill usb-input → drivermon logs `restart driver usb-input for device /pci/00:05.0`

**D4.2 — drivermon fault IPC handling**
- drivermgr registers drivermon's fault_ep via ThreadSetFaultEndpoint at spawn
- drivermon receives fault IPC (label 0xFA017): fault_type, fault_addr, rip, thread_id, reply_id
- Reply with "kill" directive (using ReplyId from fault message) → kernel destroys faulting thread
- Then tell drivermgr: RESPAWN(device_path, driver_image)
- **Verify:** force a driver page fault → drivermon catches, kills, respawns

**D4.3 — Restart budget + time window**
- RuntimeEntry: restart_count, last_restart_ms, max_restarts, window_secs
- Boot-critical: time budget (e.g. 5 restarts / 30s — reuse RestartTracker pattern from session-procmgr)
- Budget exceeded + boot-critical → init panic
- Budget exceeded + non-critical + fallback exists → REBIND
- Budget exceeded + non-critical + no fallback → mark Failed
- **Verify:** kill virtio-blk 6x in 30s → init panic. Kill usb-input 4x → tries fallback or marks Failed.

**D4.4 — Fallback chain walking**
- drivermon tracks visited_fallbacks per device (cycle detection per Oracle)
- On REBIND: tell drivermgr: REBIND(device_path, fallback_image)
- drivermgr spawns fallback driver, tells drivermon: REGISTER(new_pid, device_path, fallback_image, ...)
- drivermon replaces RuntimeEntry
- **Verify:** configure usb-input with fallback=test-usb-fallback, kill usb-input 4x → fallback spawns

**D4.5 — /proc/devices state reflection**
- drivermgr updates DeviceNode.state on Bound/Degraded/Failed transitions
- /proc/devices shows state: `state=bound` / `state=degraded` / `state=failed`
- **Verify:** `cat /proc/devices/pci/00:05.0` shows `state=degraded` during restart

**Dependencies:** D3 complete (drivermgr spawns, drivermon receives notifications).

**Risk:** Medium. Fault IPC kill path needs care (must reply with kill before respawning). Restart budget prevents livelock. Fallback chains validated at load time (D2).

---

## Phase D5 — initrd minimization (cleanup)

**Goal:** Remove driver binaries from initrd that drivermgr now loads from userdisk. initrd = 9 spawned + virtio-blk.elf + virtio-blk.manifest.toml.

**Exit criteria:**
- `ls /dev/initrd/sys/` shows 9 spawned binaries + virtio-blk.elf + virtio-blk.manifest.toml
- virtio-9p, virtio-net, virtio-snd, netd, usb-input NOT in initrd
- Boot to login works in spawn mode
- All harness cases pass

### Tasks

**D5.1 — Remove drivers from initrd + SERVICE_LIST**
- xtask: remove virtio-9p, virtio-net, virtio-snd, netd, usb-input from sys_programs (initrd)
- init/src/services.rs: remove from SERVICE_LIST and PRIMORDIAL_SERVICES
- Keep virtio-blk in SERVICE_LIST as boot-critical (spawned by drivermgr from initrd, not by init)
- Keep drivermgr, drivermon in SERVICE_LIST (init spawns them)
- **Verify:** initrd size decreases, boot to login works

**D5.2 — Update boot manifest generation**
- xtask: build_boot_manifest: remove removed services from manifest entries
- Keep virtio-blk.elf + virtio-blk.manifest.toml as initrd files (not in boot manifest — they're not spawned by init)
- **Verify:** boot manifest verification passes

**Dependencies:** D3 + D4 complete (drivermgr spawns + supervises all drivers).

**Risk:** Medium. If any driver fails to load from userdisk, boot fails. Mitigation: test each driver individually in spawn mode before removing from initrd.

---

## Phase D6 — ACPI enumeration (additive)

**Goal:** drivermgr walks DSDT for PNP devices, publishes to device tree. kbd + mouse bind via ACPI PNP IDs instead of autostart.toml.

**Exit criteria:**
- `cat /proc/devices/acpi/PNP0303` shows PS/2 keyboard controller
- kbd driver binds via ACPI (not autostart.toml)
- `cat /proc/devices/acpi/PNP0F13` shows PS/2 mouse
- mouse driver binds via ACPI
- autostart.toml no longer lists kbd or mouse

### Tasks

**D6.1 — Minimal DSDT parser in cluu_acpi**
- `userspace/acpi/src/dsdt.rs`: parse DSDT/SSDT AML bytecode for Device() objects
- Extract _HID (PNP IDs) and _CRS (I/O ports, IRQ) from each Device()
- Handle: NameOp, DeviceOp, ResourceTemplate, IRQNoFlags, IO, FixedIO
- Do NOT execute arbitrary AML methods (no method evaluation)
- ~500-1000 LOC
- **Verify:** unit test against QEMU's DSDT (dump with `acpidump` on host, parse offline)

**D6.2 — drivermgr ACPI PNP enumeration**
- `userspace/drivermgr/src/acpi_scan.rs`: walk DSDT, publish DeviceNode entries to tree
- path: `/acpi/<HID>` (e.g. `/acpi/PNP0303`)
- bus: Acpi, acpi_hid: Some(hid), io_ports, irq_line
- **Verify:** `cat /proc/devices` shows `/acpi/PNP0303`, `/acpi/PNP0F13`, etc.

**D6.3 — kbd + mouse bind via ACPI**
- D2.3 already wrote DRIVER directives for kbd (hid=PNP0303) and mouse (hid=PNP0F13)
- drivermgr matches ACPI devices to these bind rules
- Spawn kbd + mouse via drivermgr (not autostart.toml)
- Remove kbd + mouse from /etc/autostart.toml
- **Verify:** boot to login, keyboard works, `cat /proc/devices/acpi/PNP0303` shows `state=bound`, kbd not in autostart.toml

**Dependencies:** D3 complete (drivermgr spawn). D6.1 (DSDT parser).

**Risk:** Medium. DSDT parser is the most complex new code. AML is dense, QEMU's DSDT is relatively simple but real hardware DSDTs are complex. Validate with QEMU first, document limitations.

---

## Cross-cutting tasks

**CC.1 — drivermgr async runtime**
- drivermgr uses `libcluu::async_runtime` (Runtime, IpcCallFuture) — it does IPC to procmgr (spawn), devmgr (mint), VFS (/proc/devices reads)
- `userspace/drivermgr/src/main.rs`: async main loop, poll_ready, recv_any, dispatch
- drivermon also async (receives from procmgr + kernel, calls drivermgr)
- **Phase:** D1 (skeleton), D3 (full usage)

**CC.2 — /proc/devices procfs backend**
- VFS procfs backend that IPCs drivermgr on read
- Async (per AGENTS.md §7 — procfs is AsyncMountBackend)
- **Phase:** D1.4

**CC.3 — container-build validation**
- `FROM driver` requires `DRIVER bind` directive
- fallback references validated against existing /var/images/ at build time (warning if missing)
- criticality propagation: non-critical primary can't have critical fallback
- **Phase:** D2.1

**CC.4 — Harness cases**
- `drivermgr-probe`: boot in observe mode, verify /proc/devices lists all PCI devices
- `drivermgr-spawn`: boot in spawn mode, verify all drivers come up via drivermgr
- `drivermgr-restart`: kill a driver, verify re-spawn
- `drivermgr-fallback`: make primary fail, verify fallback binds
- `drivermgr-acpi`: verify /proc/devices/acpi/PNP0303 exists and kbd binds
- **Phase:** D1 (probe), D3 (spawn), D4 (restart/fallback), D6 (acpi)

## Phase dependency graph

```
D1 (skeleton) ──► D2 (bind rules) ──► D3 (spawn) ──► D4 (restart+fallback)
                                       │                    │
                                       │                    ▼
                                       │              D5 (initrd minimization)
                                       │
                                       └──► D6 (ACPI enumeration)
```

D5 and D6 can proceed in parallel after D3+D4. D6 depends on D3 (spawn) but not D4/D5.

## Total scope estimate

| Phase | New LOC | Modified files | New files | Risk |
|---|---|---|---|---|
| D1 | ~1500 | 4 (services.rs, xtask, vfs mount, ipc.rs) | 8 (drivermgr + drivermon crates) | Low |
| D2 | ~800 | 8 (Cluufiles + container-build) | 2 (bind_rules.rs, driver spec) | Low |
| D3 | ~2000 | 10 (drivers + devmgr + init + procmgr) | 3 (spawn.rs, drivermon handlers, config) | High |
| D4 | ~1000 | 4 (drivermon + drivermgr) | 2 (fault handling, restart tracker) | Medium |
| D5 | ~100 (mostly deletion) | 3 (xtask, services.rs, autostart.toml) | 0 | Medium |
| D6 | ~800 | 2 (cluu_acpi + drivermgr) | 2 (dsdt.rs, acpi_scan.rs) | Medium |
| **Total** | **~6200** | **~31** | **~19** | |
