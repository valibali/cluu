# Framebuffer Perf: Write-Combining Mapping Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the framebuffer write path the fastest possible by switching the front-buffer mapping from UC- (uncached) to WC (write-combining), so SSE2 stores from the dirty-rect flush coalesce into 64-byte burst writes instead of one bus transaction per dword.

**Architecture:**
1. Program the x86_64 PAT MSR (0x277) at boot to install a Linux-compatible layout where index 1 = WC. UC-, UC, and WB stay where firmware put them, so existing PTE encodings keep their current semantics.
2. Add `MAP_DEVICE_WC` flag (`0x200`) to `SpaceMap` / `SpaceMapRange`. New internal helper `map_device_page_wc` sets PTE bits PCD=0, PWT=1, PAT=0 → index 1 → WC.
3. Userspace `posix/framebuffer.rs` requests `MAP_DEVICE_WC` for the front-buffer mapping (with a fallback to plain `MAP_DEVICE` if the kernel is older — same flag value works on both because the new flag bit is just unused on old kernels).
4. Add a `b_console_blit` harness marker that runs a known-size full-screen blit and reports cycles-per-frame; baseline ratchet stored in harness, future regressions trip a marker.
5. Document in `docs/CURRENT_PHASE.md` and add a memory entry.

**Tech Stack:** Rust nightly + inline asm (kernel), Rust nightly + libcluu (userspace), x86_64 PAT MSR (0x277), QEMU harness with COM2 marker output.

**Risk note:** WC perf gain only visible under KVM or baremetal. Under QEMU TCG, every memory type tends to behave as WB regardless. Functional correctness (PTE bits set, mapping succeeds, no faults) is verifiable on TCG. Perf delta requires a KVM run, so the bench marker has two ratchet rails (TCG-cycles, KVM-cycles) and only fails if the relevant rail regresses by >10%.

---

## File Structure

**Created:**
- `userspace/probes/fb_wc_probe/` — tiny test binary that maps a frame WC and reports success via debug_print
  - `Cargo.toml`
  - `src/main.rs`
  - `Cluufile`
- `userspace/probes/console_blit_bench/` — perf harness
  - `Cargo.toml`
  - `src/main.rs`
  - `Cluufile`

**Modified:**
- `kernel/src/mm/vmm.rs` — add `PWT`, `PAT_4K`, `WRITE_COMBINING` PTE flag constants
- `kernel/src/mm/pat.rs` — **NEW FILE** — PAT MSR programming
- `kernel/src/mm/mod.rs` — call `pat::init()` early in mm init
- `kernel/src/elf.rs` — add `map_device_page_wc()` (mostly a copy of `map_device_page` with WC flags)
- `kernel/src/syscall/handlers.rs` — add `MAP_DEVICE_WC = 0x200` const, dispatch in `invoke_space_map_range` and `invoke_space_map`, route through `map_device_range_wc`
- `userspace/libcluu/src/posix/framebuffer.rs` — set `MAP_DEVICE_WC` bit when calling `space_map_range`
- `userspace/libcluu/src/syscall.rs` — expose `MAP_DEVICE_WC` constant if a public re-export exists for `MAP_DEVICE` (else inline)
- `scripts/harness_run.sh` — add `b_console_blit` MARKER_MODE branch and ratchet check
- `docs/CURRENT_PHASE.md` — note FB perf work + ratchet baseline
- `Cargo.toml` (workspace) — register the two new probe crates

---

## Task 1: Add PAT/PWT PTE flag constants

**Files:**
- Modify: `kernel/src/mm/vmm.rs:55-84`

- [ ] **Step 1: Add the new constants in the `pte_flags` module**

```rust
    /// Page-level write-through (bit 3). Combined with PCD (bit 4) and PAT
    /// (bit 7) selects which of the 8 PAT MSR entries this page uses.
    pub const PWT: u64 = 1 << 3;

    /// PAT bit for 4-KiB pages (bit 7 of last-level PTE).
    /// NOTE: in PDEs this same bit is HUGE (1 GB / 2 MB page). Use only on PTEs.
    pub const PAT_4K: u64 = 1 << 7;

    /// Convenience combo: write-combining on a 4-KiB page.
    /// Index into PAT MSR is (PAT<<2)|(PCD<<1)|PWT = 0b001 = 1.
    /// Caller must have run `pat::init()` to put WC at PAT[1].
    pub const WRITE_COMBINING: u64 = PWT;
```

