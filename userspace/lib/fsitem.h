/*
 * Filesystem Item (fsitem) - Shared Memory File Abstraction
 *
 * This header defines the fsitem structure used for zero-copy file access.
 * An fsitem represents a file's metadata and data in a shared memory region,
 * allowing userspace to read files without syscalls or IPC.
 *
 * Architecture:
 * - VFS server creates fsitem when file is opened
 * - fsitem stored in shared memory region
 * - Client maps shmem and accesses file data directly
 * - Zero-copy: file data stays in shared memory
 */

#ifndef FSITEM_H
#define FSITEM_H

#include <stdint.h>
#include <stddef.h>

/* fsitem magic number: "FSIT" in ASCII */
#define FSITEM_MAGIC 0x46534954

/* fsitem types */
#define FSITEM_TYPE_FILE    1
#define FSITEM_TYPE_DIR     2
#define FSITEM_TYPE_DEVICE  3
#define FSITEM_TYPE_SYMLINK 4

/* fsitem flags */
#define FSITEM_FLAG_RDONLY  0x0001
#define FSITEM_FLAG_WRONLY  0x0002
#define FSITEM_FLAG_RDWR    0x0003
#define FSITEM_FLAG_APPEND  0x0008

/* Filesystem Item Structure
 *
 * This structure lives in shared memory and contains both metadata
 * and a pointer to the file data (also in shared memory).
 *
 * Layout in shared memory:
 * [0..512)     fsitem metadata
 * [512..4096)  reserved
 * [4096..EOF)  file data
 */
struct fsitem {
    /* Header */
    uint32_t magic;              /* FSITEM_MAGIC for validation */
    uint32_t version;            /* Structure version (1) */

    /* Type and flags */
    uint32_t type;               /* FSITEM_TYPE_* */
    uint32_t flags;              /* FSITEM_FLAG_* (open mode) */

    /* File metadata */
    uint64_t size;               /* Total file size in bytes */
    uint32_t fs_type;            /* Filesystem type (TAR, EXT2, etc.) */
    uint32_t mode;               /* Unix file mode (0644, etc.) */

    /* Data location */
    uint64_t data_offset;        /* Offset to file data in this shmem region */

    /* Current position (managed by client) */
    uint64_t offset;             /* Current read/write position */

    /* Synchronization */
    uint32_t ref_count;          /* Number of FDs pointing to this fsitem */
    uint32_t lock;               /* Spinlock for atomic operations */

    /* Path information */
    char path[256];              /* Original path */

    /* Reserved for future use */
    uint8_t reserved[200];
} __attribute__((packed));

/* Verify structure size is 512 bytes */
_Static_assert(sizeof(struct fsitem) == 512, "fsitem must be 512 bytes");

/* Helper functions for fsitem manipulation */

/* Validate fsitem magic number */
static inline int fsitem_is_valid(const struct fsitem *item) {
    return item && item->magic == FSITEM_MAGIC;
}

/* Get pointer to file data */
static inline const char* fsitem_get_data(const struct fsitem *item) {
    if (!fsitem_is_valid(item)) {
        return NULL;
    }
    return (const char*)item + item->data_offset;
}

/* Get remaining bytes from current offset */
static inline uint64_t fsitem_remaining(const struct fsitem *item) {
    if (!fsitem_is_valid(item) || item->offset >= item->size) {
        return 0;
    }
    return item->size - item->offset;
}

/* Check if at end of file */
static inline int fsitem_at_eof(const struct fsitem *item) {
    return fsitem_is_valid(item) && item->offset >= item->size;
}

#endif /* FSITEM_H */
