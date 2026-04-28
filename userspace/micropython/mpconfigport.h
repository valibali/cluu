// MicroPython configuration for CLUU port
#include <stdint.h>

// Provide alloca fallback via compiler builtin if libc header doesn't define it.
#ifndef alloca
#define alloca(sz) __builtin_alloca(sz)
#endif

// Use extra features ROM level (includes os, sys, json, re, etc.)
#define MICROPY_CONFIG_ROM_LEVEL   (MICROPY_CONFIG_ROM_LEVEL_EXTRA_FEATURES)

// Core features
#define MICROPY_ENABLE_COMPILER    (1)
#define MICROPY_ENABLE_GC          (1)
#define MICROPY_ENABLE_FINALISER   (1)
#define MICROPY_STACK_CHECK        (1)
#define MICROPY_HELPER_REPL        (1)
#define MICROPY_ERROR_REPORTING    (MICROPY_ERROR_REPORTING_DETAILED)
#define MICROPY_WARNINGS           (1)

// VFS — full POSIX backend.
// libcluu provides opendir/readdir/closedir + open/read/write/close/lseek/stat
// already; statvfs is stubbed (returns -1/ENOSYS) since CLUU has no real
// filesystem-statistics concept yet, but it's only actually invoked when
// MICROPY_PY_OS_STATVFS=1 (which we keep off).
#define MICROPY_VFS                (1)
#define MICROPY_VFS_POSIX          (1)
#define MICROPY_READER_VFS         (1)
#define MICROPY_READER_POSIX       (0)
#define MICROPY_PY_OS_STATVFS      (0)

// Threading — GIL-based, using pthreads
#define MICROPY_PY_THREAD          (1)
#define MICROPY_PY_THREAD_GIL     (1)

// Disabled — not available or not needed on CLUU
#define MICROPY_PY_FFI             (0)   // No dlopen/libffi
#define MICROPY_PLAT_DEV_MEM       (0)   // No /dev/mem
#define MICROPY_EMIT_X64           (0)   // Bytecode only (safer for initial port)
#define MICROPY_PY_SOCKET          (0)   // No sockets yet
#define MICROPY_PY_TERMIOS         (0)   // Cooked TTY sufficient for REPL
#define MICROPY_PY_SELECT          (0)   // O_NONBLOCK not enforced — select/poll broken
#define MICROPY_PY_USELECT         (0)   // Same reason

// Heap / memory
#define MICROPY_GC_STACK_ENTRY_TYPE uint32_t
#define MICROPY_ALLOC_PATH_MAX     (256)

// Numeric
#define MICROPY_LONGINT_IMPL       (MICROPY_LONGINT_IMPL_MPZ)
#define MICROPY_FLOAT_IMPL         (MICROPY_FLOAT_IMPL_DOUBLE)

// Time
#define MICROPY_EPOCH_IS_1970      (1)

// Error handling
#define MICROPY_EMERGENCY_EXCEPTION_BUF_SIZE (256)

// sys module
#define MICROPY_PY_SYS_PLATFORM    "cluu"
#define MICROPY_PY_SYS_EXIT        (1)
#define MICROPY_PY_SYS_STDFILES    (0)

// Map port state to VM state (root pointers are auto-registered in VM)
#define MP_STATE_PORT MP_STATE_VM

// Port identification
#define MICROPY_HW_BOARD_NAME      "CLUU"
#define MICROPY_HW_MCU_NAME        "x86_64"

// Type definitions for this port
typedef intptr_t mp_int_t;
typedef uintptr_t mp_uint_t;
typedef long mp_off_t;

// Need to provide a declaration for mp_hal functions
#define MICROPY_BEGIN_ATOMIC_SECTION()     (mp_thread_unix_begin_atomic_section(), 0)
#define MICROPY_END_ATOMIC_SECTION(state)  (void)(state); mp_thread_unix_end_atomic_section()

void mp_thread_unix_begin_atomic_section(void);
void mp_thread_unix_end_atomic_section(void);
