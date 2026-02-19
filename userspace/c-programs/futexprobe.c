#include <stdint.h>
#include <stdio.h>

extern void debug_print(const char *msg);

typedef struct {
    uintptr_t exit_token;
    uintptr_t exit_cookie;
    uintptr_t pid;
    uintptr_t tokens[16];
    uint64_t params[8];
} ProcessInfo;

enum {
    SYSCALL_INVOKE = 5,
    INVOKE_FUTEX_WAIT = 17,
    INVOKE_FUTEX_WAKE = 18,
    TOKEN_SPACE = 5,
};

static inline long syscall6(
    long number,
    long arg1,
    long arg2,
    long arg3,
    long arg4,
    long arg5,
    long arg6) {
    register long r10 __asm__("r10") = arg4;
    register long r8 __asm__("r8") = arg5;
    register long r9 __asm__("r9") = arg6;
    long ret = 0;
    __asm__ volatile("syscall"
                     : "=a"(ret)
                     : "a"(number), "D"(arg1), "S"(arg2), "d"(arg3), "r"(r10), "r"(r8), "r"(r9)
                     : "rcx", "r11", "memory");
    return ret;
}

static inline long invoke(
    uintptr_t token,
    uintptr_t op,
    uintptr_t arg1,
    uintptr_t arg2,
    uintptr_t arg3,
    uintptr_t arg4) {
    return syscall6(
        SYSCALL_INVOKE,
        (long)token,
        (long)op,
        (long)arg1,
        (long)arg2,
        (long)arg3,
        (long)arg4);
}

static inline long futex_wait(uintptr_t space_token, volatile uint32_t *addr, uint32_t expected, uint64_t timeout_ms) {
    return invoke(
        space_token,
        INVOKE_FUTEX_WAIT,
        (uintptr_t)addr,
        (uintptr_t)expected,
        (uintptr_t)(timeout_ms & 0xFFFFffffULL),
        (uintptr_t)(timeout_ms >> 32));
}

static inline long futex_wake(uintptr_t space_token, volatile uint32_t *addr, uint32_t max_count) {
    return invoke(space_token, INVOKE_FUTEX_WAKE, (uintptr_t)addr, (uintptr_t)max_count, 0, 0);
}

int main(void) {
    volatile uint32_t word = 1;
    const ProcessInfo *info = (const ProcessInfo *)0x7fe00100ULL;
    uintptr_t space_token = info->tokens[TOKEN_SPACE];

    if (space_token == 0) {
        debug_print("futexprobe: FAIL missing space token");
        printf("futexprobe: FAIL missing space token\n");
        return 1;
    }

    long rc = futex_wait(space_token, &word, 0, 5);
    if (rc != -12) {
        debug_print("futexprobe: FAIL mismatch path");
        printf("futexprobe: FAIL mismatch path rc=%ld expected=-12\n", rc);
        return 1;
    }

    rc = futex_wake(space_token, &word, 1);
    if (rc != 0) {
        debug_print("futexprobe: FAIL wake empty");
        printf("futexprobe: FAIL wake empty rc=%ld expected=0\n", rc);
        return 1;
    }

    rc = futex_wait(space_token, &word, 1, 8);
    if (rc != -7 && rc != 0) {
        debug_print("futexprobe: FAIL timeout path");
        printf("futexprobe: FAIL timeout path rc=%ld expected=-7 or 0\n", rc);
        return 1;
    }

    debug_print("futexprobe: PASS");
    printf("futexprobe: PASS\n");
    return 0;
}
