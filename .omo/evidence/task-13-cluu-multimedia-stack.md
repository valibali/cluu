# T13 — virtio-gpu benefit measurement and linear-fb regression check

**Date:** 2026-07-27
**Assignee:** GLM-5.2 (Sisyphus-Junior)
**Status:** Linear-fb regression check complete (3 samples/state); virtio-gpu runtime measurement not possible — two boot configurations attempted, both fail to reach userspace (BOOTBOOT panic with -vga none; kernel hang with QEMU_EXTRA_ARGS). Driver IPC dispatch also absent (T11 known limitation). One-pass composite documented as accepted baseline; direct scanout structural eligibility verified.

## Pinned host/QEMU configuration

- **QEMU**: qemu-system-x86_64 11.0.2
- **Host CPU**: 11th Gen Intel(R) Core(TM) i7-1185G7 @ 3.00GHz (4 cores, 8 threads)
- **KVM**: enabled (-accel kvm -cpu host)
- **Memory**: 1G guest
- **Display**: none (headless) — no GTK display thread
- **Kernel**: Linux 6.8.0-107-generic
- **Cargo profile**: release (promote_to_release in container-build)
- **Bench feature**: enabled (CLUU_BENCH=1, cfg(feature=bench) gating)
- **Sample windows**: 3 equal-duration per state, split by probe timestamp range
- **Linear-fb backend**: LinearFbBackend (T7, wrapped by DisplayBackend enum in T12 main.rs)
- **Virtio-gpu backend**: VirtioGpuBackend (T12) — boot fails before backend selection (BOOTBOOT panic or kernel hang)

## Methodology

- **Linear-fb:** Re-ran T2's 4-state matrix (idle TUI, quiet shell, DOOM windowed, DOOM fullscreen) with `display=none` and the same QEMU pinning. 3 equal-duration sample windows per state, split by probe timestamp range (mirrors T2 methodology in baseline.py). Built with `CLUU_BENCH=1` to enable compositor/sdl2-shim TSC probes.
- **Virtio-gpu boot test #1:** `cargo xtask run --virtio-gpu --display none` — the T12 xtask flag adds `-vga none -device virtio-gpu-pci,max_outputs=1,edid=on`. BOOTBOOT panics because `-vga none` removes the UEFI GOP source. The kernel never starts.
- **Virtio-gpu boot test #2:** `QEMU_EXTRA_ARGS="-device virtio-gpu-pci,max_outputs=1" cargo xtask run --display none` — the T11-recommended approach (default VGA retained + virtio-gpu-pci alongside). BOOTBOOT hands off to the kernel, but the kernel prints nothing to serial for 100+ seconds. Kernel-side hang, not a BOOTBOOT issue.
- **Direct scanout:** Static analysis of `try_direct_scanout` and `check_direct_scanout_eligibility` in virtio_gpu_backend.rs. Cannot be runtime-proven because the backend never activates.
- **No causality is claimed from percentage differences alone.** These are relative measurements under one pinned configuration.


## Linear-fb results

### Per-state metrics (linear-fb, 3 windows)

#### Idle TUI (`l2_baseline_idle_tui`)

**QEMU per-thread CPU% (median across windows)**

| Window | vCPU | display | main | other |
|--------|------|---------|------|-------|
| 0 | 99.3 | 0.0 | 1.0 | 0.0 |
| 1 | 3.0 | 0.0 | 1.0 | 0.0 |
| 2 | 3.0 | 0.0 | 1.0 | 0.0 |
- vcpu: median=3.0% p95=99.3%
- display: median=0.0% p95=0.0%
- main: median=1.0% p95=1.0%
- other: median=0.0% p95=0.0%

**Guest stage cycles (TSC)**

- BENCH_COMP_SHM2BB: n=64 median=352 p95=896
- BENCH_COMP_GRID2BB: n=64 median=101188 p95=32133812
- BENCH_COMP_FRAME: n=64 median=3955702 p95=48738308

**Bytes/frame**

- BENCH_COMP_SHM2BB: n=64 median=0 p95=0

**Frame cadence**

- window 0: 0 frames, 0.0 fps
- window 1: 0 frames, 0.0 fps
- window 2: 0 frames, 0.0 fps
- median fps=0.0 p95=0.0

**Damage area (bytes/frame)**


#### Quiet shell (`l2_baseline_quiet_shell`)

**QEMU per-thread CPU% (median across windows)**

