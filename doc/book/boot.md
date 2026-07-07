# Boot Flow

CLUU boots from UEFI firmware through the kernel into a fully-userspace service
stack. The sequence is deterministic and each step is load-bearing.

## 1. Firmware (UEFI/OVMF)

OVMF firmware loads the boot image (`target/cluu.img`) from the IDE drive. The
image contains the kernel as `sys/core` and an initrd with boot-critical
primordials.

## 2. Kernel entry — `_start` (`kernel/src/main.rs`)

Naked assembly entry point. Reads APIC ID via CPUID leaf 1, bits 31:24 of EBX. Parks
non-BSP cores in a `hlt` loop (SMP bring-up is not implemented). Switches the
BSP to a 64 KiB, 16-byte aligned kernel stack (`BSP_STACK`). Jumps to `kstart`.

## 3. Kernel init — `kstart` (`kernel/src/main.rs`)

The Rust kernel entry. Runs the full init sequence in order:

```text
1.  UART init (COM2 for serial logging)
2.  Logger init
3.  GDT init (kernel/user segments, TSS)
4.  PIC init (8259 PIC)
5.  IDT init (exception handlers, interrupt handlers)
6.  PS/2 aux init (mouse port enable — one-shot before userspace driver)
7.  SMAP/SMEP enable
8.  Spectre V2 mitigation
9.  SysV-ABI check
10. Syscall MSRs (SYSCALL/SYSRET) + per-CPU data
11. IPC fast-path toggles (rendezvous_direct + register_fast)
12. MM init (physmap, page tables, CR3 switch)
13. Heap init (linked_list_allocator backed by PMM)
14. Frame table init (advisory per-frame ownership)
15. Crypto/token init (kernel secret, token table)
16. TSC calibration
17. APIC timer (250 Hz tick)
18. bootstrap::init (creates init thread as PID 1)
19. Telemetry snapshot
20. ThreadManager::start (scheduler starts, BSP falls through to idle_loop)
```

### Why the ordering matters

- UART before logger: the logger writes to UART.
- GDT before PIC before IDT: interrupt delivery needs a valid IDT, which needs
  a valid GDT (for the TSS and kernel stack).
- Syscall MSRs before any syscall: the SYSCALL/SYSRET path needs MSRs.
- MM before heap: heap allocation requires PMM + VMM.
- Crypto before token: tokens are HMAC-signed with the kernel secret.
- TSC before APIC timer: the timer frequency is calibrated from TSC.
- `bootstrap::init` before `ThreadManager::start`: the init thread must exist
  before the scheduler starts.

## 4. Init — PID 1 (`userspace/init/src/main.rs`)

First userspace process. Reads boot snapshot + boot manifest from initrd, then
launches `SERVICE_LIST`:

| Service | Role |
|---------|------|
| `registry` | Name → endpoint mapping |
| `timeserver` | Clock service |
| `devmgr` | Device manager |
| `root-procmgr` | System-scope process manager |
| `vfs` | Virtual filesystem |
| `virtio-blk` | Block device driver |
| `tpmd` | TPM 2.0 daemon |

Each service is launched via `wiring::launch_service`. Primordial services get
non-zero exit cookies so init can detect their death.

After launch, init:
- Extends TPM PCRs for measured boot (`measured_boot.rs`).
- Runs sealed storage + attestation PoCs (`sealed_storage.rs`,
  `attestation.rs`).
- Monitors `primordial_exit_recv` endpoint: any message = a primordial died.

### Primordial exit codes

| Code | Action |
|------|--------|
| 42 | Poweroff (ACPI 0x604) |
| 43 | Reboot (0xCF9) |
| other | Halt |

Primordial death is unrecoverable → halt always. Non-primordial services get
cookie 0 and may exit silently.

## 5. Root-procmgr — boot autostart (`userspace/root-procmgr/src/main.rs`)

Root-procmgr boots the autostart services from `autostart.toml`:

- `kbd` — keyboard driver
- `console` — framebuffer console
- `vtmgr` — VT manager
- `tty` — text-VT terminal service (per VT, spawned on demand)

Then presents the login prompt.

## 6. Login flow

1. User types credentials at the login prompt (tty or cluuterm).
2. procmgr authenticates against `/etc/users.toml` (password-hashed, TPM-backed
   via `tpmd`).
3. procmgr resolves the user's envelope from `/etc/envelopes.toml` (mount
   policy, env vars, profile).
4. procmgr `SESSION_CREATE`:
   - Spawns `session-procmgr` with the session envelope.
   - Spawns `session-vfs` with the session VFS view.
5. Session-procmgr spawns the user's shell (`/bin/shell`) or cluuterm.
6. The shell runs with the session's authority: narrowed VFS view, session-scoped
   caps.

## 7. Session lifecycle

- **Create**: `SESSION_CREATE` → spawn session-procmgr + session-vfs → spawn
  shell/cluuterm.
- **Handoff**: `SESSION_HANDOFF` — VT handoff between sessions.
- **Destroy**: `SESSION_DESTROY` → cap revocation → cascade teardown → all
  session processes lose authority and exit.

## Boot timing

Typical boot on KVM:

```text
0.0s   Firmware hands off to kernel
0.5s   kstart begins (UART, GDT, PIC, IDT)
1.0s   MM init, heap, crypto/token
2.0s   APIC timer, bootstrap::init, ThreadManager::start
3.0s   init launches boot services
5.0s   VFS mounts /, /dev, /proc
7.0s   kbd, console, vtmgr, tty started
9.4s   kbd attaches (PS/2)
9.8s   login window appears
12.0s  (harness credential sendkey prefix sleep ends here)
```

The serial log line `fb @<PHYS> <SIZE> bytes` marks framebuffer availability.
The `[USER] shell: ready` marker marks shell readiness.
