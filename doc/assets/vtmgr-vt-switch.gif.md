# vtmgr-vt-switch

**Type:** GIF
**Status:** FAILED (no frames)
**Resolution:** 1728x900 BGRA32
**Captured:** 2026-07-06 22:11:32

## Description

Alt-F1 → VT0, Alt-F2 → VT1, type in each, switch back. VT4 is owned by the compositor; text VTs are 1-3.

## Capture conditions

- QEMU: `qemu-system-x86_64 -machine q35 -m 1G -accel kvm`
- Framebuffer: 1728x900 BGRA32 (3686400 bytes)
- Login: root/root (HU QWERTZ sendkey sequence)
- Capture method: `pmemsave` via QEMU HMP monitor
- Command: `scripts/capture_cluu_shots.py vtmgr-vt-switch`

## Referenced by

- `userspace/vtmgr/src/main.rs`
- `userspace/console/src/main.rs`
