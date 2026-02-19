#include <setjmp.h>
#include <stdio.h>

extern void debug_print(const char *msg);

int main(void) {
    debug_print("setjmpprobe: main entered");
    jmp_buf buf;
    int val;

    /* Test 1: setjmp returns 0 initially, longjmp delivers specified value */
    val = setjmp(buf);
    if (val == 0) {
        longjmp(buf, 42);
        debug_print("setjmpprobe: FAIL unreachable after longjmp");
        printf("setjmpprobe: FAIL unreachable after longjmp\n");
        return 1;
    }

    if (val != 42) {
        debug_print("setjmpprobe: FAIL wrong longjmp value");
        printf("setjmpprobe: FAIL expected 42 got %d\n", val);
        return 1;
    }

    /* Test 2: nested setjmp/longjmp */
    jmp_buf outer, inner;
    volatile int stage = 0;

    val = setjmp(outer);
    if (val == 0) {
        stage = 1;
        int val2 = setjmp(inner);
        if (val2 == 0) {
            stage = 2;
            longjmp(inner, 99);
            return 1;
        }
        if (val2 != 99 || stage != 2) {
            debug_print("setjmpprobe: FAIL nested inner");
            printf("setjmpprobe: FAIL nested inner val=%d stage=%d\n", val2, stage);
            return 1;
        }
        stage = 3;
        longjmp(outer, 77);
        return 1;
    }

    if (val != 77 || stage != 3) {
        debug_print("setjmpprobe: FAIL nested outer");
        printf("setjmpprobe: FAIL nested outer val=%d stage=%d\n", val, stage);
        return 1;
    }

    debug_print("setjmpprobe: PASS");
    printf("setjmpprobe: PASS\n");
    return 0;
}
