# CLUU Microkernel - Complete Implementation Guide v2

## Project Overview

CLUU is an L4-style microkernel written in Rust targeting x86_64. The design philosophy emphasizes minimality, capability-based security, and moving as much functionality as possible to userspace.

### Target Milestone

A working shell in userspace that can execute `cat <filename>` to print file contents, demonstrating:
- Preemptive scheduling with INITMODE/NORMALMODE
- IPC-based VFS and filesystem
- Zero-copy (or minimal-copy) buffer sharing via grants
- Userspace drivers for console I/O
- Capability-based security throughout (called token)

### Core Principles

1. **Minimal kernel**: Only scheduler, memory manager, IPC, capability validation, and interrupt routing
2. **Everything else in userspace**: Process manager, VFS, filesystems, device drivers
3. **Capability-based security**: All resources accessed via unforgeable capabilities
4. **Zero-copy where possible**: Grant/map pages instead of copying data
5. **OOP and SOLID**: Clean architecture with traits, composition, and well-known patterns
6. **Test-driven development**: Extensive unit testing from day one

---

## Architecture & Design Principles

### SOLID Principles (MANDATORY)

All code MUST follow SOLID principles:

#### S - Single Responsibility
```rust
// GOOD: Each struct has one job
pub struct PhysicalPageAllocator { /* only allocates physical pages */ }
pub struct VirtualMemoryMapper { /* only maps virtual addresses */ }
pub struct AddressSpaceManager { /* coordinates allocation and mapping */ }

// BAD: God object doing everything
pub struct MemoryManager { 
    // allocates, maps, manages spaces, handles faults... NO!
}
```

#### O - Open/Closed
```rust
// GOOD: Extend via traits, not modification
pub trait Scheduler {
    fn pick_next(&mut self) -> Option<ThreadId>;
    fn add_thread(&mut self, thread: ThreadId, priority: Priority);
    fn remove_thread(&mut self, thread: ThreadId);
}

pub struct PriorityBitmapScheduler { /* ... */ }
pub struct RoundRobinScheduler { /* ... */ }
// Both implement Scheduler trait
```

#### L - Liskov Substitution
```rust
// Any implementor of a trait must be substitutable
pub trait PageAllocator {
    fn alloc(&mut self, count: usize) -> Option<PhysAddr>;
    fn free(&mut self, addr: PhysAddr, count: usize);
}

// any PageAllocator implementation, BitmapAllocator, etc. all work interchangeably
```

#### I - Interface Segregation
```rust
// GOOD: Small, focused traits
pub trait Readable {
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, Error>;
}

pub trait Writable {
    fn write(&mut self, offset: usize, buf: &[u8]) -> Result<usize, Error>;
}

pub trait Seekable {
    fn seek(&mut self, pos: SeekFrom) -> Result<usize, Error>;
}

// Combine as needed
pub trait File: Readable + Writable + Seekable {}

// BAD: One giant trait with everything
pub trait FileOperations {
    fn read(&self, ...);
    fn write(&mut self, ...);
    fn seek(&mut self, ...);
    fn truncate(&mut self, ...);
    fn sync(&self);
    fn lock(&mut self);
    // ... 20 more methods
}
```

#### D - Dependency Inversion
```rust
// GOOD: Depend on abstractions
pub struct IpcSubsystem<S: Scheduler, M: MemoryOperations> {
    scheduler: S,
    memory: M,
}

// BAD: Depend on concrete types
pub struct IpcSubsystem {
    scheduler: PriorityBitmapScheduler,  // Concrete!
    memory: any PageAllocator implementation,              // Concrete!
}
```

### Design Patterns to Use

#### Repository Pattern (for capabilities, threads, spaces)
```rust
pub trait Repository<K, V> {
    fn get(&self, key: &K) -> Option<&V>;
    fn get_mut(&mut self, key: &K) -> Option<&mut V>;
    fn insert(&mut self, key: K, value: V) -> Result<(), Error>;
    fn remove(&mut self, key: &K) -> Option<V>;
    fn contains(&self, key: &K) -> bool;
}

pub struct ThreadRepository {
    threads: BTreeMap<ThreadId, Thread>,
}

impl Repository<ThreadId, Thread> for ThreadRepository {
    // ...
}
```

#### Strategy Pattern (for scheduling, allocation)
```rust
pub trait AllocationStrategy {
    fn allocate(&mut self, size: usize, align: usize) -> Option<usize>;
    fn deallocate(&mut self, addr: usize, size: usize);
}

pub struct BuddyStrategy { /* ... */ }
pub struct BitmapStrategy { /* ... */ }
pub struct BumpStrategy { /* ... */ }
```

#### Observer Pattern (for IRQ notifications)
```rust
pub trait IrqObserver {
    fn on_irq(&mut self, irq: u8);
}

pub struct IrqDispatcher {
    observers: [Option<Box<dyn IrqObserver>>; 256],
}
```

#### Builder Pattern (for complex object construction)
```rust
pub struct ThreadBuilder {
    space: Option<SpaceId>,
    entry: Option<VirtAddr>,
    stack: Option<VirtAddr>,
    priority: Priority,
    flags: ThreadFlags,
}

impl ThreadBuilder {
    pub fn new() -> Self { /* ... */ }
    pub fn space(mut self, space: SpaceId) -> Self { self.space = Some(space); self }
    pub fn entry(mut self, entry: VirtAddr) -> Self { self.entry = Some(entry); self }
    pub fn stack(mut self, stack: VirtAddr) -> Self { self.stack = Some(stack); self }
    pub fn priority(mut self, priority: Priority) -> Self { self.priority = priority; self }
    pub fn cooperative(mut self) -> Self { self.flags |= ThreadFlags::COOPERATIVE; self }
    pub fn build(self) -> Result<Thread, Error> { /* ... */ }
}

// Usage
let thread = ThreadBuilder::new()
    .space(space_id)
    .entry(entry_point)
    .stack(stack_top)
    .priority(Priority::new(200))
    .cooperative()
    .build()?;
```

#### Factory Pattern (for creating kernel objects)
```rust
pub trait ThreadFactory {
    fn create_thread(&mut self, config: ThreadConfig) -> Result<ThreadId, Error>;
}

pub trait SpaceFactory {
    fn create_space(&mut self) -> Result<SpaceId, Error>;
}
```

---

## Build System

### Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────┐
│                        Build Pipeline                                │
├─────────────────────────────────────────────────────────────────────┤
│                                                                      │
│   ┌──────────┐     ┌──────────┐     ┌──────────┐                   │
│   │ libcluu  │     │ klibcluu │     │  NASM    │                   │
│   │  (rlib)  │     │  (rlib)  │     │  .asm    │                   │
│   └────┬─────┘     └────┬─────┘     └────┬─────┘                   │
│        │                │                │                          │
│        ▼                ▼                ▼                          │
│   ┌─────────────────────────────────────────────┐                   │
│   │              Userspace ELFs                 │                   │
│   │  init, procmgr, vfs, ramfs, console,        │                   │
│   │  shell, cat (standalone executables)        │                   │
│   └─────────────────┬───────────────────────────┘                   │
│                     │                                               │
│                     ▼                                               │
│   ┌─────────────────────────────────────────────┐                   │
│   │           initrd/ folder                    │                   │
│   │  sys/init, sys/procmgr, sys/vfs, ...       │──────┐            │
│   └─────────────────────────────────────────────┘      │            │
│                                                        ▼            │
│   ┌──────────┐     ┌──────────┐              ┌──────────────┐      │
│   │  kernel  │     │ klibcluu │              │ initrd.tar   │      │
│   │  (ELF)   │◄────┤  (rlib)  │              └──────┬───────┘      │
│   └────┬─────┘     └──────────┘                     │              │
│        │                                            │              │
│        ▼                                            ▼              │
│   ┌─────────────────────────────────────────────────────────┐      │
│   │                    BOOTBOOT                              │      │
│   │              mkbootimg → cluu.img                        │      │
│   └─────────────────────────────────────────────────────────┘      │
│                                                                      │
└─────────────────────────────────────────────────────────────────────┘
```

**Key Points:**
- **libcluu**: Userspace library (rlib) - syscall wrappers, IPC helpers
- **klibcluu**: Kernel library (rlib) - kprint!, debug utils, kernel helpers
- **Userspace programs**: Standalone ELF executables, linked against libcluu
- **Kernel**: Does NOT depend on userspace crates - they are separate
- **xtask**: Rust-based build orchestration (idiomatic pattern)

### Target Configurations

**Kernel Target** (`x86_64-cluu-kernel.json`):
```json
{
    "llvm-target": "x86_64-unknown-none-elf",
    "data-layout": "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-f80:128-n8:16:32:64-S128",
    "arch": "x86_64",
    "target-endian": "little",
    "target-pointer-width": "64",
    "target-c-int-width": "32",
    "os": "none",
    "executables": true,
    "linker-flavor": "ld.lld",
    "linker": "rust-lld",
    "panic-strategy": "abort",
    "disable-redzone": true,
    "features": "-mmx,-sse,-sse2,+soft-float",
    "relocation-model": "static",
    "code-model": "kernel",
    "exe-suffix": ".elf",
    "has-rpath": false,
    "position-independent-executables": false,
    "static-position-independent-executables": false,
    "needs-plt": false,
    "has-thread-local": false,
    "dynamic-linking": false,
    "pre-link-args": {
        "ld.lld": ["-Tkernel/linker.ld"]
    }
}
```

**Userspace Target** (`x86_64-cluu-user.json`):
```json
{
    "llvm-target": "x86_64-unknown-none-elf",
    "data-layout": "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-f80:128-n8:16:32:64-S128",
    "arch": "x86_64",
    "target-endian": "little",
    "target-pointer-width": "64",
    "target-c-int-width": "32",
    "os": "none",
    "executables": true,
    "linker-flavor": "ld.lld",
    "linker": "rust-lld",
    "panic-strategy": "abort",
    "disable-redzone": false,
    "features": "-mmx,-sse,-sse2,+soft-float",
    "relocation-model": "static",
    "code-model": "small",
    "exe-suffix": ".elf",
    "has-rpath": false,
    "position-independent-executables": false,
    "static-position-independent-executables": false,
    "needs-plt": false,
    "has-thread-local": false,
    "dynamic-linking": false,
    "pre-link-args": {
        "ld.lld": ["-Tuserspace/user.ld"]
    }
}
```

### Cargo Configuration

```toml
# .cargo/config.toml

[unstable]
build-std = ["core", "alloc"]
build-std-features = ["compiler-builtins-mem"]

# Kernel target
[target.x86_64-cluu-kernel]
rustflags = [
    "-C", "opt-level=0",
    "-C", "lto=off",
    "-C", "link-arg=-nostdlib",
    "-C", "link-arg=-static",
]

# Userspace target  
[target.x86_64-cluu-user]
rustflags = [
    "-C", "opt-level=0",
    "-C", "lto=off",
    "-C", "link-arg=-nostdlib",
    "-C", "link-arg=-static",
]

[alias]
xtask = "run --package xtask --"
```

### Workspace Structure

```toml
# Cargo.toml (workspace root)
[workspace]
resolver = "2"
members = [
    # Build orchestration
    "xtask",
    
    # Kernel
    "kernel",
    "klibcluu",
    
    # Userspace
    "userspace/libcluu",
    "userspace/init",
    "userspace/procmgr",
    "userspace/vfs",
    "userspace/ramfs",
    "userspace/console",
    "userspace/shell",
    "userspace/cat",
]

# Exclude xtask from default members (it uses std)
default-members = [
    "kernel",
    "klibcluu",
    "userspace/libcluu",
    "userspace/init",
    "userspace/procmgr",
    "userspace/vfs",
    "userspace/ramfs",
    "userspace/console",
    "userspace/shell",
    "userspace/cat",
]

[workspace.package]
version = "0.1.0"
edition = "2021"
authors = ["CLUU Team"]
license = "MIT"

[workspace.dependencies]
# Shared dependencies
bitflags = "2.4"
spin = "0.9"
log = "0.4"

# x86_64 crate for low-level CPU operations
x86_64 = "0.15"

# Testing
proptest = "1.4"
```

### klibcluu - Kernel Library

```toml
# klibcluu/Cargo.toml
[package]
name = "klibcluu"
version.workspace = true
edition.workspace = true
description = "CLUU kernel support library - debug, utils, helpers"

[dependencies]
bitflags.workspace = true
spin.workspace = true
x86_64.workspace = true

[dev-dependencies]
proptest.workspace = true

[lib]
name = "klibcluu"
crate-type = ["rlib"]

[features]
default = []
test-mock = []
```

```rust
// klibcluu/src/lib.rs
//! Kernel support library for CLUU
//! 
//! Provides:
//! - Debug output (kprint!, kprintln!)
//! - Kernel utilities
//! - Common helpers
//! - Shared types between kernel modules

#![no_std]
#![cfg_attr(test, allow(unused))]

pub mod debug;
pub mod sync;
pub mod collections;
pub mod util;

// Re-exports
pub use debug::{kprint, kprintln, set_debug_output};
pub use sync::SpinLock;
```

```rust
// klibcluu/src/debug.rs
//! Kernel debug output facilities

use core::fmt::{self, Write};
use spin::Mutex;

/// Debug output trait - implement for your output device
pub trait DebugOutput: Send {
    fn write_str(&mut self, s: &str);
    fn write_char(&mut self, c: char) {
        let mut buf = [0u8; 4];
        self.write_str(c.encode_utf8(&mut buf));
    }
}

/// Null output (discards everything)
pub struct NullOutput;

impl DebugOutput for NullOutput {
    fn write_str(&mut self, _s: &str) {}
}

/// Global debug output
static DEBUG_OUTPUT: Mutex<Option<&'static mut dyn DebugOutput>> = Mutex::new(None);

