#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

extern void debug_print(const char *msg);

int main(void) {
    char buf[128];

    /* ── /dev/null: write succeeds, read returns 0 (EOF) ── */
    int fd = open("/dev/null", O_RDWR, 0);
    if (fd < 0) {
        debug_print("devprobe: FAIL open /dev/null");
        return 1;
    }
    ssize_t w = write(fd, "hello", 5);
    if (w != 5) {
        debug_print("devprobe: FAIL null write");
        close(fd);
        return 2;
    }
    ssize_t r = read(fd, buf, sizeof(buf));
    if (r != 0) {
        debug_print("devprobe: FAIL null read not EOF");
        close(fd);
        return 3;
    }
    close(fd);

    /* ── /dev/zero: read returns zeroes ── */
    fd = open("/dev/zero", O_RDONLY, 0);
    if (fd < 0) {
        debug_print("devprobe: FAIL open /dev/zero");
        return 4;
    }
    memset(buf, 0xFF, sizeof(buf));
    r = read(fd, buf, 64);
    if (r != 64) {
        debug_print("devprobe: FAIL zero read len");
        close(fd);
        return 5;
    }
    int all_zero = 1;
    for (int i = 0; i < 64; i++) {
        if (buf[i] != 0) { all_zero = 0; break; }
    }
    if (!all_zero) {
        debug_print("devprobe: FAIL zero not zeroed");
        close(fd);
        return 6;
    }
    close(fd);

    /* ── /dev/urandom: read returns non-zero data (probabilistic) ── */
    fd = open("/dev/urandom", O_RDONLY, 0);
    if (fd < 0) {
        debug_print("devprobe: FAIL open /dev/urandom");
        return 7;
    }
    memset(buf, 0, sizeof(buf));
    r = read(fd, buf, 64);
    if (r != 64) {
        debug_print("devprobe: FAIL urandom read len");
        close(fd);
        return 8;
    }
    /* At least some bytes should be non-zero (astronomically unlikely to be all 0) */
    int nonzero = 0;
    for (int i = 0; i < 64; i++) {
        if (buf[i] != 0) { nonzero++; }
    }
    if (nonzero == 0) {
        debug_print("devprobe: FAIL urandom all zero");
        close(fd);
        return 9;
    }
    close(fd);

    debug_print("devprobe: PASS");
    printf("devprobe: PASS nonzero=%d/64\n", nonzero);
    return 0;
}
