/*
 * viewchild - VFS view passthrough test for nested containers.
 *
 * Verifies that a nested container (spawned from viewprobe) has a
 * functional view with writable /tmp.  The view is constructed via
 * Mode 1 (passthrough from parent + image overrides + container storage).
 * Writing to /tmp and reading back proves the nested view is properly
 * wired up.
 */
#include <stdio.h>
#include <string.h>
#include <fcntl.h>
#include <unistd.h>
#include <sched.h>

extern void debug_print(const char *msg);

int main(void) {
    debug_print("viewchild: start");

    /* Yield a few times to let VFS view settle */
    for (int i = 0; i < 4; i++)
        sched_yield();

    /* Write a marker file to /tmp and read it back */
    const char *content = "nested-view-ok";
    int fd = open("/tmp/viewtest", O_WRONLY | O_CREAT, 0644);
    if (fd < 0) {
        debug_print("viewchild: FAIL cannot write /tmp/viewtest");
        return 1;
    }
    write(fd, content, strlen(content));
    close(fd);

    fd = open("/tmp/viewtest", O_RDONLY);
    if (fd < 0) {
        debug_print("viewchild: FAIL cannot read /tmp/viewtest");
        return 1;
    }
    char buf[64];
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    close(fd);

    if (n > 0) {
        buf[n] = '\0';
        if (strcmp(buf, content) == 0) {
            debug_print("viewchild: PASS view passthrough");
        } else {
            debug_print("viewchild: FAIL readback mismatch");
            return 1;
        }
    } else {
        debug_print("viewchild: FAIL read returned 0");
        return 1;
    }

    unlink("/tmp/viewtest");
    return 0;
}