/// Set the global debug output
/// 
/// # Safety
/// Must only be called once during kernel initialization
pub unsafe fn set_debug_output(output: &'static mut dyn DebugOutput) {
    *DEBUG_OUTPUT.lock() = Some(output);
}

/// Writer that uses the global debug output
pub struct DebugWriter;

impl Write for DebugWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if let Some(ref mut output) = *DEBUG_OUTPUT.lock() {
            output.write_str(s);
        }
        Ok(())
    }
}

/// Print to debug output
#[macro_export]
macro_rules! kprint {
    ($($arg:tt)*) => {
        {
            use core::fmt::Write;
            let _ = write!($crate::debug::DebugWriter, $($arg)*);
        }
    };
}

/// Print line to debug output
#[macro_export]
macro_rules! kprintln {
    () => { $crate::kprint!("\n") };
    ($($arg:tt)*) => {
        $crate::kprint!("{}\n", format_args!($($arg)*))
    };
}

/// Log levels
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

/// Log with level
#[macro_export]
macro_rules! klog {
    ($level:expr, $($arg:tt)*) => {
        $crate::kprintln!("[{:5}] {}", 
            match $level {
                $crate::debug::LogLevel::Error => "ERROR",
                $crate::debug::LogLevel::Warn  => "WARN ",
                $crate::debug::LogLevel::Info  => "INFO ",
                $crate::debug::LogLevel::Debug => "DEBUG",
                $crate::debug::LogLevel::Trace => "TRACE",
            },
            format_args!($($arg)*)
        )
    };
}

#[macro_export]
macro_rules! kerror { ($($arg:tt)*) => { $crate::klog!($crate::debug::LogLevel::Error, $($arg)*) }; }
#[macro_export]
macro_rules! kwarn  { ($($arg:tt)*) => { $crate::klog!($crate::debug::LogLevel::Warn,  $($arg)*) }; }
#[macro_export]
macro_rules! kinfo  { ($($arg:tt)*) => { $crate::klog!($crate::debug::LogLevel::Info,  $($arg)*) }; }
#[macro_export]
macro_rules! kdebug { ($($arg:tt)*) => { $crate::klog!($crate::debug::LogLevel::Debug, $($arg)*) }; }
#[macro_export]
macro_rules! ktrace { ($($arg:tt)*) => { $crate::klog!($crate::debug::LogLevel::Trace, $($arg)*) }; }
```

```rust
// klibcluu/src/util.rs
//! Common utility functions

/// Align value up to alignment
#[inline]
pub const fn align_up(value: u64, align: u64) -> u64 {
    (value + align - 1) & !(align - 1)
}

/// Align value down to alignment
#[inline]
pub const fn align_down(value: u64, align: u64) -> u64 {
    value & !(align - 1)
}

/// Check if value is aligned
#[inline]
pub const fn is_aligned(value: u64, align: u64) -> bool {
    value & (align - 1) == 0
}

/// Page size constant
pub const PAGE_SIZE: u64 = 4096;

/// Align to page boundary (up)
#[inline]
pub const fn page_align_up(value: u64) -> u64 {
    align_up(value, PAGE_SIZE)
}

