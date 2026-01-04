# CLUU Kernel Test Suite

This crate contains unit tests for the CLUU microkernel, separated from the main kernel crate to avoid `no_std` conflicts with Rust's test framework.

## Structure

```
kernel-tests/
├── Cargo.toml          # Test crate configuration
├── src/
│   └── lib.rs         # Re-exports kernel for testing
└── tests/
    ├── elf_tests.rs   # ELF loader tests
    ├── mm_tests.rs    # Memory management tests
    ├── cap_tests.rs   # Capability system tests
    ├── sched_tests.rs # Scheduler tests
    └── ipc_tests.rs   # IPC tests
```

## Running Tests

```bash
cd kernel-tests
cargo test
```

Run specific test file:
```bash
cargo test --test elf_tests
cargo test --test mm_tests
```

Run with output:
```bash
cargo test -- --nocapture
```

## Migration Status

Tests are being migrated from `#[cfg(test)]` modules in the kernel to this crate.

### Completed
- ✅ Test infrastructure setup
- ✅ ELF loader basic tests
- ✅ Memory layout constant tests

### TODO
- [ ] Extract ELF parser tests (need to expose parse functions)
- [ ] Extract memory management tests
- [ ] Extract capability system tests
- [ ] Extract scheduler tests
- [ ] Extract IPC tests
- [ ] Extract syscall tests

## Adding New Tests

1. Create or edit a test file in `tests/`
2. Import types from `kernel_tests::cluu_kernel::`
3. Write standard Rust `#[test]` functions
4. Run with `cargo test`

## Testing Internal Functions

Some kernel functions are private and need special handling:

### Option 1: Feature Flag
Add a `test-internals` feature to the kernel:

```toml
[features]
test-internals = []
```

Use it in kernel code:
```rust
#[cfg_attr(feature = "test-internals", visibility::make(pub))]
fn parse_elf_header(...) { }
```

### Option 2: Test Module in Kernel
Create a `pub mod test_api` in kernel that re-exports internal functions for testing.

### Option 3: Integration Tests
Test only through public APIs (current approach for most tests).

## Benefits

- ✅ Tests actually run (no `no_std` conflicts)
- ✅ Standard test output and tooling
- ✅ Can use `std` in tests (for test helpers, assertions, etc.)
- ✅ Cleaner separation of test and production code
- ✅ Easier to maintain and extend
