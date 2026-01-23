# Newlib Port Analysis for CLUU

## Executive Summary

This document analyzes the CLUU microkernel codebase to identify what's missing to port newlib (a C standard library for embedded systems). The analysis covers syscall stubs, runtime requirements, and integration points.

## Current System Capabilities

### Syscall Interface
- **7 minimal syscalls**: Send, Recv, Call, Reply, Yield, Invoke, DebugPrint
- **Token-based security model**: All operations use capability tokens
- **IPC-driven architecture**: Services communicate via endpoints
- **No traditional POSIX syscalls**: No `open()`, `read()`, `write()`, `fork()`, etc.

### Memory Management
- ✅ **Heap allocator**: `libcluu::allocator` provides `GlobalAlloc` (512KB static heap)
- ✅ **Virtual address space**: VSpace allocator for managing virtual regions
- ✅ **Memory mapping**: `space_map`, `space_map_range` via `sys_invoke`
- ✅ **Stack setup**: Stack at 0x80000000 (16MB, grows down)
- ✅ **BSS initialization**: Linker script defines `__bss_start` and `__bss_end`

### File System
- ✅ **VFS service**: IPC-based virtual filesystem (`userspace/vfs`)
- ✅ **File operations**: Open, Close, Read (via grants), Readdir
- ✅ **Mount backends**: Initrd (tar), Remote (IPC), Virtual (procfs)
- ⚠️ **No POSIX file descriptors**: Uses IPC endpoints and client IDs

### Process Management
- ✅ **Process creation**: `procmgr` service spawns processes
- ✅ **ELF loading**: Kernel loads ELF binaries
- ✅ **Process exit**: Exit codes via IPC cookies
- ❌ **No `fork()`/`exec()`**: Processes are spawned by procmgr, not self-spawned
- ❌ **No `waitpid()`**: Exit notifications via IPC, not syscall

### Time/Clock
- ⚠️ **Scheduler ticks**: 250Hz timer (4ms per tick) available
- ⚠️ **TSC-based timestamps**: Uses RDTSC for token timestamps
- ❌ **No wall-clock time**: No `gettimeofday()`, `clock_gettime()`
- ❌ **No `timeserver` implementation**: Service exists but is a placeholder

### Runtime
- ✅ **Entry point**: `_start()` in `libcluu::runtime` initializes heap
- ✅ **Panic handler**: Custom panic handler with debug output
- ✅ **Linker script**: `user.ld` defines memory layout
- ✅ **C-compatible entry**: `extern "C" fn main() -> i32` convention

## Newlib Requirements

### Essential Syscall Stubs (13 minimum)

Newlib requires these syscall stubs to be implemented:

1. **`_exit(int status)`** - Program termination
2. **`_kill(int pid, int sig)`** - Signal handling (can be stub)
3. **`_getpid(void)`** - Process ID
4. **`_write(int fd, const void *buf, size_t count)`** - Write to file descriptor
5. **`_read(int fd, void *buf, size_t count)`** - Read from file descriptor
6. **`_close(int fd)`** - Close file descriptor
7. **`_lseek(int fd, off_t offset, int whence)`** - File seeking
8. **`_fstat(int fd, struct stat *st)`** - File status
9. **`_isatty(int fd)`** - Terminal detection
10. **`_sbrk(intptr_t increment)`** - Heap expansion (or `_sbrk_r` for reentrant)
11. **`_link(const char *old, const char *new)`** - Create hard link (can return error)
12. **`_unlink(const char *path)`** - Remove file (can return error)
13. **`_stat(const char *path, struct stat *st)`** - File status by path

### Optional but Common Stubs

- **`_open(const char *path, int flags, ...)`** - Open file
- **`_fork(void)`** - Process fork (can return error)
- **`_execve(const char *name, char *const argv[], char *const envp[])`** - Execute program
- **`_wait(int *status)`** - Wait for child
- **`_times(struct tms *buf)`** - Process times
- **`_gettimeofday(struct timeval *tv, void *tz)`** - Wall-clock time
- **`_clock_gettime(clockid_t clock_id, struct timespec *tp)`** - High-resolution time

### Runtime Requirements

1. **`crt0.o` / Startup Code**:
   - Initialize BSS (zero `.bss` section)
   - Set up stack pointer
   - Call `main()` and handle return
   - Call `_exit()` on return

2. **Reentrancy Support**:
   - Newlib can be built with `_REENT_SMALL` or full reentrancy
   - Reentrant versions require `_impure_ptr` and `_reent` structures
   - Functions like `_sbrk_r`, `_write_r`, etc. take `struct _reent *` parameter

3. **Error Handling**:
   - `errno` variable (thread-local or global)
   - Error codes should map to POSIX errno values

## What's Missing

### 1. Syscall Stub Implementation ❌

**Status**: Not implemented

