/*
 * VFS Server - Virtual File System Server for CLUU Microkernel
 *
 * This userspace server handles all file system operations via IPC.
 * Files are served from the initrd TAR archive mapped into memory.
 *
 * Architecture:
 * - Registers with well-known port name "vfs"
 * - Receives file operation requests via IPC (256-byte messages)
 * - Returns file data via shared memory regions (zero-copy)
 * - Manages file descriptors for open files
 */

#include "../lib/syscall.h"
#include "../lib/ipc.h"

/* VFS Protocol Constants (from kernel/src/vfs/protocol.rs) */
#define VFS_OPEN    1
#define VFS_READ    2
#define VFS_WRITE   3
#define VFS_CLOSE   4
#define VFS_STAT    5
#define VFS_LSEEK   6

/* VFS Error Codes */
#define VFS_SUCCESS         0
#define VFS_ERR_NOT_FOUND  -2    /* ENOENT */
#define VFS_ERR_ACCESS     -13   /* EACCES */
#define VFS_ERR_INVALID    -22   /* EINVAL */
#define VFS_ERR_NO_MEM     -12   /* ENOMEM */
#define VFS_ERR_BAD_FD     -9    /* EBADF */
#define VFS_ERR_IO         -5    /* EIO */

/* Open flags */
#define O_RDONLY  0x0000
#define O_WRONLY  0x0001
#define O_RDWR    0x0002

/* lseek whence values */
#define SEEK_SET  0
#define SEEK_CUR  1
#define SEEK_END  2

/* VFS Message field offsets */
#define OFFSET_REQUEST_TYPE  0
#define OFFSET_RESULT        4
#define OFFSET_REQUEST_ID    8
#define OFFSET_REPLY_PORT    16
#define OFFSET_FD            24
#define OFFSET_FLAGS         28
#define OFFSET_OFFSET        32
#define OFFSET_COUNT         40
#define OFFSET_SHMEM_ID      48   /* NEW: Shared memory ID */
#define OFFSET_DATA          56

/* Helper functions to access message fields */
static inline unsigned int msg_get_u32(const struct ipc_message *msg, int offset) {
    return *(unsigned int*)(&msg->data[offset]);
}

static inline void msg_set_u32(struct ipc_message *msg, int offset, unsigned int value) {
    *(unsigned int*)(&msg->data[offset]) = value;
}

static inline unsigned long msg_get_u64(const struct ipc_message *msg, int offset) {
    return *(unsigned long*)(&msg->data[offset]);
}

static inline void msg_set_u64(struct ipc_message *msg, int offset, unsigned long value) {
    *(unsigned long*)(&msg->data[offset]) = value;
}

static inline int msg_get_i32(const struct ipc_message *msg, int offset) {
    return *(int*)(&msg->data[offset]);
}

static inline void msg_set_i32(struct ipc_message *msg, int offset, int value) {
    *(int*)(&msg->data[offset]) = value;
}

static inline void msg_set_i64(struct ipc_message *msg, int offset, long value) {
    *(long*)(&msg->data[offset]) = value;
}

/* Get path string from message */
static inline const char* msg_get_path(const struct ipc_message *msg) {
    return (const char*)(&msg->data[OFFSET_DATA]);
}

/* Set path string in message */
static inline void msg_set_path(struct ipc_message *msg, const char *path) {
    const char *src = path;
    char *dst = (char*)(&msg->data[OFFSET_DATA]);
    int i = 0;
    while (*src && i < (IPC_MSG_SIZE - OFFSET_DATA - 1)) {
        dst[i++] = *src++;
    }
    dst[i] = '\0';
}

/* ========== TAR Parser ========== */

/* USTAR TAR header (512 bytes) */
struct tar_header {
    char name[100];
    char mode[8];
    char uid[8];
    char gid[8];
    char size[12];      /* Octal string */
    char mtime[12];
    char checksum[8];
    char typeflag;
    char linkname[100];
    char magic[6];      /* "ustar\0" */
    char version[2];    /* "00" */
    char uname[32];
    char gname[32];
    char devmajor[8];
    char devminor[8];
    char prefix[155];
    char padding[12];
};

