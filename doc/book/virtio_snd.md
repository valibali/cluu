# virtio-snd — audio device driver

virtio-snd is the userspace driver for QEMU's `virtio-snd-pci` device.
It is a leaf service: no downstream IPC, no async runtime needed. audiod
is its sole client (enforced by `MAX_SESSIONS=1` + registry
unregistration after first client connects).

## Boot sequence

1. PCI discovery — `pci::find_virtio_device` for vendor/device `0x1059`.
2. Enable device, read IRQ line.
3. DMA pool alloc (128 pages at `0x5100_0000`).
4. Modern transport setup — `ModernPciTransport`, read device features,
   write `VERSION_1`.
5. Read `SndConfig` — jacks/streams/chmaps/controls counts.
6. 4 virtqueues: control, event, TX, RX (64 descriptors each).
7. IRQ attach — `IrqSource` with kernel-issued scoped IRQ token.
8. Post one event buffer on the event queue.
9. `set_driver_ok`.
10. Control self-test (jack info, set_params, prepare, start, stop,
    release).
11. TX self-test (16 silence periods).
12. Registry publish as `snddev:main`.

## PCM session lifecycle

### Open (`AUDIO_OPEN_SESSION`)

Client sends `PcmParams { format, rate, channels, period_bytes }`.
virtio-snd:
1. Clamps `period_bytes` to `[64, 4096]` aligned 4.
2. Calls `pcm_set_params(stream_id, buffer_bytes=8192, period_bytes,
   channels, format, rate)`.
3. Calls `pcm_prepare`.
4. Allocates 8 xfer + 8 status DMA regions (one per ring slot).
5. Stores per-session `period_bytes`.
6. Returns `[status, session_id, driver_space_token, grant_target_va,
   actual_period_bytes]`.

### Submit (`AUDIO_SUBMIT_PCM`)

Client sends `[session_id, period_id, pcm_len, page_index]`. virtio-snd:
1. Validates `pcm_len <= session.period_bytes`, `page_index < 8`.
2. Computes PCM source VA:
   ```
   slot_stride = (period_bytes + 4095) & !4095
   pcm_va = grant_target_va + page_index * slot_stride
   ```
   **The slot stride is page-aligned**, matching audiod's grant layout.
3. Translates `pcm_va` to physical address via `virt_to_phys`.
4. Builds a 3-descriptor TX chain:
   - desc[0] = `PcmXfer` (4B, OUT)
   - desc[1] = PCM data (≤period_bytes, OUT, from granted page)
   - desc[2] = `PcmStatus` (8B, IN/WRITE)
5. Submits to TX virtqueue, notifies device.

### Completion

When the device finishes a period, it raises an IRQ. virtio-snd:
1. Reads ISR status, drains used rings.
2. For each used TX descriptor: looks up the cookie → (session_id,
   period_id, completion_endpoint, page_index).
3. Sends `AUDIO_COMPLETE` to the client's completion endpoint.
4. Acks the IRQ (`irq.ack()` — EOI to APIC/PIC).

### Close (`AUDIO_CLOSE`)

Calls `pcm_stop` + `pcm_release`, frees session.

## IRQ tokens

virtio-snd receives a scoped IRQ token from init (derived from the
kernel's root IRQ token via `token_derive_scoped_irq`). The token
carries `IRQ_HANDLE | IRQ_ACK` rights for the device's IRQ line.

`irq_ack()` sends EOI to the APIC. Without ack, the IRQ line stays
asserted and no further IRQs fire — the driver falls back to 10 ms
polling. See `gotchas/cluu-irq-token-scoping-not-supported.md`.

## Capabilities query

`AUDIO_QUERY_CAPS` (0x605) returns three bitmasks:
- `formats` — bit N set ⇒ `PCM_FMT_N` supported (S16, S32)
- `rates` — bit N set ⇒ `PCM_RATE_N` supported (all standard rates)
- `channels` — bit N set ⇒ N channels supported (mono, stereo)

audiod queries this before opening a session to pick the output rate
(44100 preferred, 48000 fallback).

## Address layout

```
0x5100_0000  DMA_POOL_VA      (128 pages — virtqueue rings + control buffers)
0x5200_0000  MMIO_VA_BASE     (virtio PCI capability BAR mapping)
0x5300_0000  GRANT_TARGET_VA  (client PCM pages granted in, 8 slots)
```

These are hardcoded — virtio-snd is a singleton driver with a fixed
address-space layout. No other process maps these VAs. Dynamic mapping
(via `space_map_auto`) is used by audiod for its per-client SHM rings,
not by virtio-snd.

## Self-test

On boot, virtio-snd runs a control self-test (jack info query, PCM
set_params/prepare/start/stop/release cycle) and a TX self-test (16
silence periods). The TX self-test may fail with `Busy` if QEMU's host
audio backend is unavailable — this is non-fatal (the driver still
registers and serves real clients).
