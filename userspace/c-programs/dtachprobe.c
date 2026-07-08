/*
 * detachprobe.c — Verify detached thread stacks are reclaimed.
 *
 * Spawns 50 detached threads in a loop. Each thread runs briefly
 * then exits. If stacks leak, the stack region (256 MB) fills up
 * and thread creation fails. If reclamation works, all 50 succeed.
 *
 * Pass: DETACH_OK
 */
#include <stdio.h>
#include <stdint.h>

typedef unsigned long pthread_t;
typedef unsigned long pthread_attr_t;

int pthread_create(pthread_t *thread, const pthread_attr_t *attr,
                   void *(*start_routine)(void *), void *arg);
int pthread_detach(pthread_t thread);

extern void debug_print(const char *msg);

static volatile int threads_ok = 0;

static void *quick_exit(void *arg) {
    (void)arg;
    volatile int x = 42;
    for (int i = 0; i < 1000; i++) x += i;
    return (void *)0;
}

int main(void) {
    debug_print("detachprobe: starting 50 detached threads");
    int ok_count = 0;
    for (int i = 0; i < 50; i++) {
        pthread_t t;
        int rc = pthread_create(&t, NULL, quick_exit, NULL);
        if (rc != 0) {
            debug_print("detachprobe: create failed");
            break;
        }
        rc = pthread_detach(t);
        if (rc != 0) {
            debug_print("detachprobe: detach failed");
            break;
        }
        ok_count++;
        /* Yield to let the thread run and exit, exercising the reap path. */
        for (volatile int spin = 0; spin < 100000; spin++) {}
    }
    debug_print("detachprobe: spawned, draining reap queue");
    /* Spawn one more to drain the reap queue (pthread_create calls reap_dead_threads). */
    pthread_t t;
    pthread_create(&t, NULL, quick_exit, NULL);
    pthread_detach(t);
    for (volatile int spin = 0; spin < 1000000; spin++) {}

    if (ok_count == 50) {
        threads_ok = 1;
    }

    if (threads_ok) {
        debug_print("detachprobe: PASS");
        debug_print("DETACH_OK");
    } else {
        debug_print("detachprobe: FAIL");
        debug_print("DETACH_FAIL");
    }
    return 0;
}
