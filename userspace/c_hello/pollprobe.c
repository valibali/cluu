#include <fcntl.h>
#include <stdio.h>
#include <unistd.h>

extern void debug_print(const char *msg);

struct pollfd {
    int fd;
    short events;
    short revents;
};

int poll(struct pollfd *fds, unsigned long nfds, int timeout);

#ifndef POLLIN
#define POLLIN 0x0001
#endif
#ifndef POLLOUT
#define POLLOUT 0x0004
#endif

int main(void) {
    int fd = open("/bin/noop", O_RDONLY, 0);
    if (fd < 0) {
        debug_print("pollprobe: FAIL open");
        printf("pollprobe: FAIL open\n");
        return 1;
    }

    struct pollfd file_pfd = {fd, POLLIN, 0};
    int n = poll(&file_pfd, 1, 0);
    if (n != 1 || (file_pfd.revents & POLLIN) == 0) {
        debug_print("pollprobe: FAIL file poll");
        printf("pollprobe: FAIL file poll n=%d revents=%d\n", n, (int)file_pfd.revents);
        close(fd);
        return 2;
    }

    struct pollfd out_pfd = {1, POLLOUT, 0};
    n = poll(&out_pfd, 1, 0);
    if (n != 1 || (out_pfd.revents & POLLOUT) == 0) {
        debug_print("pollprobe: FAIL stdout poll");
        printf("pollprobe: FAIL stdout poll n=%d revents=%d\n", n, (int)out_pfd.revents);
        close(fd);
        return 3;
    }

    struct pollfd in_pfd = {0, POLLIN, 0};
    n = poll(&in_pfd, 1, 10);
    if (n < 0) {
        debug_print("pollprobe: FAIL stdin poll");
        printf("pollprobe: FAIL stdin poll n=%d\n", n);
        close(fd);
        return 4;
    }

    close(fd);
    debug_print("pollprobe: PASS");
    printf("pollprobe: PASS n_stdin=%d\n", n);
    return 0;
}
