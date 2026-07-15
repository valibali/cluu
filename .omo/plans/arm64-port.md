# arm64-port - Work Plan

## TL;DR (For humans)

**What you'll get:** CLUU boots and runs a userspace `init` on ARM64 (AArch64) under QEMU, built with the same `cargo xtask build` command (with a new `--arch` selector), while the existing x86_64 build stays green. BOOTBOOT remains the bootloader — no new bootloader is authored.

**Why this approach:** BOOTBOOT's only AArch64 reference loader is `aarch64-rpi` (Raspberry Pi 3/4). QEMU ships a `raspi3b` machine that emulates the RPi3, so we boot CLUU on QEMU `-M raspi3b` using the existing BOOTBOOT RPi loader — this preserves "BOOTBOOT must stay" without writing a new loader. The kernel already uses `#[cfg(target_arch = ...)]` arch-gated modules (see `kernel/src/architecture/mod.rs`, `klibcluu/src/uart.rs`, `simd.rs`, `sha256.rs`), so the port adds a parallel `architecture/aarch64/` module rather than refactoring x86_64 code — the kernel is near freeze (AGENTS.md §9, minimal churn).

**What it will NOT do:**
- Will NOT write a new BOOTBOOT loader (aarch64-qemu-virt / aarch64-uefi). v1 uses the existing `aarch64-rpi` loader on QEMU `raspi3b`.
- Will NOT support SMP on AArch64 in v1. BSP boots; APs park in a WFE loop (mirrors the x86_64 AP path).
- Will NOT support virtio-blk / userdisk on AArch64 v1. RPi3 has no virtio; filesystem comes from the initrd only. The ext2 userdisk path is x86_64-only until a virtio or SD-card driver is added.
- Will NOT support PS/2 keyboard/mouse on AArch64 v1. RPi3 has no PS/2 controller; QEMU `raspi3b` has no PS/2. Framebuffer console output works; interactive input does not. Bring-up is verified via serial, not login.
- Will NOT refactor the x86_64 kernel into a trait-abstracted arch layer. Existing trait seams (BootInfoProvider, PageAllocator, VirtualMemoryMapper) are reused; everything else gets a parallel aarch64 module behind `cfg`.
- Will NOT touch the x86_64 build. Every change is `cfg`-gated or arch-parameterized; x86_64 must still build and boot.