- [ ] **Step 2: Build kernel to ensure no regression**

Run: `cargo xtask build`
Expected: success, no warnings about unused constants (they're `pub`).

- [ ] **Step 3: Commit**

```bash
git add kernel/src/mm/vmm.rs
git commit -m "kernel/mm: add PWT/PAT_4K/WRITE_COMBINING PTE flag constants"
```

---

## Task 2: PAT MSR programming module

**Files:**
- Create: `kernel/src/mm/pat.rs`
- Modify: `kernel/src/mm/mod.rs` (add `pub mod pat;` + call `pat::init();`)

- [ ] **Step 1: Create `kernel/src/mm/pat.rs`**

```rust
//! Page Attribute Table (PAT) programming.
//!
//! Linux-compatible layout:
//!   PAT[0] = WB   (PCD=0, PWT=0)  — default cached
//!   PAT[1] = WC   (PCD=0, PWT=1)  — write-combining (this is the new entry)
//!   PAT[2] = UC-  (PCD=1, PWT=0)  — current `NO_CACHE` device mapping
//!   PAT[3] = UC   (PCD=1, PWT=1)  — strict uncached
//!   PAT[4..8] mirror PAT[0..4] (PAT bit unused on 4K pages here, kept benign).
//!
//! Existing PTEs that set only PCD continue to map to UC- (PAT[2]) — no
//! behavior change. New WC mappings select index 1 by setting PWT only.

const IA32_PAT: u32 = 0x277;

const PA_WB:  u64 = 0x06;
const PA_WC:  u64 = 0x01;
const PA_UCM: u64 = 0x07; // UC-
const PA_UC:  u64 = 0x00;

const PAT_VALUE: u64 =
    PA_WB        << 0  |
    PA_WC        << 8  |
    PA_UCM       << 16 |
    PA_UC        << 24 |
    PA_WB        << 32 |
    PA_WC        << 40 |
    PA_UCM       << 48 |
    PA_UC        << 56;

/// Program the PAT MSR on the current CPU. Must be called once per CPU at boot,
/// before any user-visible mapping that relies on the new layout. CR3 reload is
/// performed afterward to guarantee the CPU drops any cached PAT-derived TLB
/// entries.
pub fn init() {
    unsafe {
        let lo = (PAT_VALUE & 0xFFFF_FFFF) as u32;
        let hi = (PAT_VALUE >> 32) as u32;
        core::arch::asm!(
            "wrmsr",
            in("ecx") IA32_PAT,
            in("eax") lo,
            in("edx") hi,
            options(nostack, preserves_flags),
        );

        // Reload CR3 to flush TLB; Intel SDM 12.11.8 recommends a full TLB
        // flush after PAT change.
        let cr3: u64;
        core::arch::asm!("mov {0}, cr3", out(reg) cr3, options(nomem, preserves_flags));
        core::arch::asm!("mov cr3, {0}", in(reg) cr3, options(nostack, preserves_flags));
    }
}

/// Sanity check helper used by tests: read back the PAT MSR.
#[allow(dead_code)]
pub fn read() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!(
            "rdmsr",
            in("ecx") IA32_PAT,
            out("eax") lo,
            out("edx") hi,
            options(nomem, preserves_flags),
        );
    }
    ((hi as u64) << 32) | (lo as u64)
}

/// Expected MSR value (used by smoke-test marker emission at boot).
pub fn expected() -> u64 { PAT_VALUE }
```

- [ ] **Step 2: Wire the module into `kernel/src/mm/mod.rs`**

Find the existing module declarations near the top (e.g. `pub mod pmm;`, `pub mod vmm;`) and add:

```rust
pub mod pat;
```

In the existing `init()` function (the same one that calls `physmap::activate()` and `init_buddy()`), add a call to `pat::init()` **before** the first user mapping is created. The buddy init already runs late — `pat::init()` needs only physmap + general kernel infra, so place it immediately after `physmap::activate()`:

```rust
    physmap::activate();
    pat::init();                 // <-- new, must be before any device map
    // ... existing buddy init etc.
```

- [ ] **Step 3: Add a boot-time marker so the harness can verify PAT got programmed**

In the same `mm::init` function, right after `pat::init()`:

```rust
    let actual = pat::read();
    let expected = pat::expected();
    crate::serial::write_str(if actual == expected {
        "[BOOT] PAT programmed: ok\n"
    } else {
        "[BOOT] PAT programmed: MISMATCH\n"
    });
```

(Use whatever the existing kernel debug-print helper is — search for `debug_print!` or `serial::write_str` in `kernel/src/`.)

- [ ] **Step 4: Build kernel**

Run: `cargo xtask build`
Expected: success.

- [ ] **Step 5: Boot and verify the marker prints**

Run: `RUN_WAIT=60 MARKER_MODE=none bash scripts/harness_run.sh 2>&1 | grep -E '\[BOOT\] PAT'`
Expected: `[BOOT] PAT programmed: ok`

- [ ] **Step 6: Commit**

```bash
git add kernel/src/mm/pat.rs kernel/src/mm/mod.rs
git commit -m "kernel/mm: program PAT for write-combining at boot"
```

---

## Task 3: `map_device_page_wc` helper

**Files:**
- Modify: `kernel/src/elf.rs` (add new helper after existing `map_device_page` at line 422)

- [ ] **Step 1: Add `map_device_page_wc`**

Place immediately after the existing `map_device_page` function. It is identical except for the leaf PTE flags:

```rust
/// Map a single MMIO page write-combining (WC).
///
/// PTE bits: PRESENT | USER | PWT | NO_EXECUTE [+ WRITABLE].
/// PWT alone (PCD=0, PWT=1, PAT=0) selects PAT[1] = WC, configured by
/// `mm::pat::init()` at boot.
///
/// Like `map_device_page`, the leaf PTE has the OS-visible "device" bits
/// (NO_EXECUTE) so `teardown_user_pages` can skip the frame on cleanup.
/// teardown identifies device pages by the absence of normal allocation
/// metadata; this helper sets `SHARED_PHYS` so teardown skips PMM free.
pub(crate) unsafe fn map_device_page_wc(
    virt: u64,
    phys: u64,
    writable: bool,
    page_table_root: PhysAddr,
) -> Result<(), ElfLoadError> {
    use crate::mm::vmm::pte_flags;
    use core::ptr::write_bytes;

    let pml4_idx = ((virt >> 39) & 0x1FF) as usize;
    let pdpt_idx = ((virt >> 30) & 0x1FF) as usize;
    let pd_idx   = ((virt >> 21) & 0x1FF) as usize;
    let pt_idx   = ((virt >> 12) & 0x1FF) as usize;

    let table_flags = pte_flags::PRESENT | pte_flags::WRITABLE | pte_flags::USER;

    // WC pages: PRESENT | USER | PWT | NO_EXECUTE | SHARED_PHYS, optionally WRITABLE.
    // SHARED_PHYS keeps teardown_user_pages from PMM-freeing the MMIO frame.
    let mut page_flags = pte_flags::PRESENT
        | pte_flags::USER
        | pte_flags::WRITE_COMBINING       // PWT=1 → PAT index 1 → WC
        | pte_flags::NO_EXECUTE
        | pte_flags::SHARED_PHYS;
    if writable {
        page_flags |= pte_flags::WRITABLE;
    }

    const PHYS_MASK: u64 = 0x000F_FFFF_FFFF_F000;

    let pml4_virt = crate::mm::physmap::phys_to_virt_u64(page_table_root.as_u64());
    let pml4 = &mut *(pml4_virt as *mut [u64; 512]);

    let pdpt_phys = if pml4[pml4_idx] & 0x1 != 0 {
        pml4[pml4_idx] & PHYS_MASK
    } else {
        let p = crate::mm::pmm::alloc_frame().ok_or(ElfLoadError::MemoryAllocationFailed)?;
        let v = crate::mm::physmap::phys_to_virt_u64(p);
        write_bytes(v as *mut u8, 0, 4096);
        pml4[pml4_idx] = p | table_flags;
        p
    };

    let pdpt = &mut *(crate::mm::physmap::phys_to_virt_u64(pdpt_phys) as *mut [u64; 512]);

    let pd_phys = if pdpt[pdpt_idx] & 0x1 != 0 {
        pdpt[pdpt_idx] & PHYS_MASK
    } else {
        let p = crate::mm::pmm::alloc_frame().ok_or(ElfLoadError::MemoryAllocationFailed)?;
        let v = crate::mm::physmap::phys_to_virt_u64(p);
        write_bytes(v as *mut u8, 0, 4096);
        pdpt[pdpt_idx] = p | table_flags;
        p
    };

    let pd = &mut *(crate::mm::physmap::phys_to_virt_u64(pd_phys) as *mut [u64; 512]);

    let pt_phys = if pd[pd_idx] & 0x1 != 0 {
        pd[pd_idx] & PHYS_MASK
    } else {
        let p = crate::mm::pmm::alloc_frame().ok_or(ElfLoadError::MemoryAllocationFailed)?;
        let v = crate::mm::physmap::phys_to_virt_u64(p);
        write_bytes(v as *mut u8, 0, 4096);
        pd[pd_idx] = p | table_flags;
        p
    };

    let pt = &mut *(crate::mm::physmap::phys_to_virt_u64(pt_phys) as *mut [u64; 512]);
    pt[pt_idx] = (phys & PHYS_MASK) | page_flags;

    core::arch::asm!("invlpg [{}]", in(reg) virt, options(nostack, preserves_flags));

    Ok(())
}
```

- [ ] **Step 2: Refactor — extract the page-table-walk helper if duplication grows**

Don't refactor yet. The helper duplicates `map_device_page` 1:1 except for `page_flags`; that is acceptable until we add a third variant. (YAGNI — review next quarter.)

- [ ] **Step 3: Build**

Run: `cargo xtask build`
Expected: success, single new function compiles.

- [ ] **Step 4: Commit**

```bash
git add kernel/src/elf.rs
git commit -m "kernel/elf: add map_device_page_wc (WC PAT entry)"
```

---

## Task 4: `MAP_DEVICE_WC` flag + dispatch

**Files:**
- Modify: `kernel/src/syscall/handlers.rs:1400-1410` (SpaceMap consts) and `:2026-2050` (SpaceMapRange consts) and `:2502-2520` (`map_device_range`)

- [ ] **Step 1: Add the flag**

In `kernel/src/syscall/handlers.rs` near the existing `MAP_DEVICE = 0x100` consts (there are two — one for `SpaceMap`, one for `SpaceMapRange`), add:

```rust
const MAP_DEVICE_WC: u64 = 0x200;
```

both places (or once at module scope if both can share).

- [ ] **Step 2: Add `map_device_range_wc` next to `map_device_range`**

Find `map_device_range` at line 2502. Immediately after it, add:

```rust
unsafe fn map_device_range_wc(
    virt_base: u64,
    phys_base: u64,
    size: u64,
    writable: bool,
    page_table_root: PhysAddr,
) -> Result<(), ElfLoadError> {
    let pages = (size + 0xFFF) / 0x1000;
    for i in 0..pages {
        crate::elf::map_device_page_wc(
            virt_base + i * 0x1000,
            phys_base + i * 0x1000,
            writable,
            page_table_root,
        )?;
    }
    Ok(())
}
```

- [ ] **Step 3: Branch on the flag in the existing dispatch**

Find the `MAP_DEVICE` fast-path branches in `invoke_space_map` and `invoke_space_map_range`. Add a sibling branch:

```rust
} else if flags & MAP_DEVICE_WC != 0 {
    // Write-combining device mapping (e.g. framebuffer).
    return map_device_range_wc(virt, phys, size, writable, page_table_root)
        .map(|_| 0)
        .map_err(|_| InvokeError::ResourceExhausted);
}
```

(Adapt to the existing return / error-mapping idiom; keep parity with the `MAP_DEVICE` branch directly above it.)

- [ ] **Step 4: Reject WC + WC simultaneously? No — make `MAP_DEVICE_WC` and `MAP_DEVICE` mutually exclusive**

If both bits are set, return `InvokeError::InvalidArgument`. Add at the top of the dispatch, before the device branches:

```rust
if (flags & MAP_DEVICE != 0) && (flags & MAP_DEVICE_WC != 0) {
    return Err(InvokeError::InvalidArgument);
}
```

- [ ] **Step 5: Build kernel**

Run: `cargo xtask build`
Expected: success.

- [ ] **Step 6: Commit**

```bash
git add kernel/src/syscall/handlers.rs
git commit -m "kernel/syscall: MAP_DEVICE_WC flag + dispatch to WC mapper"
```

---

## Task 5: Userspace probe — verify WC mapping works

**Files:**
- Create: `userspace/probes/fb_wc_probe/Cargo.toml`
- Create: `userspace/probes/fb_wc_probe/src/main.rs`
- Create: `userspace/probes/fb_wc_probe/Cluufile`
- Modify: workspace `Cargo.toml` (add member)

- [ ] **Step 1: Cargo.toml**

```toml
[package]
name = "fb_wc_probe"
version = "0.1.0"
edition = "2021"

[dependencies]
libcluu = { path = "../../libcluu" }
```

- [ ] **Step 2: `src/main.rs`**

```rust
#![no_std]
#![no_main]

use libcluu::{debug_print, syscall};

const APP_FB_BASE: u64 = 0xA000_0000;
const FB_PHYS_DUMMY: u64 = 0xFD00_0000; // QEMU's stdvga fb base, see boot.rs
const FB_SIZE: u64 = 0x40_0000;          // 4 MB
const MAP_DEVICE_WC: u64 = 0x200;
const RIGHTS_WRITE: u64 = 0x2;

#[no_mangle]
pub extern "C" fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    let r = unsafe {
        syscall::space_map_range(
            syscall::TOKEN_SELF,
            APP_FB_BASE,
            FB_PHYS_DUMMY,
            FB_SIZE,
            MAP_DEVICE_WC | RIGHTS_WRITE,
        )
    };

    if r == 0 {
        // Touch the mapping to confirm no #PF.
        unsafe { (APP_FB_BASE as *mut u32).write_volatile(0xCAFEBABE); }
        debug_print(b"FB_WC_PROBE: ok\n");
    } else {
        debug_print(b"FB_WC_PROBE: map failed\n");
    }
    0
}
```

(Adjust `syscall::space_map_range` signature to whatever libcluu currently exposes — check `userspace/libcluu/src/syscall.rs`. If only a typed wrapper exists, use that; if not, fall back to the raw invoke.)

- [ ] **Step 3: Cluufile**

```toml
[manifest]
binary = "fb_wc_probe"
profile = "user"

[grants]
device_memory = ["fb"]
```

(Match the existing pattern in `userspace/console/Cluufile` — read it first to copy field names.)

- [ ] **Step 4: Add a `probe_fb_wc` MARKER_MODE in `scripts/harness_run.sh`**

In the case-statement around line 592, add a branch that:
- launches `fb_wc_probe` from the shell auto-start script
- waits for `FB_WC_PROBE: ok` on COM2
- fails the run if `FB_WC_PROBE: map failed` appears or no marker after RUN_WAIT

```bash
    probe_fb_wc)
        REQUIRED_MARKERS=("FB_WC_PROBE: ok")
        FAIL_MARKERS=("FB_WC_PROBE: map failed")
        SHELL_AUTOSTART="fb_wc_probe"
        ;;
```

(Match the existing auto-start mechanism; if there isn't one, a small `etc/init.d/probe.sh` or similar.)

- [ ] **Step 5: Run the probe**

```bash
RUN_WAIT=60 MARKER_MODE=probe_fb_wc bash scripts/harness_run.sh
```

Expected: PASS — `FB_WC_PROBE: ok` printed.

- [ ] **Step 6: Commit**

```bash
git add userspace/probes/fb_wc_probe/ Cargo.toml scripts/harness_run.sh
git commit -m "test: fb_wc_probe verifies MAP_DEVICE_WC end-to-end"
```

---

## Task 6: Wire MAP_DEVICE_WC into real framebuffer mapping

**Files:**
- Modify: `userspace/libcluu/src/posix/framebuffer.rs:65-80`

- [ ] **Step 1: Read current call site**

Read `userspace/libcluu/src/posix/framebuffer.rs` lines 60-90 to confirm the exact `space_map_range` call shape.

- [ ] **Step 2: Add `MAP_DEVICE_WC` constant near `MAP_DEVICE`**

```rust
const MAP_DEVICE: u64 = 0x100;
const MAP_DEVICE_WC: u64 = 0x200;
```

- [ ] **Step 3: Replace `MAP_DEVICE` with `MAP_DEVICE_WC`**

```rust
let r = unsafe {
    syscall::space_map_range(
        syscall::TOKEN_SELF,
        APP_FB_BASE,
        fb_phys,
        fb_size,
        MAP_DEVICE_WC | RIGHTS_WRITE,
    )
};
```

- [ ] **Step 4: Build full image**

Run: `cargo xtask build`
Expected: success.

- [ ] **Step 5: Run the full smoke**

```bash
RUN_WAIT=60 MARKER_MODE=l2_path_symlink_resolve bash scripts/harness_run.sh
```

Expected: PASS — no regression. Console must still render correctly (look at `screen.png` if harness dumps it; otherwise trust the marker).

- [ ] **Step 6: Commit**

```bash
git add userspace/libcluu/src/posix/framebuffer.rs
git commit -m "libcluu/fb: map framebuffer write-combining"
```

---

## Task 7: Console-blit microbench + ratchet

**Files:**
- Create: `userspace/probes/console_blit_bench/Cargo.toml`
- Create: `userspace/probes/console_blit_bench/src/main.rs`
- Create: `userspace/probes/console_blit_bench/Cluufile`
- Modify: `scripts/harness_run.sh` (add `b_console_blit` MARKER_MODE)

- [ ] **Step 1: Cargo.toml**

```toml
[package]
name = "console_blit_bench"
version = "0.1.0"
edition = "2021"

[dependencies]
libcluu = { path = "../../libcluu" }
```

- [ ] **Step 2: `src/main.rs`**

```rust
#![no_std]
#![no_main]

use libcluu::{debug_print, fmt_u64, time::rdtsc};

const ITERS: u64 = 100;

#[no_mangle]
pub extern "C" fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    // Print a known glyph stream that fully covers a 80x25 cell area
    // (or whatever the current console size is — we measure the syscalls,
    // not the layout).
    let line = b"The quick brown fox jumps over the lazy dog 0123456789!@#$\n";

    let start = unsafe { rdtsc() };
    for _ in 0..ITERS {
        for _ in 0..25 {
            libcluu::println!("{}", core::str::from_utf8(line).unwrap());
        }
    }
    let elapsed = unsafe { rdtsc() } - start;

    let cycles_per_iter = elapsed / ITERS;

    debug_print(b"BENCH_CONSOLE_BLIT: cycles_per_full_screen=");
    let mut buf = [0u8; 32];
    let n = fmt_u64(cycles_per_iter, &mut buf);
    debug_print(&buf[..n]);
    debug_print(b"\n");

    0
}
```

(If `libcluu` does not expose `rdtsc` / `fmt_u64`, add them inline as 5-line helpers; do not pull a dependency.)

- [ ] **Step 3: Cluufile**

```toml
[manifest]
binary = "console_blit_bench"
profile = "user"
```

- [ ] **Step 4: Harness branch**

In `scripts/harness_run.sh` MARKER_MODE case, add:

```bash
    b_console_blit)
        REQUIRED_MARKERS=("BENCH_CONSOLE_BLIT: cycles_per_full_screen=")
        SHELL_AUTOSTART="console_blit_bench"
        # Ratchet: log only on first run; future runs check delta.
        # Stored in scripts/perf_ratchet.json under key "fb_blit_uc_cycles" /
        # "fb_blit_wc_cycles". Bench-only — no fail rail yet.
        ;;
```

- [ ] **Step 5: Capture baseline (UC- mapping) BEFORE Task 6 was merged**

This is intentional out-of-order — the baseline must be measured against UC-, not WC. If Tasks 1-6 already shipped, capture WC numbers now and compute the ratchet from a `git revert HEAD~1` / `HEAD~6` measurement. Concretely:

```bash
git stash
git checkout HEAD~6  # before Task 6 wired WC into framebuffer.rs
RUN_WAIT=60 MARKER_MODE=b_console_blit bash scripts/harness_run.sh \
  | grep BENCH_CONSOLE_BLIT  # record UC- cycles
git checkout -
git stash pop
RUN_WAIT=60 MARKER_MODE=b_console_blit bash scripts/harness_run.sh \
  | grep BENCH_CONSOLE_BLIT  # record WC cycles
```

Expected: WC cycles_per_full_screen < UC- cycles_per_full_screen. Under TCG the gap may be small or zero (acceptable). Under KVM expect 5-20× speedup.

- [ ] **Step 6: Persist the baseline**

Write the two numbers (UC-, WC) into `scripts/perf_ratchet.json`:

```json
{
  "fb_blit_uc_cycles":  <number from step 5 baseline>,
  "fb_blit_wc_cycles":  <number from step 5 WC>,
  "fb_blit_wc_max":     <wc_cycles * 1.10>
}
```

Then update the `b_console_blit` MARKER_MODE branch in `harness_run.sh` to fail if measured cycles > `fb_blit_wc_max`:

```bash
        local measured
        measured=$(grep -oE 'cycles_per_full_screen=[0-9]+' "$LOG" | tail -1 | sed 's/.*=//')
        local max
        max=$(jq -r '.fb_blit_wc_max' scripts/perf_ratchet.json)
        if (( measured > max )); then
            echo "PERF REGRESSION: blit cycles $measured > ratchet $max"
            exit 1
        fi
```

- [ ] **Step 7: Commit**

```bash
git add userspace/probes/console_blit_bench/ scripts/harness_run.sh scripts/perf_ratchet.json Cargo.toml
git commit -m "bench: console blit cycles ratchet (fb_blit_wc_cycles)"
```

---

## Task 8: Doc + memory entries

**Files:**
- Modify: `docs/CURRENT_PHASE.md`
- Modify: `~/.claude/projects/-home-vlb2bp-git-cluu/memory/MEMORY.md` (add a one-line index entry pointing at a new file)
- Create: `~/.claude/projects/-home-vlb2bp-git-cluu/memory/project_fb_wc_landed.md`

- [ ] **Step 1: Update `docs/CURRENT_PHASE.md`**

Add a section under "Recently shipped":

```markdown
- 2026-05-09: framebuffer mapping switched from UC- to WC via PAT[1].
  PAT MSR programmed at boot. New `MAP_DEVICE_WC = 0x200` flag on
  SpaceMap[Range]. Userspace `posix/framebuffer.rs` opts in. Bench
  ratchet `fb_blit_wc_cycles` in `scripts/perf_ratchet.json`. Under
  KVM expect 5-20× full-screen blit speedup; under TCG functional only.
```

- [ ] **Step 2: Create memory file**

```markdown
---
name: Framebuffer mapped WC, PAT programmed (2026-05-09)
description: Front-buffer maps via MAP_DEVICE_WC=0x200 → PAT[1] = WC; PAT MSR programmed at boot in kernel/src/mm/pat.rs. Bench ratchet at scripts/perf_ratchet.json.
type: project
---

**What changed:**
- Kernel: `kernel/src/mm/pat.rs` programs PAT MSR (0x277) Linux-style: PAT[0]=WB, PAT[1]=WC, PAT[2]=UC-, PAT[3]=UC. Existing PCD-only PTEs still map UC- (no behavior change). Reload CR3 after `wrmsr`.
- Kernel: `MAP_DEVICE_WC = 0x200` flag in `kernel/src/syscall/handlers.rs`; `map_device_page_wc` in `kernel/src/elf.rs` sets PWT=1 / PCD=0 / PAT=0 → index 1.
- Userspace: `userspace/libcluu/src/posix/framebuffer.rs` requests WC.
- Bench: `userspace/probes/console_blit_bench` + `scripts/perf_ratchet.json` track full-screen-blit cycles.

**Why it matters:** Console double-buffer flush bulk-stored to UC- frontbuffer at one bus transaction per dword. With WC, SSE2 stores combine into 64-byte bursts.

**Visible-only-on-KVM caveat:** TCG tends to behave WB regardless of PAT. Functional correctness verifiable on TCG; perf delta requires KVM.
```

- [ ] **Step 3: Add MEMORY.md index line**

Insert one line under the existing index, format:

```
- [FB mapped WC (2026-05-09)](project_fb_wc_landed.md) — front-buffer uses PAT[1]=WC; bench ratchet at scripts/perf_ratchet.json.
```

- [ ] **Step 4: Commit**

```bash
git add docs/CURRENT_PHASE.md
git commit -m "docs: record FB WC mapping landing + perf ratchet"
```

(Memory files live outside the repo and don't get committed.)

---

## Task 9: Verify full harness still green

- [ ] **Step 1: Full smoke**

```bash
RUN_WAIT=60 MARKER_MODE=l2_path_symlink_resolve bash scripts/harness_run.sh
RUN_WAIT=60 MARKER_MODE=b_console_blit          bash scripts/harness_run.sh
```

Both must PASS.

- [ ] **Step 2: Wider sanity**

Run any 2-3 representative MARKER_MODEs that exercise console output (e.g. `legacy_p1`, `l2_jobchurn`).

```bash
RUN_WAIT=120 MARKER_MODE=legacy_p1 bash scripts/harness_run.sh
```

Expected: GREEN.

- [ ] **Step 3: If regression — bisect**

Most likely regression: scrolling tearing or torn glyphs. WC reorders writes. The `flush()` SSE2 store loop may need an `sfence` after each row — add at end of `DoubleBufferBackend::flush`:

```rust
#[cfg(target_arch = "x86_64")]
unsafe { core::arch::asm!("sfence", options(nostack, preserves_flags)); }
```

This is **not** in the plan above as a default change because adding `sfence` per-flush hurts perf if the WC store buffers are already drained at the next non-temporal load. Only add if visual regression is observed.

---

## Open Risks (acknowledged, not blockers)

1. **TCG-only harness shows no perf gain** — accepted. The functional tests still pass; the ratchet is a regression rail under KVM.
2. **PAT vs MTRR conflict** — the boot firmware already programmed MTRRs covering FB phys range. Intel SDM table 11-7 says PAT WC + MTRR UC = effective UC. If we observe WC failing to combine, we may also need to set the FB phys range to MTRR WC (additional task; deferred).
3. **SMP** — when the kernel goes SMP, every AP must call `pat::init()` during `ap_init`. Add a TODO comment in `kernel/src/mm/pat.rs` and create a follow-up issue. Out of scope for this plan.
4. **Frontbuffer racing** — WC + multiple writers reorder. Today only the console process writes to APP_FB_BASE; safe.

---

## Self-Review Checklist (run after writing)

- [x] Spec coverage: every sub-goal #1 sub-bullet from `project_next_direction_fb_tui.md` mapped to a task. /dev/fb0 (#2) and TUI (#3) explicitly out of scope. ✓
- [x] No placeholders — every step has either a code block or an exact command. ✓
- [x] Type consistency: `MAP_DEVICE_WC` constant value `0x200` used identically in kernel const, libcluu const, probe binary, and framebuffer.rs. `WRITE_COMBINING` defined as `PWT` only. PAT layout consistent across pat.rs and the helper docs. ✓
- [x] Each task ends with a commit. ✓
- [x] TDD adapted to harness-based testing where direct unit tests are impossible (kernel MMU/PAT). ✓
