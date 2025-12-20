/*
 * Hello World - First CLUU Userspace Program
 *
 * This program tests the complete userspace infrastructure:
 * - ELF loading
 * - Process creation and address space management
 * - SYSCALL/SYSRET mechanism
 * - Console I/O syscalls (sys_write)
 * - Process termination (sys_exit)
 */

#include "../lib/syscall.h"

/* Entry point - called by kernel after ELF load */
int main(int argc, char **argv) {
    const char *msg1 = "Hello from userspace!\n";
    const char *msg2 = "Syscalls are working!\n";
    const char *msg3 = "Exiting gracefully...\n";

    /* Test sys_write to stdout */
    syscall_write(STDOUT_FILENO, msg1, 22);
    syscall_write(STDOUT_FILENO, msg2, 22);
    syscall_write(STDOUT_FILENO, msg3, 22);

    /* Test sys_yield (cooperative scheduling) */
    syscall_yield();

    /* Test sys_exit (should not return) */
    syscall_exit(0);

    /* Should never reach here */
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