**Effort:** XL
**Risk:** High — AArch64 paging/SVC/context-switch is a full arch bring-up; BOOTBOOT aarch64-rpi on QEMU raspi3b is the load-bearing external dependency (if it doesn't hand off cleanly, the whole boot strategy changes).
**Decisions I made for you:**
1. **QEMU `-M raspi3b` + existing `aarch64-rpi` BOOTBOOT loader** (not `virt`, not a new loader). Veto → write an aarch64-qemu-virt loader (much larger scope).
2. **BCM2836 interrupt controller** (RPi3 native), NOT ARM GIC. RPi3 has no GIC. Veto → switch to `-M virt` + GICv3 (requires new BOOTBOOT loader).
3. **No FDT in v1.** RPi path uses `bootboot.aarch64.mmio_ptr`. FDT is a `virt`-machine concern, deferred.
4. **cfg-gated parallel modules** (matches existing `architecture/mod.rs` pattern), NOT a trait refactor of x86_64. Kernel is near freeze.
5. **No SMP, no virtio, no PS/2 input on aarch64 v1.** Bring-up target is "kernel boots, init runs, serial log shows scheduler start." Veto → expand scope.
6. **`--arch` flag on xtask** (not a separate `xtask-aarch64`). Single binary, arch-parameterized.
7. **Cluufiles parameterized via `CLUU_ARCH` env** read by `container-build`, not a mirrored `containers-aarch64/` tree. Avoids duplicating ~120 Cluufiles.

Your next move: approve, or veto any of the 7 decisions above. Full execution detail follows below.

---

> TL;DR (machine): XL, High — port CLUU kernel + userspace build harness to AArch64 on QEMU raspi3b via existing BOOTBOOT aarch64-rpi loader; cfg-gated parallel arch modules; no SMP/virtio/PS2 in v1; x86_64 must stay green.

## Scope
### Must have
- AArch64 kernel boots on `qemu-system-aarch64 -M raspi3b` using the existing BOOTBOOT `aarch64-rpi` loader.
- New `triplets/aarch64-cluu-kernel.json` + `triplets/aarch64-cluu-user.json` custom target specs.
- New `kernel/link.aarch64.ld` + `userspace/user.aarch64.ld` linker scripts (BOOTBOOT aarch64 VA layout).
- New `kernel/src/architecture/aarch64/` module: entry, mmu, exceptions, syscall (SVC), interrupts (BCM2836), timer (ARM generic), context, per-cpu.
- aarch64 `BootbootAdapter` (no CR3 walk — use TTBR0 + `mmio_ptr`).
- aarch64 `Context` struct + context-switch `.S` (gas, not nasm).
- klibcluu PL011 MMIO UART for aarch64 (PortIo trait impl already stubbed).
- xtask `--arch x86_64|aarch64` flag on `build`, `run`, `kernel`, `userspace`, `create-initrd`, `create-disk-image`, `doctor`.
- xtask aarch64 QEMU invocation (`qemu-system-aarch64 -M raspi3b`).
- xtask aarch64 assembly step (gas for `.S`, replacing nasm for aarch64).
- `scripts/build-newlib.sh` aarch64 branch (`aarch64-cluu-elf`).
- aarch64 `crt0.S` + `build_c_program` aarch64 clang/gcc path.
- container-build `CLUU_ARCH` env parameterization (no Cluufile duplication).
- `.cargo/config.toml` + `kernel/.cargo/config.toml` + `rust-toolchain.toml` aarch64 additions.
- Python harness `--arch aarch64` smoke case.
- x86_64 build + boot unchanged (regression gate on every todo).

### Must NOT have (guardrails, anti-slop, scope boundaries)
- No new BOOTBOOT loader. No `aarch64-qemu-virt`, no `aarch64-uefi` authored here.
- No ARM GIC driver in v1 (BCM2836 only).
- No FDT parsing in v1.
- No SMP bring-up on aarch64 in v1 (AP WFE park only).
- No virtio-blk / userdisk / ext2 mount on aarch64 v1.
- No PS/2, USB, or keyboard/mouse driver on aarch64 v1.
- No refactor of x86_64 `vmm.rs` / `physmap.rs` / `syscall.rs` into a trait-abstracted arch layer. Existing trait seams are reused; everything else gets a parallel aarch64 module.
- No `as any`, `@ts-ignore`-equivalent, `unwrap`, or `panic!` in new aarch64 code (AGENTS.md §9, rust-best-practices skill).
- No new syscalls (AGENTS.md §2). The syscall *entry mechanism* changes (SVC vs SYSCALL); the syscall *numbers and handlers* do not.
- No runtime ACL (AGENTS.md §3). Capability model is arch-independent.
- No x86_64 regression. Every todo's acceptance includes "x86_64 build still passes."
- No commit/push without explicit request.

## Verification strategy
> Zero human intervention - all verification is agent-executed.
- Test decision: tests-after (kernel is near freeze; new arch modules get unit tests for pure logic — paging math, context layout — via `rustc --test` on host where possible; full verification is boot-on-QEMU).
- Evidence: `.omo/evidence/task-<N>-arm64-port.<ext>` (serial logs, QEMU output, build logs).
- Per-todo regression gate: `cargo xtask build --arch x86_64` must still succeed after every kernel/xtask change.
- Bring-up evidence (Wave 6): QEMU serial capture showing the expected marker string at each stage:
  - T31: `=== Kernel Boot Started ===` on aarch64 serial.
  - T32: `Memory Management Ready` on aarch64 serial.
  - T33: `Init thread created successfully!` on aarch64 serial.
  - T34: `Starting scheduler and launching init thread` on aarch64 serial.
  - T35: first init userspace log line (or serial proof init ELF was entered).
  - T36: Python harness `--arch aarch64 --case l2_login --no-build` returns PASS (or, if login is blocked by no-keyboard, the earliest harness case that needs only serial).

## Execution strategy
### Parallel execution waves

### Dependency matrix
| Todo | Depends on | Blocks | Can parallelize with |
| --- | --- | --- | --- |
| 1 | — | 5,6,7,9,11,12,18,20,22 | 2,3,4 |
| 2 | — | 5,6,7,11,18 | 1,3,4 |
| 3 | — | all kernel/user builds | 1,2,4 |
| 4 | — | 18-25 | 1,2,3 |
| 5 | 1,2,3 | 6,7,8,9,10,11,12 | — |
| 6 | 5 | 31 | 7,8,9,10,11,12 |
| 7 | 5 | 32 | 6,8,9,10,11,12 |
| 8 | 5 | 31 | 6,7,9,10,11,12 |
| 9 | 5 | 31 | 6,7,8,10,11,12 |
| 10 | 5 | 31 | 6,7,8,9,11,12 |
| 11 | 5 | 34 | 6,7,8,9,10,12 |
| 12 | 5 | 32 | 6,7,8,9,10,11 |
| 13 | 3 | 31 | 14,15,16,17 |
| 14 | 3 | 31 | 13,15,16,17 |
| 15 | 3 | 31 | 13,14,16,17 |
| 16 | 5 | 31 | 13,14,15,17 |
| 17 | 3 | 31 | 13,14,15,16 |
| 18 | 1,4 | 31 | 19,20,21,22,23,24,25 |
| 19 | 4 | 31 | 18,20,21,22,23,24,25 |
| 20 | 1,4 | 31 | 18,19,21,22,23,24,25 |
| 21 | 1,4 | 31 | 18,19,20,22,23,24,25 |
| 22 | 1,4 | 31,36 | 18,19,20,21,23,24,25 |
| 23 | 4 | — | 18,19,20,21,22,24,25 |
| 24 | 4 | 25 | 18,19,20,21,22,23,25 |
| 25 | 24 | 35 | 18,19,20,21,22,23,24 |
| 26 | 3 | 27,28 | 29,30 |
| 27 | 26 | 35 | 28,29,30 |
| 28 | 26 | 35 | 27,29,30 |
| 29 | 3 | 35 | 26,27,28,30 |
| 30 | 3 | 35 | 26,27,28,29 |
| 31 | 6,7,8,9,10,12,13,14,15,16,17,18,19,20,21,22 | 32 | — |
| 32 | 31 | 33 | — |
| 33 | 32 | 34 | — |
| 34 | 11,33 | 35 | — |
| 35 | 25,27,28,29,30,34 | 36 | — |
| 36 | 22,35 | — | — |

## Todos
> Implementation + Test = ONE todo. Never separate.
<!-- APPEND TASK BATCHES BELOW THIS LINE WITH edit/apply_patch - never rewrite the headers above. -->
- [ ] 1. Create aarch64 target spec JSONs
  What to do / Must NOT do: Create `triplets/aarch64-cluu-kernel.json` and `triplets/aarch64-cluu-user.json`. Mirror the x86_64 specs (`triplets/x86_64-cluu-kernel.json`, `triplets/x86_64-cluu-user.json`) but: `llvm-target: aarch64-unknown-none-elf`, `arch: aarch64`, `data-layout` per LLVM aarch64 ELF (drop `p270/p271/p272` and `f80:128` — no x87; keep `e-m:e-i64:64-i128:128-n32:64-S128`), `target-pointer-width: 64`, drop `disable-redzone` (aarch64 has no redzone), drop `features: -mmx,+retpoline` → `features: +v8a,+neon` (or `+fp-armv8`), drop `code-model: kernel` → `code-model: small` for kernel too (aarch64 kernel high-half is reached via TTBR1, not a code model). `pre-link-args.ld.lld` → `["-T", "kernel/link.aarch64.ld", "target/asm/syscall_entry.o"]` (kernel) and `["-Tuserspace/user.aarch64.ld"]` (user). Keep `linker: rust-lld`, `linker-flavor: ld.lld`, `panic-strategy: abort`, `exe-suffix: .elf`, `has-thread-local: false`, `position-independent-executables: false`, `dynamic-linking: false`. Must NOT invent new fields; must NOT modify the x86_64 JSONs.
  Parallelization: Wave 1 | Blocked by: — | Blocks: 5,6,7,9,11,12,18,20,22
  References (executor has NO interview context): `triplets/x86_64-cluu-kernel.json` (25 lines, full template), `triplets/x86_64-cluu-user.json` (25 lines). BOOTBOOT spec PDF at `~/Downloads/bootboot_spec_1st_ed.pdf` — §"Kernel Format" confirms `EM_AARCH64 (183)` ELF and aarch64 higher-half via linker symbols. LLVM aarch64 data-layout: see `rustc --print target-spec-json --target aarch64-unknown-none` for the canonical layout.
  Acceptance criteria (agent-executable): Both files exist; `cargo build --manifest-path kernel/Cargo.toml --target triplets/aarch64-cluu-kernel.json -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem` fails only on missing arch code (not on malformed spec — error must be a compile error in source, not a spec-parse error). `cargo xtask build --arch x86_64` still succeeds (regression). `python -c "import json; json.load(open('triplets/aarch64-cluu-kernel.json'))"` parses.
  QA scenarios (name the exact tool + invocation): happy: `rustc --print cfg --target triplets/aarch64-cluu-kernel.json` runs without error and prints `target_arch="aarch64"`. failure: `python -c "import json; json.load(open('triplets/aarch64-cluu-kernel.json'))"` succeeds (no JSON error). Evidence `.omo/evidence/task-1-arm64-port.txt`
  Commit: Y | feat(build): add aarch64-cluu-kernel and aarch64-cluu-user target specs

- [ ] 2. Create aarch64 linker scripts
  What to do / Must NOT do: Create `kernel/link.aarch64.ld` mirroring `kernel/link.ld` structure but with aarch64 BOOTBOOT VA layout. BOOTBOOT spec (§"Machine State") maps kernel at `0xffffffffffe02000` on x86_64; for aarch64 the higher-half is the TTBR1 range — use the SAME addresses as x86 (`mmio=0xfffffffff8000000`, `fb=0xfffffffffc000000`, `bootboot=0xffffffffffe00000`, `environment=0xffffffffffe01000`, kernel at `0xffffffffffe02000`) because BOOTBOOT's aarch64-rpi loader uses the same negative-address layout per the spec's sample linker script (the spec ships ONE `link.ld` for both arches). Keep `ENTRY(_start)`, single `boot PT_LOAD` segment, `__text_start`/`__bss_end` symbols (consumed by `mm/boot/bootboot.rs:71-77`). Create `userspace/user.aarch64.ld` mirroring `userspace/user.ld` but: TLS variant I (aarch64) — `.tbss` BEFORE `.tdata` (not after), drop the "variant II (x86_64)" comment, keep `.text@0x400000`, stack top `0x80000000`. Must NOT change `kernel/link.ld` or `userspace/user.ld`.
  Parallelization: Wave 1 | Blocked by: — | Blocks: 5,6,7,11,18
  References: `kernel/link.ld` (74 lines, full template), `userspace/user.ld` (74 lines, full template), BOOTBOOT spec §"A sample linker script" (lines 1026-1065 of `/tmp/bootboot_spec.txt` — confirms single shared link.ld for both arches). aarch64 TLS variant I: see AAPCS64 §"Thread-local storage".
  Acceptance: `rust-lld -T kernel/link.aarch64.ld --o /dev/null /dev/null 2>&1 || true` (script parses without linker error about unknown symbols). `cargo xtask build --arch x86_64` still succeeds. `grep -c '__text_start' kernel/link.aarch64.ld` ≥ 1.
  QA: happy: `ld.lld -T kernel/link.aarch64.ld -o /tmp/test_aarch64.elf /dev/null 2>&1 | head` (no syntax error). Evidence `.omo/evidence/task-2-arm64-port.txt`
  Commit: Y | feat(build): add aarch64 linker scripts for kernel and userspace

- [ ] 3. Add aarch64 to toolchain + cargo config
  What to do / Must NOT do: Edit `rust-toolchain.toml` to add `"aarch64-unknown-linux-gnu"` to `targets` (host target for build-std; the custom `aarch64-cluu-*` targets are built via `-Z build-std` with `rust-src`). Add `[target.aarch64-cluu-kernel]` and `[target.aarch64-cluu-user]` sections to `.cargo/config.toml` mirroring the x86_64 sections (same rustflags — `-C lto=off`, `-C link-arg=-nostdlib`, `-C link-arg=-static`; for kernel add `-C opt-level=0`, `-C force-frame-pointers=yes`). Make `kernel/.cargo/config.toml` `[build] target` arch-conditional: the simplest path is to REMOVE the hardcoded `target = "../triplets/x86_64-cluu-kernel.json"` and require explicit `--target` everywhere (xtask already passes `--target` explicitly, so this is safe and matches how userspace already works). Keep `[unstable] build-std` section. Must NOT break x86_64 builds.
  Parallelization: Wave 1 | Blocked by: — | Blocks: all kernel/user builds
  References: `rust-toolchain.toml` (currently `targets = ["x86_64-unknown-linux-gnu"]`), `.cargo/config.toml` (25 lines), `kernel/.cargo/config.toml` (hardcoded default target).
  Acceptance: `rustup target list --installed` includes `aarch64-unknown-linux-gnu` after `rustup show` (rust-toolchain.toml auto-installs). `cargo build --manifest-path kernel/Cargo.toml --target triplets/x86_64-cluu-kernel.json -Z build-std=core,alloc` still works (no regression from removing kernel/.cargo default). `cargo xtask build --arch x86_64` still succeeds.
  QA: happy: `cargo xtask doctor` shows aarch64 toolchain present. Evidence `.omo/evidence/task-3-arm64-port.txt`
  Commit: Y | chore(toolchain): add aarch64 target and cargo config for arm64 port

- [ ] 4. Add --arch flag to xtask
  What to do / Must NOT do: Add `--arch <x86_64|aarch64>` to the `Cli` struct in `xtask/src/main.rs` (default `x86_64`). Thread it through `build`, `run`, `kernel`, `userspace`, `create-initrd`, `create-disk-image`, `doctor`, and the rich-build pipeline. Replace the 7 hardcoded `triplets/x86_64-cluu-*.json` path constructions in `build_kernel`, `build_klibcluu`, `build_userspace`, `build_libcluu`, `build_syscalls`, `build_init_crate`, `build_single_container` with arch-aware lookup: `format!("triplets/{}-cluu-kernel.json", arch)` / `format!("triplets/{}-cluu-user.json", arch)`. Replace `CLUU_TARGET_TRIPLET`, `NEWLIB_CLUU_TRIPLET`, `CLUU_CLANG_TARGET` consts with arch-aware functions. Replace `target/x86_64-cluu-kernel/` and `target/x86_64-cluu-user/` path constructions in `create_initrd` with `format!("target/{}-cluu-kernel/{}", arch, cargo_profile)`. Must NOT remove the x86_64 paths — they are the default. Must NOT touch the rich-build DAG structure (only the task args).
  Parallelization: Wave 1 | Blocked by: — | Blocks: 18-25
  References: `xtask/src/main.rs:25-28` (consts), `:1469-1513` (build_kernel), `:1383-1467` (build_userspace), `:1560-1647` (create_initrd), `:2516-2700` (build_klibcluu/build_libcluu/build_syscalls/build_init_crate/build_single_container).
  Acceptance: `cargo xtask build --arch x86_64` succeeds (regression). `cargo xtask build --arch aarch64` runs and fails only on missing aarch64 kernel source (not on path resolution). `cargo xtask doctor --arch x86_64` still works.
  QA: happy: `cargo xtask build --arch x86_64 --ui linear 2>&1 | tail` shows "✓ Build complete". Evidence `.omo/evidence/task-4-arm64-port.txt`
  Commit: Y | feat(xtask): add --arch flag and arch-aware target path resolution

- [ ] 5. Create kernel architecture/aarch64/ skeleton
  What to do / Must NOT do: Create `kernel/src/architecture/aarch64/mod.rs` declaring submodules: `mmu`, `exceptions`, `interrupts` (BCM2836), `syscall`, `timer`, `context`, `percpu`. Each submodule is a stub that compiles under `cfg(target_arch = "aarch64")` — functions return `unimplemented!()` or a sensible default, with a `klibcluu::info!("aarch64 <subsystem>: stub")` so boot fails loudly at the right place, not silently. Add `#[cfg(target_arch = "aarch64")] pub mod aarch64;` to `kernel/src/architecture/mod.rs` (mirroring the existing x86_64 gate on line 1). Create empty `.S` files: `kernel/src/architecture/aarch64/{boot,context,exceptions,syscall_entry}.S` (gas syntax, not nasm). Must NOT modify `kernel/src/architecture/x86_64/`.
  Parallelization: Wave 2 | Blocked by: 1,2,3 | Blocks: 6,7,8,9,10,11,12
  References: `kernel/src/architecture/mod.rs` (15 lines — the cfg gate pattern), `kernel/src/architecture/x86_64/mod.rs` (31 lines — submodule list to mirror).
  Acceptance: `cargo build --manifest-path kernel/Cargo.toml --target triplets/aarch64-cluu-kernel.json -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem 2>&1 | tail` compiles the aarch64 cfg branch (fails only on unimplemented stubs being called, or on downstream x86_64-crate usage that isn't yet cfg-gated — capture the error list). x86_64 build still passes.
  QA: happy: aarch64 kernel crate compiles with cfg(target_arch=aarch64). Evidence `.omo/evidence/task-5-arm64-port.txt`
  Commit: Y | feat(kernel): add aarch64 architecture module skeleton

- [ ] 6. Write aarch64 _start + kstart arch branch
  What to do / Must NOT do: In `kernel/src/main.rs`, gate the existing x86_64 `_start` naked_asm behind `#[cfg(target_arch = "x86_64")]` and add a parallel `#[cfg(target_arch = "aarch64")] pub unsafe extern "C" fn _start()` with aarch64 naked_asm: read `MPIDR_EL1` (`mrs x0, mpidr_el1; and x0, x0, #0xff`), compare to `bootboot.bspid` (load via `adrp x1, bootboot; ldrh w2, [x1, #0x0C]`), if not BSP branch to AP park (`1: wfe; b 1b`), else load `BSP_STACK` top into `sp`, `b kstart`. Gate the existing `kstart` body's x86_64-specific calls (`architecture::x86_64::gdt::init()`, `pic::init()`, `idt::init()`, `ps2::init_aux()`, SMEP/SMAP asm, `spectre::init`, `abi_check`, `syscall::init`, `tsc::calibrate`, `apic::init_timer`) behind `#[cfg(target_arch = "x86_64")]` and add aarch64 equivalents calling the stub modules from T5 in the same order. Gate the panic handler's `cli; hlt` and the idle_loop `hlt` behind cfg and add aarch64 `msr daifset, #7` + `wfi`. Must NOT change the x86_64 kstart sequence.
  Parallelization: Wave 2 | Blocked by: 5 | Blocks: 31
  References: `kernel/src/main.rs:45-91` (_start x86 naked_asm), `:101-272` (kstart body), `:274-280` (idle_loop), `:283-318` (panic handler). BOOTBOOT spec §"A sample Symmetric Multi Processing code" (lines 1087-1094 of `/tmp/bootboot_spec.txt` — aarch64 MPIDR_EL1 park pattern).
  Acceptance: aarch64 kernel compiles. `cargo xtask build --arch x86_64` still succeeds. The aarch64 `_start` uses only `naked_asm!` (no inline asm in safe code).
  QA: happy: `cargo build --manifest-path kernel/Cargo.toml --target triplets/aarch64-cluu-kernel.json -Z build-std=core,alloc 2>&1 | grep -c error` is 0 or errors are only downstream. Evidence `.omo/evidence/task-6-arm64-port.txt`
  Commit: Y | feat(kernel): add aarch64 _start entry and arch-gate kstart

- [ ] 7. Write aarch64 MMU + physmap module
  What to do / Must NOT do: Create `kernel/src/architecture/aarch64/mmu.rs` implementing AArch64 EL1 paging: TTBR0_EL1 (user) / TTBR1_EL1 (kernel) split, 4-level page tables (PGD→PUD→PMD→PTE, 9 bits each, 4KB granule), MAIR_EL1 attributes (Device-nGnRnE, Normal WT, Normal NC for physmap), TCR_EL1 (T0SZ=16, T1SZ=16, IRGN0/1 WB, ORGN0/1 WB, SH0/1 inner, TG0/TG1 4K), SCTLR_EL1 enable. Provide `pub unsafe fn init(max_phys: u64)` that builds initial TTBR1 tables mapping: kernel image (from `__text_start`/`__bss_end`), physmap (aarch64 physmap base — choose `0xffff_8000_0000_0000` same as x86 for trait reuse, in TTBR1 range), BOOTBOOT mmio/fb/info regions. Provide `pub unsafe fn enable(pml4_phys: u64)` analogue that writes TTBR1_EL1 + flush TLB (`tlbi vmalle1is`) + sets SCTLR_EL1.M. Add aarch64 `physmap` counterpart or cfg-gate `mm/physmap.rs` `PHYS_MAP_BASE` (the constant is arch-neutral; the CR3 references in comments are not load-bearing). Create `kernel/src/mm/aarch64_vmm.rs` (or cfg-gate `mm/vmm.rs`) providing `create_initial_page_tables` and `switch_to_page_tables` with aarch64 semantics — reuse the `mm/traits.rs` `VirtualMemoryMapper` / `PageAllocator` traits so `mm/mod.rs::init()` calls work unchanged. Must NOT use the `x86_64` crate (it is x86-only); use raw `u64` page-table entry manipulation like `vmm.rs:pte_flags` does.
  Parallelization: Wave 2 | Blocked by: 5 | Blocks: 32
  References: `kernel/src/mm/mod.rs:168-242` (init sequence — must call the same trait methods), `kernel/src/mm/vmm.rs:42-104` (pte_flags pattern to mirror for aarch64 PTE bits: bits 0=Valid,1=WRITE,2=USER(? aarch64 uses AP),10=UXN,53=PXN, etc.), `kernel/src/mm/physmap.rs:33` (PHYS_MAP_BASE), `kernel/src/mm/traits.rs` (PageAllocator, VirtualMemoryMapper, PageFlags traits to satisfy). ARM ARM B3.6 for aarch64 page table formats.
  Acceptance: aarch64 kernel compiles. Unit test (host, `rustc --test`): aarch64 PTE address-extraction mask `0x0000_FFFF_FFFF_F000` matches expected bits. x86_64 build unchanged.
  QA: happy: `rustc --edition 2021 --test kernel/src/architecture/aarch64/mmu.rs -o /tmp/aarch64_mmu_test 2>&1` (if host-compilable) or compile-only check. Evidence `.omo/evidence/task-7-arm64-port.txt`
  Commit: Y | feat(kernel): add aarch64 MMU, physmap, and TTBR1 paging

- [ ] 8. Write aarch64 exceptions + VBAR vector table
  What to do / Must NOT do: Create `kernel/src/architecture/aarch64/exceptions.rs` + `kernel/src/architecture/aarch64/exceptions.S`. Set VBAR_EL1 to the vector table base (16 vectors × 128 bytes, 4 exception classes × 4 types: Synchronous, IRQ, FIQ, SError). Each vector saves GP regs (X0-X30), SP_EL0, ELR_EL1, SPSR_EL1 to a frame, calls a Rust handler (`extern "C" fn exception_handler(frame: &ExceptionFrame, kind: u64)`), restores, `eret`. Provide `pub fn init()` that writes VBAR_EL1 via `msr vbar_el1, <addr>` + `isb`. Map x86 exception semantics: synchronous→current-EL-with-SPx is the "kernel exception" path; SVC is a separate synchronous vector distinguished by ESR_EL1.EC=0x15. Must NOT implement IRQ routing here (BCM2836 driver is T10); IRQ vector just calls a stub that will be wired in T10.
  Parallelization: Wave 2 | Blocked by: 5 | Blocks: 31
  References: `kernel/src/architecture/x86_64/idt.rs` (43KB — the x86 IDT to mirror semantically), `kernel/src/architecture/x86_64/interrupts.asm` (24KB — the x86 handler asm to mirror structurally). ARM ARM D1.10 for VBAR vector table layout.
  Acceptance: aarch64 kernel compiles with exceptions.S assembled. `grep -c 'vbar_el1' kernel/src/architecture/aarch64/exceptions.S` ≥ 1. x86_64 build unchanged.
  QA: happy: gas assembles exceptions.S. Evidence `.omo/evidence/task-8-arm64-port.txt`
  Commit: Y | feat(kernel): add aarch64 exception vectors and VBAR_EL1 setup

- [ ] 9. Write aarch64 syscall (SVC + ERET)
  What to do / Must NOT do: Create `kernel/src/architecture/aarch64/syscall.rs` + `syscall_entry.S`. AArch64 has no SYSCALL/SYSRET MSR pair at EL1; use the SVC instruction (exception class 0x15). The SVC vector (from T8) saves user X0-X30, SP_EL0, ELR_EL1, SPSR_EL1, calls `syscall_dispatch` (the SAME `crate::syscall::dispatch_syscall` used by x86 — syscall numbers and handlers are arch-independent per AGENTS.md §2). Replace `PerCpuData` (x86 GS-base) with aarch64 per-CPU via `TPIDR_EL1` (or a static for BSP in v1 since no SMP). aarch64 SysV ABI: X0-X7 args, X0 return, X9-X15 caller-saved, X19-X28 callee-saved — different from x86_64 SysV (RDI/RDI/RDX/R8/R9 + RAX return). The `SyscallArgs` struct in `kernel/src/syscall/mod.rs` must be populated from X0-X7, not RDI/RSI/etc. Provide `pub unsafe fn init()` that installs the SVC vector (part of VBAR setup in T8) — no MSR LSTAR/STAR/FMASK equivalent needed. Must NOT change syscall numbers or `dispatch_syscall`.
  Parallelization: Wave 2 | Blocked by: 5 | Blocks: 31
  References: `kernel/src/architecture/x86_64/syscall.rs:1-200` (PerCpuData layout, init), `:333-417` (MSR init to replace), `kernel/src/architecture/x86_64/syscall_entry.asm` (24KB — the x86 entry to mirror semantically), `kernel/src/syscall/mod.rs` (SyscallArgs, dispatch_syscall — arch-neutral), `kernel/src/syscall/handlers.rs` (handlers — arch-neutral).
  Acceptance: aarch64 kernel compiles. `grep -c 'svc' kernel/src/architecture/aarch64/syscall_entry.S` ≥ 1. x86_64 build unchanged.
  QA: happy: gas assembles syscall_entry.S. Evidence `.omo/evidence/task-9-arm64-port.txt`
  Commit: Y | feat(kernel): add aarch64 SVC syscall entry and per-cpu via TPIDR

- [ ] 10. Write aarch64 BCM2836 interrupt controller + ARM generic timer
  What to do / Must NOT do: Create `kernel/src/architecture/aarch64/interrupts.rs` (BCM2836 IRQ controller — NOT ARM GIC; RPi3 uses BCM2836) and `kernel/src/architecture/aarch64/timer.rs` (ARM generic timer via CNTV_CTL_EL0 / CNTV_TVAL_EL0 / CNTVCT_EL0, replacing x86 APIC timer + TSC). BCM2836: IRQ_SOURCE/IRQ_ENABLE/IRQ_DISABLE regs at the MMIO base from `bootboot.aarch64.mmio_ptr`. Provide `pub fn init()` (enable IRQs, set vector in T8's IRQ vector), `pub fn init_timer(hz: u32)` (program CNTV timer, route IRQ). Provide `pub fn calibrate() -> u64` returning the timer frequency (read CNTFRQ_EL0) — replaces `tsc::calibrate()`. `klibcluu::set_tsc_hz` is arch-neutral; feed it CNTFRQ_EL0. Must NOT implement GIC. Must NOT touch x86 apic.rs/pic.rs/tsc.rs.
  Parallelization: Wave 2 | Blocked by: 5 | Blocks: 31
  References: `kernel/src/architecture/x86_64/apic.rs` (5.5KB — APIC timer to mirror), `kernel/src/architecture/x86_64/tsc.rs` (2.6KB — TSC calibrate to mirror), `kernel/src/architecture/x86_64/pic.rs` (5.4KB — to understand what's being replaced), `kernel/src/main.rs:229-238` (TSC + APIC init calls to mirror). BCM2836 datasheet §7 for IRQ controller regs. ARM ARM D8.4 for generic timer.
  Acceptance: aarch64 kernel compiles. `grep -c 'cntfrq_el0\|CNTV_CTL' kernel/src/architecture/aarch64/timer.rs` ≥ 1. x86_64 build unchanged.
  QA: happy: compile-only. Evidence `.omo/evidence/task-10-arm64-port.txt`
  Commit: Y | feat(kernel): add aarch64 BCM2836 IRQ and ARM generic timer

- [ ] 11. Write aarch64 Context struct + context switch
  What to do / Must NOT do: Create `kernel/src/architecture/aarch64/context.rs` defining `#[repr(C)] pub struct Context` with: X0-X30 (31 GP regs), SP, PC, PSTATE, TTBR0 (user page table root — aarch64 uses TTBR0 for user, TTBR1 for kernel, vs x86 CR3 for both). Total size and alignment must satisfy aarch64 AAPCS64 (16-byte stack alignment at function entry). Provide `Context::for_new_thread(entry, stack, ttbr0)` setting PSTATE=0x3C5 (EL0, IRQs enabled, AArch64), SP=stack-16 (AAPCS64 entry alignment), PC=entry. Create `kernel/src/architecture/aarch64/context.S` with `context_switch(old: *mut Context, new: *const Context)` saving X19-X30 (callee-saved per AAPCS64) + SP + PC into old, loading from new, `ret` — the aarch64 equivalent of the x86 context.asm. Gate the x86 `Context` in `kernel/src/sched/context.rs` behind `#[cfg(target_arch = "x86_64")]` and re-export the aarch64 `Context` under the same module path so `sched/thread.rs` and `sched/thread_manager.rs` compile unchanged. Must NOT change `sched/thread.rs` call sites — the `Context` type name and `for_new_thread` signature stay identical.
  Parallelization: Wave 2 | Blocked by: 5 | Blocks: 34
  References: `kernel/src/sched/context.rs` (264 lines — x86 Context to mirror field-for-field semantically), `kernel/src/sched/thread.rs:619-625` (entry/stack setters), `kernel/src/architecture/x86_64/interrupts.asm` (context switch asm). AAPCS64 §"The AArch64 Procedure Call Standard".
  Acceptance: aarch64 kernel compiles. `grep -c 'PSTATE\|pstate' kernel/src/architecture/aarch64/context.rs` ≥ 1. Unit test: `core::mem::size_of::<aarch64::Context>()` is a multiple of 16. x86_64 build unchanged (Context still 184 bytes).
  QA: happy: `rustc --test` on context.rs host-side checks size/alignment. Evidence `.omo/evidence/task-11-arm64-port.txt`
  Commit: Y | feat(kernel): add aarch64 CPU context and context switch

- [ ] 12. Write aarch64 BootbootAdapter (replace CR3 walk)
  What to do / Must NOT do: Create `kernel/src/mm/boot/bootboot_aarch64.rs` (or cfg-gate `bootboot.rs`) implementing `BootInfoProvider` for aarch64 WITHOUT the CR3/PML4 walk in `bootboot.rs:115-158`. AArch64 replacement for `kernel_physical_range`: read `bootboot.aarch64.mmio_ptr` and the mmap (the mmap parsing in `parse_max_physical_address` is arch-neutral — keep it); for the kernel physical range, walk TTBR1_EL1 instead of CR3, OR (simpler, recommended for v1) derive kernel phys from the mmap + the known kernel VA base (`0xffffffffffe02000`) by subtracting the TTBR1 offset — but aarch64 TTBR1 is a physical address of PGD, requiring a walk. Recommended v1 approach: BOOTBOOT identity-maps RAM in the positive range; the kernel's own physical address can be found by reading the mmap for the `MMAP_USED` entry covering the kernel image (BOOTBOOT marks the kernel region as used). `translate_bootboot_virt` (lines 190-228) needs an aarch64 TTBR walk equivalent. Gate the x86 `BootbootAdapter` behind `#[cfg(target_arch = "x86_64")]`. Must NOT break the `BootInfoProvider` trait contract (`mm/traits.rs` / `mm/boot/info.rs`).
  Parallelization: Wave 2 | Blocked by: 5 | Blocks: 32
  References: `kernel/src/mm/boot/bootboot.rs` (228 lines — x86 adapter to mirror), `kernel/src/bootboot.rs:77-88` (`arch_aarch64` struct with `mmio_ptr`), `kernel/src/mm/boot/info.rs` (BootInfoProvider trait). BOOTBOOT spec §"Memory Map Entries" (lines 533-552 of `/tmp/bootboot_spec.txt`).
  Acceptance: aarch64 kernel compiles with `mm::boot::BootbootAdapter` resolving to the aarch64 impl. `mm::init()` in `mm/mod.rs:168` calls trait methods unchanged. x86_64 build unchanged.
  QA: happy: compile-only; trait impl satisfies `BootInfoProvider`. Evidence `.omo/evidence/task-12-arm64-port.txt`
  Commit: Y | feat(kernel): add aarch64 BootbootAdapter without CR3 walk

- [ ] 13. klibcluu PL011 MMIO UART for aarch64
  What to do / Must NOT do: `klibcluu/src/uart.rs` already defines a `PortIo` trait with `X86PortIo` impl + `DummyUart` fallback for `#[cfg(not(target_arch = "x86_64"))]`. Add an `Aarch64MmioUart` impl of `PortIo` targeting the PL011 UART on RPi3 (MMIO base from `bootboot.aarch64.mmio_ptr` + PL011 offset 0x201000, or the QEMU raspi3b PL011 at the standard BCM2837 base). Wire the `#[cfg(target_arch = "aarch64")]` branch of `klibcluu::uart::init()` to initialize the PL011 (set baud 115200 8N1 via PL011 IBRD/FBRD/LCRH regs). The `COM2` global and `klibcluu::COM2.write_str()` calls in `main.rs`/`panic` must resolve to the PL011 on aarch64. Must NOT change the `COM2` API surface.
  Parallelization: Wave 3 | Blocked by: 3 | Blocks: 31
  References: `klibcluu/src/uart.rs` (full file — has the PortIo trait + X86PortIo + DummyUart stubs), `klibcluu/src/lib.rs` (COM2 global). PL011 TRM for register layout. QEMU raspi3b PL011 base: 0x3f201000 (BCM2837).
  Acceptance: aarch64 kernel compiles; `klibcluu::uart::init()` resolves to PL011 init. x86_64 build unchanged (X86PortIo still used).
  QA: happy: compile-only. Evidence `.omo/evidence/task-13-arm64-port.txt`
  Commit: Y | feat(klibcluu): add PL011 MMIO UART for aarch64

- [ ] 14. klibcluu sha256 aarch64 fallback
  What to do / Must NOT do: `klibcluu/src/crypto/sha256.rs` has a `#[cfg(target_arch = "x86_64")]` SHA-NI accel path + a generic fallback. Verify the generic fallback compiles and is selected under `cfg(target_arch = "aarch64")`. If an aarch64 SHA2 crypto extension path is desired, add it behind `#[cfg(all(target_arch = "aarch64", target_feature = "sha2"))]` using `sha256h`/`sha256su0`/`sha256su1` instructions — but this is OPTIONAL for v1; the generic fallback is sufficient. Must NOT break the x86_64 SHA-NI path.
  Parallelization: Wave 3 | Blocked by: 3 | Blocks: 31
  References: `klibcluu/src/crypto/sha256.rs` (full file).
  Acceptance: aarch64 klibcluu compiles; `cargo build -p klibcluu --target triplets/aarch64-cluu-kernel.json -Z build-std=core,alloc` succeeds. x86_64 SHA-NI path still compiles.
  QA: happy: `cargo build -p klibcluu --target triplets/aarch64-cluu-kernel.json -Z build-std=core,alloc 2>&1 | tail`. Evidence `.omo/evidence/task-14-arm64-port.txt`
  Commit: Y | feat(klibcluu): verify aarch64 sha256 generic fallback

- [ ] 15. klibcluu timing — aarch64 cntvct for tsc
  What to do / Must NOT do: Audit `klibcluu` for x86-specific `rdtsc` / `rdtscp` usage. Replace with aarch64 `mrs x0, cntvct_el0` under cfg. Any `klibcluu::set_tsc_hz` consumer must still work — CNTFRQ_EL0 is the aarch64 equivalent of TSC frequency. If `klibcluu` has a `read_tsc()` helper, add an aarch64 branch. Must NOT change the `set_tsc_hz` / `get_tsc_hz` API.
  Parallelization: Wave 3 | Blocked by: 3 | Blocks: 31
  References: `klibcluu/src/` (grep for `rdtsc`, `tsc`, `cpuid`). `kernel/src/main.rs:229-232` (set_tsc_hz call site).
  Acceptance: aarch64 klibcluu compiles with no x86-only intrinsics. x86_64 build unchanged.
  QA: happy: `cargo build -p klibcluu --target triplets/aarch64-cluu-kernel.json -Z build-std=core,alloc 2>&1 | grep -c rdtsc` is 0. Evidence `.omo/evidence/task-15-arm64-port.txt`
  Commit: Y | feat(klibcluu): use cntvct_el0 for timestamp on aarch64

- [ ] 16. kernel/build.rs aarch64 objcopy branch
  What to do / Must NOT do: `kernel/build.rs` hardcodes `objcopy -O elf64-x86-64 -B i386` for embedding `font.psf`. Add a `#[cfg(target_arch = "aarch64")]` branch using `objcopy -O elf64-littleaarch64 -B aarch64`. The font.psf payload is arch-neutral; only the ELF wrapping arch changes. Must NOT change the x86_64 objcopy args.
  Parallelization: Wave 3 | Blocked by: 5 | Blocks: 31
  References: `kernel/build.rs` (full file).
  Acceptance: aarch64 kernel builds with font.psf embedded. x86_64 build unchanged.
  QA: happy: `cargo build --manifest-path kernel/Cargo.toml --target triplets/aarch64-cluu-kernel.json -Z build-std=core,alloc 2>&1 | tail`. Evidence `.omo/evidence/task-16-arm64-port.txt`
  Commit: Y | fix(kernel): aarch64 objcopy target for font embedding

- [ ] 17. kernel Cargo.toml — gate x86_64 crate dep
  What to do / Must NOT do: In `kernel/Cargo.toml`, change `x86_64 = { workspace = true }` to `x86_64 = { workspace = true, optional = true }` and add `x86_64 = ["dep:x86_64"]` to `[features]`. Then cfg-gate every `use x86_64::...` in `kernel/src/` behind `#[cfg(target_arch = "x86_64")]` (the grep in exploration found 38 files; the load-bearing ones are `mm/vmm.rs`, `mm/physmap.rs`, `mm/boot/bootboot.rs`, `architecture/x86_64/*`, `main.rs`). The aarch64 modules must NOT pull `x86_64`. Alternatively (simpler, less churn): keep the dep non-optional but ensure no aarch64 code path references it — the x86_64 crate compiles fine on aarch64 host as a data-only dep, but its `instructions::hlt` / `registers::control::Cr3` are x86-inline-asm and WILL fail to compile for aarch64 target. So the dep MUST be gated or all touching code must be cfg'd. Prefer cfg-gating the `use` sites. Must NOT remove the dep from `workspace.dependencies` (host tests in `tests/kernel` still need it).
  Parallelization: Wave 3 | Blocked by: 3 | Blocks: 31
  References: `kernel/Cargo.toml:14` (`x86_64 = { workspace = true }`), `Cargo.toml:223` (workspace dep), grep output of 38 files with `target_arch|x86_64`.
  Acceptance: `cargo build --manifest-path kernel/Cargo.toml --target triplets/aarch64-cluu-kernel.json -Z build-std=core,alloc` compiles with zero x86_64-crate symbols in the aarch64 artifact (`nm target/aarch64-cluu-kernel/debug/deps/kernel-*.elf | grep -c x86_64` is 0 or near-0). x86_64 build unchanged.
  QA: happy: aarch64 kernel links without x86_64 crate. Evidence `.omo/evidence/task-17-arm64-port.txt`
  Commit: Y | refactor(kernel): gate x86_64 crate dependency behind cfg(target_arch=x86_64)

- [ ] 18. xtask build_kernel/build_klibcluu arch-aware
  What to do / Must NOT do: With T4's `--arch` flag in place, verify `build_kernel()` and `build_klibcluu()` resolve `triplets/{arch}-cluu-kernel.json` and pass it to cargo. The `assemble_nasm` call in `build_kernel` must be cfg'd: for aarch64, call a new `assemble_gas()` that runs `aarch64-linux-gnu-as` (or `clang -c -target aarch64-unknown-none-elf`) on `kernel/src/architecture/aarch64/*.S` into `target/asm/*.o`. Update the aarch64 kernel triplet's `pre-link-args` if the .o path differs. Must NOT break the x86_64 nasm path.
  Parallelization: Wave 4 | Blocked by: 1,4 | Blocks: 31
  References: `xtask/src/main.rs:1469-1558` (build_kernel + assemble_nasm).
  Acceptance: `cargo xtask kernel --arch aarch64` runs gas and attempts the aarch64 kernel build. `cargo xtask kernel --arch x86_64` still works.
  QA: happy: `cargo xtask kernel --arch aarch64 2>&1 | tail`. Evidence `.omo/evidence/task-18-arm64-port.txt`
  Commit: Y | feat(xtask): aarch64 assembly and kernel build path

- [ ] 19. xtask assemble_gas for aarch64 .S files
  What to do / Must NOT do: Add `fn assemble_gas(arch: &str) -> Result<()>` to xtask that, for aarch64, runs `aarch64-linux-gnu-as` (fallback: `clang -c --target=aarch64-unknown-none-elf`) on each `.S` file under `kernel/src/architecture/aarch64/`, outputting `target/asm/<basename>.o`. Wire `build_kernel` to call `assemble_gas("aarch64")` when `arch == "aarch64"` and `assemble_nasm` when `arch == "x86_64"`. The kernel triplet's `pre-link-args.ld.lld` already references `target/asm/syscall_entry.o` — this must resolve for both arches. Must NOT use nasm for aarch64 (nasm has no aarch64 support).
  Parallelization: Wave 4 | Blocked by: 4 | Blocks: 31
  References: `xtask/src/main.rs:1515-1558` (assemble_nasm template).
  Acceptance: `cargo xtask kernel --arch aarch64` assembles all `.S` files without error. `target/asm/syscall_entry.o` exists and is an aarch64 ELF (`file target/asm/syscall_entry.o` says "ELF 64-bit aarch64").
  QA: happy: `file target/asm/syscall_entry.o` after aarch64 build. Evidence `.omo/evidence/task-19-arm64-port.txt`
  Commit: Y | feat(xtask): add assemble_gas for aarch64 .S assembly

- [ ] 20. xtask create_initrd arch-aware
  What to do / Must NOT do: `create_initrd()` hardcodes `target/x86_64-cluu-kernel` and `target/x86_64-cluu-user`. With T4's arch flag, resolve these to `target/{arch}-cluu-kernel` and `target/{arch}-cluu-user`. The rest of the initrd logic (copy kernel as `sys/core`, copy 8 init primordials, write `boot.manifest`) is arch-neutral. Must NOT change the manifest format or HMAC key.
  Parallelization: Wave 4 | Blocked by: 1,4 | Blocks: 31
  References: `xtask/src/main.rs:1560-1647` (create_initrd).
  Acceptance: `cargo xtask create-initrd --arch aarch64` produces `target/initrd/sys/core` that is an aarch64 ELF (`file target/initrd/sys/core`). `cargo xtask create-initrd --arch x86_64` still works.
  QA: happy: `file target/initrd/sys/core` after aarch64 initrd build. Evidence `.omo/evidence/task-20-arm64-port.txt`
  Commit: Y | feat(xtask): arch-aware initrd staging

- [ ] 21. xtask create_disk_image arch-aware (mkbootimg for rpi)
  What to do / Must NOT do: `create_disk_image()` generates `target/mkbootimg.json` hardcoded for x86 (ISO9660, ESP partition). For aarch64, generate a config targeting the RPi boot format: SD-card image (not ISO), FAT partition with `KERNEL8.IMG` (the bootboot.img), `BOOTBOOT/INITRD` (the initrd), and the RPi firmware files (`start.elf`, `fixup.dat`, `bootcode.bin` — already vendored in `tools/mkbootimg/aarch64-rpi/` per the exploration). Use mkbootimg's multiarch initrd array form if both arches should coexist, OR emit arch-specific `target/cluu-{arch}.img`. Recommended: `target/cluu-aarch64.img` as a raw SD-card image. The `bootboot_config` content stays the same (`screen=1728x900`, `kernel=sys/core`). Must NOT change the x86_64 `target/cluu.img` output.
  Parallelization: Wave 4 | Blocked by: 1,4 | Blocks: 31
  References: `xtask/src/main.rs:1748-1813` (create_disk_image), `tools/mkbootimg/Makefile` (INCBINs aarch64-rpi firmware), `tools/mkbootimg/example.json` (multiarch array form), `bootboot_image/README.md` (documents disk-rpi.img). BOOTBOOT spec §"Raspberry Pi 3 / 4" (lines 786-830 of `/tmp/bootboot_spec.txt`).
  Acceptance: `cargo xtask create-disk-image --arch aarch64` produces `target/cluu-aarch64.img` that QEMU can boot (`qemu-system-aarch64 -M raspi3b -sd target/cluu-aarch64.img ...`). x86_64 `target/cluu.img` still produced.
  QA: happy: `file target/cluu-aarch64.img` shows a disk image; QEMU recognizes it (next todo verifies boot). Evidence `.omo/evidence/task-21-arm64-port.txt`
  Commit: Y | feat(xtask): aarch64 SD-card disk image via mkbootimg

- [ ] 22. xtask run_qemu aarch64 branch
  What to do / Must NOT do: `run_qemu()` hardcodes `qemu-system-x86_64`, OVMF, `-accel kvm`, `-cpu host`, IDE boot drive, virtio-blk-pci data drive. For aarch64, add a branch: `qemu-system-aarch64 -M raspi3b -m 1G -serial stdio -no-reboot -no-shutdown -drive file=target/cluu-aarch64.img,format=raw,if=sd,index=0` (SD card boot, no OVMF, no virtio-blk — userdisk is x86-only in v1 per scope). Use `-accel kvm -cpu host` if on aarch64 host, else `-accel tcg -cpu cortex-a53` (RPi3 is Cortex-A53). Debug mode: add `-s -S` + telnet serial like x86. Must NOT remove the x86_64 QEMU path.
  Parallelization: Wave 4 | Blocked by: 1,4 | Blocks: 31,36
  References: `xtask/src/main.rs:1973-2072` (run_qemu). BOOTBOOT spec §"Raspberry Pi 3 / 4" (QEMU boot).
  Acceptance: `cargo xtask run --arch aarch64 --build` launches QEMU `raspi3b` and produces serial output (even if kernel panics, QEMU must start and show SOMETHING on serial). `cargo xtask run --arch x86_64` still works.
  QA: happy: `timeout 30 cargo xtask run --arch aarch64 --build 2>&1 | head -50` shows QEMU start + serial. Evidence `.omo/evidence/task-22-arm64-port.txt`
  Commit: Y | feat(xtask): aarch64 QEMU raspi3b launch

- [ ] 23. xtask doctor arch-aware tools
  What to do / Must NOT do: `doctor()` hardcodes `qemu-system-x86_64`, `nasm`, `x86_64-linux-gnu-{as,ld,gcc}`. Add arch awareness: for aarch64, check `qemu-system-aarch64`, `aarch64-linux-gnu-{as,ld,gcc}` (or `clang` with aarch64 target), and NOT nasm. Keep x86_64 tool checks for `--arch x86_64`.
  Parallelization: Wave 4 | Blocked by: 4 | Blocks: —
  References: `xtask/src/main.rs:2205-2275` (doctor).
  Acceptance: `cargo xtask doctor --arch aarch64` checks aarch64 tools. `cargo xtask doctor --arch x86_64` unchanged.
  QA: happy: `cargo xtask doctor --arch aarch64 2>&1 | head`. Evidence `.omo/evidence/task-23-arm64-port.txt`
  Commit: Y | feat(xtask): arch-aware doctor tool checks

- [ ] 24. container-build CLUU_ARCH parameterization
  What to do / Must NOT do: `tools/container-build/src/main.rs` `promote_to_release()` hardcodes `target/x86_64-cluu-user/debug/` → `release/` rewrite. Add `CLUU_ARCH` env var (default `x86_64`) and parameterize all `x86_64-cluu-user` string occurrences to `{arch}-cluu-user`. The Cluufile BUILD commands also hardcode `--target triplets/x86_64-cluu-user.json` and `target/x86_64-cluu-user/debug/<name>.elf` — container-build must substitute these when invoking the BUILD command, OR (preferred) pass `--target` and a `CLUU_TARGET_DIR` env that the BUILD command respects. Simplest: container-build reads `CLUU_ARCH` and does a string replace on the BUILD command's `x86_64-cluu-user` → `{arch}-cluu-user` before invoking it. Must NOT modify the Cluufiles themselves (T25 handles Cluufile-level changes if needed).
  Parallelization: Wave 4 | Blocked by: 4 | Blocks: 25
  References: `tools/container-build/src/main.rs` (full file — find promote_to_release + BUILD invocation).
  Acceptance: `CLUU_ARCH=aarch64 cargo run -p container-build -- containers/hello/Cluufile` produces an aarch64 container image. x86_64 default unchanged.
  QA: happy: `CLUU_ARCH=aarch64 cargo run -p container-build -- containers/hello/Cluufile 2>&1 | tail`. Evidence `.omo/evidence/task-24-arm64-port.txt`
  Commit: Y | feat(container-build): CLUU_ARCH env parameterization

- [ ] 25. Cluufiles — CLUU_ARCH substitution support
  What to do / Must NOT do: If T24's BUILD-command substitution is sufficient, this todo is a no-op verification. If Cluufiles need explicit arch awareness (e.g., a Cluufile references a hardcoded path that container-build can't intercept), add `ARCH` default to each Cluufile's BUILD line via `$(CLUU_ARCH:x86_64)` shell expansion, OR migrate the 120 Cluufiles to use a `{{ARCH}}` template that container-build substitutes. Prefer the T24 interception approach to avoid editing 120 files. If editing is required, use a sed script: `sed -i 's/x86_64-cluu-user/${CLUU_ARCH}-cluu-user/g' containers/*/Cluufile` and ensure container-build expands the env var. Must NOT break x86_64 default.
  Parallelization: Wave 4 | Blocked by: 24 | Blocks: 35
  References: `containers/*/Cluufile` (~120 files), `tools/container-build/src/main.rs`.
  Acceptance: `cargo xtask build --arch aarch64` builds all containers without "file not found" errors from stale x86_64 paths. `cargo xtask build --arch x86_64` still builds all containers.
  QA: happy: `cargo xtask build --arch aarch64 --ui linear 2>&1 | grep -c 'not found'` is 0. Evidence `.omo/evidence/task-25-arm64-port.txt`
  Commit: Y | feat(containers): arch-parameterized Cluufile BUILD paths

- [ ] 26. scripts/build-newlib.sh aarch64 branch
  What to do / Must NOT do: `scripts/build-newlib.sh` hardcodes `TARGET_TRIPLET="x86_64-cluu-elf"` and `CLANG_TARGET="x86_64-unknown-none-elf"`. Add an `ARCH` env var (default `x86_64`) and branch: for aarch64, `TARGET_TRIPLET="aarch64-cluu-elf"`, `CLANG_TARGET="aarch64-unknown-none-elf"`. The newlib configure + make + install flow is otherwise arch-neutral. Must NOT change the x86_64 newlib build.
  Parallelization: Wave 5 | Blocked by: 3 | Blocks: 27,28
  References: `scripts/build-newlib.sh` (full file), `xtask/src/main.rs:25-29` (NEWLIB consts).
  Acceptance: `CLUU_ARCH=aarch64 bash scripts/build-newlib.sh` builds newlib for aarch64; `target/sysroot-aarch64/lib/libc.a` exists. x86_64 newlib still builds.
  QA: happy: `file target/sysroot-aarch64/lib/libc.a` shows aarch64 archive. Evidence `.omo/evidence/task-26-arm64-port.txt`
  Commit: Y | feat(newlib): aarch64 cross-compile branch

- [ ] 27. aarch64 crt0.S + build_syscalls
  What to do / Must NOT do: `userspace/newlib/crt0.S` (or wherever crt0 lives) is x86_64 asm. Create `userspace/newlib/crt0.aarch64.S` that: sets up the stack (SP from a linker symbol), zeroes .bss, calls `main`, calls `exit` syscall on return. `build_syscalls()` and `build_crt0()` in xtask must branch on arch to assemble the right file with the right assembler (gas for aarch64, nasm for x86). The `libcluu_syscalls` static library is Rust and arch-neutral (compiles per-target). Must NOT change the x86_64 crt0.
  Parallelization: Wave 5 | Blocked by: 26 | Blocks: 35
  References: `userspace/newlib/crt0.S` (or `userspace/libcluu_syscalls/`), `xtask/src/main.rs` build_syscalls/build_crt0 functions.
  Acceptance: `cargo xtask build-syscalls --arch aarch64` and `cargo xtask build-crt0 --arch aarch64` produce aarch64 archives in sysroot. x86_64 unchanged.
  QA: happy: `file target/sysroot-aarch64/lib/crt0.o` shows aarch64 ELF. Evidence `.omo/evidence/task-27-arm64-port.txt`
  Commit: Y | feat(newlib): aarch64 crt0 and syscalls library

- [ ] 28. xtask build_c_program aarch64
  What to do / Must NOT do: `build_c_program()` uses `clang --target=x86_64-unknown-none-elf` (fallback `x86_64-linux-gnu-gcc`) + `ld.lld -T userspace/user.ld`. For aarch64, branch to `clang --target=aarch64-unknown-none-elf` (fallback `aarch64-linux-gnu-gcc`) + `ld.lld -T userspace/user.aarch64.ld`. The sysroot path must resolve to the aarch64 sysroot from T26. Must NOT change the x86_64 C build.
  Parallelization: Wave 5 | Blocked by: 26 | Blocks: 35
  References: `xtask/src/main.rs` build_c_program + build_c_programs functions.
  Acceptance: `cargo xtask build-c --arch aarch64 hello userspace/c-programs/hello.c` produces an aarch64 ELF. x86_64 C build unchanged.
  QA: happy: `file target/aarch64-cluu-user/debug/hello.elf` shows aarch64 ELF. Evidence `.omo/evidence/task-28-arm64-port.txt`
  Commit: Y | feat(xtask): aarch64 C program build path

- [ ] 29. userspace libcluu simd arch gate audit
  What to do / Must NOT do: `userspace/libcluu/src/simd.rs` has `#[cfg(target_arch = "x86_64")]` SSE2/WC paths + generic fallback. Verify the generic fallback compiles for aarch64. If an aarch64 NEON path is desired (optional for v1), add `#[cfg(target_arch = "aarch64")]` using NEON intrinsics — but the generic fallback is sufficient. Must NOT break x86_64 SSE2.
  Parallelization: Wave 5 | Blocked by: 3 | Blocks: 35
  References: `userspace/libcluu/src/simd.rs` (full file).
  Acceptance: `cargo build -p libcluu --target triplets/aarch64-cluu-user.json -Z build-std=core,alloc` succeeds. x86_64 build unchanged.
  QA: happy: `cargo build -p libcluu --target triplets/aarch64-cluu-user.json -Z build-std=core,alloc 2>&1 | tail`. Evidence `.omo/evidence/task-29-arm64-port.txt`
  Commit: Y | chore(libcluu): verify aarch64 simd fallback

- [ ] 30. userspace console simd arch gate audit
  What to do / Must NOT do: `userspace/console/src/backend/simd.rs` has x86_64 SSE/AVX blit paths + generic fallback. Verify the generic fallback compiles for aarch64. Same pattern as T29. Must NOT break x86_64.
  Parallelization: Wave 5 | Blocked by: 3 | Blocks: 35
  References: `userspace/console/src/backend/simd.rs` (full file).
  Acceptance: `cargo build -p cluu-console --target triplets/aarch64-cluu-user.json -Z build-std=core,alloc` succeeds. x86_64 unchanged.
  QA: happy: `cargo build -p cluu-console --target triplets/aarch64-cluu-user.json -Z build-std=core,alloc 2>&1 | tail`. Evidence `.omo/evidence/task-30-arm64-port.txt`
  Commit: Y | chore(console): verify aarch64 simd fallback

- [ ] 31. Bring-up: aarch64 kernel reaches "Kernel Boot Started"
  What to do / Must NOT do: Run `cargo xtask run --arch aarch64 --build` and capture serial output. The kernel must print `=== Kernel Boot Started ===` (from `kernel/src/main.rs:113`). This proves: BOOTBOOT aarch64-rpi handoff works, PL011 UART works, _start naked_asm + MPIDR check works, kstart Rust entry works. If it fails BEFORE this string, the failure is in BOOTBOOT initrd packaging (T20/T21), _start asm (T6), or UART (T13). Must NOT declare success on a partial log — the exact string must appear.
  Parallelization: Wave 6 | Blocked by: 6,7,8,9,10,12,13,14,15,16,17,18,19,20,21,22 | Blocks: 32
  References: `kernel/src/main.rs:113` (the marker string).
  Acceptance: Serial log contains the literal `=== Kernel Boot Started ===`. Save as `.omo/evidence/task-31-arm64-port.serial.log`.
  QA scenarios: happy: marker present. failure: if marker absent, capture QEMU serial + `qemu-system-aarch64 -d int -M raspi3b ...` exception log; do NOT proceed to T32 until T31 passes. Evidence `.omo/evidence/task-31-arm64-port.serial.log`
  Commit: N | (bring-up checkpoint, no code change unless fixing a found bug)

- [ ] 32. Bring-up: MMU + physmap ready
  What to do / Must NOT do: Serial log must show `Memory Management Ready` (from `mm/mod.rs:237`). This proves: aarch64 MMU init (T7), TTBR1 switch, physmap activation, BootbootAdapter (T12) all work. If it fails between "Kernel Boot Started" and "Memory Management Ready", the failure is in MMU (T7) or BootbootAdapter (T12).
  Parallelization: Wave 6 | Blocked by: 31 | Blocks: 33
  References: `kernel/src/mm/mod.rs:237` (marker), `:168-242` (init sequence).
  Acceptance: Serial log contains `Memory Management Ready`. Evidence `.omo/evidence/task-32-arm64-port.serial.log`
  QA: happy: marker present. failure: capture exception log; likely a page-table format bug in T7 or a wrong phys base in T12. Evidence `.omo/evidence/task-32-arm64-port.serial.log`
  Commit: N | (bring-up checkpoint)

- [ ] 33. Bring-up: init thread created
  What to do / Must NOT do: Serial log must show `Init thread created successfully!` (from `bootstrap.rs:290`). This proves: crypto/token init, ELF loading, user stack allocation, boot token minting, initrd parsing all work on aarch64. The ELF loader (`kernel/src/elf.rs`) is arch-neutral but the userspace init ELF is an aarch64 binary — loading it validates the aarch64 user target spec (T1) and user linker script (T2).
  Parallelization: Wave 6 | Blocked by: 32 | Blocks: 34
  References: `kernel/src/bootstrap.rs:290` (marker), `:68-295` (init flow).
  Acceptance: Serial log contains `Init thread created successfully!`. Evidence `.omo/evidence/task-33-arm64-port.serial.log`
  QA: happy: marker present. failure: ELF parse error → check aarch64 user ELF header (EM_AARCH64=183). Evidence `.omo/evidence/task-33-arm64-port.serial.log`
  Commit: N | (bring-up checkpoint)

- [ ] 34. Bring-up: scheduler starts
  What to do / Must NOT do: Serial log must show `Starting scheduler and launching init thread` (from `main.rs:262`). This proves: aarch64 Context (T11), context switch asm, timer (T10) for preemption, and the scheduler's `ThreadManager::start()` work on aarch64. The scheduler will then ERET to the init userspace binary.
  Parallelization: Wave 6 | Blocked by: 11,33 | Blocks: 35
  References: `kernel/src/main.rs:262` (marker), `kernel/src/sched/thread_manager.rs` (start).
  Acceptance: Serial log contains `Starting scheduler and launching init thread`. Evidence `.omo/evidence/task-34-arm64-port.serial.log`
  QA: happy: marker present. failure: context switch crash → check T11 Context layout + context.S. Evidence `.omo/evidence/task-34-arm64-port.serial.log`
  Commit: N | (bring-up checkpoint)

- [ ] 35. Bring-up: first userspace program runs on aarch64
  What to do / Must NOT do: After the scheduler starts (T34), the aarch64 init binary begins executing in EL0. Capture serial output showing init's first log line (init uses `klibcluu`/`libcluu` logging which goes to the kernel serial via IPC — or if init prints directly, via its own UART access). This proves: SVC syscall entry (T9), userspace EL0 entry, user page tables (TTBR0), userspace ELF execution all work. The init binary is built with `triplets/aarch64-cluu-user.json` (T1) and `userspace/user.aarch64.ld` (T2). This is the "userspace works" milestone.
  Parallelization: Wave 6 | Blocked by: 25,27,28,29,30,34 | Blocks: 36
  References: `userspace/init/src/main.rs` (init entry).
  Acceptance: Serial log shows evidence that init executed (an init-specific log line, or an IPC message reaching another service). If init silently faults, capture the exception log from the kernel. Evidence `.omo/evidence/task-35-arm64-port.serial.log`
  QA: happy: init log present. failure: SVC fault or page fault → check T9 SVC vector + T7 user page tables. Evidence `.omo/evidence/task-35-arm64-port.serial.log`
  Commit: N | (bring-up checkpoint)

- [ ] 36. Python harness --arch aarch64 smoke case
  What to do / Must NOT do: Add `--arch` to `python/cluu_harness/config.py` and branch `_qemu_args()` in `python/cluu_harness/qemu.py` for aarch64 (mirror T22's QEMU args). Run the earliest harness case that needs only serial (NOT login — no keyboard on aarch64 v1). If no serial-only case exists, add a minimal `arm64_boot_smoke` case that asserts the `Memory Management Ready` marker (or T35's init marker) appears within 30s. Must NOT break x86_64 harness cases.
  Parallelization: Wave 6 | Blocked by: 22,35 | Blocks: —
  References: `python/cluu_harness/config.py`, `python/cluu_harness/qemu.py`, `python/cluu_harness/` (case registry).
  Acceptance: `python -m cluu_harness --case arm64_boot_smoke --arch aarch64 --no-build` returns PASS. x86_64 cases still pass.
  QA: happy: harness PASS. failure: harness FAIL with serial log attached. Evidence `.omo/evidence/task-36-arm64-port.harness.log`
  Commit: Y | test(harness): add arm64 boot smoke case and --arch flag

## Final verification wave
> Runs in parallel after ALL todos. ALL must APPROVE. Surface results and wait for the user's explicit okay before declaring complete.
- [ ] F1. Plan compliance audit — every todo's acceptance criterion met; no scope creep; no Must-NOT-Have violated. Run via Oracle: "Audit .omo/plans/arm64-port.md against the actual repo state. Every todo must have evidence in .omo/evidence/. Flag any todo marked done without evidence, any Must-NOT-Have violated, any x86_64 regression."
- [ ] F2. Code quality review — no `as any`/`unwrap`/`panic!` in new aarch64 code; `Result<T>` used; `no_std` + explicit `alloc`; `debug_print`/`klibcluu` logging used; rust-best-practices skill applied. Run via Oracle on the diff.
- [ ] F3. Real manual QA — `cargo xtask run --arch aarch64 --build` boots to the scheduler-start marker on QEMU raspi3b; `cargo xtask run --arch x86_64 --build` still boots to login. Capture both serial logs as evidence.
- [ ] F4. Scope fidelity — x86_64 build + harness matrix (`cargo xtask harness-matrix --no-build`) still green; no new syscalls added (syscall numbers unchanged); no runtime ACL introduced; BOOTBOOT is the bootloader (no new loader authored).

## Commit strategy
One commit per todo marked `Commit: Y`. No commits for bring-up checkpoints (T31-T35). Conventional Commits format: `feat(kernel):`, `feat(xtask):`, `feat(build):`, `feat(klibcluu):`, `chore(toolchain):`, `test(harness):`. No commit without explicit user request per AGENTS.md §9. No push.

## Success criteria
- `cargo xtask build --arch aarch64` produces `target/cluu-aarch64.img`.
- `cargo xtask run --arch aarch64` boots QEMU raspi3b; serial shows `Starting scheduler and launching init thread` and evidence of init EL0 execution.
- `cargo xtask build --arch x86_64` still produces `target/cluu.img` unchanged.
- `cargo xtask run --arch x86_64` still boots to login (x86_64 harness cases pass).
- No new BOOTBOOT loader authored.
- No new syscalls; no runtime ACL; no x86_64 refactor.
- All evidence in `.omo/evidence/task-*-arm64-port.*`.
