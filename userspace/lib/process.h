/*
 * Userspace Process Management Interface
 *
 * C header providing process control syscall prototypes and types
 * for CLUU userspace programs.
 */

#ifndef PROCESS_H
#define PROCESS_H

/* Process ID type */
typedef int pid_t;

/* Error codes */
#define ENOENT 2   /* No such file or directory */
#define ECHILD 10  /* No child processes */

/* waitpid options */
#define WNOHANG 1  /* Don't block if no child has exited */

/**
 * Get current process ID
 * Returns: process ID (always >= 0)
 */
pid_t syscall_getpid(void);

/**
 * Get parent process ID
 * Returns: parent process ID, or 0 if no parent
 */
pid_t syscall_getppid(void);

/**
 * Spawn new process from ELF binary
 *
 * Arguments:
 *   path - Path to ELF binary in initrd (e.g., "bin/hello")
 *   argv - NULL-terminated array of argument strings (can be NULL for no args)
 *
 * Returns: child process ID on success, or negative error code
 *   -ENOENT: File not found in initrd
 *   -ENOMEM: Out of memory or ELF loading failed
 */
pid_t syscall_spawn(const char *path, char **argv);

/**
 * Wait for process to change state
 *
 * Arguments:
 *   pid - Process ID to wait for
 *   status - Pointer to store exit status (can be NULL)
 *   options - Wait options (0 for blocking, WNOHANG for non-blocking)
 *
 * Returns: PID of child that changed state, or negative error code
 *   -ECHILD: Process is not a child of current process
 *   -EINVAL: Process still running (non-blocking wait only)
 */
pid_t syscall_waitpid(pid_t pid, int *status, int options);

/* Helper macros for status codes */

/**
 * Extract exit status from waitpid status value
 */
#define WEXITSTATUS(status) ((status) & 0xFF)

/**
 * Check if process exited normally
 */
#define WIFEXITED(status) (1)  /* Simplified: we only support normal exit for now */

#endif /* PROCESS_H */