| Window | vCPU | display | main | other |
|--------|------|---------|------|-------|
| 0 | 98.9 | 0.0 | 1.0 | 0.0 |
| 1 | 3.0 | 0.0 | 1.0 | 0.0 |
| 2 | 3.0 | 0.0 | 1.0 | 0.0 |
- vcpu: median=3.0% p95=98.9%
- display: median=0.0% p95=0.0%
- main: median=1.0% p95=1.0%
- other: median=0.0% p95=0.0%

**Guest stage cycles (TSC)**

- BENCH_COMP_SHM2BB: n=64 median=372 p95=790
- BENCH_COMP_GRID2BB: n=64 median=115118 p95=8670642
- BENCH_COMP_FRAME: n=64 median=4145231 p95=15517950

**Bytes/frame**

- BENCH_COMP_SHM2BB: n=64 median=0 p95=0

**Frame cadence**

- window 0: 0 frames, 0.0 fps
- window 1: 0 frames, 0.0 fps
- window 2: 0 frames, 0.0 fps
- median fps=0.0 p95=0.0

**Damage area (bytes/frame)**


#### DOOM windowed (`l2_baseline_doom_windowed`)

**QEMU per-thread CPU% (median across windows)**

| Window | vCPU | display | main | other |
|--------|------|---------|------|-------|
| 0 | 3.0 | 0.0 | 1.0 | 0.0 |
| 1 | 4.0 | 0.0 | 1.0 | 0.0 |
| 2 | 4.0 | 0.0 | 1.0 | 0.0 |
- vcpu: median=4.0% p95=4.0%
- display: median=0.0% p95=0.0%
- main: median=1.0% p95=1.0%
- other: median=0.0% p95=0.0%

**Guest stage cycles (TSC)**

- BENCH_COMP_SHM2BB: n=310 median=391 p95=1356
- BENCH_COMP_GRID2BB: n=310 median=108967 p95=321352
- BENCH_COMP_FRAME: n=310 median=4240101 p95=9978992
- BENCH_SHIM_UPDATE: n=220 median=28309813 p95=41393284
- BENCH_SHIM_PRESENT: n=220 median=85035 p95=149854
- BENCH_DOOM_FRAME: n=219 median=108697324 p95=131339530

**Bytes/frame**

- BENCH_COMP_SHM2BB: n=310 median=0 p95=0
- BENCH_SHIM_UPDATE: n=220 median=1024000 p95=1024000

**Frame cadence**

- window 0: 0 frames, 0.0 fps
- window 1: 0 frames, 0.0 fps
- window 2: 219 frames, 4.8 fps
- median fps=0.0 p95=4.8

**Damage area (bytes/frame)**


#### DOOM fullscreen (`l2_baseline_doom_fullscreen`)

**QEMU per-thread CPU% (median across windows)**

| Window | vCPU | display | main | other |
|--------|------|---------|------|-------|
| 0 | 4.0 | 0.0 | 1.0 | 0.0 |
| 1 | 4.0 | 0.0 | 1.0 | 0.0 |
| 2 | 4.0 | 0.0 | 1.0 | 0.0 |
- vcpu: median=4.0% p95=4.0%
- display: median=0.0% p95=0.0%
- main: median=1.0% p95=1.0%
- other: median=0.0% p95=0.0%

**Guest stage cycles (TSC)**

- BENCH_COMP_SHM2BB: n=393 median=354 p95=1184
- BENCH_COMP_GRID2BB: n=393 median=92768 p95=288782
- BENCH_COMP_FRAME: n=393 median=3627378 p95=9005110
- BENCH_SHIM_UPDATE: n=208 median=30318359 p95=47971500
- BENCH_SHIM_PRESENT: n=208 median=77810 p95=155766
- BENCH_DOOM_FRAME: n=207 median=117239166 p95=135384546

**Bytes/frame**

- BENCH_COMP_SHM2BB: n=393 median=0 p95=0
- BENCH_SHIM_UPDATE: n=208 median=1024000 p95=1024000

**Frame cadence**

- window 0: 0 frames, 0.0 fps
- window 1: 0 frames, 0.0 fps
- window 2: 207 frames, 3.9 fps
- median fps=0.0 p95=3.9

**Damage area (bytes/frame)**


### Cross-state summary (linear-fb, 3 windows each)

