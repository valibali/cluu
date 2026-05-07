#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

extern void debug_print(const char *msg);

struct pollfd {
    int fd;
    short events;
    short revents;
};

int poll(struct pollfd *fds, unsigned long nfds, int timeout);
int pipe(int pipefd[2]);

#ifndef POLLIN
#define POLLIN 0x0001
#endif
#ifndef POLLOUT
#define POLLOUT 0x0004
#endif

int main(void) {
    int fd = open("/etc/motd", O_RDONLY, 0);
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

    /* Pipe poll readiness: empty pipe is not POLLIN-ready, after a write the
     * read-end becomes POLLIN-ready, after the matching read it goes back to
     * not-ready. Validates EndpointPeek invoke op + libcluu poll() pipe branch. */
    int pfd[2];
    if (pipe(pfd) != 0) {
        debug_print("pollprobe: FAIL pipe create");
        close(fd);
        return 5;
    }

    struct pollfd p_empty = {pfd[0], POLLIN, 0};
    n = poll(&p_empty, 1, 0);
    if (n != 0 || (p_empty.revents & POLLIN) != 0) {
        debug_print("pollprobe: FAIL pipe empty not idle");
        printf("pollprobe: FAIL pipe empty n=%d revents=%d\n", n, (int)p_empty.revents);
        close(pfd[0]); close(pfd[1]); close(fd);
        return 6;
    }

    const char *msg = "x";
    if (write(pfd[1], msg, 1) != 1) {
        debug_print("pollprobe: FAIL pipe write");
        close(pfd[0]); close(pfd[1]); close(fd);
        return 7;
    }

    struct pollfd p_full = {pfd[0], POLLIN, 0};
    n = poll(&p_full, 1, 0);
    if (n != 1 || (p_full.revents & POLLIN) == 0) {
        debug_print("pollprobe: FAIL pipe POLLIN missing");
        printf("pollprobe: FAIL pipe full n=%d revents=%d\n", n, (int)p_full.revents);
        close(pfd[0]); close(pfd[1]); close(fd);
        return 8;
    }

    char buf[8];
    if (read(pfd[0], buf, sizeof(buf)) != 1 || buf[0] != 'x') {
        debug_print("pollprobe: FAIL pipe read");
        close(pfd[0]); close(pfd[1]); close(fd);
        return 9;
    }

    struct pollfd p_drained = {pfd[0], POLLIN, 0};
    n = poll(&p_drained, 1, 0);
    if (n != 0 || (p_drained.revents & POLLIN) != 0) {
        debug_print("pollprobe: FAIL pipe drained not idle");
        printf("pollprobe: FAIL pipe drained n=%d revents=%d\n", n, (int)p_drained.revents);
        close(pfd[0]); close(pfd[1]); close(fd);
        return 10;
    }

    close(pfd[0]);
    close(pfd[1]);
    debug_print("pollprobe: pipe PASS");

    close(fd);
    debug_print("pollprobe: PASS");
    printf("pollprobe: PASS n_stdin=%d\n", n);
    return 0;
}
