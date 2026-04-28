// CLUU port-local dirent.h shim.
//
// Newlib's bundled <dirent.h> in target/sysroot is gimped (says
// "<dirent.h> not supported") because the upstream newlib build for our
// target doesn't expose the Linux-style DIR/dirent surface. libcluu's
// POSIX shim DOES provide opendir/readdir/closedir + a Linux-compatible
// dirent layout (see userspace/libcluu/src/posix/dir.rs); this header
// just declares the interface so MicroPython's extmod/vfs_posix.c can
// link against it.
//
// The struct layout below MUST match userspace/libcluu/src/posix/dir.rs
// exactly. Keep the two in sync.
//
// We override newlib's dirent.h via -I$(PORT_DIR) precedence in the
// MicroPython Makefile.

#ifndef _CLUU_DIRENT_H
#define _CLUU_DIRENT_H

#include <sys/types.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define DT_UNKNOWN 0
#define DT_REG     8
#define DT_DIR     4

#define NAME_MAX   255

/// Linux-style directory entry. Layout matches libcluu's `Dirent` struct.
struct dirent {
    uint64_t      d_ino;
    off_t         d_off;
    uint16_t      d_reclen;
    unsigned char d_type;
    char          d_name[NAME_MAX + 1];
};

/// Opaque DIR handle. The underlying type lives in libcluu (Rust); we
/// only ever use it through the API below.
typedef struct DIR DIR;

DIR           *opendir(const char *path);
struct dirent *readdir(DIR *dirp);
int            closedir(DIR *dirp);

#ifdef __cplusplus
}
#endif

#endif /* _CLUU_DIRENT_H */
