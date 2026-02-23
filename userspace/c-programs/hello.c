/*
 * CLUU C integration test (container smoke test)
 *
 * Exercises newlib stdio and syscalls inside a container.  RBX
 * preservation across syscalls is implicitly validated: printf
 * involves multiple call/write cycles that would crash or produce
 * garbage if the callee-saved RBX were corrupted by syscall_raw.
 */

#include <stdio.h>
#include <string.h>
#include <unistd.h>

void debug_print(const char *msg);

int main(int argc, char **argv) {
    (void)argc;
    (void)argv;

    debug_print("Hello from C on CLUU!\n");

    /* Exercise printf (buffered stdio → write syscall chain).
     * If RBX were not preserved across syscalls the callee-saved
     * register contract would be violated and this would likely
     * crash or produce wrong output. */
    printf("Hello from container (printf path)\n");

    /* A second printf with formatting to stress the stdio codepath. */
    char buf[64];
    int n = snprintf(buf, sizeof(buf), "snprintf: %d %s\n", 42, "ok");
    if (n > 0) {
        (void)write(STDOUT_FILENO, buf, (size_t)n);
    }

    /* If we reached here the C runtime, syscalls, and register
     * preservation are working inside the container. */
    debug_print("[RBX TEST] PASSED");
    printf("[RBX TEST] PASSED\n");

    return 0;
}
