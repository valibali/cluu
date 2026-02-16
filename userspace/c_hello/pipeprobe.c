#include <stdio.h>
#include <string.h>
#include <unistd.h>

extern void debug_print(const char *msg);
extern int pipe(int pipefd[2]);

int main(void) {
    debug_print("pipeprobe: main entered");

    int pfd[2];
    if (pipe(pfd) != 0) {
        debug_print("pipeprobe: FAIL pipe() returned error");
        printf("pipeprobe: FAIL pipe() returned error\n");
        return 1;
    }
    debug_print("pipeprobe: pipe created");

    /* Test 1: write and read */
    const char *msg = "hello pipe";
    ssize_t written = write(pfd[1], msg, strlen(msg));
    if (written != (ssize_t)strlen(msg)) {
        debug_print("pipeprobe: FAIL write wrong count");
        printf("pipeprobe: FAIL write returned %ld expected %ld\n",
               (long)written, (long)strlen(msg));
        return 1;
    }
    debug_print("pipeprobe: write ok");

    char buf[64];
    memset(buf, 0, sizeof(buf));
    ssize_t nread = read(pfd[0], buf, sizeof(buf));
    if (nread != (ssize_t)strlen(msg)) {
        debug_print("pipeprobe: FAIL read wrong count");
        printf("pipeprobe: FAIL read returned %ld expected %ld\n",
               (long)nread, (long)strlen(msg));
        return 1;
    }
    if (memcmp(buf, msg, strlen(msg)) != 0) {
        debug_print("pipeprobe: FAIL data mismatch");
        printf("pipeprobe: FAIL data mismatch\n");
        return 1;
    }
    debug_print("pipeprobe: read ok, data matches");

    /* Test 2: close write end, read should return 0 (EOF) */
    close(pfd[1]);
    debug_print("pipeprobe: write end closed");

    memset(buf, 0, sizeof(buf));
    nread = read(pfd[0], buf, sizeof(buf));
    if (nread != 0) {
        debug_print("pipeprobe: FAIL expected EOF");
        printf("pipeprobe: FAIL expected EOF got %ld\n", (long)nread);
        return 1;
    }
    debug_print("pipeprobe: EOF received ok");

    close(pfd[0]);

    debug_print("pipeprobe: PASS");
    printf("pipeprobe: PASS\n");
    return 0;
}
