# arm64-port — Planning Draft

**Status:** awaiting-approval
**Pending action:** write `.omo/plans/arm64-port.md` (DONE)
**Approach:** UNCLEAR intent — adopted best-practice defaults, no user interview.

## Destination
Port CLUU (kernel + userspace build harness) to ARM64 (AArch64), preserving BOOTBOOT as the bootloader. x86_64 must stay green.

## Key decisions adopted (user can veto)
1. **QEMU `-M raspi3b` + existing `aarch64-rpi` BOOTBOOT loader.** BOOTBOOT spec (2021 1st ed, ~/Downloads/bootboot_spec_1st_ed.pdf) confirms aarch64-rpi is the ONLY aarch64 loader (lines 309-312, 635). QEMU raspi3b emulates RPi3. No new loader authored.
2. **BCM2836 interrupt controller** (RPi3 native), NOT ARM GIC. RPi3 has no GIC.
3. **No FDT in v1.** RPi path uses `bootboot.aarch64.mmio_ptr`.
4. **cfg-gated parallel modules** (matches `kernel/src/architecture/mod.rs:1` pattern), NOT a trait refactor of x86_64. Kernel near freeze (AGENTS.md §9).
5. **No SMP, no virtio-blk/userdisk, no PS/2 input on aarch64 v1.** Bring-up target = "kernel boots, init runs, scheduler starts." Verified via serial, NOT login.
6. **`--arch` flag on xtask**, single binary.
7. **Cluufiles parameterized via `CLUU_ARCH` env** in container-build, not a mirrored `containers-aarch64/` tree.

## Context gathered
- BOOTBOOT spec fully extracted (`/tmp/bootboot_spec.txt`, 1260 lines).
- Kernel arch surface: 38 files with `target_arch|x86_64`. Arch switch at `kernel/src/architecture/mod.rs`. x86_64 submodules: abi_check, apic, gdt, idt, interrupts, pic, syscall, spectre, tsc + 4 asm files. Key arch-dep: main.rs `_start` naked_asm, sched/context.rs (x86 Context), mm/vmm.rs (x86_64 crate OffsetPageTable), mm/physmap.rs (x86 high-half), mm/boot/bootboot.rs (CR3 walk), syscall.rs (MSRs + PerCpuData), klibcluu uart/simd/sha256 (already have cfg stubs).
- Build harness: full map from explore agent (bg_db734486). triplets/{x86_64-cluu-kernel,x86_64-cluu-user}.json, xtask 3444 LOC with 7 hardcoded target paths, .cargo/config.toml, kernel/.cargo/config.toml, rust-toolchain.toml, 120 Cluufiles, container-build, newlib, python harness, CI.
- BOOTBOOT struct `kernel/src/bootboot.rs` already has `arch_aarch64` variant.
- `tools/mkbootimg/` already vendors aarch64-rpi firmware (start.elf, fixup.dat, bootcode.bin).

## Stuck bg tasks cancelled
- bg_4827114a (kernel arch map) — cancelled after 1h27m, stuck on read.
- bg_0ef9418b (BOOTBOOT ARM64 loaders) — cancelled after 1h25m, stuck.
Sufficient context from direct reads + bg_db734486 to write the plan.

## Plan
`.omo/plans/arm64-port.md` — 36 todos + 4 final verification, 6 waves, XL effort, High risk.

## Approval gate
Present brief to user. Wait for explicit okay or veto of any of the 7 decisions.