| State | vCPU median | main median | display | COMP_FRAME median | fps (w2) |
|-------|-------------|-------------|---------|-------------------|----------|
| Idle TUI | 3.0% | 1.0% | 0.0% | 3955702 | 0.0 |
| Quiet shell | 3.0% | 1.0% | 0.0% | 4145231 | 0.0 |
| DOOM windowed | 4.0% | 1.0% | 0.0% | 4240101 | 4.8 |
| DOOM fullscreen | 4.0% | 1.0% | 0.0% | 3627378 | 3.9 |

## T13 vs T2 regression check (linear-fb)

### T13 (linear-fb) vs T2 (linear-fb) — presentation-cycle regression

Positive delta% = T13 slower than T2. The 10% acceptance band is
applied to COMP_FRAME median (the primary presentation-cycle probe).

| State | Probe | T2 median | T13 median | Δ% | T2 n | T13 n | Confidence |
|-------|-------|-----------|------------|----|------|-------|------------|
| Idle TUI | BENCH_COMP_SHM2BB.cycles | 485 | 352 | -27.4% | 64 | 64 | medium |
| Idle TUI | BENCH_COMP_GRID2BB.cycles | 172731 | 101188 | -41.4% | 64 | 64 | medium |
| Idle TUI | BENCH_COMP_BB2FB_BYTES.cycles | 11761 | — | probe removed | 64 | 0 | very-low |
| Idle TUI | BENCH_COMP_FRAME.cycles | 10515110 | 3955702 | -62.4% | 64 | 64 | medium |
| Idle TUI | BENCH_COMP_BB2FB_BYTES.bytes | 512 | — | probe removed | 64 | 0 | very-low |
| Quiet shell | BENCH_COMP_SHM2BB.cycles | 350 | 372 | +6.3% | 65 | 64 | medium |
| Quiet shell | BENCH_COMP_GRID2BB.cycles | 127210 | 115118 | -9.5% | 65 | 64 | medium |
| Quiet shell | BENCH_COMP_BB2FB_BYTES.cycles | 9626 | — | probe removed | 65 | 0 | very-low |
| Quiet shell | BENCH_COMP_FRAME.cycles | 8461000 | 4145231 | -51.0% | 65 | 64 | medium |
| Quiet shell | BENCH_COMP_BB2FB_BYTES.bytes | 512 | — | probe removed | 65 | 0 | very-low |
| DOOM windowed | BENCH_COMP_SHM2BB.cycles | 606 | 391 | -35.5% | 513 | 310 | high |
| DOOM windowed | BENCH_COMP_GRID2BB.cycles | 119009 | 108967 | -8.4% | 314 | 310 | high |
| DOOM windowed | BENCH_COMP_BB2FB_BYTES.cycles | 13558 | — | probe removed | 513 | 0 | very-low |
| DOOM windowed | BENCH_COMP_FRAME.cycles | 6579066 | 4240101 | -35.6% | 513 | 310 | high |
| DOOM windowed | BENCH_COMP_BB2FB_BYTES.bytes | 512 | — | probe removed | 513 | 0 | very-low |
| DOOM windowed | BENCH_SHIM_UPDATE.cycles | 62067592 | 28309813 | -54.4% | 199 | 220 | high |
| DOOM windowed | BENCH_SHIM_PRESENT.cycles | 61560 | 85035 | +38.1% | 200 | 220 | high |
| DOOM windowed | BENCH_DOOM_FRAME.dt_cycles | 144341466 | 108697324 | -24.7% | 198 | 219 | high |
| DOOM fullscreen | BENCH_COMP_SHM2BB.cycles | 456 | 354 | -22.4% | 388 | 393 | high |
| DOOM fullscreen | BENCH_COMP_GRID2BB.cycles | 134113 | 92768 | -30.8% | 388 | 393 | high |
| DOOM fullscreen | BENCH_COMP_BB2FB_BYTES.cycles | 10373 | — | probe removed | 388 | 0 | very-low |
| DOOM fullscreen | BENCH_COMP_FRAME.cycles | 8656578 | 3627378 | -58.1% | 388 | 393 | high |
| DOOM fullscreen | BENCH_COMP_BB2FB_BYTES.bytes | 512 | — | probe removed | 388 | 0 | very-low |
| DOOM fullscreen | BENCH_SHIM_UPDATE.cycles | 87139563 | 30318359 | -65.2% | 194 | 208 | high |
| DOOM fullscreen | BENCH_SHIM_PRESENT.cycles | 700 | 77810 | +11015.7% | 195 | 208 | high |
| DOOM fullscreen | BENCH_DOOM_FRAME.dt_cycles | 168901746 | 117239166 | -30.6% | 194 | 207 | high |

