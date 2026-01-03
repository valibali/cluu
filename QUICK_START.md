# CLUU Quick Start Guide

## Phase 1 Complete! ✓

The project has been completely restructured following SOLID principles and idiomatic Rust patterns.

## What Changed

### Old Structure
```
cluu/
├── kernel/                 # Standalone kernel
├── userspace/             # Mixed C/Rust
└── Makefile              # Complex build
```

### New Structure
```
cluu/
├── Cargo.toml                 # Workspace root
├── xtask/                     # Build orchestration
├── klibcluu/                  # Kernel support library
├── kernel/                    # Kernel (uses klibcluu)
└── userspace/
    ├── libcluu/              # Userspace library
    ├── init/                 # System servers...
    ├── procmgr/
    ├── vfs/
    ├── ramfs/
    ├── console/
    └── shell/
```

## Quick Commands

```bash
# Build everything
cargo xtask build
# or
make build

# Run in QEMU
cargo xtask run
# or
make run

# Run tests
cargo xtask test
# or
make test

# Clean
cargo xtask clean
# or
make clean

# Build only kernel
cargo xtask kernel

# Build only userspace
cargo xtask userspace
```

## Testing Individual Components

```bash
# Test kernel support library
cargo test --package klibcluu

# Test userspace library
cargo test --package libcluu

# Check kernel compiles
cargo check --package cluu-kernel

# Check userspace program
cargo check --package cluu-init
```

## Key Features

### klibcluu - Kernel Support Library

```rust
use klibcluu::*;

// Debug output
kprintln!("Hello from kernel!");
kinfo!("System initialized");
kerror!("Critical error: {}", error);

// Utilities
let aligned = align_up(addr, 4096);
let page_addr = page_align_down(addr);
```

### libcluu - Userspace Library

```rust
use libcluu::*;

// IPC
let mut msg = Message::new(0, [arg1, arg2, 0, 0, 0, 0], 2);
call(cap_index, &mut msg, IpcFlags::empty())?;

// Syscalls
sys_yield();
sys_thread_exit(0);
```

## Build System Details

### xtask (Rust-based build orchestration)

The build process:
1. **Build userspace** with `x86_64-cluu-user` target + build-std
2. **Assemble NASM** files (if present)
3. **Build kernel** with `x86_64-cluu-kernel` target + build-std
4. **Create initrd** by staging binaries
5. **Generate tar** archive
6. **Create disk image** with mkbootimg

### Custom Targets

**Kernel** (`x86_64-cluu-kernel.json`):
- Higher-half: 0xFFFFFFFF80000000
- Code model: kernel
- No red zone
- Soft float

**Userspace** (`x86_64-cluu-user.json`):
- Low memory: 0x400000
- Code model: small
- Soft float

### Profiles (Workspace-wide)

```toml
[profile.dev]
opt-level = 0
lto = false
panic = "abort"

[profile.release]
opt-level = 2
lto = true
panic = "abort"
```

## Project Status

✅ **Phase 1 COMPLETE**
- Workspace structure
- Build system (xtask)
- klibcluu with tests
- libcluu
- All userspace skeletons
- Custom targets
- Linker scripts

🚧 **Phase 2 NEXT: Physical Memory Manager**
- BuddyAllocator implementation
- 100% test coverage
- Property-based tests
- Mock allocator for testing

## File Organization

### Configuration
- `Cargo.toml` - Workspace definition
- `rust-toolchain.toml` - Nightly toolchain
- `.cargo/config.toml` - Cargo settings
- `x86_64-cluu-*.json` - Custom targets

### Build
- `xtask/src/main.rs` - Build orchestration
- `kernel/build.rs` - Kernel build script
- `Makefile` - Convenience wrapper

### Linker Scripts
- `kernel/linker.ld` - Kernel linking
- `userspace/user.ld` - Userspace linking

### Libraries
- `klibcluu/` - Kernel support
- `userspace/libcluu/` - Userspace support

## Architecture Principles

Following SOLID:
- **S**ingle Responsibility: Each module has one job
- **O**pen/Closed: Extend via traits
- **L**iskov Substitution: Interfaces work correctly
- **I**nterface Segregation: Small, focused traits
- **D**ependency Inversion: Depend on abstractions

## Testing Strategy

- Unit tests on host (no cross-compilation)
- Property-based testing with proptest
- Mock implementations for isolation
- Target: ~100% coverage for critical paths

## Next Steps

1. **Update kernel main.rs** to use new structure
2. **Implement Physical Memory Manager** (Phase 2)
3. **Add BOOTBOOT integration**
4. **Set up initial page tables**

## Help

```bash
# Show available commands
cargo xtask --help

# Or
make help
```

## Documentation

- `PHASE1_COMPLETE.md` - Detailed phase 1 summary
- `IMPLEMENTATION_GUIDE.md` - Your comprehensive guide
- This file - Quick reference

---

**Status**: Phase 1 Complete - Ready for Phase 2! 🚀
