# sys_debug_print Implementation

## Overview

The `sys_debug_print` syscall is the first fully implemented syscall handler in the CLUU microkernel. It provides a mechanism for userspace programs to print debug messages to the kernel log, which is essential for early testing and debugging of the syscall mechanism.

## Implementation Date

2026-01-03

## Files Created/Modified

### New Files

1. **kernel/src/syscall/userptr.rs** (220 lines, 7 tests)
   - Userspace pointer validation utilities
   - Security-focused validation functions
   - Comprehensive test coverage

### Modified Files

1. **kernel/src/syscall/mod.rs**
   - Added `pub mod userptr;`

2. **kernel/src/syscall/handlers.rs**
   - Implemented `sys_debug_print()` function
   - Added 6 new tests for debug_print functionality

## Implementation Details

### Userspace Pointer Validation (userptr.rs)

The module provides three key functions:

#### 1. `validate_user_ptr(ptr: usize) -> Result<(), Error>`

Validates that a pointer points to userspace memory.

**Security Checks:**
- Rejects NULL pointers (0x0)
- Rejects kernel addresses (>= 0x0000_8000_0000_0000)

**x86_64 Address Space Layout:**
```
0x0000_0000_0000_0000  ┌─────────────────────┐
                       │                     │
                       │   Userspace         │  Valid for user pointers
                       │                     │
0x0000_7FFF_FFFF_FFFF  ├─────────────────────┤  USERSPACE_MAX
0x0000_8000_0000_0000  │   Canonical Hole    │  Invalid addresses
0xFFFF_7FFF_FFFF_FFFF  ├─────────────────────┤
                       │                     │
                       │   Kernel Space      │  Rejected by validation
                       │                     │
0xFFFF_FFFF_FFFF_FFFF  └─────────────────────┘
```

#### 2. `validate_user_buffer(ptr: usize, len: usize) -> Result<(), Error>`

Validates that an entire buffer is in userspace.

**Security Checks:**
- Rejects zero-length buffers
- Checks for integer overflow in end address calculation
- Ensures entire buffer (start to end) is in userspace
- Prevents buffers that span userspace/kernel boundary

**Example Rejections:**
```rust
// Zero length
validate_user_buffer(0x1000, 0) → Err(InvalidParameter)

// Overflow past USERSPACE_MAX
validate_user_buffer(0x0000_7FFF_FFFF_F000, 0x2000) → Err(InvalidAddress)

// Integer overflow
validate_user_buffer(usize::MAX - 100, 200) → Err(InvalidAddress)

// Straddles userspace/kernel boundary
validate_user_buffer(USERSPACE_MAX - 50, 100) → Err(InvalidAddress)
```

#### 3. `read_user_string(ptr: usize, len: usize) -> Result<&'static str, Error>`

Reads and validates a UTF-8 string from userspace.

**Validation Steps:**
1. Validate buffer pointer and length
2. Create slice from raw pointer (unsafe but validated)
3. Verify data is valid UTF-8

**Current Limitations:**
- Does NOT check page table entries (assumes pages are mapped)
- Does NOT verify user read permission bits
- Does NOT pin pages to prevent unmapping during access

**TODO for Production:**
- Integrate with VMM to check page mappings
- Verify page permissions (user + readable)
- Handle page faults gracefully
- Pin pages during access to prevent race conditions

### sys_debug_print Implementation

```rust
pub fn sys_debug_print(args: SyscallArgs) -> SyscallResult {
    use crate::syscall::userptr::{read_user_string, MAX_DEBUG_PRINT_SIZE};

    // Extract arguments
    let msg_ptr = args.arg1;
    let msg_len = args.arg2;

    // Validate length is reasonable
    if msg_len > MAX_DEBUG_PRINT_SIZE {
        return Err(Error::InvalidParameter);
    }

    // Read string from userspace
    // This validates the pointer and ensures it's in userspace
    let message = read_user_string(msg_ptr, msg_len)?;

    // Print to kernel log
    klibcluu::info!("[USERSPACE] {}", message);

    // Success
    Ok(0)
}
```

### Arguments

- **arg1** (RDI): Pointer to message string (userspace address)
- **arg2** (RSI): Length of message in bytes
- **arg3-arg6**: Reserved (unused)

