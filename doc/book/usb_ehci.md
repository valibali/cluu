# USB EHCI Driver

The EHCI (Enhanced Host Controller Interface) driver provides USB 2.0
high-speed keyboard and mouse support in CLUU. It is a userspace driver —
the kernel has no USB code. The driver lives in `userspace/ehci-core/` and
the service binary in `userspace/usb-input/`.

## Why EHCI, not xHCI

CLUU targets QEMU 6.2. QEMU 6.2 has a bug where xHCI 64-bit MMIO register
writes (DCBAAP, CRCR, ERSTBA) silently lose address bits — only the RCS bit
sticks. This makes xHCI unusable. EHCI (USB 2.0) works correctly in QEMU 6.2
and is sufficient for keyboard/mouse input.

UHCI was also attempted (`userspace/uhci-core/`) but abandoned: on q35,
I/O port reads from `piix3-usb-uhci` return 0xFFFF (BAR0 not programmed by
BIOS, and manual programming to 0xC000 still reads 0xFFFF). `ich9-usb-uhci1`
is not found by PCI enumeration on q35.

## Architecture

```text
┌─────────────────────────────────────────────────────┐
│                  usb-input (binary)                  │
│                                                      │
│  DmaPool ──→ EhciController                         │
│               ├── probe()    — PCI enum, MMIO map   │
│               ├── reset()    — HC reset, CONFIGFLAG │
│               ├── start()    — periodic + async on  │
│               ├── reset_port() — port reset, speed  │
│               ├── set_address()                     │
│               ├── get_device_descriptor()           │
│               ├── set_configuration()               │
│               ├── set_idle() / set_protocol()       │
│               ├── setup_interrupt_in()              │
│               └── poll_interrupt()                  │
│                                                      │
│  driver-framework: PciDeviceInfo, BarInfo, enumerate│
│  dma-core: DmaPool, DmaRegion (phys + virt)         │
└─────────────────────────────────────────────────────┘
         │
         │ PCI MMIO (SpaceMap device pages)
         ▼
┌─────────────────────────────────────────────────────┐
│            QEMU usb-ehci controller                  │
│  MMIO registers (USBCMD, USBSTS, PORTSC, ...)       │
│  Async schedule: circular QH list                   │
│  Periodic schedule: frame list → intr QH            │
└─────────────────────────────────────────────────────┘
         │
         │ USB 2.0 high-speed
         ▼
    usb-kbd  /  usb-mouse
```

### Crate dependencies

- **`ehci-core`** — the EHCI controller driver (regs, queue, controller)
- **`driver-framework`** — shared PCI enumeration, device probe, IRQ guards
- **`dma-core`** — DMA-able memory pool with physical address tracking
- **`libcluu`** — IPC, registry, syscall wrappers, boot info

### QEMU invocation

The harness configures QEMU with:

```
-device usb-ehci,id=ehci
-device usb-kbd,bus=ehci.0
-device usb-mouse,bus=ehci.0
```

QEMU's `usb-kbd`/`usb-mouse` default to USB 2.0 high-speed (`usb_version=2`),
working directly on `usb-ehci` without companion controllers.

## EHCI register interface

`regs.rs` defines `EhciRegs`, providing MMIO access to all EHCI operational
registers. Key registers:

| Register | Offset | Purpose |
|---|---|---|
| USBCMD | 0x20 | Run/Stop, HCReset, ASEN, PSEN, frame list size |
| USBSTS | 0x24 | HCHalted, PCD, FLR, USBERRINT, USBINT, ASS, PSS, IAA, REC |
| PERIODICLISTBASE | 0x2C | Physical address of periodic frame list |
| ASYNCLISTBASE | 0x18 | Physical address of async head QH |
| CONFIGFLAG | 0x40 | Route ports to EHCI (bit 0) |
| PORTSC[n] | 0x44+4n | Port status/control (PED, PR, POWNER, speed) |

USBCMD bit layout: bit 0 = RUN/STOP, bit 1 = HCRESET, bit 4 = PSEN,
bit 5 = ASEN, bit 6 = IAAD (doorbell), bits 3:2 = frame list size.

## Queue structures

### Queue Head (QH)

48 bytes, 32-byte aligned. `#[repr(C, align(32))]` on `QueueHead`.

