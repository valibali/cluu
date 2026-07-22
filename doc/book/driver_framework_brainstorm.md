# Driver Framework — Refined Design (Post-Decision, Post-Oracle)

> Refined through 10-fork design conversation on 2026-07-21, then reviewed
> by Oracle (architecture validation). Oracle findings integrated as
> amendments. All forks decided. Next step: formal plan.

## Oracle review summary

Oracle reviewed 12 architecture concerns. Findings:
- **1 blocker (RESOLVED):** Shared IRQ — kernel already broadcasts (see §11 amendment)
- **6 should-fix:** Cap-flow (§9 amendment), restart races (§5 amendment), §3 advisory scope honesty (§9 amendment), boot-critical livelock (§5 amendment), fault kill path (§5 amendment), fallback chains (§6 amendment)
- **3 non-issue:** Deadlocks (design is safe), §5 session encapsulation, §7 async runtime
- **1 nice-to-have:** driver.toml discovery via readdir (§8 amendment)

Amendments marked **[Oracle]** below.

## 1. Where CLUU is today (one screen)

CLUU has **a pile of independent driver binaries**, each spawned by `init`
from a hardcoded `SERVICE_LIST` (`userspace/init/src/services.rs:153`). Every
driver does its **own PCI scan** at boot — virtio-core scans for vendor
`0x1AF4`, xhci-core scans for class `0x0C0330`, ehci-core for `0x0C0320`,
uhci-core for `0x0C0300`. If the device isn't present, the driver returns
`NotFound` and either idles or exits. Two drivers scanning the same PCI
device would race — **no arbitration**.

`userspace/driver-framework/` is a grab-bag of low-level helpers (`MmioRegion`,
`pci::enumerate`, `IrqGuard`, a `DriverProbe` trait with **zero
implementors**). It is not a Fuchsia-like driver host. `devmgr` is a device
registry + capability-token broker — drivers self-register via IPC, procmgr
queries at spawn time to build VFS views. Neither orchestrates probing,
binding, restart, or fallback.

The `acpi` crate (`userspace/acpi/`) parses RSDP/RSDT/XSDT/FADT/MCFG but
**nothing consumes it** for enumeration. The `acpiprobe` and `xhciprobe`
containers demonstrate probe patterns but are standalone test binaries.

`RestartPolicy` exists in `cluu_wire::spawn` (`Never` / `Always` /
`OnFailure { max, window_ms }`) but lives only in Cluufiles — and the boot
drivers (virtio-blk, virtio-net, etc.) don't have Cluufiles because they're
spawned by init directly, not as containers.

Config-file-driven autostart exists for **non-primordial** services:
`/etc/autostart.toml` lists image names, procmgr reads per-image
`/var/images/<name>/manifest.toml` for capabilities/devices/restart. The
`[hardware] devices` field exists but only `"irq"` produces a token.

Kernel `InvokeOp`s for device discovery: `PciConfigRead(50)`,
`PciConfigWrite(51)`, `PortIn8/16/32`, `PortOut8/16/32`, `IrqAttach(30)`,
`IrqAck(31)`. No PCI scan op, no ACPI op, no IRQ routing op.

## 2. The 10 design decisions

| Fork | Choice | Rationale |
|---|---|---|
| **F1** Driver host topology | **A: Standalone binaries** | One process per driver, cap-scoped containers. Fits CLUU identity (§2, §4). No new loading mechanism. Fault isolation per-driver. |
| **F2** Service topology | **C: Three services** | drivermgr (probe+bind+spawn) + devmgr (caps, unchanged sync leaf) + drivermon (supervise). Supervision isolated from binding. More Fuchsia-faithful. |
| **F3** Probe model | **C1: Hybrid publish + claim** | drivermgr scans + matches bind rules + spawns. Driver receives device path, queries drivermgr for device info, claims defensively, does own cap walk. |
| **F4** Bind rules | **S1+S3: FROM driver + [driver] in manifest.toml** | `FROM driver` at Cluufile line 1 = discriminator. `DRIVER` directives in Cluufile → `[driver]` section in manifest.toml. ONE file per image. Cluufile is single source of truth, manifest.toml is single output. procmgr reads `[container]/[profile]/...`, drivermgr reads `[driver]`. |
| **F5** Device tree | **A: In-memory + `/proc/devices`** | drivermgr holds `BTreeMap<DevicePath, DeviceNode>`, publishes to `/proc/devices` via new procfs entry. IPC for programmatic clients, procfs for humans. |
| **F6** Cap minting | **A: devmgr mints all (simplified for v1)** | devmgr mints IRQ_HANDLE tokens per driver. PCI: shared token for v1 (all drivers get same derived pci_access_token, no per-BDF). devmgr stays device cap authority. v2: kernel-enforced per-BDF scope. |
| **F7** Supervision policy | **B+D: Exit-code-aware + tiered** | Exit 0 = don't restart (non-critical clean exit). Exit ≠ 0 or fault = restart. Boot-critical = unlimited restart, Failed → init panic. Non-critical = budget-limited, Failed → device offline. |
| **F8** Initrd boundary | **D: 9 spawned + virtio-blk.elf file** | 9 primordial services + virtio-blk as ELF file in initrd. drivermgr loads virtio-blk from initrd via VFS, everything else from userdisk. Uniform loading model. |
| **F9** ACPI role | **A: Enumeration + MCFG** | Minimal DSDT walk for PNP device enumeration (PS/2, serial, PIT). MCFG for ECAM (optional optimization). FADT S5 poweroff continues. No full AML interpreter. |
| **F10** IRQ routing | **A: PIC + shared IRQ (no kernel change)** | drivermgr reads irq_line from PCI config, manages shared IRQ endpoints centrally. No kernel change. v2: IO-APIC + MSI. |