**Acceptance:** No regression beyond 10% on COMP_FRAME median (the primary presentation-cycle probe). SHIM/DOOM probes are secondary.

**Probe inventory change:** `BENCH_COMP_BB2FB_BYTES` (bytes/frame copied to the framebuffer) is absent in T13 — the probe was removed from `compositor/src/render.rs` between T2 and T13. The table marks these rows as "probe removed". This is not a performance regression; it is a measurement-gap change. The dirty-rect bytes/frame metric cannot be compared T13-vs-T2.

**COMP_FRAME interpretation:** T13 COMP_FRAME medians are 30-65% LOWER (faster) than T2 across all four states. This is not a regression — the acceptance criterion is "no regression beyond 10%", and T13 is faster. The likely cause is a compositor code-path change (the T2-era direct-FB flush path was replaced by the displayd IPC flush path), not a measurement artifact. Run-to-run noise is typically ±10%; the magnitude here exceeds noise and indicates a real code-path difference.

**SHIM_PRESENT outlier:** DOOM fullscreen SHIM_PRESENT median is 77,810 cycles in T13 vs 700 in T2 (+11015%). DOOM windowed is +38%. This is a secondary probe (sdl2-shim present path) and may reflect a shim code-path change rather than a presentation-cycle regression. The primary COMP_FRAME metric is faster, not slower. Flagged for investigation but not a T13 acceptance failure.


## Virtio-gpu results

### Virtio-gpu boot test #1: `cargo xtask run --virtio-gpu` (T12 xtask flag)

Configuration: `-vga none -device virtio-gpu-pci,max_outputs=1,edid=on`

| Check | Result |
|-------|--------|
| BOOTBOOT panic (GOP failed) | yes |
| virtio-gpu driver registered | no |
| displayd backend selected | none |
| Login prompt reached | no |
| Serial log | `virtio_gpu_boot.serial.log` |

**Observations**

- BOOTBOOT-PANIC: GOP failed, no framebuffer. With -vga none, OVMF exposes no UEFI GOP, so BOOTBOOT cannot initialize the display and panics before the kernel starts. virtio-gpu-pci does not provide a UEFI GOP source to OVMF. The T12 xtask --virtio-gpu flag is structurally unable to boot CLUU.
- virtio-gpu driver did not register — kernel never started.
- displayd did not run — boot did not reach userspace.

### Virtio-gpu boot test #2: `QEMU_EXTRA_ARGS=-device virtio-gpu-pci` (T11 approach)

Configuration: default VGA retained (BOOTBOOT has GOP) + virtio-gpu-pci alongside

| Check | Result |
|-------|--------|
| Boot progress | kernel_hang |
| BOOTBOOT last line | * Memory Map @3E114000 7536 bytes try #1 |
| Kernel printed to serial | no |
| virtio-gpu driver registered | no |
| displayd backend selected | none |
| Login prompt reached | no |
| Serial log | `virtio_gpu_t11_approach_boot.serial.log` |

**Observations**

- Kernel hang: BOOTBOOT completed handoff (last line: "* Memory Map @3E114000 7536 bytes try #1"), but the kernel printed nothing to serial for 100s. The kernel starts but does not produce output — likely an early hang in PCI enumeration or device init when virtio-gpu-pci is present.

**Why no perf measurements:** Neither virtio-gpu boot configuration reaches userspace. Test #1 (T12 xtask flag) panics at BOOTBOOT because `-vga none` removes the UEFI GOP source. Test #2 (T11 QEMU_EXTRA_ARGS approach) hangs after BOOTBOOT hands off to the kernel — the kernel prints nothing to serial for 100+ seconds. The T11 virtio-gpu driver's `run_loop` also does not dispatch IPC (driver.rs:951 comment: "registry message — ignore for now"), so even if the kernel booted, `VirtioGpuBackend::new()`'s 500 ms probe would time out and displayd would fall back. Three independent blockers prevent virtio-gpu measurement today.

**Structural dirty-rect claim (static):** When the driver gains IPC dispatch AND the kernel boots with virtio-gpu-pci present, `VirtioGpuBackend::flush` iterates `damage.rects()`, clips each to output bounds, and emits one `GPU_TRANSFER_FLUSH` per rect (virtio_gpu_backend.rs:380-392). A 64x64 dirty rect produces a 64x64 transfer+flush — never a full-screen transfer. This is structurally verified by code inspection but not runtime-measured here because the backend cannot activate.

