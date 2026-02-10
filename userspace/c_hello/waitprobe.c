#include <spawn.h>
#include <stdio.h>
#include <sys/wait.h>

extern void debug_print(const char *msg);

#ifndef WNOHANG
#define WNOHANG 1
#endif

int main(void) {
    pid_t child = 0;
    char *argv[] = {"sleepy", NULL};

    int spawn_rc = posix_spawn(&child, "/bin/sleepy", NULL, NULL, argv, NULL);
    if (spawn_rc != 0) {
        debug_print("waitprobe: FAIL spawn");
        printf("waitprobe: FAIL spawn rc=%d\n", spawn_rc);
        return 1;
    }

    int status = -1;
    pid_t first = waitpid(child, &status, WNOHANG);
    if (first < 0) {
        debug_print("waitprobe: FAIL wnohang error");
        printf("waitprobe: FAIL wnohang error pid=%d\n", child);
        return 2;
    }

    if (first == 0) {
        debug_print("waitprobe: PASS wnohang no-exit");
        printf("waitprobe: PASS wnohang no-exit pid=%d\n", child);
    } else if (first == child) {
        debug_print("waitprobe: WARN child exited before wnohang check");
        printf("waitprobe: WARN child exited before wnohang check pid=%d\n", child);
    } else {
        debug_print("waitprobe: FAIL unexpected wnohang pid");
        printf("waitprobe: FAIL unexpected wnohang pid=%d got=%d\n", child, first);
        return 3;
    }

    status = -1;
    pid_t done = waitpid(child, &status, 0);
    if (done != child) {
        debug_print("waitprobe: FAIL blocking wait mismatch");
        printf("waitprobe: FAIL blocking wait mismatch pid=%d got=%d\n", child, done);
        return 4;
    }
    if (status != 0) {
        debug_print("waitprobe: FAIL nonzero exit");
        printf("waitprobe: FAIL nonzero exit pid=%d status=%d\n", child, status);
        return 5;
    }

    debug_print("waitprobe: PASS waitpid queue+wnohang");
    printf("waitprobe: PASS waitpid queue+wnohang pid=%d\n", child);
    return 0;
}