/// Align to page boundary (down)
#[inline]
pub const fn page_align_down(value: u64) -> u64 {
    align_down(value, PAGE_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_align_up() {
        assert_eq!(align_up(0, 4096), 0);
        assert_eq!(align_up(1, 4096), 4096);
        assert_eq!(align_up(4096, 4096), 4096);
        assert_eq!(align_up(4097, 4096), 8192);
    }
    
    #[test]
    fn test_align_down() {
        assert_eq!(align_down(0, 4096), 0);
        assert_eq!(align_down(1, 4096), 0);
        assert_eq!(align_down(4096, 4096), 4096);
        assert_eq!(align_down(4097, 4096), 4096);
    }
}
```

### Kernel Cargo.toml

```toml
# kernel/Cargo.toml
[package]
name = "cluu-kernel"
version.workspace = true
edition.workspace = true
description = "CLUU microkernel"

[dependencies]
bitflags.workspace = true
spin.workspace = true
log.workspace = true
x86_64.workspace = true

# Kernel support library
klibcluu = { path = "../klibcluu" }

[build-dependencies]
cc = "1.0"

[dev-dependencies]
proptest.workspace = true

[features]
default = []
test-mock = ["klibcluu/test-mock"]

[[bin]]
name = "kernel"
path = "src/main.rs"

[profile.dev]
opt-level = 0
lto = false
panic = "abort"

[profile.release]
opt-level = 0
lto = false
panic = "abort"
```

### Userspace Library Cargo.toml

```toml
# userspace/libcluu/Cargo.toml
[package]
name = "libcluu"
version.workspace = true
edition.workspace = true
description = "CLUU userspace library - syscalls, IPC, runtime"

[dependencies]
bitflags.workspace = true

[dev-dependencies]
proptest.workspace = true

[lib]
name = "libcluu"
crate-type = ["rlib"]

[features]
default = []
std = []  # For testing on host
```

### Userspace Program Cargo.toml (Standalone ELF)

```toml
# userspace/init/Cargo.toml
[package]
name = "cluu-init"
version.workspace = true
edition.workspace = true
description = "CLUU init process"

[dependencies]
libcluu = { path = "../libcluu" }

# Produces standalone ELF executable
[[bin]]
name = "init"
path = "src/main.rs"

[profile.dev]
opt-level = 0
lto = false
panic = "abort"

[profile.release]
opt-level = 0
lto = false
panic = "abort"
```

```toml
# userspace/procmgr/Cargo.toml
[package]
name = "cluu-procmgr"
version.workspace = true
edition.workspace = true
description = "CLUU process manager server"

[dependencies]
libcluu = { path = "../libcluu" }

[[bin]]
name = "procmgr"
path = "src/main.rs"

[profile.dev]
opt-level = 0
lto = false
panic = "abort"

[profile.release]
opt-level = 0
lto = false
panic = "abort"
```

### Userspace Linker Script

```ld
/* userspace/user.ld */
/* Linker script for CLUU userspace programs */

ENTRY(_start)

SECTIONS
{
    /* Userspace programs start at 0x400000 */
    . = 0x400000;
    
    .text : ALIGN(4K)
    {
        *(.text._start)   /* Entry point first */
        *(.text .text.*)
    }
    
    .rodata : ALIGN(4K)
    {
        *(.rodata .rodata.*)
    }
    
    .data : ALIGN(4K)
    {
        *(.data .data.*)
    }
    
    .bss : ALIGN(4K)
    {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        __bss_end = .;
    }
    
    /* Stack grows down from 0x800000 */
    . = 0x800000;
    __stack_top = .;
    
    /DISCARD/ :
    {
        *(.eh_frame)
        *(.comment)
        *(.note.*)
    }
}
```

### xtask - Build Orchestration (Idiomatic Rust Pattern)

```toml
# xtask/Cargo.toml
[package]
name = "xtask"
version = "0.1.0"
edition = "2021"

# xtask uses std - it runs on the host
[dependencies]
anyhow = "1.0"
clap = { version = "4.4", features = ["derive"] }
```

```rust
// xtask/src/main.rs
//! Build orchestration for CLUU
//! 
//! Usage:
//!   cargo xtask build          # Build everything
//!   cargo xtask run            # Build and run in QEMU
//!   cargo xtask test           # Run all tests
//!   cargo xtask clean          # Clean all build artifacts

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::fs;

#[derive(Parser)]
#[command(name = "xtask", about = "CLUU build system")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Build everything (kernel + userspace + disk image)
    Build {
        #[arg(long, default_value = "release")]
        profile: String,
    },
    /// Build and run in QEMU
    Run {
        #[arg(long, default_value = "release")]
        profile: String,
    },
    /// Run all tests
    Test,
    /// Clean all build artifacts
    Clean,
    /// Build only userspace programs
    Userspace {
        #[arg(long, default_value = "release")]
        profile: String,
    },
    /// Build only kernel
    Kernel {
        #[arg(long, default_value = "release")]
        profile: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    
    match cli.command {
        Commands::Build { profile } => {
            build_userspace(&profile)?;
            build_kernel(&profile)?;
            create_initrd(&profile)?;
            create_disk_image(&profile)?;
            println!("✓ Build complete: target/cluu.img");
        }
        Commands::Run { profile } => {
            build_userspace(&profile)?;
            build_kernel(&profile)?;
            create_initrd(&profile)?;
            create_disk_image(&profile)?;
            run_qemu()?;
        }
        Commands::Test => {
            run_tests()?;
        }
        Commands::Clean => {
            clean()?;
        }
        Commands::Userspace { profile } => {
            build_userspace(&profile)?;
        }
        Commands::Kernel { profile } => {
            build_kernel(&profile)?;
        }
    }
    
    Ok(())
}

fn project_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn build_userspace(profile: &str) -> Result<()> {
    println!("▸ Building userspace programs...");
    
    let userspace_crates = [
        "userspace/init",
        "userspace/procmgr",
        "userspace/vfs",
        "userspace/ramfs",
        "userspace/console",
        "userspace/shell",
        "userspace/cat",
    ];
    
    let target_json = project_root().join("x86_64-cluu-user.json");
    
    for crate_path in &userspace_crates {
        let crate_name = Path::new(crate_path)
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        
        println!("  Building {}...", crate_name);
        
        let mut cmd = Command::new("cargo");
        cmd.current_dir(project_root())
            .args([
                "build",
                "--manifest-path", &format!("{}/Cargo.toml", crate_path),
                "--target", target_json.to_str().unwrap(),
                "-Z", "build-std=core,alloc",
                "-Z", "build-std-features=compiler-builtins-mem",
            ]);
        
        if profile == "release" {
            cmd.arg("--release");
        }
        
        let status = cmd.status().context("Failed to run cargo")?;
        if !status.success() {
            bail!("Failed to build {}", crate_name);
        }
    }
    
    println!("  ✓ Userspace built");
    Ok(())
}

fn build_kernel(profile: &str) -> Result<()> {
    println!("▸ Building kernel...");
    
    let target_json = project_root().join("x86_64-cluu-kernel.json");
    
    // First, assemble NASM files
    assemble_nasm()?;
    
    let mut cmd = Command::new("cargo");
    cmd.current_dir(project_root())
        .args([
            "build",
            "--manifest-path", "kernel/Cargo.toml",
            "--target", target_json.to_str().unwrap(),
            "-Z", "build-std=core,alloc",
            "-Z", "build-std-features=compiler-builtins-mem",
        ]);
    
    if profile == "release" {
        cmd.arg("--release");
    }
    
    let status = cmd.status().context("Failed to run cargo")?;
    if !status.success() {
        bail!("Failed to build kernel");
    }
    
    println!("  ✓ Kernel built");
    Ok(())
}

fn assemble_nasm() -> Result<()> {
    println!("  Assembling NASM files...");
    
    let asm_dir = project_root().join("kernel/src/arch/x86_64");
    let out_dir = project_root().join("target/asm");
    
    fs::create_dir_all(&out_dir)?;
    
    let asm_files = ["boot.asm", "context.asm", "interrupts.asm", "syscall.asm"];
    
    for asm_file in &asm_files {
        let src = asm_dir.join(asm_file);
        if !src.exists() {
            continue; // Skip if file doesn't exist yet
        }
        
        let obj_name = asm_file.replace(".asm", ".o");
        let obj = out_dir.join(&obj_name);
        
        let status = Command::new("nasm")
            .args([
                "-f", "elf64",
                "-g",
                "-F", "dwarf",
                "-o", obj.to_str().unwrap(),
                src.to_str().unwrap(),
            ])
            .status()
            .context("Failed to run NASM")?;
        
        if !status.success() {
            bail!("NASM failed for {}", asm_file);
        }
    }
    
    Ok(())
}

fn create_initrd(profile: &str) -> Result<()> {
    println!("▸ Creating initrd...");
    
    let target_dir = project_root()
        .join("target/x86_64-cluu-user")
        .join(profile);
    
    let initrd_dir = project_root().join("target/initrd");
    
    // Create directory structure
    fs::create_dir_all(initrd_dir.join("sys"))?;
    fs::create_dir_all(initrd_dir.join("bin"))?;
    fs::create_dir_all(initrd_dir.join("etc"))?;
    
    // Copy system servers to initrd/sys/
    let sys_programs = ["init", "procmgr", "vfs", "ramfs", "console"];
    for prog in &sys_programs {
        let src = target_dir.join(prog);
        let dst = initrd_dir.join("sys").join(prog);
        if src.exists() {
            fs::copy(&src, &dst)
                .with_context(|| format!("Failed to copy {}", prog))?;
            println!("  Copied sys/{}", prog);
        } else {
            println!("  Warning: {} not found", prog);
        }
    }
    
    // Copy user programs to initrd/bin/
    let bin_programs = ["shell", "cat"];
    for prog in &bin_programs {
        let src = target_dir.join(prog);
        let dst = initrd_dir.join("bin").join(prog);
        if src.exists() {
            fs::copy(&src, &dst)
                .with_context(|| format!("Failed to copy {}", prog))?;
            println!("  Copied bin/{}", prog);
        }
    }
    
    // Create etc/motd
    fs::write(initrd_dir.join("etc/motd"), "Welcome to CLUU!\n")?;
    
    // Create tar archive
    let tar_path = project_root().join("target/initrd.tar");
    let status = Command::new("tar")
        .current_dir(&initrd_dir)
        .args(["cvf", tar_path.to_str().unwrap(), "."])
        .status()
        .context("Failed to create tar")?;
    
    if !status.success() {
        bail!("Failed to create initrd.tar");
    }
    
    println!("  ✓ initrd.tar created");
    Ok(())
}

fn create_disk_image(profile: &str) -> Result<()> {
    println!("▸ Creating disk image...");
    
    let bootboot_json = project_root().join("bootboot.json");
    let output_img = project_root().join("target/cluu.img");
    
    let status = Command::new("mkbootimg")
        .current_dir(project_root())
        .args([
            bootboot_json.to_str().unwrap(),
            output_img.to_str().unwrap(),
        ])
        .status()
        .context("Failed to run mkbootimg - is it installed?")?;
    
    if !status.success() {
        bail!("mkbootimg failed");
    }
    
    println!("  ✓ cluu.img created");
    Ok(())
}

fn run_qemu() -> Result<()> {
    println!("▸ Starting QEMU...");
    
    let img_path = project_root().join("target/cluu.img");
    
    let status = Command::new("qemu-system-x86_64")
        .args([
            "-drive", &format!("file={},format=raw", img_path.display()),
            "-m", "256M",
            "-serial", "stdio",
            "-display", "gtk",
            "-d", "int,cpu_reset",
            "-no-reboot",
        ])
        .status()
        .context("Failed to run QEMU")?;
    
    if !status.success() {
        bail!("QEMU exited with error");
    }
    
    Ok(())
}

fn run_tests() -> Result<()> {
    println!("▸ Running tests...");
    
    // Run host-based unit tests
    let status = Command::new("cargo")
        .current_dir(project_root())
        .args([
            "test",
            "--workspace",
            "--exclude", "cluu-init",
            "--exclude", "cluu-procmgr",
            "--exclude", "cluu-vfs",
            "--exclude", "cluu-ramfs",
            "--exclude", "cluu-console",
            "--exclude", "cluu-shell",
            "--exclude", "cluu-cat",
            "--features", "test-mock",
        ])
        .status()
        .context("Failed to run tests")?;
    
    if !status.success() {
        bail!("Tests failed");
    }
    
    println!("  ✓ All tests passed");
    Ok(())
}

fn clean() -> Result<()> {
    println!("▸ Cleaning...");
    
    let _ = Command::new("cargo")
        .current_dir(project_root())
        .args(["clean"])
        .status();
    
    let _ = fs::remove_dir_all(project_root().join("target/initrd"));
    let _ = fs::remove_file(project_root().join("target/initrd.tar"));
    let _ = fs::remove_file(project_root().join("target/cluu.img"));
    let _ = fs::remove_dir_all(project_root().join("target/asm"));
    
    println!("  ✓ Cleaned");
    Ok(())
}
```

### Kernel Build Script (Simpler)

```rust
// kernel/build.rs
use std::env;
use std::path::PathBuf;

fn main() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    
    // Link pre-assembled NASM objects
    // (assembled by xtask before cargo build)
    let asm_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .parent()
        .unwrap()
        .join("target/asm");
    
    if asm_dir.exists() {
        println!("cargo:rustc-link-search=native={}", asm_dir.display());
        
        for entry in std::fs::read_dir(&asm_dir).unwrap() {
            if let Ok(entry) = entry {
                let path = entry.path();
                if path.extension().map_or(false, |e| e == "o") {
                    let name = path.file_stem().unwrap().to_str().unwrap();
                    // Create archive for linking
                    let ar_path = asm_dir.join(format!("lib{}.a", name));
                    let _ = std::process::Command::new("ar")
                        .args(["rcs", ar_path.to_str().unwrap(), path.to_str().unwrap()])
                        .status();
                    println!("cargo:rustc-link-lib=static={}", name);
                }
            }
        }
    }
    
    // Rerun if assembly sources change
    println!("cargo:rerun-if-changed=src/arch/x86_64/boot.asm");
    println!("cargo:rerun-if-changed=src/arch/x86_64/context.asm");
    println!("cargo:rerun-if-changed=src/arch/x86_64/interrupts.asm");
    println!("cargo:rerun-if-changed=src/arch/x86_64/syscall.asm");
}
```

### Makefile (Thin Wrapper)

```makefile
# Makefile - Convenience wrapper around xtask

