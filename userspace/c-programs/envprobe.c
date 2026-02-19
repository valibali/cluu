#include <stdio.h>
#include <stdlib.h>
#include <string.h>

extern void debug_print(const char *msg);

int main(void) {
    /* Test 1: getenv("PATH") should return "/bin" (set by procmgr default env) */
    const char *path = getenv("PATH");
    if (path == NULL) {
        debug_print("envprobe: FAIL getenv(PATH) returned NULL");
        printf("envprobe: FAIL getenv(PATH) returned NULL\n");
        return 1;
    }
    if (strcmp(path, "/bin") != 0) {
        debug_print("envprobe: FAIL getenv(PATH) wrong value");
        printf("envprobe: FAIL getenv(PATH) = '%s' expected '/bin'\n", path);
        return 1;
    }

    /* Test 2: getenv("HOME") should return "/" */
    const char *home = getenv("HOME");
    if (home == NULL || strcmp(home, "/") != 0) {
        debug_print("envprobe: FAIL getenv(HOME)");
        printf("envprobe: FAIL getenv(HOME) = '%s'\n", home ? home : "(null)");
        return 1;
    }

    /* Test 3: setenv + getenv */
    if (setenv("FOO", "bar", 1) != 0) {
        debug_print("envprobe: FAIL setenv(FOO)");
        printf("envprobe: FAIL setenv(FOO) returned nonzero\n");
        return 1;
    }
    const char *foo = getenv("FOO");
    if (foo == NULL || strcmp(foo, "bar") != 0) {
        debug_print("envprobe: FAIL getenv(FOO) after setenv");
        printf("envprobe: FAIL getenv(FOO) = '%s' expected 'bar'\n", foo ? foo : "(null)");
        return 1;
    }

    /* Test 4: setenv overwrite=0 should not replace */
    if (setenv("FOO", "baz", 0) != 0) {
        debug_print("envprobe: FAIL setenv(FOO, baz, 0)");
        return 1;
    }
    foo = getenv("FOO");
    if (foo == NULL || strcmp(foo, "bar") != 0) {
        debug_print("envprobe: FAIL setenv no-overwrite changed value");
        printf("envprobe: FAIL FOO = '%s' expected 'bar'\n", foo ? foo : "(null)");
        return 1;
    }

    /* Test 5: setenv overwrite=1 should replace */
    if (setenv("FOO", "baz", 1) != 0) {
        debug_print("envprobe: FAIL setenv(FOO, baz, 1)");
        return 1;
    }
    foo = getenv("FOO");
    if (foo == NULL || strcmp(foo, "baz") != 0) {
        debug_print("envprobe: FAIL setenv overwrite");
        printf("envprobe: FAIL FOO = '%s' expected 'baz'\n", foo ? foo : "(null)");
        return 1;
    }

    /* Test 6: unsetenv */
    if (unsetenv("FOO") != 0) {
        debug_print("envprobe: FAIL unsetenv(FOO)");
        return 1;
    }
    foo = getenv("FOO");
    if (foo != NULL) {
        debug_print("envprobe: FAIL getenv(FOO) not NULL after unsetenv");
        printf("envprobe: FAIL FOO = '%s' after unsetenv\n", foo);
        return 1;
    }

    /* Test 7: getenv for nonexistent var returns NULL */
    if (getenv("NONEXISTENT_VAR_12345") != NULL) {
        debug_print("envprobe: FAIL nonexistent var not NULL");
        return 1;
    }

    debug_print("envprobe: PASS");
    printf("envprobe: PASS\n");
    return 0;
}