**Required**:
- Create `syscalls.c` (or `syscalls/` directory) with all stub functions
- Map CLUU syscalls/IPC to POSIX-like interface
- Implement file descriptor table mapping (IPC endpoints → fd numbers)
- Handle stdin/stdout/stderr (currently tokens, need fd mapping)

**Location**: Should be in `userspace/libcluu/src/syscalls/` or separate crate

### 2. File Descriptor Management ❌

**Status**: Not implemented

**Current**: VFS uses IPC endpoints and client IDs, not file descriptors

**Required**:
- File descriptor table per process
- Map fd → (endpoint, client_id, file_info)
- Standard fd numbers: 0=stdin, 1=stdout, 2=stderr
- `open()` returns fd, `close()` releases fd

**Implementation**: Similar to `userspace/vfs/src/fd_table.rs` but process-local

### 3. Process ID Support ⚠️

**Status**: Partial

**Current**: Kernel has `ProcessId` but no userspace access

**Required**:
- `_getpid()` syscall stub
- Expose process ID via boot info or new syscall
- Or use IPC to query procmgr for current PID

**Options**:
1. Add PID to `ProcessInfo` structure
2. Add `sys_getpid()` syscall (minimal, just returns current thread's PID)
3. Query via IPC to procmgr (more complex)

### 4. Heap Expansion (`_sbrk`) ⚠️

**Status**: Partial

**Current**: Fixed 512KB heap in `libcluu::allocator`

**Required**:
- `_sbrk()` to expand heap dynamically
- Map new pages via `space_map` when heap grows
- Track heap break pointer (`__heap_end` from linker script)

**Implementation**: 
- Use `__heap_start` and `__heap_end` from linker script
- On `_sbrk()`, map additional pages via `space_map`
- Update allocator to use dynamic heap bounds

### 5. Time/Clock Functions ❌

**Status**: Not implemented

**Required**:
- `_gettimeofday()` - Wall-clock time
- `_clock_gettime()` - High-resolution time
- `_times()` - Process times

**Options**:
1. Implement `timeserver` service with time IPC protocol
2. Use scheduler ticks (250Hz) for monotonic time
3. Stub with fixed values for initial port

### 6. Signal Handling ❌

**Status**: Not implemented

**Required**:
- `_kill()` stub (can return error for now)
- Signal infrastructure (optional for initial port)

**Note**: Can be stubbed to return error initially

### 7. Process Operations (`fork`, `exec`, `wait`) ❌

**Status**: Not implemented

**Current**: Processes spawned by procmgr, not self-spawned

**Required** (optional for initial port):
- `_fork()` - Can return error initially
- `_execve()` - Can return error initially  
- `_wait()` - Can return error initially

**Note**: These can be stubbed to return `-ENOSYS` for initial port

### 8. C Runtime (`crt0`) ⚠️

**Status**: Partial

**Current**: `_start()` in Rust handles initialization

**Required**:
- C-compatible `_start()` that:
  - Clears BSS (`__bss_start` to `__bss_end`)
  - Sets up stack
  - Calls `main()`
  - Calls `_exit()` on return
- Or ensure Rust `_start()` is compatible with C expectations

**Current Implementation**: `libcluu::runtime::_start()` already does most of this, but:
- BSS clearing may need to be explicit
- Should call `_exit()` instead of IPC exit

### 9. Error Code Mapping ⚠️

**Status**: Partial

**Current**: CLUU has custom `Error` enum with negative errno values

**Required**:
- Map CLUU errors to POSIX errno values
- `errno` variable (thread-local or global)
- `__errno()` function for reentrant newlib

**Mapping Needed**:
- `Error::InvalidArgument` → `EINVAL` (22)
- `Error::NotFound` → `ENOENT` (2)
- `Error::PermissionDenied` → `EPERM` (1)
- `Error::OutOfMemory` → `ENOMEM` (12)
- etc.

### 10. Reentrancy Support ❌

**Status**: Not implemented

**Required** (if using full newlib):
- `_impure_ptr` for non-reentrant functions
- `struct _reent` for reentrant functions
- Reentrant versions of syscall stubs (`_write_r`, `_read_r`, etc.)

**Option**: Build newlib with `_REENT_SMALL` to reduce requirements

### 11. Build System Integration ❌

**Status**: Not implemented

**Required**:
- C compiler toolchain setup (clang/gcc with custom target)
- Newlib build configuration
- Link newlib with userspace programs
- Ensure `crt0.o` is linked (or provide custom)

**Target**: `x86_64-unknown-none-elf` (matches current Rust target)

## Implementation Priority

### Phase 1: Minimal Viable Port (Essential)

1. **Syscall stubs** (`syscalls.c`):
   - `_exit()` - Use `notify_exit()` IPC
   - `_getpid()` - Query or expose PID
   - `_write()` - Map fd 0/1/2 to stdin/stdout/stderr tokens
   - `_read()` - Map fd 0 to stdin token
   - `_sbrk()` - Implement heap expansion
   - `_open()`, `_close()`, `_read()`, `_lseek()`, `_fstat()`, `_stat()` - VFS IPC mapping
   - `_isatty()` - Check if fd is stdin/stdout/stderr
   - Stub others to return errors

2. **File descriptor table**: Process-local fd → IPC endpoint mapping

3. **BSS initialization**: Ensure `_start()` clears BSS explicitly

4. **Error mapping**: Map CLUU errors to POSIX errno

### Phase 2: Functional Port

1. **Heap expansion**: Dynamic `_sbrk()` with page mapping

2. **Time functions**: Basic `_gettimeofday()` using scheduler ticks

3. **Process ID**: Expose via boot info or minimal syscall

### Phase 3: Full Port

1. **Reentrancy**: Full `_reent` support

2. **Process operations**: `fork()`, `exec()`, `wait()` (if needed)

3. **Signal handling**: Basic signal infrastructure

4. **Advanced time**: `clock_gettime()` with proper time sources

## Recommended File Structure

```
userspace/
├── libcluu/
│   ├── src/
│   │   ├── syscalls/          # NEW: Syscall stubs
│   │   │   ├── mod.rs
│   │   │   ├── file.rs        # open, close, read, write, lseek
│   │   │   ├── process.rs     # exit, getpid, fork, exec, wait
│   │   │   ├── memory.rs      # sbrk
│   │   │   ├── time.rs        # gettimeofday, clock_gettime
│   │   │   └── stat.rs        # fstat, stat
│   │   └── fd_table.rs        # NEW: File descriptor management
│   └── Cargo.toml
├── newlib/                    # NEW: Newlib integration
│   ├── syscalls.c            # C syscall stubs (if separate)
│   ├── crt0.S                # Startup code (if needed)
│   └── build.rs              # Build newlib
└── c-example/                 # NEW: Test C program
    ├── Cargo.toml
    └── src/
        └── main.c
```

## Integration Points

### 1. VFS Integration

Map POSIX file operations to VFS IPC:

```rust
// Pseudo-code
fn open(path: &str, flags: i32) -> Result<i32> {
    let vfs_endpoint = get_vfs_endpoint()?;
    let (client_id, fd) = vfs_client.open(vfs_endpoint, path)?;
    fd_table.insert(fd, FdEntry { endpoint: vfs_endpoint, client_id, ... });
    Ok(fd)
}
```

### 2. Stdio Integration

Map stdin/stdout/stderr to existing tokens:

```rust
// From boot info
let stdin_token = process_info().tokens[TOKEN_STDIN];
let stdout_token = process_info().tokens[TOKEN_STDOUT];
let stderr_token = process_info().tokens[TOKEN_STDERR];

// Map to fd 0, 1, 2
fd_table.insert(0, FdEntry { token: stdin_token, ... });
fd_table.insert(1, FdEntry { token: stdout_token, ... });
fd_table.insert(2, FdEntry { token: stderr_token, ... });
```

### 3. Heap Integration

Extend current allocator with `_sbrk()`:

```rust
static mut HEAP_BRK: usize = __heap_start;

#[no_mangle]
pub extern "C" fn _sbrk(increment: isize) -> *mut c_void {
    let space_token = get_space_token();
    let old_brk = HEAP_BRK;
    let new_brk = old_brk + increment;
    
    // Map pages if needed
    if new_brk > old_brk {
        let pages = pages_for_size(new_brk - old_brk);
        space_map_range(space_token, old_brk, pages, 0)?;
    }
    
    HEAP_BRK = new_brk;
    old_brk as *mut c_void
}
```

## Testing Strategy

1. **Minimal C program**: `int main() { return 42; }`
2. **Hello world**: `printf("Hello, world!\n");`
3. **File I/O**: Open, read, write files via VFS
4. **Heap test**: `malloc()`/`free()` with `_sbrk()`
5. **Stdio test**: `fprintf()`, `fscanf()`

## Next Steps

1. **Create syscall stub skeleton** in `userspace/libcluu/src/syscalls/`
2. **Implement file descriptor table** in `userspace/libcluu/src/fd_table.rs`
3. **Add `_sbrk()` implementation** with dynamic page mapping
4. **Set up C build system** (clang/gcc with custom target)
5. **Build newlib** with appropriate configuration
6. **Create test C program** to verify port
7. **Iterate** on missing functionality

## References

- [Newlib Porting Guide](https://www.embecosm.com/appnotes/ean9/html/index.html)
- [OSDev Newlib Porting](https://wiki.osdev.org/Porting_Newlib)
- [Newlib Syscall Documentation](https://sourceware.org/newlib/libc.html#Syscalls)