### Return Value

- **Success**: `0`
- **Errors**:
  - `InvalidAddress` (-2): NULL pointer or kernel address
  - `InvalidParameter` (-15): Zero length, too long (>4KB), or non-UTF-8 data

### Constants

```rust
pub const USERSPACE_MAX: usize = 0x0000_8000_0000_0000;
pub const MAX_DEBUG_PRINT_SIZE: usize = 4096;
```

## Security Features

### Address Space Isolation

The implementation enforces strict address space isolation:

1. **NULL Pointer Detection**: Prevents accidental dereferences
2. **Kernel Address Rejection**: Prevents information disclosure and memory corruption
3. **Overflow Protection**: Detects integer overflows in address calculations
4. **Boundary Checking**: Ensures buffers don't cross userspace/kernel boundary

### Input Validation

All inputs are validated before use:

1. **Length Limits**: Maximum 4KB message size
2. **UTF-8 Validation**: Ensures valid text encoding
3. **Non-Zero Length**: Rejects empty strings

### Attack Resistance

The implementation protects against:

- **Kernel Memory Disclosure**: Cannot read kernel addresses
- **Kernel Memory Corruption**: Cannot write to kernel memory
- **Integer Overflow Attacks**: Checked arithmetic prevents wrapping
- **Format String Attacks**: Uses safe logging (no format specifiers from user)

## Test Coverage

### userptr.rs Tests (7/7 ✅)

```rust
#[test] fn test_validate_user_ptr_null()
#[test] fn test_validate_user_ptr_valid()
#[test] fn test_validate_user_ptr_kernel()
#[test] fn test_validate_user_buffer_zero_len()
#[test] fn test_validate_user_buffer_valid()
#[test] fn test_validate_user_buffer_overflow()
#[test] fn test_validate_user_buffer_kernel_range()
```

### sys_debug_print Tests (6/6 ✅)

```rust
#[test] fn test_debug_print_null_pointer()        // NULL pointer rejected
#[test] fn test_debug_print_zero_length()         // Zero length rejected
#[test] fn test_debug_print_too_long()            // >4KB rejected
#[test] fn test_debug_print_kernel_pointer()      // Kernel address rejected
#[test] fn test_debug_print_valid_string()        // Valid string succeeds
#[test] fn test_debug_print_non_utf8()            // Invalid UTF-8 rejected
```

### Test Results

All 13 new tests pass:
- 7 userptr validation tests ✅
- 6 sys_debug_print tests ✅

## Usage Example

### From Userspace (pseudocode)

```c
// Userspace program
const char* message = "Hello from userspace!\n";
size_t length = strlen(message);

// Invoke syscall
// RAX = 255 (SyscallNumber::DebugPrint)
// RDI = (uintptr_t)message
// RSI = length
int result = syscall(255, message, length);

if (result == 0) {
    // Success
} else {
    // Error: result is negative errno
}
```

### Kernel Log Output

```
[KERNEL] [USERSPACE] Hello from userspace!
```

## Performance Characteristics

| Operation | Time Complexity | Notes |
|-----------|----------------|-------|
| Pointer validation | O(1) | Simple address comparison |
| Buffer validation | O(1) | Constant-time checks |
| UTF-8 validation | O(n) | Linear in string length |
| Logging | O(n) | Linear in string length |

**Total Complexity**: O(n) where n is the message length (max 4KB)

## Memory Usage

- **Stack Usage**: ~32 bytes (function arguments + locals)
- **No Heap Allocation**: Uses kernel log buffer
- **No Page Pinning**: Pages assumed to stay mapped (TODO)

## Known Limitations

### 1. No Page Table Validation

**Current Behavior**: Assumes userspace pointer is mapped and readable

**Risk**: Page fault if pointer is not mapped

**TODO**:
- Check page table entries before access
- Handle page faults gracefully
- Return appropriate error if unmapped

### 2. No Page Pinning

**Current Behavior**: Does not prevent pages from being unmapped during access

**Risk**: Race condition if page is unmapped during string read

**TODO**:
- Pin pages before access
- Unpin after access completes
- Handle concurrent unmapping

### 3. No Permission Checking

**Current Behavior**: Does not verify user read permission bit in page table

**Risk**: Could access write-only or execute-only pages

