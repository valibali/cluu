# login-and-shell

**Type:** GIF
**Status:** FAILED (no frames)
**Resolution:** 1728x900 BGRA32
**Captured:** 2026-07-06 22:10:40

## Description

Login as root → shell prompt appears. Uses the standard root/root credential sendkey sequence with a 12s prefix sleep (kbd attaches at ~9.4s, login window at ~9.8s).

## Capture conditions

- QEMU: `qemu-system-x86_64 -machine q35 -m 1G -accel kvm`
- Framebuffer: 1728x900 BGRA32 (3686400 bytes)
- Login: root/root (HU QWERTZ sendkey sequence)
- Capture method: `pmemsave` via QEMU HMP monitor
- Command: `scripts/capture_cluu_shots.py login-and-shell`

## Referenced by

- `userspace/shell/src/main.rs`
- `userspace/console/src/main.rs`
