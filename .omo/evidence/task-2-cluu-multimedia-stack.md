# T2 — Multimedia Baseline Report

## Pinned host/QEMU configuration

- **QEMU**: qemu-system-x86_64 11.0.2
- **Host CPU**: 11th Gen Intel(R) Core(TM) i7-1185G7 @ 3.00GHz (4 cores, 8 threads)
- **KVM**: enabled (-accel kvm -cpu host)
- **Memory**: 1G guest
- **Display**: none (headless) — GTK backend supported via `CLUU_DISPLAY=gtk` / `cargo xtask run --display gtk`, but baseline pinned to `-display none` for reproducibility (no host display dependency)
- **Kernel**: Linux 6.8.0-107-generic
- **Cargo profile**: release (promote_to_release in container-build)
- **Bench feature**: enabled (CLUU_BENCH=1, `#[cfg(feature = "bench")]` gating in compositor + sdl2-shim)
- **Sample windows**: 3 equal-duration per state, split by probe timestamp range (durations vary per state — see each section)

## Methodology notes

- QEMU per-thread CPU% is measured via `/proc/<qemu-pid>/task/*/stat` polling at 1 Hz. Thread classification: `vcpu` = vCPU thread, `main` = qemu-system main thread, `display` = GTK display thread (absent with `-display none`), `other` = IO/virtio threads.
- Guest stage cycles are TSC (timestamp counter) readings from `#[cfg(feature = "bench")]` probes in the compositor render pipeline and SDL2 shim, emitted via `debug_print` on COM2.
- Window 0 for idle TUI and quiet shell includes the boot phase (vCPU ~100%). Windows 1-2 represent steady-state.
- DOOM probes (BENCH_SHIM_*, BENCH_DOOM_FRAME) appear only when DOOM is running — windows 0-1 for DOOM states contain only compositor probes.
- **No causality is claimed from percentage differences alone.** These are relative measurements under one pinned configuration.

## Idle TUI (`l2_baseline_idle_tui`, display=none)

### QEMU per-thread CPU%

| Window | vCPU | display | main | other |
|--------|------|---------|------|-------|
| 0 | 99.9 | 0.0 | 1.0 | 0.0 |
| 1 | 4.0 | 0.0 | 1.0 | 0.0 |
| 2 | 4.0 | 0.0 | 1.0 | 0.0 |
- vcpu: median=4.0% p95=99.9%
- display: median=0.0% p95=0.0%
- main: median=1.0% p95=1.0%
- other: median=0.0% p95=0.0%

### Guest stage cycles (TSC)

- BENCH_COMP_SHM2BB: n=64 median=485 p95=690
- BENCH_COMP_GRID2BB: n=64 median=172731 p95=41345928
- BENCH_COMP_BB2FB_BYTES: n=64 median=11761 p95=810056
- BENCH_COMP_FRAME: n=64 median=10515110 p95=49721212

### Bytes/frame

- BENCH_COMP_SHM2BB: n=64 median=0 p95=0
- BENCH_COMP_BB2FB_BYTES: n=64 median=512 p95=2116608

### Frame cadence

- window 0: 0 frames, 0.0 fps
- window 1: 0 frames, 0.0 fps
- window 2: 0 frames, 0.0 fps
- median fps=0.0 p95=0.0

### Damage area

- BENCH_COMP_BB2FB_BYTES bytes/frame: n=64 median=512 p95=2116608

## Quiet shell (`l2_baseline_quiet_shell`, display=none)

### QEMU per-thread CPU%

| Window | vCPU | display | main | other |
|--------|------|---------|------|-------|
| 0 | 99.9 | 0.0 | 1.0 | 0.0 |
| 1 | 4.0 | 0.0 | 1.0 | 0.0 |
| 2 | 4.0 | 0.0 | 1.0 | 0.0 |
- vcpu: median=4.0% p95=99.9%
- display: median=0.0% p95=0.0%
- main: median=1.0% p95=1.0%
- other: median=0.0% p95=0.0%

### Guest stage cycles (TSC)

- BENCH_COMP_SHM2BB: n=65 median=350 p95=1106
- BENCH_COMP_GRID2BB: n=65 median=127210 p95=47931810
- BENCH_COMP_BB2FB_BYTES: n=65 median=9626 p95=750852
- BENCH_COMP_FRAME: n=65 median=8461000 p95=54071752

### Bytes/frame

- BENCH_COMP_SHM2BB: n=65 median=0 p95=0
- BENCH_COMP_BB2FB_BYTES: n=65 median=512 p95=2116608

### Frame cadence

- window 0: 0 frames, 0.0 fps
- window 1: 0 frames, 0.0 fps
- window 2: 0 frames, 0.0 fps
- median fps=0.0 p95=0.0

### Damage area

- BENCH_COMP_BB2FB_BYTES bytes/frame: n=65 median=512 p95=2116608

## DOOM windowed (`l2_baseline_doom_windowed`, display=none)

### QEMU per-thread CPU%

| Window | vCPU | display | main | other |
|--------|------|---------|------|-------|
| 0 | 4.0 | 0.0 | 1.0 | 0.0 |
| 1 | 4.0 | 0.0 | 1.0 | 0.0 |
| 2 | 4.0 | 0.0 | 1.0 | 0.0 |
- vcpu: median=4.0% p95=4.0%
- display: median=0.0% p95=0.0%
- main: median=1.0% p95=1.0%
- other: median=0.0% p95=0.0%

