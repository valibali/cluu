/*
 * Userspace Syscall Wrappers (x86_64)
 *
 * These are low-level assembly wrappers that invoke the kernel via
 * the SYSCALL instruction following the System V ABI calling convention:
 *
 * Syscall number: RAX
 * Arguments: RDI, RSI, RDX, R10, R8, R9
 * Return value: RAX (negative values indicate errors)
 *
 * The syscall instruction:
 * - Saves RIP to RCX
 * - Saves RFLAGS to R11
 * - Loads new RIP from IA32_LSTAR MSR
 * - Switches to Ring 0 (kernel mode)
 */

.section .text

/*
 * ssize_t syscall_read(int fd, void *buf, size_t count)
 * Syscall number: 0 (SYS_READ)
 */
.global syscall_read
.type syscall_read, @function
syscall_read:
    mov $0, %rax        # SYS_READ = 0
    # RDI = fd (already set by caller)
    # RSI = buf (already set by caller)
    # RDX = count (already set by caller)
    syscall
    ret
.size syscall_read, . - syscall_read

/*
 * ssize_t syscall_write(int fd, const void *buf, size_t count)
 * Syscall number: 1 (SYS_WRITE)
 */
.global syscall_write
.type syscall_write, @function
syscall_write:
    mov $1, %rax        # SYS_WRITE = 1
    # RDI = fd (already set by caller)
    # RSI = buf (already set by caller)
    # RDX = count (already set by caller)
    syscall
    ret
.size syscall_write, . - syscall_write

/*
 * int syscall_close(int fd)
 * Syscall number: 3 (SYS_CLOSE)
 */
.global syscall_close
.type syscall_close, @function
syscall_close:
    mov $3, %rax        # SYS_CLOSE = 3
    # RDI = fd (already set by caller)
    syscall
    ret
.size syscall_close, . - syscall_close

/*
 * int syscall_fstat(int fd, struct stat *statbuf)
 * Syscall number: 5 (SYS_FSTAT)
 */
.global syscall_fstat
.type syscall_fstat, @function
syscall_fstat:
    mov $5, %rax        # SYS_FSTAT = 5
    # RDI = fd (already set by caller)
    # RSI = statbuf (already set by caller)
    syscall
    ret
.size syscall_fstat, . - syscall_fstat

/*
 * off_t syscall_lseek(int fd, off_t offset, int whence)
 * Syscall number: 8 (SYS_LSEEK)
 */
.global syscall_lseek
.type syscall_lseek, @function
syscall_lseek:
    mov $8, %rax        # SYS_LSEEK = 8
    # RDI = fd (already set by caller)
    # RSI = offset (already set by caller)
    # RDX = whence (already set by caller)
    syscall
    ret
.size syscall_lseek, . - syscall_lseek

/*
 * void *syscall_brk(void *addr)
 * Syscall number: 12 (SYS_BRK)
 */
.global syscall_brk
.type syscall_brk, @function
syscall_brk:
    mov $12, %rax       # SYS_BRK = 12
    # RDI = addr (already set by caller)
    syscall
    ret
.size syscall_brk, . - syscall_brk

/*
 * int syscall_isatty(int fd)
 * Syscall number: 16 (SYS_ISATTY) - Custom, not in Linux
 */
.global syscall_isatty
.type syscall_isatty, @function
syscall_isatty:
    mov $16, %rax       # SYS_ISATTY = 16
    # RDI = fd (already set by caller)
    syscall
    ret
.size syscall_isatty, . - syscall_isatty

/*
 * void syscall_exit(int status)
 * Syscall number: 60 (SYS_EXIT)
 * Does not return
 */
.global syscall_exit
.type syscall_exit, @function
syscall_exit:
    mov $60, %rax       # SYS_EXIT = 60
    # RDI = status (already set by caller)
    syscall
    # Never returns
    ud2                 # Undefined instruction (should never reach here)
.size syscall_exit, . - syscall_exit

/*
 * int syscall_yield(void)
 * Syscall number: 158 (SYS_YIELD) - sched_yield
 */
.global syscall_yield
.type syscall_yield, @function
syscall_yield:
    mov $158, %rax      # SYS_YIELD = 158
    syscall
    ret
.size syscall_yield, . - syscall_yield

/*
 * long syscall_shmem_create(size_t size, unsigned int permissions)
 * Syscall number: 1007 (SYS_SHMEM_CREATE)
 */
.global syscall_shmem_create
.type syscall_shmem_create, @function
syscall_shmem_create:
    mov $1007, %rax     # SYS_SHMEM_CREATE = 1007
    # RDI = size (already set by caller)
    # RSI = permissions (already set by caller)
    syscall
    ret
.size syscall_shmem_create, . - syscall_shmem_create

/*
 * void *syscall_shmem_map(long shmem_id, void *hint_addr, unsigned int permissions)
 * Syscall number: 1008 (SYS_SHMEM_MAP)
 */
.global syscall_shmem_map
.type syscall_shmem_map, @function
syscall_shmem_map:
    mov $1008, %rax     # SYS_SHMEM_MAP = 1008
    # RDI = shmem_id (already set by caller)
    # RSI = hint_addr (already set by caller)
    # RDX = permissions (already set by caller)
    syscall
    ret
.size syscall_shmem_map, . - syscall_shmem_map

/*
 * int syscall_shmem_unmap(void *addr)
 * Syscall number: 1009 (SYS_SHMEM_UNMAP)
 */
.global syscall_shmem_unmap
.type syscall_shmem_unmap, @function
syscall_shmem_unmap:
    mov $1009, %rax     # SYS_SHMEM_UNMAP = 1009
    # RDI = addr (already set by caller)
    syscall
    ret
.size syscall_shmem_unmap, . - syscall_shmem_unmap

/*
 * int syscall_shmem_destroy(long shmem_id)
 * Syscall number: 1010 (SYS_SHMEM_DESTROY)
 */
.global syscall_shmem_destroy
.type syscall_shmem_destroy, @function
syscall_shmem_destroy:
    mov $1010, %rax     # SYS_SHMEM_DESTROY = 1010
    # RDI = shmem_id (already set by caller)
    syscall
    ret
.size syscall_shmem_destroy, . - syscall_shmem_destroy