## 3. Architecture overview

```
              init (PID 1)
                │
   ┌────────────┼────────────────────────────────────────┐
   │            │                                        │
   root-procmgr  vfs                              drivermgr  ← NEW primordial
   (processes)   (filesystem)                         │
   (spawn auth)  (/proc/devices)                      │
   │            │                                      │
   │            │           ┌──────────────────────────┼──────────────┐
   │            │           │                          │              │
   │            │        devmgr                     drivermon     PCI scan
   │            │      (cap broker,                (supervise,    ACPI walk
   │            │       sync leaf)                 exit+fault)   (cluu_acpi)
   │            │           │                          │              │
   │            │           │                     watches pids      publishes
   │            │           │                     catches faults    DeviceTree
   │            │           │                     restart/fallback     │
   │            │           │                          │              │
   │            │           │              ┌───────────┴──────────────┘
   │            │           │              │
   │            │           │         procmgr:spawn
   │            │           │              │
   │            │           │         ┌────┴────────────────┐
   │            │           │         │                     │
   │            │           │     virtio-blk          usb-input
   │            │           │     (from initrd)      (from userdisk)
   │            │           │     (boot-critical)    (non-critical)
   │            │           │         │                     │
   │            │           └─────────┘                     │
   │            │              ↑ self-register              │
   │            │              │ with devmgr                │
   │            │                                           │
   │            └── /dev/* ─────────────────────────────────┘
   │
   └── spawn session-procmgr ←────────────────────────────────
```

## 4. The three new/changed services

### drivermgr (NEW primordial, async orchestrator)

**Owns:**
- `DeviceTree`: `BTreeMap<DevicePath, DeviceNode>` — all discovered devices
- `BindRuleTable`: loaded from `/var/images/*/driver.toml` at boot
- PCI_ACCESS token (one — only process that scans PCI config)

**Does:**
1. At boot (after VFS "mounted"): PCI scan + ACPI PNP enumeration
2. Read bind rules from `/var/images/*/driver.toml` (readdir-based — see §8)
3. Match devices to drivers via bind rules
4. For boot-critical drivers (virtio-blk): load ELF from initrd, spawn via procmgr
5. For non-critical drivers: load from userdisk, spawn via procmgr
6. Ask devmgr to mint scoped PCI_ACCESS + IRQ_HANDLE tokens per driver
7. Pass scoped tokens + device params to procmgr:spawn
8. Tell drivermon: "spawned pid P for device D, driver R, policy X, fallback [R2]"
9. Publish device tree to `/proc/devices` via VFS procfs
10. On drivermon "respawn" / "rebind" request: re-spawn or try fallback

**Does NOT:**
- Mint capability tokens (devmgr does — F6.A)
- Supervise drivers (drivermon does — F2.C)
- Touch PCI config for non-probe purposes (drivers do their own cap walk)
- **[Oracle] Manage shared IRQ endpoints** — each driver attaches its OWN endpoint to the IRQ line; kernel broadcasts (see §11)

### drivermon (NEW primordial, async supervisor)

**Owns:**
- `DriverRuntimeTable`: `BTreeMap<Pid, RuntimeEntry>`
  ```rust
  struct RuntimeEntry {
      device_path: String,
      driver_image: String,
      restart_count: u32,
      last_restart_ms: u64,
      restart_policy: RestartPolicy,  // Never | Always | OnFailure
      max_restarts: u32,
      window_secs: u64,
      fallback_chain: Vec<String>,    // remaining fallback drivers
      boot_critical: bool,
  }
  ```

