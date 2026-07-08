/*
 * stackprobe.c — Verify pthread_attr_setstacksize is honored.
 *
 * Creates a thread with a 256 KiB stack. The thread touches the
 * far end of the stack (128 KiB deep) to prove the stack is at
 * least that large. If the attr were ignored and the stack were
 * only 64 KiB (16 pages), touching 128 KiB deep would hit the
 * guard page and crash.
 *
 * Pass: STACK_OK
 */
#include <stdio.h>
#include <stdint.h>

typedef unsigned long pthread_t;
typedef unsigned long pthread_attr_t;

int pthread_create(pthread_t *thread, const pthread_attr_t *attr,
                   void *(*start_routine)(void *), void *arg);
int pthread_join(pthread_t thread, void **retval);
int pthread_attr_init(pthread_attr_t *attr);
int pthread_attr_setstacksize(pthread_attr_t *attr, unsigned long stacksize);

extern void debug_print(const char *msg);

/* Recurse to a given depth, writing a sentinel at each frame. */
static int recurse(int depth, volatile char *marker) {
    volatile char buf[4096]; /* 4 KB per frame */
    buf[0] = (char)depth;
    if (depth <= 0) {
        /* Touch the marker to prove we reached this depth. */
        *marker = (char)0x42;
        return buf[0];
    }
    return recurse(depth - 1, marker) + buf[0];
}

static void *use_stack(void *arg) {
    volatile char marker = 0;
    debug_print("stackprobe: thread entered");
    /* 32 frames × 4 KB = 128 KB — double old 64 KB default */
    int sum = recurse(32, &marker);
    debug_print("stackprobe: recurse done");
    if (marker == 0x42 && sum > 0) {
        return (void *)1;
    }
    return (void *)0;
}

int main(void) {
    pthread_t t;
    pthread_attr_t attr;
    void *retval = NULL;

    pthread_attr_init(&attr);
    pthread_attr_setstacksize(&attr, 256 * 1024);

    debug_print("stackprobe: creating thread 256KB");
    pthread_create(&t, &attr, use_stack, NULL);
    debug_print("stackprobe: joining");
    pthread_join(t, &retval);
    debug_print("stackprobe: joined");

    if (retval == (void *)1) {
        debug_print("stackprobe: PASS");
        debug_print("STACK_OK");
    } else {
        debug_print("stackprobe: FAIL");
        debug_print("STACK_FAIL");
    }
    return 0;
}