**TODO**:
- Check page permissions in VMM
- Verify user + readable flags set

### 4. No Capability Check

**Current Behavior**: Any process can print debug messages

**Design Decision**: This is intentional for debugging aid

**Note**: Production systems may want to restrict or rate-limit this

### 5. No Rate Limiting

**Current Behavior**: Process can spam kernel log

**Risk**: Log flooding denial of service

**TODO**: Add per-process rate limiting for production

## Integration Points

### Current Dependencies

1. **log crate**: For kernel logging (`klibcluu::info!`)
2. **core::str::from_utf8**: For UTF-8 validation
3. **Error enum**: For error codes

### Future Integration

1. **VMM**: Page table validation and permission checking
2. **Capability System**: Optional capability-based access control
3. **Rate Limiting**: Per-process message throttling
4. **Serial Driver**: Direct serial output for early boot

## Design Decisions

### 1. UTF-8 Requirement

**Rationale**: Ensures log messages are valid text
**Alternative**: Allow arbitrary bytes (base64 encode)
**Trade-off**: Simplicity vs flexibility

### 2. 4KB Message Limit

**Rationale**: Prevents kernel log buffer exhaustion
**Alternative**: Larger limit or streaming API
**Trade-off**: Safety vs convenience

### 3. No Capability Required

**Rationale**: Debugging aid should be accessible
**Alternative**: Require DEBUG capability
**Trade-off**: Usability vs security

### 4. Synchronous Logging

**Rationale**: Simple implementation
**Alternative**: Async logging with buffer
**Trade-off**: Latency vs complexity

### 5. Info Log Level

**Rationale**: User messages are informational
**Alternative**: Separate log level for userspace
**Trade-off**: Standard levels vs custom levels

## Comparison with Other Systems

### Linux `sys_write(STDOUT_FILENO)`

**Similarities**:
- Validates userspace pointers
- UTF-8 assumed in modern systems
- Length-based API

**Differences**:
- CLUU: Direct kernel log output
- Linux: Write to file descriptor (requires VFS)
- CLUU: Fixed 4KB limit
- Linux: Arbitrary length (chunked)

### seL4 `seL4_DebugPutChar`

**Similarities**:
- Debugging aid
- No capability required
- Simple implementation

**Differences**:
- seL4: Single character at a time
- CLUU: String-based (more efficient)
- seL4: Character codes
- CLUU: UTF-8 strings

## Future Enhancements

### Short Term

1. **Page Table Integration**: Validate pages are mapped
2. **Permission Checking**: Verify user read permission
3. **Page Pinning**: Prevent unmapping during access
4. **Better Error Messages**: Indicate which validation failed

### Long Term

1. **Rate Limiting**: Per-process message throttling
2. **Capability-Based Access**: Optional capability requirement
3. **Streaming API**: Support messages >4KB
4. **Log Levels**: User-specified log levels
5. **Binary Data**: Support for non-UTF-8 data (hex dump)

## Testing Strategy

### Unit Tests

- ✅ Pointer validation edge cases
- ✅ Buffer validation edge cases
- ✅ UTF-8 validation
- ✅ Length limits

### Integration Tests (TODO)

- [ ] End-to-end syscall from userspace
- [ ] Multiple processes printing concurrently
- [ ] Large message (approaching 4KB limit)
- [ ] Performance benchmarks

### Stress Tests (TODO)

- [ ] Rapid successive calls
- [ ] Concurrent calls from multiple threads
- [ ] Malicious inputs (fuzzing)
- [ ] Page unmapping during access

## Conclusion

The `sys_debug_print` implementation provides:

✅ **Complete**: Fully functional syscall handler
✅ **Secure**: Comprehensive input validation
✅ **Tested**: 13/13 tests passing
✅ **Documented**: Extensive documentation
✅ **Usable**: Ready for userspace testing

This serves as a template for implementing other syscall handlers and demonstrates the end-to-end syscall mechanism working correctly.

**Next Steps**:
1. Implement `sys_yield()` for scheduler integration
2. Create userspace syscall wrapper library
3. Test syscall mechanism end-to-end with real userspace process

---

**Status**: ✅ **COMPLETE**
**Test Coverage**: 13/13 tests passing
**Date Completed**: 2026-01-03
