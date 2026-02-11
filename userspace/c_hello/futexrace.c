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
    SYSCALL_YIELD = 4,
    SYSCALL_INVOKE = 5,
    INVOKE_THREAD_CREATE = 0,
    INVOKE_THREAD_DESTROY = 1,
    INVOKE_FUTEX_WAIT = 17,
    INVOKE_FUTEX_WAKE = 18,
    TOKEN_SPACE = 5,
};

static volatile uint32_t g_futex_word = 0;
static volatile uint32_t g_waiter_ready = 0;
static volatile uint32_t g_waiter_done = 0;
static volatile uint32_t g_wait_result = 0;
static volatile uint32_t g_waiter_phase = 0;
static uintptr_t g_space_token = 0;

static uint8_t g_waiter_stack[16 * 1024] __attribute__((aligned(16)));

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

static inline long syscall0(long number) {
    return syscall6(number, 0, 0, 0, 0, 0, 0);
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

static inline void yield_cpu(void) {
    (void)syscall0(SYSCALL_YIELD);
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

__attribute__((noreturn)) static void waiter_entry(void) {
    g_waiter_ready = 1;
    g_waiter_phase = 1; // entering futex wait path
    long rc = futex_wait(g_space_token, &g_futex_word, 0, 4000);
    g_waiter_phase = 2; // futex wait returned
    if (rc == 0) {
        g_wait_result = 1;
    } else {
        g_wait_result = (uint32_t)(1000u + (uint32_t)(-rc));
    }
    g_waiter_done = 1;
    for (;;) {
        yield_cpu();
    }
}

static int wait_flag(volatile uint32_t *flag, uint32_t expected, uint32_t spins) {
    for (uint32_t i = 0; i < spins; i++) {
        if (*flag == expected) {
            return 0;
        }
        yield_cpu();
    }
    return -1;
}

int main(void) {
    const ProcessInfo *info = (const ProcessInfo *)0x7fe00100ULL;
    g_space_token = info->tokens[TOKEN_SPACE];
    if (g_space_token == 0) {
        debug_print("futexrace: FAIL missing space token");
        printf("futexrace: FAIL missing space token\n");
        return 1;
    }

    g_futex_word = 0;
    g_waiter_ready = 0;
    g_waiter_done = 0;
    g_wait_result = 0;
    g_waiter_phase = 0;

    uintptr_t stack_top = (uintptr_t)&g_waiter_stack[sizeof(g_waiter_stack)];
    stack_top &= ~(uintptr_t)0xFULL;
    long waiter_token = invoke(
        g_space_token,
        INVOKE_THREAD_CREATE,
        (uintptr_t)&waiter_entry,
        stack_top,
        200,
        0);
    if (waiter_token <= 0) {
        debug_print("futexrace: FAIL thread_create");
        printf("futexrace: FAIL thread_create rc=%ld\n", waiter_token);
        return 1;
    }

    if (wait_flag(&g_waiter_ready, 1, 20000) != 0) {
        debug_print("futexrace: FAIL waiter_ready timeout");
        printf("futexrace: FAIL waiter_ready timeout\n");
        return 1;
    }

    if (wait_flag(&g_waiter_phase, 1, 50000) != 0) {
        debug_print("futexrace: FAIL waiter phase timeout");
        printf("futexrace: FAIL waiter phase timeout\n");
        return 1;
    }
    for (uint32_t i = 0; i < 5000; i++) {
        yield_cpu();
    }

    long woken = 0;
    for (uint32_t i = 0; i < 100000; i++) {
        woken = futex_wake(g_space_token, &g_futex_word, 1);
        if (woken > 0 || g_waiter_done == 1) {
            break;
        }
        yield_cpu();
    }
    long destroy_rc = invoke((uintptr_t)waiter_token, INVOKE_THREAD_DESTROY, 0, 0, 0, 0);
    if (destroy_rc < 0) {
        debug_print("futexrace: FAIL thread_destroy");
        printf("futexrace: FAIL thread_destroy rc=%ld\n", destroy_rc);
        return 1;
    }

    g_futex_word = 5;
    long mismatch_rc = futex_wait(g_space_token, &g_futex_word, 0, 10);
    if (mismatch_rc != -12) {
        debug_print("futexrace: FAIL mismatch");
        printf("futexrace: FAIL mismatch rc=%ld expected=-12\n", mismatch_rc);
        return 1;
    }

    long wake_empty = futex_wake(g_space_token, &g_futex_word, 1);
    if (wake_empty != 0) {
        debug_print("futexrace: FAIL wake-empty");
        printf("futexrace: FAIL wake-empty rc=%ld expected=0\n", wake_empty);
        return 1;
    }

    debug_print("futexrace: PASS");
    printf("futexrace: PASS\n");
    return 0;
}
