/*
 * tlsprobe.c — Verify TLS (thread-local storage) works on CLUU.
 *
 * Tests:
 * 1. FS base is non-zero (TLS initialized by __cluu_init)
 * 2. Reading/writing a __thread variable works
 * 3. printf (which uses errno internally) works after TLS init
 */
#include <stdio.h>
#include <stdint.h>

/* debug_print writes to kernel serial log (COM2) for test harness visibility */
extern void debug_print(const char *msg);

/* Read FS base directly via inline asm (rdfsbase if available, else __cluu_init set it) */
static inline uintptr_t read_fs_base(void) {
    uintptr_t val;
    /* Read %fs:0 which should be the TCB self-pointer */
    __asm__ volatile("mov %%fs:0, %0" : "=r"(val));
    return val;
}

__thread int tls_var = 42;

int main(void) {
    int pass = 1;

    /* Test 1: FS base is set (TCB self-pointer readable) */
    uintptr_t fs0 = read_fs_base();
    if (fs0 != 0) {
        printf("OK: FS:0 self-pointer = 0x%lx\n", (unsigned long)fs0);
    } else {
        debug_print("tlsprobe: FAIL FS:0 is zero");
        printf("FAIL: FS:0 is zero (TLS not initialized)\n");
        pass = 0;
    }

    /* Test 2: __thread variable has correct initial value */
    if (tls_var == 42) {
        printf("OK: __thread var initial = %d\n", tls_var);
    } else {
        debug_print("tlsprobe: FAIL __thread var wrong initial");
        printf("FAIL: __thread var initial = %d (expected 42)\n", tls_var);
        pass = 0;
    }

    /* Test 3: __thread variable can be modified */
    tls_var = 99;
    if (tls_var == 99) {
        printf("OK: __thread var modified = %d\n", tls_var);
    } else {
        debug_print("tlsprobe: FAIL __thread var wrong modified");
        printf("FAIL: __thread var modified = %d (expected 99)\n", tls_var);
        pass = 0;
    }

    if (pass) {
        debug_print("tlsprobe: PASS");
        printf("tlsprobe: PASS\n");
    } else {
        debug_print("tlsprobe: FAIL");
        printf("tlsprobe: FAIL\n");
    }
    return pass ? 0 : 1;
}
