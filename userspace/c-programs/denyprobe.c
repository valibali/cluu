/*
 * denyprobe - deny_inherit view restriction test.
 *
 * Runs inside a container with DENY_INHERIT.  Verifies:
 * 1. /dev/initrd is NOT visible (deny_inherit blocks passthrough)
 * 2. /tmp is writable (container-scoped storage still available)
 */
#include <stdio.h>
#include <fcntl.h>
#include <unistd.h>
#include <sched.h>

extern void debug_print(const char *msg);

int main(void) {
    debug_print("denyprobe: start");

    /* Yield to let VFS view settle */
    for (int i = 0; i < 4; i++)
        sched_yield();

    /* /dev/initrd should NOT be visible with deny_inherit */
    int fd = open("/dev/initrd", O_RDONLY);
    if (fd >= 0) {
        close(fd);
        debug_print("denyprobe: FAIL /dev/initrd should not be visible");
        return 1;
    }
    debug_print("denyprobe: /dev/initrd correctly hidden");

    /* /tmp should still be available (container-scoped) */
    fd = open("/tmp/denytest", O_WRONLY | O_CREAT, 0644);
    if (fd < 0) {
        debug_print("denyprobe: FAIL /tmp not writable");
        return 1;
    }
    close(fd);
    unlink("/tmp/denytest");

    debug_print("denyprobe: PASS deny_inherit");
    return 0;
}
