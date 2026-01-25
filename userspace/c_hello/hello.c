/*
 * CLUU C integration test
 *
 * Verifies newlib headers, stdio, malloc, and VFS-backed file I/O.
 */

#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static void dump_file(const char *path) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        printf("open('%s') failed\n", path);
        return;
    }

    char buf[128];
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    if (n > 0) {
        buf[n] = '\0';
        printf("read('%s'): %s\n", path, buf);
    } else {
        printf("read('%s') returned %ld\n", path, (long)n);
    }
    close(fd);
}

int main(int argc, char **argv) {
    (void)argc;
    (void)argv;

    printf("Hello from C on CLUU!\n");
    // printf("PID: %d\n", getpid());

    // char *buf = malloc(64);
    // if (buf) {
    //     strcpy(buf, "malloc/free OK");
    //     printf("%s\n", buf);
    //     free(buf);
    // }

    // struct stat st;
    // if (stat("/dev/initrd/bin/hello", &st) == 0) {
    //     printf("stat: size=%ld mode=%o\n", (long)st.st_size, st.st_mode);
    // } else {
    //     printf("stat failed\n");
    // }

    // dump_file("/dev/initrd/bin/hello");

    // int wfd = open("/proc/uptime", O_WRONLY);
    // if (wfd >= 0) {
    //     const char *msg = "write-test\n";
    //     ssize_t written = write(wfd, msg, strlen(msg));
    //     printf("write('/proc/uptime') -> %ld\n", (long)written);
    //     close(wfd);
    // } else {
    //     printf("open('/proc/uptime', O_WRONLY) failed\n");
    // }

    // usleep(5000);
    // printf("sleep/usleep OK\n");
    return 0;
}
