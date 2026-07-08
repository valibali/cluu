/*
 * errnoprobe.c — Verify per-thread errno isolation.
 *
 * Two threads each set a distinct errno, then read it back.
 * Passes if each thread sees its own errno (not the other's).
 */
#include <stdio.h>
#include <stdint.h>

typedef unsigned long pthread_t;
typedef struct { int __dummy; } pthread_attr_t;

int pthread_create(pthread_t *thread, const pthread_attr_t *attr,
                   void *(*start_routine)(void *), void *arg);
int pthread_join(pthread_t thread, void **retval);

extern void debug_print(const char *msg);
extern int *__errno(void);
#define errno (*__errno())

static volatile int thread_a_ok = 0;
static volatile int thread_b_ok = 0;

static void *set_errno_a(void *arg) {
    (void)arg;
    errno = 11; /* EAGAIN */
    /* yield to let thread B set its errno */
    volatile int spin = 0;
    for (int i = 0; i < 1000000; i++) spin += i;
    /* check our errno is still ours */
    if (errno == 11) {
        thread_a_ok = 1;
    }
    return (void *)0;
}

static void *set_errno_b(void *arg) {
    (void)arg;
    errno = 12; /* ENOMEM */
    volatile int spin = 0;
    for (int i = 0; i < 1000000; i++) spin += i;
    if (errno == 12) {
        thread_b_ok = 1;
    }
    return (void *)0;
}

int main(void) {
    pthread_t ta, tb;

    /* set main thread errno */
    errno = 42;

    pthread_create(&ta, NULL, set_errno_a, NULL);
    pthread_create(&tb, NULL, set_errno_b, NULL);

    pthread_join(ta, NULL);
    pthread_join(tb, NULL);

    if (thread_a_ok && thread_b_ok && errno == 42) {
        debug_print("errnoprobe: PASS");
        debug_print("ERRNO_OK");
    } else {
        debug_print("errnoprobe: FAIL");
        debug_print("ERRNO_FAIL");
    }
    return 0;
}