.PHONY: all build run test clean userspace kernel help

all: build

build:
	cargo xtask build

run:
	cargo xtask run

test:
	cargo xtask test

clean:
	cargo xtask clean

userspace:
	cargo xtask userspace

kernel:
	cargo xtask kernel

help:
	@echo "CLUU Build System"
	@echo ""
	@echo "Usage:"
	@echo "  make build     - Build everything"
	@echo "  make run       - Build and run in QEMU"
	@echo "  make test      - Run all tests"
	@echo "  make clean     - Clean build artifacts"
	@echo "  make userspace - Build only userspace"
	@echo "  make kernel    - Build only kernel"
	@echo ""
	@echo "Or use cargo directly:"
	@echo "  cargo xtask <command>"
```

---


## Using the x86_64 Crate

Use the external `x86_64` crate wherever possible instead of raw inline assembly:

```rust
// kernel/src/arch/x86_64/mod.rs

use x86_64::structures::gdt::{GlobalDescriptorTable, Descriptor, SegmentSelector};
use x86_64::structures::idt::InterruptDescriptorTable;
use x86_64::structures::paging::{
    PageTable, PageTableFlags, PhysFrame, Size4KiB, Size2MiB,
    OffsetPageTable, Mapper, Page, FrameAllocator,
};
use x86_64::registers::control::{Cr3, Cr3Flags};
use x86_64::instructions::interrupts;
use x86_64::instructions::port::Port;
use x86_64::addr::{PhysAddr, VirtAddr};

// ─── GDT Setup ───

pub struct GdtInfo {
    gdt: GlobalDescriptorTable,
    kernel_code: SegmentSelector,
    kernel_data: SegmentSelector,
    user_code: SegmentSelector,
    user_data: SegmentSelector,
    tss: SegmentSelector,
}

impl GdtInfo {
    pub fn new(tss: &'static TaskStateSegment) -> Self {
        let mut gdt = GlobalDescriptorTable::new();
        
        let kernel_code = gdt.add_entry(Descriptor::kernel_code_segment());
        let kernel_data = gdt.add_entry(Descriptor::kernel_data_segment());
        let user_data = gdt.add_entry(Descriptor::user_data_segment());
        let user_code = gdt.add_entry(Descriptor::user_code_segment());
        let tss_sel = gdt.add_entry(Descriptor::tss_segment(tss));
        
        Self {
            gdt,
            kernel_code,
            kernel_data,
            user_code,
            user_data,
            tss: tss_sel,
        }
    }
    
    pub fn load(&'static self) {
        use x86_64::instructions::segmentation::{CS, DS, SS, Segment};
        use x86_64::instructions::tables::load_tss;
        
        self.gdt.load();
        
        unsafe {
            CS::set_reg(self.kernel_code);
            DS::set_reg(self.kernel_data);
            SS::set_reg(self.kernel_data);
            load_tss(self.tss);
        }
    }
}

// ─── IDT Setup ───