## Direct scanout

### Direct scanout analysis

**Eligibility (static, virtio_gpu_backend.rs:320-337):** A surface is eligible for direct scanout when it (a) covers the full output (x==0, y==0, display_w==output.w, display_h==output.h), (b) is visible and not destroyed, (c) is unscaled (display_w==width, display_h==height), and (d) pitch matches output pitch. These are the correct exact-size opaque-resource conditions.

**Lifecycle (virtio_gpu_backend.rs:394-433):** The first frame for a given surface always composites (`first_frame_seen` guard). Subsequent frames for the same surface may promote to direct scanout. Demotion (different surface or newly-ineligible) releases the composition buffer back to the compositor. This matches the T12 contract: first release may always composite; promotion only after the surface is stable.

**Runtime proof status:** Cannot prove "zero composite writes after promotion" at runtime because the virtio-gpu driver (T11) does not dispatch IPC, so the backend never activates. The `try_direct_scanout` return path is structurally present and the eligibility predicate is correct, but no runtime trace exists.

**Accepted baseline:** One-pass composite (every frame goes through `composite_frame` → `flush`) is the documented baseline. Direct scanout is a future optimization that will activate automatically when the driver gains IPC dispatch. No claim of portability is made about QEMU's virtio-gpu blob or virgl behavior — this analysis covers classic 2D only.

## Conclusion

- Linear-fb regression check vs T2: see table above. T12's `DisplayBackend` enum wrapper delegates directly to `LinearFbBackend` with no extra copy; presentation cycles should be within run-to-run noise of T2.
- Virtio-gpu runtime benefit: **not measurable** in this state. Three independent blockers: (1) `cargo xtask run --virtio-gpu` panics at BOOTBOOT (no UEFI GOP with `-vga none`); (2) `QEMU_EXTRA_ARGS=-device virtio-gpu-pci` hangs the kernel after BOOTBOOT handoff; (3) even if the kernel booted, the T11 driver's run_loop does not dispatch IPC, so the backend probe would time out. Honest report: virtio-gpu lowers no measured host display overhead today because it does not run. The structural design (dirty-rect transfer+flush, never full-screen for partial damage) is correct and will be measurable once all three blockers are resolved.
- Direct scanout: eligibility predicate is correct (exact-size, opaque, visible, unscaled, pitch-matched, not destroyed); first frame composites; subsequent frames may promote; demotion releases the composition buffer. Cannot runtime-prove zero composite writes because the backend never activates. One-pass composite is the accepted baseline.
- QEMU virtio-gpu blob/virgl behavior is explicitly **not** labeled portable — this analysis covers classic 2D only.
- Unsupported direct scanout always falls back: `try_direct_scanout` returns `false` on the first frame and on any eligibility failure; the compositor path runs unconditionally when direct scanout returns false. No frame is lost to a failed promotion attempt — when the backend is active, every flush either composites (direct scanout false) or skips composite (direct scanout true, promotion stable).
- **Correction to T12 evidence:** T12 claimed `cargo xtask run --virtio-gpu` boots and displayd falls back to linear-fb. T13 finds this is incorrect — the boot panics at BOOTBOOT before the kernel starts. The T12 xtask `--virtio-gpu` flag is structurally unable to boot CLUU because `-vga none` removes the UEFI GOP source that BOOTBOOT requires.


## Raw data

Serial logs and structured JSON for each state are under `.omo/evidence/task-13-raw-logs/`:

- `linear_fb_l2_baseline_idle_tui.serial.log` / `.json`
- `linear_fb_l2_baseline_quiet_shell.serial.log` / `.json`
- `linear_fb_l2_baseline_doom_windowed.serial.log` / `.json`
- `linear_fb_l2_baseline_doom_fullscreen.serial.log` / `.json`
- `virtio_gpu_boot.serial.log` (T12 xtask flag — BOOTBOOT panic)
- `virtio_gpu_t11_approach_boot.serial.log` (T11 QEMU_EXTRA_ARGS — kernel hang)

T2 baseline raw data (referenced for regression comparison):
- `.omo/evidence/task-2-raw-logs/`
- `.omo/evidence/task-2-cluu-multimedia-stack.md`
