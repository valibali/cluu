// CLUU port-local poll.h shim.
//
// MicroPython's vfs_posix_file.c #includes <poll.h> unconditionally for
// the file polling code path. With MICROPY_PY_SELECT=0 set in our
// mpconfigport.h the polling code is compiled out, but the include
// happens before the #if and we need a header that exists.
//
// CLUU does not yet implement poll(2). All symbols below are minimal
// declarations to satisfy the include.

#ifndef _CLUU_POLL_H
#define _CLUU_POLL_H

#ifdef __cplusplus
extern "C" {
#endif

#define POLLIN  0x0001
#define POLLOUT 0x0004
#define POLLERR 0x0008
#define POLLHUP 0x0010
#define POLLNVAL 0x0020

struct pollfd {
    int   fd;
    short events;
    short revents;
};

typedef unsigned long nfds_t;

int poll(struct pollfd *fds, nfds_t nfds, int timeout);

#ifdef __cplusplus
}
#endif

#endif /* _CLUU_POLL_H */