pub fn create_idt() -> InterruptDescriptorTable {
    let mut idt = InterruptDescriptorTable::new();
    
    idt.breakpoint.set_handler_fn(breakpoint_handler);
    idt.double_fault.set_handler_fn(double_fault_handler)
        .set_stack_index(DOUBLE_FAULT_IST_INDEX);
    idt.page_fault.set_handler_fn(page_fault_handler);
    
    // Timer (IRQ 0 -> interrupt 32)
    idt[32].set_handler_fn(timer_handler);
    
    // Keyboard (IRQ 1 -> interrupt 33)
    idt[33].set_handler_fn(keyboard_handler);
    
    // Syscall (interrupt 0x80)
    idt[0x80].set_handler_fn(syscall_handler)
        .set_privilege_level(x86_64::PrivilegeLevel::Ring3);
    
    idt
}

// ─── Page Table Operations ───

pub struct PageTableManager {
    mapper: OffsetPageTable<'static>,
    frame_allocator: FrameAllocatorWrapper,
}

impl PageTableManager {
    pub unsafe fn new(
        phys_offset: VirtAddr,
        frame_allocator: impl FrameAllocator<Size4KiB>,
    ) -> Self {
        let level_4_table = active_level_4_table(phys_offset);
        let mapper = OffsetPageTable::new(level_4_table, phys_offset);
        
        Self {
            mapper,
            frame_allocator: FrameAllocatorWrapper::new(frame_allocator),
        }
    }
    
    pub fn map_page(
        &mut self,
        page: Page<Size4KiB>,
        frame: PhysFrame<Size4KiB>,
        flags: PageTableFlags,
    ) -> Result<(), MapError> {
        unsafe {
            self.mapper.map_to(page, frame, flags, &mut self.frame_allocator)?
                .flush();
        }
        Ok(())
    }
    
    pub fn unmap_page(&mut self, page: Page<Size4KiB>) -> Result<PhysFrame, UnmapError> {
        let (frame, flush) = self.mapper.unmap(page)?;
        flush.flush();
        Ok(frame)
    }
}

unsafe fn active_level_4_table(phys_offset: VirtAddr) -> &'static mut PageTable {
    let (frame, _) = Cr3::read();
    let phys = frame.start_address();
    let virt = phys_offset + phys.as_u64();
    let page_table_ptr: *mut PageTable = virt.as_mut_ptr();
    &mut *page_table_ptr
}

// ─── Interrupt Control ───

pub fn enable_interrupts() {
    interrupts::enable();
}

pub fn disable_interrupts() {
    interrupts::disable();
}

pub fn without_interrupts<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    interrupts::without_interrupts(f)
}

pub fn halt_loop() -> ! {
    loop {
        interrupts::enable_and_hlt();
    }
}

// ─── Port I/O ───

pub struct PortIo {
    port: Port<u8>,
}

impl PortIo {
    pub fn new(port: u16) -> Self {
        Self { port: Port::new(port) }
    }
    
    pub unsafe fn read(&mut self) -> u8 {
        self.port.read()
    }
    
    pub unsafe fn write(&mut self, value: u8) {
        self.port.write(value);
    }
}

// ─── CR3 Operations ───

pub fn read_cr3() -> PhysAddr {
    let (frame, _) = Cr3::read();
    frame.start_address()
}

pub unsafe fn write_cr3(addr: PhysAddr) {
    Cr3::write(
        PhysFrame::containing_address(addr),
        Cr3Flags::empty(),
    );
}

pub fn flush_tlb() {
    // Read and write CR3 to flush TLB
    let (frame, flags) = Cr3::read();
    unsafe { Cr3::write(frame, flags) };
}
```

---

## NASM Assembly Files

All assembly code must be in separate `.asm` files using NASM syntax.

### Context Switch (kernel/src/arch/x86_64/context.asm)

```nasm
; kernel/src/arch/x86_64/context.asm
; Context switching routines

section .text
bits 64

; ─── Constants ───
%define CONTEXT_RBX     0x00
%define CONTEXT_RBP     0x08
%define CONTEXT_R12     0x10
%define CONTEXT_R13     0x18
%define CONTEXT_R14     0x20
%define CONTEXT_R15     0x28
%define CONTEXT_RSP     0x30
%define CONTEXT_RIP     0x38
%define CONTEXT_RFLAGS  0x40
%define CONTEXT_CS      0x48
%define CONTEXT_SS      0x50
%define CONTEXT_CR3     0x58

; ─── context_switch ───
; extern "C" fn context_switch(old_ctx: *mut Context, new_ctx: *const Context)
; RDI = old_ctx (save current context here)
; RSI = new_ctx (load this context)
global context_switch
context_switch:
    ; Save current context to old_ctx
    mov [rdi + CONTEXT_RBX], rbx
    mov [rdi + CONTEXT_RBP], rbp
    mov [rdi + CONTEXT_R12], r12
    mov [rdi + CONTEXT_R13], r13
    mov [rdi + CONTEXT_R14], r14
    mov [rdi + CONTEXT_R15], r15
    
    ; Save stack pointer
    mov [rdi + CONTEXT_RSP], rsp
    
    ; Save return address (next instruction after call)
    mov rax, [rsp]
    mov [rdi + CONTEXT_RIP], rax
    
    ; Save flags
    pushfq
    pop rax
    mov [rdi + CONTEXT_RFLAGS], rax
    
    ; Save CR3
    mov rax, cr3
    mov [rdi + CONTEXT_CR3], rax
    
    ; Load new context from new_ctx
    ; First, switch page tables if different
    mov rax, [rsi + CONTEXT_CR3]
    mov rcx, cr3
    cmp rax, rcx
    je .skip_cr3
    mov cr3, rax
