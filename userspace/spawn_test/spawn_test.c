/*
 * Spawn Test - Testing Process Spawning
 *
 * This program tests the spawn/waitpid syscalls:
 * - Spawning a child process
 * - Waiting for child to complete
 * - Retrieving exit status
 * - Parent-child relationship
 */

#include "../lib/syscall.h"
#include "../lib/process.h"

/* Helper: strlen */
static int strlen(const char *s) {
    int len = 0;
    while (*s++) len++;
    return len;
}

/* Helper: print string to stdout */
static void print_msg(const char *msg) {
    syscall_write(STDOUT_FILENO, msg, strlen(msg));
}

/* Helper: print integer as decimal */
static void print_int(int n) {
    char buf[20];
    int i = 0;

    if (n == 0) {
        buf[i++] = '0';
    } else {
        if (n < 0) {
            syscall_write(STDOUT_FILENO, "-", 1);
            n = -n;
        }

        /* Convert to string (reverse order) */
        int tmp = n;
        while (tmp > 0) {
            buf[i++] = '0' + (tmp % 10);
            tmp /= 10;
        }

        /* Reverse the string */
        for (int j = 0; j < i / 2; j++) {
            char temp = buf[j];
            buf[j] = buf[i - 1 - j];
            buf[i - 1 - j] = temp;
        }
    }

    syscall_write(STDOUT_FILENO, buf, i);
}

/* Entry point */
int main(int argc, char **argv) {
    (void)argc;  /* Suppress unused parameter warning */
    (void)argv;  /* Suppress unused parameter warning */

    print_msg("========================================\n");
    print_msg("SPAWN TEST - Process Spawning Test\n");
    print_msg("========================================\n");

    /* Get our process ID */
    pid_t my_pid = syscall_getpid();
    print_msg("Parent: My PID is ");
    print_int(my_pid);
    print_msg("\n");

    /* Get parent process ID (should be kernel or shell) */
    pid_t my_ppid = syscall_getppid();
    print_msg("Parent: My parent PID is ");
    print_int(my_ppid);
    print_msg("\n\n");

    /* Spawn a child process (hello program) */
    print_msg("Parent: Spawning child process (bin/hello)...\n");

    char *args[] = {(char *)"hello", (char *)0};
    pid_t child_pid = syscall_spawn("bin/hello", args);

    if (child_pid < 0) {
        print_msg("Parent: ERROR! spawn failed with code ");
        print_int(child_pid);
        print_msg("\n");
        syscall_exit(1);
    }

    print_msg("Parent: Child spawned successfully! Child PID = ");
    print_int(child_pid);
    print_msg("\n\n");

    /* Wait for child to complete */
    print_msg("Parent: Waiting for child to exit...\n");

    int status;
    pid_t waited_pid = syscall_waitpid(child_pid, &status, 0);

    if (waited_pid < 0) {
        print_msg("Parent: ERROR! waitpid failed with code ");
        print_int(waited_pid);
        print_msg("\n");
        syscall_exit(1);
    }

    print_msg("Parent: Child exited! PID = ");
    print_int(waited_pid);
    print_msg(", exit status = ");
    print_int(WEXITSTATUS(status));
    print_msg("\n\n");

    print_msg("========================================\n");
    print_msg("SPAWN TEST COMPLETE - All tests passed!\n");
    print_msg("========================================\n");

    syscall_exit(0);
    return 0;
}

/* Minimal _start entry point for freestanding binary */
void _start(void) {
    /* Call main with dummy argc/argv */
    int argc = 0;
    char **argv = (char **)0;

    int ret = main(argc, argv);

    /* Exit with return code from main */
    syscall_exit(ret);
}
