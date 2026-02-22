/*
 * containerprobe - container storage end-to-end test.
 *
 * Verifies Phase D container plumbing:
 * 1. /data is writable (create + write + read back)
 * 2. /tmp is writable (create + write + read back + unlink)
 */
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>
#include <sched.h>

extern void debug_print(const char *msg);

static int try_open_create(const char *path, int retries) {
    for (int i = 0; i < retries; i++) {
        int fd = open(path, O_WRONLY | O_CREAT, 0644);
        if (fd >= 0)
            return fd;
        /* View may not be registered yet — yield and retry */
        sched_yield();
    }
    return -1;
}

static int write_and_verify(const char *path, const char *content) {
    int fd = try_open_create(path, 10);
    if (fd < 0) {
        printf("containerprobe: FAIL open-write %s errno=%d\n", path, errno);
        return -1;
    }
    ssize_t written = write(fd, content, strlen(content));
    close(fd);
    if (written != (ssize_t)strlen(content)) {
        printf("containerprobe: FAIL write %s written=%ld\n", path, (long)written);
        return -1;
    }

    fd = open(path, O_RDONLY);
    if (fd < 0) {
        printf("containerprobe: FAIL open-read %s errno=%d\n", path, errno);
        return -1;
    }
    char buf[128];
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    close(fd);
    if (n <= 0) {
        printf("containerprobe: FAIL read %s n=%ld\n", path, (long)n);
        return -1;
    }
    buf[n] = '\0';
    if (strcmp(buf, content) != 0) {
        printf("containerprobe: FAIL verify %s expected='%s' got='%s'\n", path, content, buf);
        return -1;
    }
    return 0;
}

int main(void) {
    debug_print("containerprobe: start");

    /* Step 1: Verify /data is writable */
    if (write_and_verify("/data/hello.txt", "container-data\n") != 0) {
        debug_print("containerprobe: FAIL /data");
        return 1;
    }
    debug_print("containerprobe: /data OK");

    /* Step 2: Verify /tmp is writable */
    if (write_and_verify("/tmp/scratch.txt", "container-tmp\n") != 0) {
        debug_print("containerprobe: FAIL /tmp");
        return 2;
    }
    debug_print("containerprobe: /tmp OK");

    /* Cleanup */
    unlink("/data/hello.txt");
    unlink("/tmp/scratch.txt");

    debug_print("containerprobe: PASS");
    printf("containerprobe: PASS\n");
    return 0;
}