.skip_cr3:
    
    ; Load callee-saved registers
    mov rbx, [rsi + CONTEXT_RBX]
    mov rbp, [rsi + CONTEXT_RBP]
    mov r12, [rsi + CONTEXT_R12]
    mov r13, [rsi + CONTEXT_R13]
    mov r14, [rsi + CONTEXT_R14]
    mov r15, [rsi + CONTEXT_R15]
    
    ; Load stack pointer
    mov rsp, [rsi + CONTEXT_RSP]
    
    ; Push return address
    mov rax, [rsi + CONTEXT_RIP]
    push rax
    
    ; Return (will jump to new context's RIP)
    ret


; ─── switch_to_user ───
; extern "C" fn switch_to_user(user_rsp: u64, user_rip: u64, user_rflags: u64)
; Switches to ring 3 via IRETQ
global switch_to_user
switch_to_user:
    ; RDI = user RSP
    ; RSI = user RIP  
    ; RDX = user RFLAGS
    
    ; Build IRETQ frame on stack
    ; SS
    mov rax, 0x23  ; User data segment | RPL 3
    push rax
    ; RSP
    push rdi
    ; RFLAGS
    push rdx
    ; CS
    mov rax, 0x1b  ; User code segment | RPL 3
    push rax
    ; RIP
    push rsi
    
    ; Clear registers for security
    xor rax, rax
    xor rbx, rbx
    xor rcx, rcx
    xor rdx, rdx
    xor rdi, rdi
    xor rsi, rsi
    xor r8, r8
    xor r9, r9
    xor r10, r10
    xor r11, r11
    ; Keep rbp, r12-r15 for ABI (will be set by user code)
    
    iretq


; ─── save_context_and_call ───
; Save full context, then call a Rust function
; Used by interrupt handlers
; extern "C" fn save_context_and_call(
;     handler: extern "C" fn(*mut Context),
;     ctx_ptr: *mut Context
; )
global save_context_and_call
save_context_and_call:
    ; RDI = handler function pointer
    ; RSI = context pointer
    
    ; Save all registers to context
    mov [rsi + CONTEXT_RBX], rbx
    mov [rsi + CONTEXT_RBP], rbp
    mov [rsi + CONTEXT_R12], r12
    mov [rsi + CONTEXT_R13], r13
    mov [rsi + CONTEXT_R14], r14
    mov [rsi + CONTEXT_R15], r15
    mov [rsi + CONTEXT_RSP], rsp
    
    ; RIP is the return address on stack
    mov rax, [rsp]
    mov [rsi + CONTEXT_RIP], rax
    
    ; Save RFLAGS
    pushfq
    pop rax
    mov [rsi + CONTEXT_RFLAGS], rax
    
    ; Save CR3
    mov rax, cr3
    mov [rsi + CONTEXT_CR3], rax
    
    ; Call handler with context pointer as argument
    mov rax, rdi       ; handler
    mov rdi, rsi       ; context pointer as first argument
    call rax
    
    ; Handler returned, restore context (handler may have modified it)
    ; Note: We restore from the same context pointer
    mov rsi, rdi       ; Context was first argument
    
    ; Restore CR3 if changed
    mov rax, [rsi + CONTEXT_CR3]
    mov rcx, cr3
    cmp rax, rcx
    je .skip_cr3_restore
    mov cr3, rax
.skip_cr3_restore:
    
    ; Restore registers
    mov rbx, [rsi + CONTEXT_RBX]
    mov rbp, [rsi + CONTEXT_RBP]
    mov r12, [rsi + CONTEXT_R12]
    mov r13, [rsi + CONTEXT_R13]
    mov r14, [rsi + CONTEXT_R14]
    mov r15, [rsi + CONTEXT_R15]
    mov rsp, [rsi + CONTEXT_RSP]
    
    ; Return
    ret
```

### Interrupt Stubs (kernel/src/arch/x86_64/interrupts.asm)

```nasm
; kernel/src/arch/x86_64/interrupts.asm
; Interrupt handler stubs

section .text
bits 64

; ─── Interrupt frame structure (pushed by CPU) ───
; [rsp + 0x28] SS
; [rsp + 0x20] RSP
; [rsp + 0x18] RFLAGS
; [rsp + 0x10] CS
; [rsp + 0x08] RIP
; [rsp + 0x00] Error code (or 0 if none)

; ─── Macro for interrupt without error code ───
%macro ISR_NOERR 1
global isr%1
isr%1:
    push 0              ; Dummy error code
    push %1             ; Interrupt number
    jmp isr_common_stub
%endmacro

; ─── Macro for interrupt with error code ───
%macro ISR_ERR 1
global isr%1
isr%1:
    push %1             ; Interrupt number (error code already pushed)
    jmp isr_common_stub
%endmacro

; ─── Common stub ───
extern interrupt_dispatch

isr_common_stub:
    ; Save all registers
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    
    ; Save segment registers
    mov ax, ds
    push rax
    mov ax, es
    push rax
    
    ; Load kernel data segment
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    
    ; Call Rust interrupt dispatcher
    ; RDI = pointer to saved registers (stack frame)
    mov rdi, rsp
    call interrupt_dispatch
    
    ; Restore segment registers
    pop rax
    mov es, ax
    pop rax
    mov ds, ax
    
    ; Restore all registers
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax
    
    ; Remove interrupt number and error code
    add rsp, 16
    
    iretq

; ─── Generate interrupt stubs ───
; Exceptions
ISR_NOERR 0     ; Divide by zero
ISR_NOERR 1     ; Debug
ISR_NOERR 2     ; NMI
ISR_NOERR 3     ; Breakpoint
ISR_NOERR 4     ; Overflow
ISR_NOERR 5     ; Bound range exceeded
ISR_NOERR 6     ; Invalid opcode
ISR_NOERR 7     ; Device not available
ISR_ERR   8     ; Double fault
ISR_NOERR 9     ; Coprocessor segment overrun
ISR_ERR   10    ; Invalid TSS
ISR_ERR   11    ; Segment not present
ISR_ERR   12    ; Stack-segment fault
ISR_ERR   13    ; General protection fault
ISR_ERR   14    ; Page fault
ISR_NOERR 15    ; Reserved
ISR_NOERR 16    ; x87 FPU error
ISR_ERR   17    ; Alignment check
ISR_NOERR 18    ; Machine check
ISR_NOERR 19    ; SIMD FPU exception
ISR_NOERR 20    ; Virtualization exception

; IRQs (remapped to 32-47)
%assign i 32
%rep 16
    ISR_NOERR i
%assign i i+1
%endrep

; Syscall
ISR_NOERR 128   ; 0x80

; ─── Interrupt table ───
section .data
global isr_table
isr_table:
%assign i 0
%rep 21
    dq isr%+i
%assign i i+1
%endrep

; Fill gap 21-31
%rep 11
    dq 0
%endrep

; IRQs 32-47
%assign i 32
%rep 16
    dq isr%+i
%assign i i+1
%endrep

; Fill gap 48-127
%rep 80
    dq 0
%endrep

; Syscall 128
dq isr128
```

### Syscall Entry (kernel/src/arch/x86_64/syscall.asm)

```nasm
; kernel/src/arch/x86_64/syscall.asm
; SYSCALL/SYSRET entry point

section .text
bits 64

; ─── SYSCALL ABI ───
; RAX = syscall number
; RDI = arg1
; RSI = arg2
; RDX = arg3
; R10 = arg4 (RCX is clobbered by SYSCALL)
; R8  = arg5
; R9  = arg6
;
; Returns:
; RAX = result (or negative error)

extern syscall_dispatch

; ─── syscall_entry ───
; Called by SYSCALL instruction
; RCX = user RIP (saved by CPU)
; R11 = user RFLAGS (saved by CPU)
global syscall_entry
syscall_entry:
    ; Swap to kernel stack
    ; We need to save user RSP and load kernel RSP
    ; This assumes a per-CPU kernel stack stored in GS base
    swapgs
    mov [gs:0x08], rsp      ; Save user RSP to per-CPU area
    mov rsp, [gs:0x00]      ; Load kernel RSP from per-CPU area
    
    ; Now on kernel stack - save user context
    push rcx                ; User RIP
    push r11                ; User RFLAGS
    push QWORD [gs:0x08]    ; User RSP
    
    ; Save callee-saved registers (we'll restore them)
    push rbx
    push rbp
    push r12
    push r13
    push r14
    push r15
    
    ; Move R10 to RCX for standard calling convention
    ; syscall_dispatch(num, arg1, arg2, arg3, arg4, arg5, arg6)
    mov rcx, r10
    
    ; Push 7th argument (arg6 = R9) onto stack
    push r9
    
    ; Call Rust syscall handler
    ; RDI = syscall number (currently in RAX, need to swap)
    push rdi            ; Save arg1
    mov rdi, rax        ; syscall number
    pop rsi             ; arg1 -> RSI
    ; RDX = arg2 (already there)
    ; RCX = arg3 (was R10)
    ; R8 = arg4
    ; R9 = arg5
    ; [rsp] = arg6
    
    call syscall_dispatch
    
    ; RAX now contains return value
    
    ; Clean up pushed arg6
    add rsp, 8
    
    ; Restore callee-saved registers
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbp
    pop rbx
    
    ; Restore user context
    pop rsp             ; User RSP (temporary, will be in RCX's place)
    pop r11             ; User RFLAGS
    pop rcx             ; User RIP
    
    ; Actually restore user RSP
    mov [gs:0x08], rsp  ; Save current (wrong) RSP
    mov rsp, [gs:0x08]  ; This is a bit convoluted, fix:
    
    ; Better approach: use the value we popped
    ; Let's redo this properly
    ; ... actually the stack manipulation above is wrong. Fix:

; ─── Corrected syscall_entry ───
global syscall_entry_v2
syscall_entry_v2:
    ; Swap GS for per-CPU data
    swapgs
    
    ; Save user stack, load kernel stack
    mov [gs:0x08], rsp          ; gs:0x08 = user_rsp storage
    mov rsp, [gs:0x00]          ; gs:0x00 = kernel_rsp
    
    ; Build stack frame
    push QWORD 0x23             ; User SS
    push QWORD [gs:0x08]        ; User RSP
    push r11                    ; User RFLAGS
    push QWORD 0x1b             ; User CS  
    push rcx                    ; User RIP
    
    ; Save all general registers
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    
    ; Call Rust handler
    ; First arg = pointer to register frame
    mov rdi, rsp
    call syscall_dispatch
    ; Return value in RAX, already saved in frame if needed
    
    ; Restore registers
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    ; Skip RAX - it has the return value
    add rsp, 8
    
    ; Restore user RIP and RFLAGS for SYSRET
    pop rcx                     ; User RIP
    add rsp, 8                  ; Skip CS
    pop r11                     ; User RFLAGS
    pop rsp                     ; User RSP (skip SS)
    
    ; Swap back to user GS
    swapgs
    
    ; Return to userspace
    sysretq


; ─── setup_syscall_msrs ───
; Configure MSRs for SYSCALL/SYSRET
; extern "C" fn setup_syscall_msrs(handler_addr: u64, kernel_cs: u16, user_cs: u16)
global setup_syscall_msrs
setup_syscall_msrs:
    ; RDI = handler address
    ; RSI = kernel CS
    ; RDX = user CS
    
    ; IA32_STAR (0xC0000081)
    ; [63:48] = User CS (for SYSRET, actual CS = this + 16, SS = this + 8)
    ; [47:32] = Kernel CS (for SYSCALL)
    mov ecx, 0xC0000081
    mov eax, 0                  ; Low 32 bits unused
    shl rsi, 32                 ; Kernel CS to bits 47:32
    shl rdx, 48                 ; User CS to bits 63:48
    or rsi, rdx
    mov rdx, rsi
    shr rdx, 32                 ; High 32 bits
    mov eax, esi                ; Low 32 bits
    wrmsr
    
    ; IA32_LSTAR (0xC0000082) = syscall handler address
    mov ecx, 0xC0000082
    mov rax, rdi
    mov rdx, rdi
    shr rdx, 32
    wrmsr
    
    ; IA32_FMASK (0xC0000084) = RFLAGS mask
    ; Clear IF (interrupt flag) on syscall
    mov ecx, 0xC0000084
    mov eax, 0x200              ; Mask IF
    xor edx, edx
    wrmsr
    
    ret
```

---

## Implementation Phases (Updated)

### Phase 1: Project Setup & Testing Infrastructure
- [ ] Workspace structure with all crates
- [ ] Custom target configuration
- [ ] Build script for NASM
- [ ] Mock implementations for testing
- [ ] CI/CD pipeline

### Phase 2: Physical Memory Manager (100% tested)
- [ ] any PageAllocator implementation with full test coverage
- [ ] Property-based tests for allocation
- [ ] Edge case tests (fragmentation, exhaustion)
- [ ] Mock allocator for dependent components

### Phase 3: Virtual Memory Manager (100% tested)
- [ ] PageTableManager using x86_64 crate
- [ ] AddressSpace management
- [ ] Grant/Map/Unmap operations
- [ ] Page fault handling

### Phase 4: Scheduler (100% tested)
- [ ] Thread struct and ThreadRepository
- [ ] PriorityBitmapScheduler
- [ ] INITMODE/NORMALMODE
- [ ] Context switching (NASM)

### Phase 5: IPC (100% tested)
- [ ] Message types
- [ ] Rendezvous mechanism
- [ ] Buffer transfer modes
- [ ] Integration tests

### Phase 6: Capability System (100% tested)
- [ ] CapabilityTable
- [ ] CryptoToken with HMAC
- [ ] Revocation epochs
- [ ] Access control

### Phase 7: Interrupts & Syscalls
- [ ] IDT setup using x86_64 crate
- [ ] Interrupt stubs (NASM)
- [ ] Syscall entry (NASM)
- [ ] Syscall handlers

### Phase 8: Boot & Integration
- [ ] BOOTBOOT structures
- [ ] Page table setup
- [ ] Framebuffer console
- [ ] ELF loading

### Phase 9-15: Userspace
- [ ] libcluu
- [ ] Console driver
- [ ] Process manager
- [ ] VFS
- [ ] RamFS
- [ ] Shell
- [ ] cat

---

## Key Dependencies

```toml
[workspace.dependencies]
# Core
bitflags = "2.4"
spin = "0.9"
log = "0.4"

# x86_64 architecture support
x86_64 = "0.15"

# Testing
proptest = "1.4"

# Optional: For more advanced testing
mockall = "0.12"
```

---

## Summary of Requirements

| Requirement | Implementation |
|-------------|----------------|
| Kernel target | `x86_64-cluu-kernel.json` |
| Userspace target | `x86_64-cluu-user.json` |
| Optimization | `opt-level = 0`, `lto = off` |
| Linking | Static, ELF format |
| Userspace programs | **Standalone ELF executables** (not staticlib) |
| libcluu | Userspace library (rlib) for syscalls, IPC |
| **klibcluu** | **Kernel library (rlib) for kprint!, utils** |
| x86_64 crate | Use for GDT, IDT, paging, port I/O |
| OOP/SOLID | Mandatory - traits, patterns, DI |
| Testing | ~100% coverage for PMM, VMM, scheduler |
| Assembly | NASM `.asm` files, no inline asm |
| Build orchestration | **xtask pattern** (idiomatic Rust) |
| initrd creation | xtask copies ELFs to initrd/, creates tar |
| Disk image | mkbootimg via xtask |


