# boot-to-login

**Type:** GIF
**Status:** FAILED (no frames)
**Resolution:** 1728x900 BGRA32
**Captured:** 2026-07-06 22:09:19

## Description

Firmware → kernel boot → service spawn → login prompt. Captured at 3s intervals during boot, before any login.

## Capture conditions

- QEMU: `qemu-system-x86_64 -machine q35 -m 1G -accel kvm`
- Framebuffer: 1728x900 BGRA32 (3686400 bytes)
- Login: root/root (HU QWERTZ sendkey sequence)
- Capture method: `pmemsave` via QEMU HMP monitor
- Command: `scripts/capture_cluu_shots.py boot-to-login`

## Referenced by

- `userspace/init/src/main.rs`
- `userspace/console/src/main.rs`
