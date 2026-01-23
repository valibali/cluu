# Newlib Port for CLUU

This directory contains support files for running C programs with newlib on CLUU.

## Files

- `crt0.S` - C runtime startup code
- (Future) `syscalls.c` - Optional C wrappers for syscall stubs

## Building Newlib

### Prerequisites

1. Cross-compiler toolchain (clang or gcc targeting x86_64-elf)
2. Newlib source code

### Configuration

```bash
# Download newlib
wget ftp://sourceware.org/pub/newlib/newlib-4.4.0.20231231.tar.gz
tar xzf newlib-4.4.0.20231231.tar.gz
cd newlib-4.4.0.20231231

# Create build directory
mkdir build-cluu && cd build-cluu

# Configure for CLUU
../configure \
    --target=x86_64-cluu \
    --prefix=$HOME/opt/x86_64-cluu \
    --disable-newlib-supplied-syscalls \
    --enable-newlib-reent-small \
    --disable-multilib \
    CC_FOR_TARGET="clang --target=x86_64-unknown-none-elf" \
    AR_FOR_TARGET=llvm-ar \
    RANLIB_FOR_TARGET=llvm-ranlib

# Build
make -j$(nproc)

# Install
make install
```

### Using with CLUU Programs

1. Compile with newlib headers:
```bash
clang -c -ffreestanding -nostdlib \
    -I$HOME/opt/x86_64-cluu/x86_64-cluu/include \
    -o program.o program.c
```

2. Link with newlib and CLUU syscall stubs:
```bash
clang -nostdlib -static \
    -T ../user.ld \
    -L$HOME/opt/x86_64-cluu/x86_64-cluu/lib \
    crt0.o program.o libcluu_posix.a -lc -lm \
    -o program.elf
```

## POSIX Syscall Stubs

The syscall stubs are implemented in Rust in `libcluu/src/posix/`. To use them from C:

1. Build libcluu with the `posix` feature enabled
2. Export the stubs as a static library
3. Link with your C program

### Supported Functions

| Category | Functions |
|----------|-----------|
| File I/O | `_open`, `_close`, `_read`, `_write`, `_lseek` |
| File status | `_fstat`, `_stat`, `_isatty` |
| Process | `_exit`, `_getpid`, `posix_spawn`, `system`, `_wait` |
| Memory | `_sbrk`, `brk` |
| Time | `_gettimeofday`, `_times`, `time`, `sleep`, `usleep` |
| Stubs | `_fork` (ENOSYS), `_execve` (ENOSYS), `_link`, `_unlink` |

### Limitations

- **No fork()**: CLUU uses spawn semantics. Use `posix_spawn()` instead.
- **No exec()**: Use `posix_spawn()` to create new processes.
- **Limited time support**: Time functions return approximate values.
- **No signals**: `_kill()` sends termination request to procmgr, but signal handlers don't exist.

## Example C Program

```c
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char *argv[]) {
    printf("Hello from C on CLUU!\n");
    
    // Allocate some memory
    char *buf = malloc(1024);
    if (buf) {
        sprintf(buf, "Allocated %d bytes\n", 1024);
        printf("%s", buf);
        free(buf);
    }
    
    return 0;
}
```