| Offset | Field | QEMU name | Purpose |
|---|---|---|---|
| 0x00 | `next_qh` | `next` | QH horizontal link pointer (T-bit, type bits 2:1, ptr 31:5) |
| 0x04 | `charac` | `epchar` | Device address (6:0), endpoint (11:8), speed (13:12), DTC (14), H-bit (15), max_pkt (26:16), control_ep (27) |
| 0x08 | `cap` | `epcap` | Mult (3:0), S-mask (7:4), C-mask (11:8) |
| 0x0C | `cur_td` | `current_qtd` | Written by HC — current qTD being processed |
| 0x10 | `overlay.next_td` | `next_qtd` | Next qTD link (T-bit, ptr 31:5) |
| 0x14 | `overlay.alt_next_td` | `altnext_qtd` | Alternate next qTD (for short packet) |
| 0x18 | `overlay.token` | `token` | qTD token (PID, CERR, IOC, total_bytes, toggle, active) |
| 0x1C | `overlay.buffers[5]` | `bufptr[5]` | 5 buffer page pointers (4KB each) |

**Critical**: The `QtD` struct embedded as the QH overlay must NOT have
`align(32)`. See [Gotchas](#qtd-align32-overlay-padding) below.

### Queue Element Transfer Descriptor (qTD)

32 bytes, 32-byte aligned when standalone. `#[repr(C)]` on `QtD`.

| Offset | Field | Purpose |
|---|---|---|
| 0x00 | `next_td` | Next qTD link (T-bit, ptr 31:5) |
| 0x04 | `alt_next_td` | Alternate next qTD (T-bit, ptr 31:5) |
| 0x08 | `token` | PID (9:8), CERR (11:10), IOC (15), total_bytes (30:16), toggle (31), active (7), status (6:0) |
| 0x0C | `buffers[5]` | 5 buffer page pointers |

qTD token encoding:
- PID: bits 9:8 — 0=OUT, 1=IN, 2=SETUP
- CERR: bits 11:10 — error counter (typically 3)
- IOC: bit 15 — interrupt on complete
- Total bytes: bits 30:16 — bytes to transfer
- Data toggle: bit 31 — 0=DATA0, 1=DATA1
- Active: bit 7 — HC processes qTD when set, clears on completion
- Status: bits 6:0 — halt, data buffer error, babble, xact err, missed microframe, split, ping

### Link pointer encoding

EHCI link pointers (QH next, qTD next) use bits 4:0 for flags:

| Bits | Field | Values |
|---|---|---|
| 0 | T-bit | 0 = valid pointer, 1 = terminate (end of list) |
| 2:1 | Type | 00 = iTD, 01 = QH, 10 = siTD, 11 = FSTN |
| 4:3 | Reserved | Must be 0 |
| 31:5 | Pointer | 32-byte aligned physical address |

**For async schedule QH links**: type must be 01 (QH), T-bit must be 0.
The OR value is `0x2` (bit 1 set). NOT `0x1` (which sets T-bit = terminate).

## Transfer flow

### Control transfer

A USB control transfer is a SETUP → optional DATA → STATUS sequence:

1. **SETUP qTD**: PID=SETUP, 8 bytes, data_toggle=0, buffer = setup packet
2. **DATA qTD** (optional): PID=IN or OUT, N bytes, data_toggle=1, buffer = data
3. **STATUS qTD**: PID=IN (for OUT control) or OUT (for IN control), 0 bytes, data_toggle=1, IOC=1

The QH overlay's `next_td` points to the SETUP qTD. The qTDs are chained
via their `next_td` pointers. The QH is linked into the async schedule
(circular list with `async_head`).

### Async schedule

The async schedule is a circular linked list of QHs:

```text
async_head (H-bit) ──→ transfer_QH ──→ async_head (circular)
```

- `async_head` has the H-bit (head of reclamation) set in `charac`.
- `ASYNCLISTBASE` points to `async_head`.
- `async_head.next_qh` → transfer_QH (type=QH, T-bit=0)
- `transfer_QH.next_qh` → async_head (type=QH, T-bit=0, circular)

QEMU's EHCI state machine:
1. `waitlisthead` — sets REC flag, scans for H-bit QH from ASYNCLISTBASE
2. `fetchentry` — validates T-bit and type bits
3. `fetchqh` — reads QH, checks H-bit reclamation, checks overlay token
4. `advancequeue` — follows overlay `next_qtd` to find qTD
5. `fetchqtd` — reads qTD, checks ACTIVE bit
6. `execute` — writes `current_qtd`, executes USB transaction
7. `writeback` — writes qTD status back to memory, advances queue

The reclamation logic: `waitlisthead` sets REC. On first visit to the
H-bit QH, REC is cleared and processing continues. On the second visit
(after traversing the circular list back to the H-bit QH), REC is clear
→ schedule stops (EHCI 4.8.6). `execute` re-arms REC to keep the
schedule alive during active transfers.

### Device enumeration pipeline

```text
1. probe()           — PCI enumerate class 0x0C0320 (EHCI), map MMIO BAR
2. reset()           — HCRESET, wait, CONFIGFLAG=1
3. start()           — periodic frame list, async head QH, USBCMD=RUN|ASEN|PSEN
4. find_connected_port() — scan PORTSC for CCS (connect status)
5. reset_port()      — PR=1, wait, PED=true, read speed from PORTSC
6. set_address(2)    — control transfer: SET_ADDRESS to device addr 0
7. get_device_descriptor() — control transfer: GET_DESCRIPTOR(DEVICE) to addr 2
8. set_configuration(1) — control transfer: SET_CONFIGURATION to addr 2
9. set_idle()        — HID class request: SET_IDLE (no reports for 0 ms)
10. set_protocol(0)  — HID class request: SET_PROTOCOL (boot protocol)
11. setup_interrupt_in() — queue interrupt IN QH on periodic schedule
12. poll_interrupt()  — poll interrupt QH for HID report data
```

### Interrupt polling

After enumeration, the driver sets up an interrupt IN endpoint on the
periodic schedule:

1. Allocate a QH with `devaddr`, `ep=1`, `eps=speed`, `max_pkt=8`
2. Allocate a qTD with PID=IN, `total_bytes=max_pkt`, ACTIVE
3. Link QH into periodic frame list (all 1024 entries point to intr QH)
4. Frame list entries use type=QH (`| 0x2`), T-bit=0

The service binary polls the qTD's ACTIVE bit in its main loop. When the
device sends a HID report, the HC clears ACTIVE and writes the report data
to the qTD's buffer. The driver reads the report, re-arms the qTD, and
continues.

## Capability model

The `usb-input` binary is spawned with:
- `TOKEN_SPACE` — for `SpaceMap` (MMIO BAR mapping, DMA pool)
- `TOKEN_EXTRA_0` — own IPC endpoint
- `TOKEN_EXTRA_1` — PCI access token (`PCI_ACCESS` right)
- `TOKEN_EXTRA_2` — IRQ token (reserved for future MSI/IRQ support)

No new syscalls or InvokeOps were added. All hardware access goes through
existing `SpaceMap` and `port_in/out` paths.

## Gotchas

### qtd-align32-overlay-padding

`QtD` must NOT have `#[repr(C, align(32))]` when embedded as the QH overlay.
The `align(32)` on `QtD` forces the `overlay` field in `QueueHead` to
offset 0x20 instead of 0x10 (16 bytes of padding between `cur_td` and
`overlay`). QEMU reads 12 dwords (48 bytes) from the QH and expects the
overlay at offset 0x10. With padding, QEMU reads zeros for `next_qtd`,
`token`, and `bufptr` — causing it to chase address 0 instead of the real
qTDs.

Standalone qTDs get 32-byte alignment from `DmaPool::alloc(size, 32)`,
not from the type's alignment. The `align(32)` on `QtD` is both
unnecessary and harmful.

### qh-link-type-bits

QH link pointers must use `| 0x2` (type=QH, bits 2:1=01, T-bit=0). Using
`| 0x1` sets the T-bit (Terminate), making QEMU treat the link as invalid
and never follow it. This is the #1 most common EHCI driver bug.

### periodic-frame-list-type-bits

Periodic frame list entries are link pointers too — they need `| 0x2`
(type=QH, T-bit=0), not `| 0x1` (terminate).

### qemu-ehci-async-reclamation

QEMU's EHCI async schedule stops after traversing the circular list back
to the H-bit QH a second time (REC flag clear). The `execute` state
re-arms REC, keeping the schedule alive during active transfers. But if
the QH has no active qTDs, the schedule stops after one full traversal.
This is correct behavior — the driver doesn't need to disable/re-enable
the async schedule between transfers. Just link the new QH and the
schedule will pick it up on the next scan.

### qemu-62-xhci-mmio-bug

QEMU 6.2's xHCI has a bug where 64-bit MMIO register writes (DCBAAP,
CRCR, ERSTBA) silently lose address bits — only the RCS bit sticks. This
makes xHCI unusable. Use EHCI instead.

### qemu-q35-uhci-io-port-0xffff

On q35, UHCI I/O port reads return 0xFFFF. `piix3-usb-uhci` BAR0 is not
programmed by BIOS, and manual programming to 0xC000 still reads 0xFFFF.
`ich9-usb-uhci1` is not found by PCI enumeration. Use EHCI instead.

## Testing

The `usb_input_probe` harness case boots CLUU with `-device usb-ehci`,
`-device usb-kbd`, `-device usb-mouse`, spawns the `usb-input` container,
and validates the `USB_INPUT_OK` marker on serial output.

```bash
cd python
python3 -m cluu_harness --case usb_input_probe --no-build
```

The test verifies the full pipeline: PCI probe → HC reset → port reset →
SET_ADDRESS → GET_DESCRIPTOR → SET_CONFIGURATION → SET_IDLE →
SET_PROTOCOL → interrupt queue → `USB_INPUT_OK`.
