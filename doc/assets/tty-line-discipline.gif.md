# tty-line-discipline

**Type:** GIF
**Status:** FAILED (no frames)
**Resolution:** 1728x900 BGRA32
**Captured:** 2026-07-06 22:12:51

## Description

Type a line, backspace mid-line, Ctrl-C, ↑/↓ history, enter. Shows cooked-mode line discipline (ICANON, ECHO, ^C/^Z/^D).

## Capture conditions

- QEMU: `qemu-system-x86_64 -machine q35 -m 1G -accel kvm`
- Framebuffer: 1728x900 BGRA32 (3686400 bytes)
- Login: root/root (HU QWERTZ sendkey sequence)
- Capture method: `pmemsave` via QEMU HMP monitor
- Command: `scripts/capture_cluu_shots.py tty-line-discipline`

## Referenced by

- `userspace/tty/src/main.rs`
- `userspace/libcluu/src/tty_core/line_discipline.rs`