**Does:**
1. Receive exit notifications (procmgr `PROC_EXIT_LABEL` — drivermgr registers drivermon as notify endpoint at spawn)
2. Receive fault IPC (`ThreadSetFaultEndpoint` label `0xFA017` — drivermgr registers drivermon's fault endpoint on each driver thread)
3. On exit/fault, apply supervision policy (F7.B+D):
   - Exit 0 + non-critical → mark Unbound, don't restart
   - Exit ≠ 0 or fault → restart per policy
   - Boot-critical + budget unlimited → always restart
   - Non-critical + budget exhausted → try fallback
   - Fallback exhausted + boot-critical → init panic
   - Fallback exhausted + non-critical → mark Failed, notify drivermgr
4. Tell drivermgr: "respawn driver R for device D" or "rebind device D to fallback R2"

**Does NOT:**
- Spawn drivers directly (drivermgr does via procmgr)
- Touch capability tokens
- Probe buses

### devmgr (EXISTING, sync leaf — gains 2 IPC labels)

**Existing labels (unchanged):**
- `DEVMGR_REGISTER_LABEL` (driver self-registers block device)
- `DEVMGR_REGISTER_CHAR_LABEL` (driver self-registers char device)
- `DEVMGR_GRANT_REGION_LABEL` (procmgr queries at session spawn)
- `DEVMGR_GRANT_DEVICE_LABEL` (procmgr queries at session spawn)
- `DEVMGR_REVOKE_LABEL` (procmgr at session exit)
- `DEVMGR_LIST_FOR_ENVELOPE_LABEL` (procmgr/VFS queries visible devices)

**New labels (F6.A, simplified for v1 — shared PCI token):**
- `DEVMGR_MINT_IRQ_CAP_LABEL` (drivermgr → devmgr): mint scoped IRQ_HANDLE token for an IRQ line
- ~~`DEVMGR_MINT_PCI_CAP_LABEL`~~ — NOT needed for v1 (shared PCI token, no per-BDF mint)

**New boot tokens (derived by init from `root_token`, NOT kernel-minted):**
- `irq_handle_root_token` (IRQ_HANDLE right) — derived by init, wired to devmgr
- ~~`pci_access_root_token`~~ — NOT needed for v1 (drivermgr derives its own from `root_token` via init, same as today's `pci_token` in `init/src/context.rs`)

**Scope is advisory for v1** (kernel checks rights, not ObjectRef BDF/IRQ scope). Shared PCI token is honest about this — no per-BDF pretense. v2 adds kernel-enforced scope.

## 5. The driver lifecycle — [Oracle amended]

```
                    ┌─────────────────────────────────────────┐
                    │                                         │
  Probed ──► Binding ──► Bound ──► (crash) ──► Degraded ──┬─► Bound (restart)
                    │                  │                   │
                    │                  │                   └─► Failed (budget exhausted)
                    │                  │                         │
                    │                  │                         └─► try fallback ──► Binding
                    │                  │
                    └─── (spawn fail) ─┴─► Failed ──► try fallback

  (exit 0, non-critical) ──► Unbound (no restart)
  (Failed + boot-critical) ──► init panic
```

**[Oracle] Restart race fix — generation + cookie matching.** A driver
crash can produce TWO events: kernel fault IPC (synchronous, immediate) AND
procmgr `PROC_EXIT_LABEL` (async notification). Both could trigger respawn
→ double-spawn.

`RuntimeEntry` gains `generation: u64` and `state: enum { Live, Respawning, Dead }`:
1. First event (fault OR exit) sets `Respawning`, bumps generation, triggers respawn.
2. Second event for same PID+cookie → check state, if `Respawning` → no-op.
3. Respawn completes → new PID, new generation, state `Live`.

For fallback races: `PROC_EXIT_LABEL` already carries a cookie (see
`session-procmgr/src/dispatch.rs:160`). drivermon matches on cookie, not
PID. Old cookie → ignore. drivermgr's rebind mints a new cookie for the
fallback spawn.

**[Oracle] Boot-critical livelock fix — time budget, not unlimited restart.**
Original design said "boot-critical = unlimited restart." This is a livelock
risk: if virtio-blk crashes repeatedly during phase 1 (before userdisk
mounts), drivermon respawns forever, never reaching phase 2.

Fix: boot-critical restart needs a TIME BUDGET, not unlimited. Matches
existing `session-procmgr/src/restart.rs` `RestartTracker` which already has
`WINDOW_TICKS` + `THRESHOLD` (5 attempts / 30s). Boot-critical means "panic
on give-up" not "no give-up." drivermon reuses the RestartTracker pattern,
just panics instead of marking Failed when budget exhausts.

**[Oracle] Fault kill path — fault-reply-with-kill, then respawn.**
Fault IPC is synchronous — faulting thread is BLOCKED in kernel waiting for
reply. drivermon decides "restart." Original design said "tells drivermgr:
RESPAWN" but didn't address killing the faulting thread.

drivermon's fault handler must:
1. Reply to fault IPC with "kill" directive (using existing fault reply
   protocol — `ReplyId` from the fault message, label `0xFA017`). Kernel
   unblocks the thread and destroys it.
2. THEN tell drivermgr: RESPAWN. drivermgr spawns fresh thread via procmgr.

This matches today's procmgr pattern for child crashes (procmgr replies to
fault IPC with kill/resume directive).

**Spawn flow:**
```
1. drivermgr probes PCI, finds device D at BDF X, IRQ line Z
2. drivermgr matches bind rule → driver R, policy P, fallback [R2]
3. drivermgr asks devmgr: MINT_PCI_CAP(bdf=X) → scoped pci_token
4. drivermgr asks devmgr: MINT_IRQ_CAP(irq=Z) → scoped irq_token
5. drivermgr calls procmgr:spawn(
     image = R,
     envelope = { pci_token, irq_token, TOKEN_SPACE, TOKEN_EXTRA_0 },
     params = { PARAM_DEVICE_PATH="/pci/00:04.0",
                PARAM_PCI_BDF=X, PARAM_PCI_BAR0..5, PARAM_IRQ_LINE=Z,
                PARAM_DMA_BASE, PARAM_DMA_PAGES },
     notify = drivermon_notify_ep,
     fault_ep = drivermon_fault_ep,
   ) → pid P
6. drivermgr tells drivermon: REGISTER(P, D, R, P, [R2])
7. driver R starts:
   a. Read PARAM_DEVICE_PATH
   b. drivermgr::query_device(path) → DeviceNode (BDF, BARs, IRQ, dma info)
   c. drivermgr::claim(path) → Ok (atomic, prevents double-bind)
   d. Cap walk: pci::config_read(pci_token, bdf, offset) for MSI-X, virtio vendor caps
   e. Map MMIO (TOKEN_SPACE + BARs), attach IRQ (irq_token + IRQ line)
   f. Register with devmgr (existing IPC: DEVMGR_REGISTER_LABEL)
   g. Serve clients
8. drivermgr marks device Bound in tree + /proc/devices
```

**Crash flow:**
```
1. driver R crashes (exit ≠ 0) or faults (label 0xFA017)
2. procmgr sends PROC_EXIT_LABEL to drivermon (or kernel sends fault IPC)
3. drivermon looks up RuntimeEntry for P (match on cookie, not PID)
4. Check state: if Respawning → no-op (race guard). Else set Respawning.
5. If fault IPC: reply with "kill" directive (unblocks + destroys faulting thread)
6. Policy (F7.B+D):
   - exit_code == 0 && !boot_critical → mark Unbound, done
   - boot_critical || restart_count < max_restarts →
       drivermon tells drivermgr: RESPAWN(D, R)
       drivermgr calls procmgr:spawn(R, ...) → new pid
       drivermon updates RuntimeEntry (new PID, new generation, state Live)
   - restart_count >= max_restarts && fallback exists →
       drivermon tells drivermgr: REBIND(D, R2)
       drivermgr calls procmgr:spawn(R2, ...) → new pid
       drivermon replaces RuntimeEntry
   - fallback exhausted && boot_critical → init panic
   - fallback exhausted && !boot_critical → mark Failed
7. drivermgr updates device tree state, /proc/devices reflects new state
```

## 6. Bind rules — Cluufile DRIVER directives → [driver] section in manifest.toml

**Model (S1+S3):** `FROM driver` at Cluufile line 1 discriminates driver
containers. `DRIVER` directives in the Cluufile express bind rules,
hardware config, lifecycle, source. `container-build` emits these into a
`[driver]` section in the SAME `manifest.toml`. No separate driver.toml
file. procmgr reads `[container]/[profile]/[exec]/[lifecycle]/[mounts]`,
skips `[driver]`. drivermgr reads `[driver]`, skips the rest.

**Cluufile for virtio-blk (boot-critical, loaded from initrd):**
```text
# containers/virtio-blk/Cluufile
FROM driver
PROFILE ipc vfs registry pci space_map irq_handle irq_ack grant create
MOUNT /tmp inherit
BUILD "cargo build ..." target/.../virtio-blk.elf /bin/virtio-blk
ENTRYPOINT /bin/virtio-blk

DRIVER bind pci vendor=0x1af4 devices=[0x1001,0x1042] class=0x010000
DRIVER dma pages=64 base=0x5100_0000
DRIVER lifecycle critical=true restart=always
DRIVER source initrd_path="sys/virtio-blk.elf"
DRIVER priority=180
```

**Generated manifest.toml (ONE file, [driver] section added):**
```toml
# /var/images/virtio-blk/manifest.toml (auto-generated from Cluufile)
[container]
name = "virtio-blk"
[profile]
capabilities = ["ipc", "vfs", "registry", "pci", "space_map",
                "irq_handle", "irq_ack", "grant", "create"]
[exec]
binary = "bin/virtio-blk"
[lifecycle]
restart_policy = "always"
[scheduling]
priority = 180

# [driver] section — read by drivermgr, skipped by procmgr
[driver.bind]
bus = "pci"
vendor = 0x1af4
devices = [0x1001, 0x1042]
class = 0x010000
[driver.hardware]
dma_pages = 64
dma_base = 0x5100_0000
[driver.lifecycle]
critical = true
[driver.source]
initrd_path = "sys/virtio-blk.elf"
```

**Cluufile for usb-input (non-critical, from userdisk):**
```text
# containers/usb-input/Cluufile
FROM driver
PROFILE ipc vfs registry pci space_map irq_handle irq_ack grant create
BUILD "cargo build ..." target/.../usb-input.elf /bin/usb-input
ENTRYPOINT /bin/usb-input

DRIVER bind pci class=0x0c0320
DRIVER dma pages=64 base=0x4200_0000
DRIVER lifecycle critical=false restart=on_failure max=3 window=30 fallback=ohci-input
DRIVER priority=170
```

**Cluufile for kbd (ACPI-bound PS/2):**
```text
# containers/kbd/Cluufile
FROM driver
PROFILE ipc vfs registry irq_handle irq_ack
BUILD "cargo build ..." target/.../kbd.elf /bin/kbd
ENTRYPOINT /bin/kbd

DRIVER bind acpi hid=PNP0303
DRIVER hardware irq=1 io_ports=[0x60,0x64]
DRIVER lifecycle critical=false restart=always
DRIVER priority=175
```

**Cluufile for rm (regular container — unchanged):**
```text
# containers/rm/Cluufile
FROM minimal
PROFILE ipc vfs registry
MOUNT /tmp inherit
BUILD "cargo build ..." target/.../rm.elf /bin/rm
ENTRYPOINT /bin/rm
```

**container-build behavior:**
- `FROM minimal` → emit manifest.toml WITHOUT `[driver]` section (today's behavior)
- `FROM driver` → parse `DRIVER` directives, emit manifest.toml WITH `[driver]` section
- `FROM driver` without any `DRIVER` directives → build error ("FROM driver requires DRIVER directives")

**drivermgr discovery:**
- Phase 1 (initrd): `readdir("/dev/initrd/sys/")`, filter `*.manifest.toml`, parse each, check for `[driver]` section
- Phase 2 (userdisk): `readdir("/var/images/")`, for each try `open("/var/images/<name>/manifest.toml")`, parse, check for `[driver]` section. Missing `[driver]` = not a driver (skip).

**[Oracle] Fallback chain validation (load-time + runtime):**
1. **Load-time validation:** drivermgr validates all `fallback` references
   when loading `[driver]` sections. Missing fallback → log warning, drop
   from chain.
2. **Criticality propagation rule:** fallback's `critical` flag must be ≤
   primary's. Non-critical primary → non-critical fallback only. Reject
   inconsistent Cluufile at build time (`container-build` validates).
3. **Runtime cycle detection:** drivermon tracks
   `visited_fallbacks: Vec<String>` per device in `RuntimeEntry`. If a
   fallback is already visited → mark Failed, stop.

**Example (virtio-blk — boot-critical, loaded from initrd):**
```toml
[bind]
bus = "pci"
vendor = 0x1af4
devices = [0x1001, 0x1042]   # transitional + modern
class = 0x010000             # mass storage

[hardware]
irq = "auto"                 # read irq_line from PCI config
mmio = "auto"                # read BARs from PCI config
dma_pages = 64
dma_base = 0x5100_0000

[lifecycle]
critical = true              # boot-critical: unlimited restart, Failed → panic
restart = "always"
# no max_restarts (unlimited for critical)
# no fallback (if virtio-blk fails permanently, system can't run)

[source]
# Primary: load from userdisk after it's mounted
image = "virtio-blk"
# Fallback: load from initrd (before userdisk mounts)
initrd_path = "sys/virtio-blk.elf"

[envelope]
profile = ["ipc", "vfs", "registry", "pci", "space_map",
           "irq_handle", "irq_ack", "grant", "create"]
priority = 180
```

**Example (usb-input — non-critical, loaded from userdisk):**
```toml
[bind]
bus = "pci"
class = 0x0c0320             # EHCI
# vendor/device omitted → match any EHCI

[hardware]
irq = "auto"
mmio = "auto"
dma_pages = 64
dma_base = 0x4200_0000

[lifecycle]
critical = false
restart = "on_failure"
max_restarts = 3
window_secs = 30
fallback = "ohci-input"      # try this if usb-input fails 3x

[source]
image = "usb-input"
# no initrd_path — only loaded from userdisk

[envelope]
profile = ["ipc", "vfs", "registry", "pci", "space_map",
           "irq_handle", "irq_ack", "grant", "create"]
priority = 170
```

**Example (kbd — ACPI-bound PS/2 keyboard):**
```toml
[bind]
bus = "acpi"
hid = "PNP0303"

[hardware]
irq = 1                      # PS/2 kbd is always IRQ 1
io_ports = [0x60, 0x64]      # PS/2 controller I/O ports

[lifecycle]
critical = false
restart = "always"

[source]
image = "kbd"

[envelope]
profile = ["ipc", "vfs", "registry", "irq_handle", "irq_ack"]
priority = 175
```

## 7. Bind rule matching

drivermgr walks bind rules in priority order (high to low). First match wins.

```rust
fn match_device(tree: &DeviceTree, rules: &BindRuleTable) -> Vec<(DevicePath, &BindRule)> {
    let mut matches = Vec::new();
    for (path, node) in tree.iter() {
        for rule in rules.iter().sorted_by_key(|r| r.envelope.priority).rev() {
            if !rule.bind.bus.matches(node.bus) { continue; }
            if let Some(vid) = rule.bind.vendor {
                if node.vendor_id != Some(vid) { continue; }
            }
            if let Some(dids) = &rule.bind.devices {
                if !dids.contains(&node.device_id.unwrap_or(0)) { continue; }
            }
            if let Some(cls) = rule.bind.class {
                if node.class_code != Some(cls) { continue; }
            }
            if let Some(hid) = &rule.bind.hid {
                if node.acpi_hid.as_deref() != Some(hid) { continue; }
            }
            matches.push((path.clone(), rule));
            break;  // first match wins
        }
    }
    matches
}
```

## 8. Initrd contents (F8.D) — [Oracle amended]

```
initrd (9 spawned + 2 files):
  Spawned by init:
    sys/init
    sys/registry
    sys/timeserver
    sys/devmgr
    sys/root-procmgr
    sys/vfs
    sys/drivermgr          ← NEW
    sys/drivermon          ← NEW
    sys/tpmd
  Files (not spawned, loaded by drivermgr):
    sys/virtio-blk.elf              ← loaded from initrd before userdisk mounts
    sys/virtio-blk.manifest.toml    ← boot-critical [driver] section, read before userdisk
```

**[Oracle] driver.toml discovery — readdir-based, no hardcoding.**
- Phase 1 (before userdisk mounts): drivermgr calls
  `readdir("/dev/initrd/sys/")` via VFS, filters `*.manifest.toml`, parses
  each, checks for `[driver]` section. `InitrdBackend` implements `readdir`
  (confirmed `userspace/vfs/src/mount.rs` `MountBackend::readdir`).
- Phase 2 (after VFS "mounted"): drivermgr calls `readdir("/var/images/")`,
  for each entry tries `open("/var/images/<name>/manifest.toml")`. Parses,
  checks for `[driver]` section. Missing `[driver]` = not a driver (skip;
  e.g. `console/`, `compositor/`).
- No hardcoded boot-critical list — discovery is uniform.

userdisk (loaded by drivermgr after VFS mounts):
```
  /var/images/virtio-blk/     (manifest.toml with [driver] + bin/)
  /var/images/virtio-9p/      (manifest.toml with [driver] + bin/)
  /var/images/virtio-net/     (manifest.toml with [driver] + bin/)
  /var/images/virtio-snd/     (manifest.toml with [driver] + bin/)
  /var/images/netd/           (manifest.toml with [driver] + bin/)
  /var/images/usb-input/      (manifest.toml with [driver] + bin/)
  /var/images/kbd/            (manifest.toml with [driver] + bin/, ACPI-bound)
  /var/images/mouse/          (manifest.toml with [driver] + bin/, ACPI-bound)
  /var/images/console/        (manifest.toml WITHOUT [driver], autostarted)
  /var/images/compositor/     (manifest.toml WITHOUT [driver], autostarted)
  /var/images/vtmgr/          (manifest.toml WITHOUT [driver], autostarted)
  /var/images/inputd/         (manifest.toml WITHOUT [driver], autostarted)
  /var/images/shell/, /bin/*, /etc/*, ...
```

Boot sequence:
1. Kernel → init (PID 1)
2. init spawns 9 primordials
3. VFS mounts initrd at `/dev/initrd`
4. drivermgr starts:
   - Phase 1: `readdir("/dev/initrd/sys/")` → find `virtio-blk.manifest.toml`
   - Parse manifest.toml, extract `[driver]` section → bind rules
   - PCI scan + ACPI PNP enumeration → device tree
   - Match virtio-blk device → virtio-blk driver (boot-critical)
   - Ask devmgr for scoped IRQ_HANDLE token (shared PCI token for v1 — see §9)
   - procmgr:spawn with `source.initrd_path = "sys/virtio-blk.elf"`
   - virtio-blk starts, claims device, registers with devmgr
5. VFS mounts userdisk (ext2 on virtio-blk block device) — publishes "mounted"
6. drivermgr phase 2:
   - `readdir("/var/images/")` → find all manifest.toml files
   - Parse each, extract `[driver]` sections where present
   - Match remaining devices → drivers
   - Spawn virtio-9p, virtio-net, virtio-snd, netd, usb-input, kbd, mouse from userdisk
7. drivermon monitors all spawned drivers

## 9. Capability model (preserving §2/§3) — [Oracle amended]

No new syscalls. No runtime ACL. Authority flows through capability tokens
minted at spawn, same as today.

**[Oracle] Root tokens derived by init, NOT minted by kernel.** Today
`kernel/src/bootstrap.rs` mints `root_token` (Rights::all()),
`clock_token`, `view_mgr_token`, `device_region_token`. `init/src/context.rs`
then derives `pci_token`, `kbd_irq_token`, `virtio_blk_irq_token`, etc. from
`boot.root_token` directly. No per-purpose root tokens exist at kernel boot.

The design's claim "kernel boot mints pci_access_root_token +
irq_handle_root_token" would be a **kernel change → §9 freeze violation.**
Corrected:

- **init** derives `pci_access_root_token` (PCI_ACCESS right) and
  `irq_handle_root_token` (IRQ_HANDLE right) from `boot.root_token` at boot
  wiring (same pattern as existing `pci_token` derivation in context.rs:91-95).
- init passes both to **devmgr** via init wiring (TOKEN_EXTRA slots).
- devmgr holds them as the minting authority for per-device scoped tokens.
- drivermgr receives ONE `pci_access_token` (full-scope PCI_ACCESS) from
  devmgr for scanning. Drivers receive scoped per-BDF tokens (advisory — see below).

**Cap derivation chain:**
```
root_token (kernel mints at boot, Rights::all)
  →(init TokenDerive)→ pci_access_root_token (PCI_ACCESS right, devmgr holds)
    →(devmgr TokenDeriveScoped)→ per-driver pci_token (PCI_ACCESS, advisory BDF scope)
      →(procmgr spawn, TOKEN_EXTRA_1)→ driver uses for PciConfigRead
```

- **Each driver** receives at spawn (from drivermgr, which got them from devmgr):
  - `TOKEN_SPACE` — for `SpaceMap` (MMIO BARs, DMA pool)
  - `TOKEN_EXTRA_0` — own IPC endpoint
  - `TOKEN_EXTRA_1` — scoped PCI_ACCESS token (from devmgr)
  - `TOKEN_EXTRA_2` — scoped IRQ_HANDLE token (from devmgr)
  - Params: `PARAM_DEVICE_PATH`, `PARAM_PCI_BDF`, `PARAM_PCI_BAR0..5`, `PARAM_IRQ_LINE`, `PARAM_DMA_BASE`, `PARAM_DMA_PAGES`
- **devmgr** still mints `BlockRegion` / `DeviceRegion` scoped tokens for procmgr at session creation. Unchanged.

**[Oracle] §3 honesty amendment — advisory scope is NOT structural enforcement.**
Verified in `kernel/src/syscall/handlers.rs:3179`: `invoke_pci_config_read`
checks `Rights::PCI_ACCESS` only. The `_obj_ref` parameter is unused (leading
underscore). A driver with any PCI_ACCESS-bearing token can read/write ANY
BDF.

The design's claim "No driver can access another driver's device" is
**aspirational, not enforced for v1.** The per-BDF token is metadata; the
kernel ignores it. A buggy driver CAN probe any PCI device.

**Honest statement for v1:** per-BDF tokens are advisory — the kernel checks
PCI_ACCESS right but not BDF scope. Drivers are trusted to use only their
assigned BDF. v2: kernel-enforced BDF scope (post-freeze kernel change,
justified by a specific userspace failure).

**Alternative (simpler, more honest):** for v1, give every driver the SAME
`pci_access_token` derived from root (no per-BDF derivation). Drop the
pretense of per-device isolation. Reintroduce per-BDF tokens when kernel
enforces scope. This avoids false claims and simplifies devmgr (no
`MINT_PCI_CAP` label needed for v1).

**[RESOLVED 2026-07-21: shared PCI token for v1.]** All drivers receive the
SAME derived `pci_access_token` from devmgr (or init derives it directly,
same as today's `pci_token` in `init/src/context.rs`). No per-BDF derivation.
No `DEVMGR_MINT_PCI_CAP_LABEL` needed for v1. Honest about v1 limits —
isolation is by convention (drivers only read their assigned BDF), not
enforced. v2 adds kernel-enforced per-BDF scope (post-freeze kernel change).

**v1 IRQ token model:** Each driver receives a scoped `IRQ_HANDLE` token
from devmgr (via `DEVMGR_MINT_IRQ_CAP_LABEL`). Each driver attaches its OWN
endpoint to the IRQ line — kernel broadcasts (see §11).

**Simplified devmgr changes for v1:**
- ONE new IPC label: `DEVMGR_MINT_IRQ_CAP_LABEL` (drivermgr → devmgr): mint IRQ_HANDLE token
- ONE new boot token wired to devmgr: `irq_handle_root_token` (derived by init from `root_token`)
- NO `DEVMGR_MINT_PCI_CAP_LABEL` for v1 (shared PCI token, no per-BDF mint)
- NO `pci_access_root_token` to devmgr for v1 (drivermgr derives its own from `root_token` via init, same as today's `pci_token`)

## 10. ACPI consumption (F9.A)

drivermgr links `cluu_acpi` (existing crate) and adds a minimal DSDT walker:

**Existing in `cluu_acpi`:**
- `find_rsdp(space_token)` → RSDP
- `find_fadt_from_rsdp(space_token, &rsdp)` → FADT
- MCFG parse → ECAM base

**New in drivermgr (or `cluu_acpi`):**
- Walk DSDT/SSDT for `Device()` objects
- Extract `_HID` (PNP IDs) and `_CRS` (I/O ports, IRQ)
- **Minimal AML**: parse only the Device() / Name(_HID) / ResourceTemplate structures. Do NOT execute arbitrary AML methods.
- ~500-1000 LOC

**Devices published to tree:**
- PS/2 keyboard: `PNP0303`, I/O 0x60/0x64, IRQ 1
- PS/2 mouse: `PNP0F13`, I/O 0x60/0x64, IRQ 12
- Serial: `PNP0501`, I/O 0x3F8, IRQ 4
- PIT: `PNP0100`, I/O 0x40, IRQ 0
- RTC: `PNP0B00`, I/O 0x70, IRQ 8

**Not parsed (v2+):**
- `_PRT` (PCI IRQ routing — needs full AML)
- `_TZ` (thermal zones)
- `_PSx` (device power states)

## 11. IRQ routing (F10.A) — [Oracle amended]

PIC 8259 + LAPIC. drivermgr reads `irq_line` from PCI config (offset 0x3C).

**[Oracle] Kernel already broadcasts shared IRQs.** Verified in
`kernel/src/devices/irq.rs`:
- `attach(irq, endpoint_id)` (line 45): up to `MAX_ENDPOINTS_PER_IRQ = 4`
  endpoints can attach to the SAME IRQ line.
- `dispatch_irq(irq, label, data)` (line 63): iterates ALL attached
  endpoints, calls `try_send` to EACH. **Broadcast, not last-write-wins.**
- Comment at line 28-32: "dispatch_irq delivers to ALL attached endpoints;
  each driver checks its own device ISR to decide if the IRQ is its own."

**[Oracle] Design simplification — no SharedIrqTable needed.** The original
design proposed a `SharedIrqTable` in drivermgr with one shared endpoint
per IRQ line. This was wrong for the kernel's rendezvous model (one
receiver wakes per message on a single endpoint). The correct pattern —
which the kernel already supports — is: **each driver attaches its OWN
endpoint to the IRQ line.** Kernel broadcasts to all. Each driver checks
its device ISR, handles or re-arms.

This is exactly what `init/src/context.rs` already does today (separate
`irq_token` per driver). drivermgr's role for IRQs is just: **pass the IRQ
line number + scoped IRQ_HANDLE token to each driver at spawn.** Each
driver calls `irq_attach(its_token, its_ep, its_irq_line)` itself.

```rust
// drivermgr: pass IRQ info at spawn (no endpoint management)
fn spawn_driver(node: &DeviceNode, rule: &BindRule) -> Pid {
    let irq_token = devmgr::mint_irq_cap(node.irq_line);  // scoped IRQ_HANDLE token
    procmgr::spawn(
        image: &rule.driver_image,
        envelope: { irq_token, ... },
        params: { PARAM_IRQ_LINE: node.irq_line, ... },
    )
}

// driver: attaches its OWN endpoint to the IRQ line
fn run() -> Result<()> {
    let irq_token = info.tokens[TOKEN_EXTRA_2];
    let irq_line = info.params[PARAM_IRQ_LINE] as u8;
    let irq_ep = endpoint_create(ipc_token)?;
    irq_attach(irq_token, irq_ep, irq_line as usize)?;
    loop {
        ipc_recv(irq_ep, &mut buf)?;
        if !my_device_interrupted() { continue; }  // check ISR
        irq_ack(irq_token)?;
        handle_interrupt();
    }
}
```

drivermgr does NOT create or manage shared IRQ endpoints. Each driver is
self-sufficient for IRQ. This is simpler than the original design and
matches the existing kernel capability.

**v2+:** IO-APIC routing (`InvokeOp::IrqRoute`), MSI/MSI-X
(`InvokeOp::MsiConfigure`), ACPI `_PRT` parsing. All need kernel changes +
full AML.

## 12. Migration path (phased)

### Phase D1: drivermgr + drivermon skeleton (non-disruptive)

- Create `userspace/drivermgr/` — new primordial service, spawned by init after vfs
- Create `userspace/drivermon/` — new primordial service
- drivermgr links `cluu_acpi` + `driver-framework` (pci::enumerate)
- drivermgr scans PCI + ACPI at boot, publishes device tree to `/proc/devices`
- drivermon registers exit-notify endpoint, receives PROC_EXIT_LABEL
- **Neither spawns drivers yet.** Just observes. Existing drivers keep self-probing.
- Exit criteria: `ls /proc/devices` shows all PCI + ACPI devices. Serial log shows drivermgr + drivermon ready.

### Phase D2: driver.toml + bind rules (non-disruptive)

- Add `driver.toml` support to `container-build` (new directive or post-build generation)
- Emit `driver.toml` for virtio-blk, virtio-net, virtio-snd, virtio-9p, usb-input, kbd, mouse
- drivermgr loads bind rules from `/var/images/*/driver.toml` at boot
- drivermgr logs "device X matched driver Y" but does not spawn yet
- Exit criteria: serial log shows bind-rule matches for all present devices. No behavior change.

### Phase D3: devmgr cap minting + drivermgr spawn (disruptive, opt-in)

- devmgr gains `DEVMGR_MINT_PCI_CAP_LABEL` + `DEVMGR_MINT_IRQ_CAP_LABEL`
- devmgr receives `pci_access_root_token` + `irq_handle_root_token` at boot (init wiring change)
- drivermgr asks devmgr for scoped caps, passes to procmgr:spawn
- Drivers gain "param-driven init" path: read PARAM_DEVICE_PATH, query drivermgr, claim, cap walk
- Keep self-probe path as fallback during migration
- Toggle: `/etc/drivermgr.toml` has `spawn_mode = "observe" | "spawn" | "hybrid"`
- Exit criteria: boot with `spawn_mode = "spawn"`, all drivers come up via drivermgr

### Phase D4: restart + fallback (additive)

- drivermon monitors driver exits via notify endpoint
- drivermon catches faults via ThreadSetFaultEndpoint (drivermgr registers at spawn)
- Implement supervision policy (F7.B+D): exit-code-aware + tiered criticality
- Implement fallback chain walking
- Exit criteria: kill a driver, system recovers. Kill 4x in 30s, system tries fallback or marks Failed.

### Phase D5: initrd minimization (cleanup)

- Remove virtio-9p, virtio-net, virtio-snd, netd, usb-input from initrd (load from userdisk via drivermgr)
- initrd = 9 spawned + virtio-blk.elf file + virtio-blk.driver.toml
- Exit criteria: `ls /dev/initrd` shows 9 files + virtio-blk artifacts. Boot to login works.

### Phase D6: ACPI enumeration (additive)

- Add minimal DSDT walker to `cluu_acpi` (Device() / _HID / _CRS)
- drivermgr publishes ACPI PNP devices to tree
- kbd and mouse migrate from autostart.toml to drivermgr bind (via ACPI PNP IDs)
- Exit criteria: `cat /proc/devices/acpi/PNP0303` shows PS/2 kbd. kbd driver binds via ACPI.

## 13. What stays the same

- **Kernel**: no new syscalls, no new InvokeOps for v1. The freeze rule (AGENTS.md §9) is respected. drivermgr/drivermon/devmgr changes are pure userspace.
- **devmgr**: stays the cap broker. Sync, leaf, existing IPC surface. Gains 2 new labels + 2 new boot tokens.
- **procmgr**: stays the authority broker + spawner. drivermgr calls procmgr:spawn — same as autostart.toml does today.
- **Cluufile model**: unchanged. `driver.toml` is a new file alongside `manifest.toml`, not a Cluufile change (could add a DRIVER directive to container-build to generate it, but the file itself is separate).
- **Capability tokens**: authority model untouched. devmgr mints, drivermgr passes through, drivers receive at spawn.
- **VFS**: unchanged. `/proc/devices` is a new procfs entry (procfs already supports arbitrary entries).
- **Registry**: unchanged. drivermgr publishes `drivermgr:query`, `drivermgr:device-added`, etc.

## 14. Open questions (post-Oracle — mostly resolved)

1. **[RESOLVED] drivermgr's own PCI_ACCESS token:** devmgr derives it from
   `pci_access_root_token` (which init derived from `root_token`). devmgr
   stays single holder of root tokens. drivermgr gets ONE full-scope
   pci_access_token for scanning.

2. **[RESOLVED] Boot-critical driver.toml in initrd:** file in initrd at
   `sys/virtio-blk.driver.toml`, discovered via `readdir("/dev/initrd/sys/")`.
   Uniform model, no hardcoding.

3. **[RESOLVED] drivermgr re-reads driver.toml after userdisk mounts:** yes,
   phase 2 does `readdir("/var/images/")` after VFS "mounted". Already-bound
   devices are NOT rebound (drivermgr skips devices already in Bound state).

4. **[DEFERRED to v2] drivermgr + drivermon mutual supervision:** for v1,
   both are primordial (init panics if either dies). v2: drivermon could
   restart drivermgr. Out of scope for v1.

5. **[CONFIRMED] Does drivermgr need its own async runtime?** Yes — it does
   IPC to procmgr (spawn), devmgr (mint), drivers (fault handling).
   Single-threaded async (`libcluu::async_runtime`, AGENTS.md §7) is the
   canonical deadlock-avoidance path. Oracle confirmed no deadlock cycle
   exists (Issue 1 — non-issue).

6. **[RESOLVED] Two-phase boot:** phase 1 reads initrd driver.toml + spawns
   virtio-blk; phase 2 (after VFS "mounted") reads userdisk driver.toml +
   spawns the rest. Synchronization via VFS "mounted" output (fires after
   ext2 is live, which requires virtio-blk serving).

7. **[OPEN] Testing:** a `drivermgr-probe` harness case (observe mode),
   `drivermgr-restart` case (kill driver, verify re-spawn),
   `drivermgr-fallback` case (make primary fail, verify fallback binds).
   To be specified in formal plan.

8. **[OPEN] DSDT parser scope:** minimal AML walk for Device() / _HID /
   _CRS. Need to handle NameOp, DeviceOp, ResourceTemplate. ~500-1000 LOC.
   Not a full AML interpreter. Validate with QEMU's DSDT first. To be
   specified in formal plan.

9. **[RESOLVED 2026-07-21] Shared PCI token for v1.** All drivers get the
   same derived `pci_access_token`. No per-BDF pretense. Simpler, honest
   about v1 limits. Reintroduce per-BDF when kernel enforces scope (v2,
   post-freeze). See §9 for details.