### Guest stage cycles (TSC)

- BENCH_COMP_SHM2BB: n=513 median=606 p95=1337196
- BENCH_COMP_GRID2BB: n=314 median=119009 p95=340726
- BENCH_COMP_BB2FB_BYTES: n=513 median=13558 p95=1329084
- BENCH_COMP_FRAME: n=513 median=6579066 p95=16687768
- BENCH_SHIM_UPDATE: n=199 median=62067592 p95=78349556
- BENCH_SHIM_PRESENT: n=200 median=61560 p95=101902
- BENCH_DOOM_FRAME: n=198 median=144341466 p95=167104342

### Bytes/frame

- BENCH_COMP_SHM2BB: n=513 median=0 p95=4096000
- BENCH_COMP_BB2FB_BYTES: n=513 median=512 p95=4096000
- BENCH_SHIM_UPDATE: n=199 median=1024000 p95=1024000

### Frame cadence

- window 0: 0 frames, 0.0 fps
- window 1: 0 frames, 0.0 fps
- window 2: 198 frames, 4.3 fps
- median fps=0.0 p95=4.3

### Damage area

- BENCH_COMP_BB2FB_BYTES bytes/frame: n=513 median=512 p95=4096000

## DOOM fullscreen (`l2_baseline_doom_fullscreen`, display=none)

### QEMU per-thread CPU%

| Window | vCPU | display | main | other |
|--------|------|---------|------|-------|
| 0 | 4.0 | 0.0 | 1.0 | 0.0 |
| 1 | 5.0 | 0.0 | 1.0 | 0.0 |
| 2 | 5.0 | 0.0 | 1.0 | 0.0 |
- vcpu: median=5.0% p95=5.0%
- display: median=0.0% p95=0.0%
- main: median=1.0% p95=1.0%
- other: median=0.0% p95=0.0%

### Guest stage cycles (TSC)

- BENCH_COMP_SHM2BB: n=388 median=456 p95=1742
- BENCH_COMP_GRID2BB: n=388 median=134113 p95=423260
- BENCH_COMP_BB2FB_BYTES: n=388 median=10373 p95=21000
- BENCH_COMP_FRAME: n=388 median=8656578 p95=18669988
- BENCH_SHIM_UPDATE: n=194 median=87139563 p95=107346740
- BENCH_SHIM_PRESENT: n=195 median=700 p95=1200
- BENCH_DOOM_FRAME: n=194 median=168901746 p95=192461548

### Bytes/frame

- BENCH_COMP_SHM2BB: n=388 median=0 p95=0
- BENCH_COMP_BB2FB_BYTES: n=388 median=512 p95=1024
- BENCH_SHIM_UPDATE: n=194 median=1024000 p95=1024000

### Frame cadence

- window 0: 0 frames, 0.0 fps
- window 1: 0 frames, 0.0 fps
- window 2: 194 frames, 3.6 fps
- median fps=0.0 p95=3.6

### Damage area

- BENCH_COMP_BB2FB_BYTES bytes/frame: n=388 median=512 p95=1024

## Cross-state summary

### vCPU thread CPU% (steady-state windows 1-2 median)

| State | vCPU median | main median | display |
|-------|-------------|-------------|---------|
| Idle TUI | 4.0% | 1.0% | n/a (none) |
| Quiet shell | 4.0% | 1.0% | n/a (none) |
| DOOM windowed | 4.0% | 1.0% | n/a (none) |
| DOOM fullscreen | 5.0% | 1.0% | n/a (none) |

### Guest stage cycles median (TSC)

| Probe | Idle TUI | Quiet shell | DOOM windowed | DOOM fullscreen |
|-------|----------|-------------|---------------|-----------------|
| COMP_SHM2BB | 485 | 350 | 606 | 456 |
| COMP_GRID2BB | 172,731 | 127,210 | 119,009 | 134,113 |
| COMP_BB2FB | 11,761 | 9,626 | 13,558 | 10,373 |
| COMP_FRAME | 10,515,110 | 8,461,000 | 6,579,066 | 8,656,578 |
| SHIM_UPDATE | — | — | 62,067,592 | 87,139,563 |
| SHIM_PRESENT | — | — | 61,560 | 700 |
| DOOM_FRAME dt | — | — | 144,341,466 | 168,901,746 |

### Bytes/frame median

| Probe | Idle TUI | Quiet shell | DOOM windowed | DOOM fullscreen |
|-------|----------|-------------|---------------|-----------------|
| COMP_SHM2BB | 0 | 0 | 0 | 0 |
| COMP_BB2FB | 512 | 512 | 512 | 512 |
| SHIM_UPDATE | — | — | 1,024,000 | 1,024,000 |

### Frame cadence (DOOM states, window 2)

| State | Frames | FPS |
|-------|--------|-----|
| DOOM windowed | 198 | 4.3 |
| DOOM fullscreen | 194 | 3.6 |

## Raw data

Serial logs and structured JSON data for each state are under `.omo/evidence/task-2-raw-logs/`:
- `l2_baseline_idle_tui.serial.log` / `.json`
- `l2_baseline_quiet_shell.serial.log` / `.json`
- `l2_baseline_doom_windowed.serial.log` / `.json`
- `l2_baseline_doom_fullscreen.serial.log` / `.json`