/* Convert octal string to integer */
static unsigned long octal_to_int(const char *str, int len) {
    unsigned long result = 0;
    for (int i = 0; i < len && str[i] >= '0' && str[i] <= '7'; i++) {
        result = result * 8 + (str[i] - '0');
    }
    return result;
}

/* Check if TAR header is valid */
static int tar_header_is_valid(const struct tar_header *header) {
    /* Check magic "ustar" */
    if (header->magic[0] != 'u' || header->magic[1] != 's' ||
        header->magic[2] != 't' || header->magic[3] != 'a' ||
        header->magic[4] != 'r') {
        return 0;
    }
    return 1;
}

/* String comparison helper */
static int str_equals(const char *a, const char *b) {
    while (*a && *b) {
        if (*a != *b) return 0;
        a++;
        b++;
    }
    return *a == *b;
}

/* Find file in TAR archive */
static const char* tar_find_file(const char *tar_base, size_t tar_size,
                                  const char *path, size_t *out_size) {
    const char *ptr = tar_base;
    const char *end = tar_base + tar_size;

    while (ptr + 512 <= end) {
        const struct tar_header *header = (const struct tar_header *)ptr;

        /* Check if we've reached end of TAR (all zeros) */
        if (header->name[0] == '\0') {
            break;
        }

        /* Validate header */
        if (!tar_header_is_valid(header)) {
            break;
        }

        /* Get file size */
        size_t file_size = octal_to_int(header->size, 12);

        /* Check if this is the file we're looking for */
        if (str_equals(header->name, path)) {
            *out_size = file_size;
            return ptr + 512;  /* Data starts after header */
        }

        /* Skip to next entry (header + data, rounded to 512 bytes) */
        size_t data_blocks = (file_size + 511) / 512;
        ptr += 512 + (data_blocks * 512);
    }

    return (const char *)0;  /* Not found */
}

/* ========== File Descriptor Table ========== */

#define MAX_FDS 256
#define FIRST_FD 3  /* Start at FD 3 (0,1,2 reserved for stdin/stdout/stderr) */

struct file_descriptor {
    int in_use;
    const char *data;       /* Pointer into initrd TAR (zero-copy!) */
    size_t size;            /* File size */
    size_t offset;          /* Current read/write offset */
    int flags;              /* Open flags */
    long shmem_id;          /* Shared memory ID for this file (-1 if none) */
    int is_special;         /* 1 for /dev/null, etc. */
};

static struct file_descriptor fd_table[MAX_FDS];

/* Initialize FD table */
static void fd_table_init(void) {
    for (int i = 0; i < MAX_FDS; i++) {
        fd_table[i].in_use = 0;
        fd_table[i].data = (const char *)0;
        fd_table[i].size = 0;
        fd_table[i].offset = 0;
        fd_table[i].flags = 0;
        fd_table[i].shmem_id = -1;
        fd_table[i].is_special = 0;
    }
}

/* Allocate a new FD */
static int fd_alloc(void) {
    for (int i = FIRST_FD; i < MAX_FDS; i++) {
        if (!fd_table[i].in_use) {
            fd_table[i].in_use = 1;
            fd_table[i].data = (const char *)0;
            fd_table[i].size = 0;
            fd_table[i].offset = 0;
            fd_table[i].flags = 0;
            fd_table[i].shmem_id = -1;
            fd_table[i].is_special = 0;
            return i;
        }
    }
    return -1;  /* No free FDs */
}

/* Free an FD */
static void fd_free(int fd) {
    if (fd >= FIRST_FD && fd < MAX_FDS) {
        /* Clean up shared memory if allocated */
        if (fd_table[fd].shmem_id >= 0) {
            syscall_shmem_destroy(fd_table[fd].shmem_id);
        }
        fd_table[fd].in_use = 0;
        fd_table[fd].shmem_id = -1;
    }
}

/* Validate FD */
static int fd_is_valid(int fd) {
    return (fd >= FIRST_FD && fd < MAX_FDS && fd_table[fd].in_use);
}

