# Test Migration Guide

## ✅ Completed Setup

The kernel test infrastructure is now fully functional:
- Created separate `tests/kernel/` crate
- Tests successfully running with `cargo test`
- Proper separation from kernel `no_std` environment

## Test Results

```bash
$ cargo test

running 8 tests
✅ ELF tests: 4 passed
✅ Memory tests: 1 passed
✅ Capability tests: 1 passed
✅ Scheduler tests: 1 passed
✅ IPC tests: 1 passed

test result: ok. 8 passed; 0 failed
```

## Architecture

### Feature Flag Solution

Added `testing` feature to kernel `Cargo.toml`:
```toml
[features]
testing = []  # Disables no_std runtime components
```

Used in `kernel/src/mm/heap.rs`:
```rust
#[cfg(not(feature = "testing"))]
#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();
```

tests/kernel imports kernel with feature enabled:
```toml
cluu-kernel = { path = "../kernel", features = ["testing"] }
```

This prevents conflicts between std's allocator and the kernel's no_std allocator.

## Migration Process

### Files with Tests to Migrate

From `find kernel/src -name "*.rs" -exec grep -l "#\[cfg(test)\]"`:

**Priority 1 - Core Systems:**
- ✅ `kernel/src/elf.rs` - Partially migrated (4 tests)
- ✅ `kernel/src/mm/space.rs` - Partially migrated (1 test)
- ⏳ `kernel/src/mm/physmap.rs`
- ⏳ `kernel/src/mm/pmm.rs`
- ⏳ `kernel/src/mm/traits.rs`
- ⏳ `kernel/src/mm/vmm.rs`

**Priority 2 - Subsystems:**
- ⏳ `kernel/src/cap/*.rs` (5 files)
- ⏳ `kernel/src/sched/*.rs` (4 files)
- ⏳ `kernel/src/ipc/*.rs` (4 files)
- ⏳ `kernel/src/syscall/*.rs` (3 files)

**Priority 3 - Architecture:**
- ⏳ `kernel/src/arch/x86_64/syscall.rs`

### Migration Steps

For each test module:

1. **Extract test code** from `#[cfg(test)] mod tests { ... }`
2. **Create test file** in `tests/kernel/tests/`
3. **Update imports** to use `kernel_tests::cluu_kernel::`
4. **Handle private functions:**
   - Either make them `pub` with `#[doc(hidden)]`
   - Or test through public APIs only
5. **Run tests**: `cargo test --test <name>`
6. **Remove** `#[cfg(test)]` block from kernel once migrated

### Example Migration

**Before** (`kernel/src/elf.rs`):
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_something() {
        // test code
    }
}
```

**After** (`tests/kernel/tests/elf_tests.rs`):
```rust
use kernel_tests::cluu_kernel::elf::*;

#[test]
fn test_something() {
    // same test code
}
```

## Benefits Achieved

✅ **Tests actually run** - No more `no_std` conflicts
✅ **Standard tooling** - `cargo test`, test output, coverage tools work
✅ **Can use std in tests** - Better test utilities and assertions
✅ **Cleaner code** - Separation of test and production code
✅ **Faster iteration** - Don't need to rebuild kernel binary for tests
✅ **CI friendly** - Easy to integrate with CI/CD pipelines

## Next Steps

1. **Migrate remaining tests** from kernel source files
2. **Add integration tests** for complex scenarios
3. **Set up CI** to run tests automatically
4. **Add code coverage** reporting

## Running Tests

```bash
# All tests
cargo test

# Specific test file
cargo test --test elf_tests

# With output
cargo test -- --nocapture

# Just kernel tests (not userspace)
cd tests/kernel && cargo test
```
