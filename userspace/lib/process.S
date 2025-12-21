/*
 * Userspace Process Management Syscall Wrappers (x86_64)
 *
 * Low-level assembly wrappers for process control syscalls
 * following the System V ABI calling convention.
 *
 * Syscall number: RAX
 * Arguments: RDI, RSI, RDX, R10, R8, R9
 * Return value: RAX (negative values indicate errors)
 */

.section .text

/*
 * pid_t syscall_getpid(void)
 * Syscall number: 39 (SYS_GETPID)
 */
.global syscall_getpid
.type syscall_getpid, @function
syscall_getpid:
    mov $39, %rax       # SYS_GETPID = 39
    syscall
    ret
.size syscall_getpid, . - syscall_getpid

/*
 * pid_t syscall_getppid(void)
 * Syscall number: 110 (SYS_GETPPID)
 */
.global syscall_getppid
.type syscall_getppid, @function
syscall_getppid:
    mov $110, %rax      # SYS_GETPPID = 110
    syscall
    ret
.size syscall_getppid, . - syscall_getppid

/*
 * pid_t syscall_spawn(const char *path, char **argv)
 * Syscall number: 57 (SYS_SPAWN)
 */
.global syscall_spawn
.type syscall_spawn, @function
syscall_spawn:
    mov $57, %rax       # SYS_SPAWN = 57
    # RDI = path (already set by caller)
    # RSI = argv (already set by caller)
    syscall
    ret
.size syscall_spawn, . - syscall_spawn

/*
 * pid_t syscall_waitpid(pid_t pid, int *status, int options)
 * Syscall number: 61 (SYS_WAITPID)
 */
.global syscall_waitpid
.type syscall_waitpid, @function
syscall_waitpid:
    mov $61, %rax       # SYS_WAITPID = 61
    # RDI = pid (already set by caller)
    # RSI = status (already set by caller)
    # RDX = options (already set by caller)
    syscall
    ret
.size syscall_waitpid, . - syscall_waitpid