/* ========== Debug Helpers ========== */

/* Simple hex printer for debugging */
static void print_hex(unsigned long val) {
    char buf[20];
    int i = 0;

    if (val == 0) {
        print("0");
        return;
    }

    /* Convert to hex */
    while (val > 0 && i < 19) {
        int digit = val % 16;
        buf[i++] = (digit < 10) ? ('0' + digit) : ('a' + digit - 10);
        val /= 16;
    }

    /* Print in reverse */
    while (i > 0) {
        char c[2] = {buf[--i], '\0'};
        print(c);
    }
}

/* Simple decimal printer for debugging */
static void print_dec(unsigned long val) {
    char buf[20];
    int i = 0;

    if (val == 0) {
        print("0");
        return;
    }

    /* Convert to decimal */
    while (val > 0 && i < 19) {
        buf[i++] = '0' + (val % 10);
        val /= 10;
    }

    /* Print in reverse */
    while (i > 0) {
        char c[2] = {buf[--i], '\0'};
        print(c);
    }
}

/* ========== Argument Parsing ========== */

/* Parse hex string (e.g., "0x500000000") to unsigned long */
static unsigned long parse_hex(const char *str) {
    unsigned long result = 0;
    const char *p = str;

    /* Skip "0x" prefix if present */
    if (p[0] == '0' && (p[1] == 'x' || p[1] == 'X')) {
        p += 2;
    }

    /* Parse hex digits */
    while (*p) {
        char c = *p;
        unsigned int digit;

        if (c >= '0' && c <= '9') {
            digit = c - '0';
        } else if (c >= 'a' && c <= 'f') {
            digit = c - 'a' + 10;
        } else if (c >= 'A' && c <= 'F') {
            digit = c - 'A' + 10;
        } else {
            break;  /* Invalid character */
        }

        result = result * 16 + digit;
        p++;
    }

    return result;
}

/* Parse decimal string to unsigned long */
static unsigned long parse_decimal(const char *str) {
    unsigned long result = 0;
    const char *p = str;

    while (*p >= '0' && *p <= '9') {
        result = result * 10 + (*p - '0');
        p++;
    }

    return result;
}

/* ========== Global State ========== */

static const char *initrd_base = (const char *)0;  /* Will be set from args */
static size_t initrd_size = 0;

