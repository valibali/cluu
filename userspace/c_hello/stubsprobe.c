#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <libgen.h>
#include <sys/select.h>

extern void debug_print(const char *msg);

/* access() and realpath() are declared in unistd.h/stdlib.h */
#include <stdlib.h>

#ifndef F_OK
#define F_OK 0
#endif

int main(void) {
    char buf[256];

    /* Test 1: access() for existing file */
    if (access("/bin/hello", F_OK) != 0) {
        debug_print("stubsprobe: FAIL access(/bin/hello) not found");
        printf("stubsprobe: FAIL access(/bin/hello) returned -1\n");
        return 1;
    }

    /* Test 2: access() for nonexistent file */
    if (access("/nonexistent_file_xyz", F_OK) != -1) {
        debug_print("stubsprobe: FAIL access(nonexistent) succeeded");
        printf("stubsprobe: FAIL access(nonexistent) returned 0\n");
        return 1;
    }

    /* Test 3: getuid() returns 0 (root) */
    if (getuid() != 0) {
        debug_print("stubsprobe: FAIL getuid");
        printf("stubsprobe: FAIL getuid()=%u\n", (unsigned)getuid());
        return 1;
    }

    /* Test 4: geteuid/getgid/getegid all return 0 */
    if (geteuid() != 0 || getgid() != 0 || getegid() != 0) {
        debug_print("stubsprobe: FAIL uid/gid stubs");
        return 1;
    }

    /* Test 5: realpath normalization */
    char *rp = realpath("/bin/../bin/./hello", buf);
    if (rp == NULL) {
        debug_print("stubsprobe: FAIL realpath returned NULL");
        printf("stubsprobe: FAIL realpath returned NULL\n");
        return 1;
    }
    if (strcmp(rp, "/bin/hello") != 0) {
        debug_print("stubsprobe: FAIL realpath wrong result");
        printf("stubsprobe: FAIL realpath='%s' expected '/bin/hello'\n", rp);
        return 1;
    }

    /* Test 6: basename */
    char path1[] = "/usr/bin/hello";
    char *bn = basename(path1);
    if (bn == NULL || strcmp(bn, "hello") != 0) {
        debug_print("stubsprobe: FAIL basename");
        printf("stubsprobe: FAIL basename='%s'\n", bn ? bn : "(null)");
        return 1;
    }

    /* Test 7: dirname */
    char path2[] = "/usr/bin/hello";
    char *dn = dirname(path2);
    if (dn == NULL || strcmp(dn, "/usr/bin") != 0) {
        debug_print("stubsprobe: FAIL dirname");
        printf("stubsprobe: FAIL dirname='%s'\n", dn ? dn : "(null)");
        return 1;
    }

    /* Test 8: select with NULL fd_sets + timeout acts as sleep */
    struct timeval tv = {0, 1000}; /* 1ms */
    int n = select(0, NULL, NULL, NULL, &tv);
    if (n != 0) {
        debug_print("stubsprobe: FAIL select timeout");
        printf("stubsprobe: FAIL select returned %d\n", n);
        return 1;
    }

    debug_print("stubsprobe: PASS");
    printf("stubsprobe: PASS\n");
    return 0;
}
