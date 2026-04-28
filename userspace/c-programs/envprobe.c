#include <stdio.h>
#include <stdlib.h>
#include <string.h>

extern void debug_print(const char *msg);

int main(int argc, char **argv) {
    debug_print("envprobe: start");

    /* Print key=value for each requested key (reads from envelope). */
    for (int i = 1; i < argc; i++) {
        const char *val = getenv(argv[i]);
        char buf[256];
        snprintf(buf, sizeof buf, "envprobe: %s=%s", argv[i], val ? val : "(null)");
        debug_print(buf);
    }

    /* Self-check: setenv/getenv/unsetenv semantics still work. */
    if (setenv("FOO", "bar", 1) != 0) { debug_print("envprobe: FAIL setenv(FOO)"); return 1; }
    const char *foo = getenv("FOO");
    if (!foo || strcmp(foo, "bar") != 0) { debug_print("envprobe: FAIL getenv(FOO) after setenv"); return 1; }

    if (setenv("FOO", "baz", 0) != 0) { debug_print("envprobe: FAIL setenv(FOO,baz,0)"); return 1; }
    foo = getenv("FOO");
    if (!foo || strcmp(foo, "bar") != 0) { debug_print("envprobe: FAIL setenv no-overwrite"); return 1; }

    if (setenv("FOO", "baz", 1) != 0) { debug_print("envprobe: FAIL setenv(FOO,baz,1)"); return 1; }
    foo = getenv("FOO");
    if (!foo || strcmp(foo, "baz") != 0) { debug_print("envprobe: FAIL setenv overwrite"); return 1; }

    if (unsetenv("FOO") != 0) { debug_print("envprobe: FAIL unsetenv(FOO)"); return 1; }
    if (getenv("FOO") != NULL) { debug_print("envprobe: FAIL FOO not NULL after unsetenv"); return 1; }
    if (getenv("NONEXISTENT_VAR_12345") != NULL) { debug_print("envprobe: FAIL nonexistent not NULL"); return 1; }

    debug_print("envprobe: PASS");
    return 0;
}