/* Entry point */
int main(int argc, char **argv) {
    print("[VFS] VFS Server starting...\n");

    /* Parse command-line arguments for initrd location */
    if (argc >= 3) {
        /* argv[1] = initrd address (hex string like "0x500000000") */
        /* argv[2] = initrd size (decimal string) */
        unsigned long addr = parse_hex(argv[1]);
        unsigned long size = parse_decimal(argv[2]);

        initrd_base = (const char *)addr;
        initrd_size = (size_t)size;

        print("[VFS] Initrd mapped at 0x");
        print_hex(addr);
        print(", size=");
        print_dec(size);
        print(" bytes\n");
    } else {
        print("[VFS] ERROR: Missing initrd arguments!\n");
        print("[VFS] Usage: vfs_server <initrd_addr_hex> <initrd_size_dec>\n");
        syscall_exit(1);
    }

    /* Initialize file descriptor table */
    fd_table_init();
    print("[VFS] File descriptor table initialized\n");

    /* Create IPC port for receiving VFS requests */
    port_id_t vfs_port = port_create();
    if (vfs_port < 0) {
        print("[VFS] ERROR: Failed to create port\n");
        syscall_exit(1);
    }

    print("[VFS] Created port: ");
    print_dec(vfs_port);
    print("\n");

    /* Register with well-known name "vfs" */
    if (register_port_name("vfs", vfs_port) < 0) {
        print("[VFS] ERROR: Failed to register port name 'vfs'\n");
        syscall_exit(1);
    }

    print("[VFS] Registered as 'vfs' port\n");
    print("[VFS] VFS Server ready - waiting for requests\n");

    /* Main message loop */
    struct ipc_message request;
    while (1) {
        /* Block waiting for a request */
        if (port_recv(vfs_port, &request) < 0) {
            print("[VFS] ERROR: port_recv failed\n");
            continue;
        }

        /* Parse request */
        unsigned int req_type = msg_get_u32(&request, OFFSET_REQUEST_TYPE);
        unsigned long req_id = msg_get_u64(&request, OFFSET_REQUEST_ID);
        unsigned long reply_port_id = msg_get_u64(&request, OFFSET_REPLY_PORT);

        print("[VFS] Received request type: ");
        /* TODO: Print req_type */
        print("\n");

        /* Handle request */
        struct ipc_message response = request;  /* Copy request to response */

        switch (req_type) {
            case VFS_OPEN: {
                const char *path = msg_get_path(&request);
                int flags = msg_get_i32(&request, OFFSET_FLAGS);

                print("[VFS] OPEN: ");
                print(path);
                print("\n");

                /* Check for special devices */
                if (str_equals(path, "/dev/null")) {
                    /* Special case: /dev/null */
                    int fd = fd_alloc();
                    if (fd < 0) {
                        msg_set_i32(&response, OFFSET_RESULT, VFS_ERR_NO_MEM);
                        msg_set_i32(&response, OFFSET_FD, -1);
                        break;
                    }

                    fd_table[fd].is_special = 1;
                    fd_table[fd].data = (const char *)0;
                    fd_table[fd].size = 0;
                    fd_table[fd].offset = 0;
                    fd_table[fd].flags = flags;
                    fd_table[fd].shmem_id = -1;

                    print("[VFS] /dev/null opened as FD ");
                    print_dec(fd);
                    print("\n");

                    msg_set_i32(&response, OFFSET_RESULT, VFS_SUCCESS);
                    msg_set_i32(&response, OFFSET_FD, fd);
                    msg_set_i64(&response, OFFSET_SHMEM_ID, -1);
                    break;
                }

                /* Search for file in initrd TAR */
                size_t file_size = 0;
                const char *file_data = tar_find_file(initrd_base, initrd_size, path, &file_size);

                if (file_data == (const char *)0) {
                    /* File not found */
                    print("[VFS] File not found: ");
                    print(path);
                    print("\n");
                    msg_set_i32(&response, OFFSET_RESULT, VFS_ERR_NOT_FOUND);
                    msg_set_i32(&response, OFFSET_FD, -1);
                    break;
                }

                /* Allocate file descriptor */
                int fd = fd_alloc();
                if (fd < 0) {
                    msg_set_i32(&response, OFFSET_RESULT, VFS_ERR_NO_MEM);
                    msg_set_i32(&response, OFFSET_FD, -1);
                    break;
                }

                /* Create shared memory region for file data */
                long shmem_id = syscall_shmem_create(file_size, SHMEM_READ | SHMEM_WRITE);
                if (shmem_id < 0) {
                    /* Failed to create shmem */
                    fd_free(fd);
                    print("[VFS] Failed to create shmem\n");
                    msg_set_i32(&response, OFFSET_RESULT, VFS_ERR_NO_MEM);
                    msg_set_i32(&response, OFFSET_FD, -1);
                    break;
                }

                /* Map shmem into our address space temporarily to copy data */
                void *shmem_ptr = syscall_shmem_map(shmem_id, (void *)0, SHMEM_READ | SHMEM_WRITE);
                if ((long)shmem_ptr < 0) {
                    /* Failed to map shmem */
                    syscall_shmem_destroy(shmem_id);
                    fd_free(fd);
                    print("[VFS] Failed to map shmem\n");
                    msg_set_i32(&response, OFFSET_RESULT, VFS_ERR_NO_MEM);
                    msg_set_i32(&response, OFFSET_FD, -1);
                    break;
                }

                /* Copy file data from TAR into shmem */
                char *dst = (char *)shmem_ptr;
                const char *src = file_data;
                for (size_t i = 0; i < file_size; i++) {
                    dst[i] = src[i];
                }

                /* Unmap from our address space (client will map it) */
                syscall_shmem_unmap(shmem_ptr);

                /* Store FD info */
                fd_table[fd].data = file_data;  /* Keep pointer for VFS_READ fallback */
                fd_table[fd].size = file_size;
                fd_table[fd].offset = 0;
                fd_table[fd].flags = flags;
                fd_table[fd].shmem_id = shmem_id;
                fd_table[fd].is_special = 0;

                print("[VFS] File opened successfully, FD=");
                print_dec(fd);
                print("\n");

                /* Success - return FD and shmem_id */
                msg_set_i32(&response, OFFSET_RESULT, VFS_SUCCESS);
                msg_set_i32(&response, OFFSET_FD, fd);
                msg_set_i64(&response, OFFSET_SHMEM_ID, shmem_id);
                break;
            }

            case VFS_READ: {
                int fd = msg_get_i32(&request, OFFSET_FD);
                unsigned long count = msg_get_u64(&request, OFFSET_COUNT);

                print("[VFS] READ: fd=");
                /* TODO: Print fd */
                print(", count=");
                /* TODO: Print count */
                print("\n");

                /* Validate FD */
                if (!fd_is_valid(fd)) {
                    msg_set_i32(&response, OFFSET_RESULT, VFS_ERR_BAD_FD);
                    break;
                }

                /* Special case: /dev/null returns EOF */
                if (fd_table[fd].is_special) {
                    msg_set_i32(&response, OFFSET_RESULT, VFS_SUCCESS);
                    msg_set_u64(&response, OFFSET_COUNT, 0);  /* 0 bytes read (EOF) */
                    break;
                }

                /* Calculate how much we can read */
                size_t remaining = fd_table[fd].size - fd_table[fd].offset;
                size_t to_read = count;
                if (to_read > remaining) {
                    to_read = remaining;
                }

                /* Limit to message size */
                size_t max_data = IPC_MSG_SIZE - OFFSET_DATA;
                if (to_read > max_data) {
                    to_read = max_data;
                }

                /* Copy data into response message */
                const char *src = fd_table[fd].data + fd_table[fd].offset;
                char *dst = (char *)(&response.data[OFFSET_DATA]);
                for (size_t i = 0; i < to_read; i++) {
                    dst[i] = src[i];
                }

                /* Update offset */
                fd_table[fd].offset += to_read;

                print("[VFS] Read ");
                /* TODO: Print to_read */
                print(" bytes\n");

                /* Success - return bytes read */
                msg_set_i32(&response, OFFSET_RESULT, VFS_SUCCESS);
                msg_set_u64(&response, OFFSET_COUNT, to_read);
                break;
            }

            case VFS_WRITE: {
                int fd = msg_get_i32(&request, OFFSET_FD);
                unsigned long count = msg_get_u64(&request, OFFSET_COUNT);

                print("[VFS] WRITE: fd=");
                /* TODO: Print fd */
                print(", count=");
                /* TODO: Print count */
                print("\n");

                /* Validate FD */
                if (!fd_is_valid(fd)) {
                    msg_set_i32(&response, OFFSET_RESULT, VFS_ERR_BAD_FD);
                    break;
                }

                /* Special case: /dev/null accepts all writes and discards them */
                if (fd_table[fd].is_special) {
                    print("[VFS] Write to /dev/null (discarded)\n");
                    msg_set_i32(&response, OFFSET_RESULT, VFS_SUCCESS);
                    msg_set_u64(&response, OFFSET_COUNT, count);  /* Pretend we wrote all bytes */
                    break;
                }

                /* Regular files: read-only filesystem (initrd) */
                print("[VFS] Write failed: read-only filesystem\n");
                msg_set_i32(&response, OFFSET_RESULT, VFS_ERR_ACCESS);
                break;
            }

            case VFS_CLOSE: {
                int fd = msg_get_i32(&request, OFFSET_FD);

                print("[VFS] CLOSE: fd=");
                /* TODO: Print fd */
                print("\n");

                /* Validate FD */
                if (!fd_is_valid(fd)) {
                    msg_set_i32(&response, OFFSET_RESULT, VFS_ERR_BAD_FD);
                    break;
                }

                /* Destroy associated shmem if present */
                if (fd_table[fd].shmem_id >= 0) {
                    syscall_shmem_destroy(fd_table[fd].shmem_id);
                }

                /* Free FD */
                fd_free(fd);

                print("[VFS] File closed successfully\n");

                /* Success */
                msg_set_i32(&response, OFFSET_RESULT, VFS_SUCCESS);
                break;
            }

            case VFS_LSEEK: {
                int fd = msg_get_i32(&request, OFFSET_FD);
                long offset = (long)msg_get_u64(&request, OFFSET_OFFSET);
                int whence = msg_get_i32(&request, OFFSET_FLAGS);  /* Reuse FLAGS field for whence */

                print("[VFS] LSEEK: fd=");
                /* TODO: Print fd */
                print("\n");

                /* Validate FD */
                if (!fd_is_valid(fd)) {
                    msg_set_i32(&response, OFFSET_RESULT, VFS_ERR_BAD_FD);
                    break;
                }

                /* Calculate new offset based on whence */
                long new_offset = 0;
                if (whence == SEEK_SET) {
                    /* Absolute position */
                    new_offset = offset;
                } else if (whence == SEEK_CUR) {
                    /* Relative to current position */
                    new_offset = (long)fd_table[fd].offset + offset;
                } else if (whence == SEEK_END) {
                    /* Relative to end of file */
                    new_offset = (long)fd_table[fd].size + offset;
                } else {
                    /* Invalid whence */
                    msg_set_i32(&response, OFFSET_RESULT, VFS_ERR_INVALID);
                    break;
                }

                /* Validate new offset */
                if (new_offset < 0 || new_offset > (long)fd_table[fd].size) {
                    msg_set_i32(&response, OFFSET_RESULT, VFS_ERR_INVALID);
                    break;
                }

                /* Update offset */
                fd_table[fd].offset = (size_t)new_offset;

                print("[VFS] Seek successful, new offset=");
                /* TODO: Print offset */
                print("\n");

                /* Success - return new offset */
                msg_set_i32(&response, OFFSET_RESULT, VFS_SUCCESS);
                msg_set_u64(&response, OFFSET_OFFSET, (unsigned long)new_offset);
                break;
            }

            default:
                print("[VFS] ERROR: Unknown request type\n");
                msg_set_i32(&response, OFFSET_RESULT, VFS_ERR_INVALID);
                break;
        }

        /* Send response back to client via reply port */
        port_id_t reply_port = (port_id_t)reply_port_id;
        if (port_send(reply_port, &response) < 0) {
            print("[VFS] ERROR: Failed to send response\n");
        }

        /* Yield to allow other processes to run */
        syscall_yield();
    }

    /* Should never reach here */
    return 0;
}

/* _start entry point - NAKED function to avoid prologue messing with RSP */
__attribute__((naked))
void _start(void) {
    /* CRITICAL: This function must be naked (no prologue/epilogue)
     * because we're reading argc/argv directly from the stack where
     * the kernel placed them. A normal function prologue would push
     * RBP and modify RSP, breaking our assumptions.
     *
     * Stack layout when entering _start:
     * [rsp+0]  = argc
     * [rsp+8]  = argv[0] (program name)
     * [rsp+16] = argv[1]
     * ...
     */
    __asm__ volatile(
        /* Read argc and argv from stack */
        "mov (%%rsp), %%rdi\n"      /* RDI = argc (first parameter to main) */
        "lea 8(%%rsp), %%rsi\n"     /* RSI = &argv[0] (second parameter to main) */

        /* Call main(argc, argv) */
        "call main\n"

        /* Exit with return value from main (in RAX) */
        "mov %%rax, %%rdi\n"        /* Move return value to RDI (exit code parameter) */
        "mov $60, %%rax\n"          /* SYS_EXIT = 60 */
        "syscall\n"

        /* Should never reach here */
        "ud2\n"
        :
        :
        : "rdi", "rsi", "rax"
    );
}
