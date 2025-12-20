/*
 * Userspace Syscall Interface
 *
 * C header providing syscall function prototypes and type definitions
 * for CLUU userspace programs.
 */

#ifndef SYSCALL_H
#define SYSCALL_H

/* Standard types */
typedef long ssize_t;
typedef unsigned long size_t;
typedef long off_t;

/* File descriptor constants */
#define STDIN_FILENO  0
#define STDOUT_FILENO 1
#define STDERR_FILENO 2

/* lseek whence values */
#define SEEK_SET 0
#define SEEK_CUR 1
#define SEEK_END 2

/* Errno values (negative return codes) */
#define EBADF  9   /* Bad file descriptor */
#define EFAULT 14  /* Bad address */
#define EINVAL 22  /* Invalid argument */
#define ESPIPE 29  /* Illegal seek */
#define ENOMEM 12  /* Out of memory */

/* Simplified stat structure (matches kernel's minimal implementation) */
struct stat {
    unsigned int st_mode;   /* File mode */
    /* Other fields would go here in a full implementation */
};

/* Syscall function prototypes */

/**
 * Read from file descriptor
 * Returns: number of bytes read, or negative error code
 */
ssize_t syscall_read(int fd, void *buf, size_t count);

/**
 * Write to file descriptor
 * Returns: number of bytes written, or negative error code
 */
ssize_t syscall_write(int fd, const void *buf, size_t count);

/**
 * Close file descriptor
 * Returns: 0 on success, or negative error code
 */
int syscall_close(int fd);

/**
 * Get file status
 * Returns: 0 on success, or negative error code
 */
int syscall_fstat(int fd, struct stat *statbuf);

/**
 * Seek to position in file
 * Returns: new file position, or negative error code
 */
off_t syscall_lseek(int fd, off_t offset, int whence);

/**
 * Set program break (heap boundary)
 * Pass addr=0 to query current brk
 * Returns: new break value, or negative error code
 */
void *syscall_brk(void *addr);

/**
 * Check if file descriptor is a TTY
 * Returns: 1 if TTY, 0 if not, or negative error code
 */
int syscall_isatty(int fd);

/**
 * Exit current thread/process
 * Does not return
 */
void syscall_exit(int status) __attribute__((noreturn));

/**
 * Yield CPU to scheduler
 * Returns: 0 on success
 */
int syscall_yield(void);

/* Higher-level helper functions */

/**
 * Write a null-terminated string to stdout
 * Returns: number of bytes written, or negative error code
 */
static inline ssize_t print(const char *str) {
    const char *p = str;
    size_t len = 0;
    while (*p++) len++;
    return syscall_write(STDOUT_FILENO, str, len);
}

/**
 * Simple heap allocation using brk
 * Returns: pointer to allocated memory, or NULL on failure
 */
static inline void *sbrk(long increment) {
    void *old_brk = syscall_brk((void *)0);
    if ((long)old_brk < 0) {
        return (void *)-1;  /* Query failed */
    }

    if (increment == 0) {
        return old_brk;
    }

    void *new_brk = syscall_brk((char *)old_brk + increment);
    if ((long)new_brk < 0) {
        return (void *)-1;  /* Allocation failed */
    }

    return old_brk;
}

#endif /* SYSCALL_H */
