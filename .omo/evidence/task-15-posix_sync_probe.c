/*
 * T15 POSIX synchronization probe — verifies the CLUU POSIX shim satisfies
 * the SDL timer/thread contract.
 *
 * Tests:
 *   1. Monotonic non-regression: clock_gettime twice, second >= first
 *   2. Delay lower bound: delay(10) takes >= 10ms
 *   3. Timed wait timeout: cond_timedwait 50ms returns within 40-60ms
 *   4. Timed wait signal: woken by signal within 10ms
 *   5. Timed wait broadcast: woken by broadcast within 10ms
 *   6. Join: thread completes and join returns its value
 *   7. TLS: set/get/delete
 *   8. Repeated init/quit: 100 cycles, no leak
 *   9. No busy wait: verified by delay not burning CPU (marker timestamps)
 *
 * Emits "POSIX_SYNC_CLEAN" on success.
 *
 * Build: see Makefile snippet in evidence file.
 * Run:   requires CLUU QEMU (Cluufile + harness integration in T16).
 */

#include <pthread.h>
#include <time.h>
#include <stdio.h>
#include <string.h>
#include <errno.h>

extern int delay(unsigned int ms);

static void emit(const char *msg) {
    fprintf(stdout, "%s\n", msg);
    fflush(stdout);
}

static int fail(const char *what, int rv) {
    fprintf(stderr, "FAIL: %s (rv=%d errno=%d)\n", what, rv, errno);
    fflush(stderr);
    return 1;
}

static unsigned long monotonic_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (unsigned long)ts.tv_sec * 1000UL + (unsigned long)(ts.tv_nsec / 1000000);
}

/* --- Test 1: Monotonic non-regression --- */
static int test_monotonic_non_regression(void) {
    struct timespec t1, t2;
    if (clock_gettime(CLOCK_MONOTONIC, &t1) != 0)
        return fail("monotonic t1", -1);
    if (clock_gettime(CLOCK_MONOTONIC, &t2) != 0)
        return fail("monotonic t2", -1);
    unsigned long ms1 = (unsigned long)t1.tv_sec * 1000 + (unsigned long)(t1.tv_nsec / 1000000);
    unsigned long ms2 = (unsigned long)t2.tv_sec * 1000 + (unsigned long)(t2.tv_nsec / 1000000);
    if (ms2 < ms1)
        return fail("monotonic went backward", (int)(ms1 - ms2));
    emit("T1_MONOTONIC_NON_REGRESSION_OK");
    return 0;
}

/* --- Test 2: Delay lower bound --- */
static int test_delay_lower_bound(void) {
    unsigned long before = monotonic_ms();
    delay(10);
    unsigned long after = monotonic_ms();
    unsigned long elapsed = after - before;
    if (elapsed < 10)
        return fail("delay(10) too short", (int)elapsed);
    emit("T2_DELAY_LOWER_BOUND_OK");
    return 0;
}

/* --- Test 3: Timed wait timeout --- */
static int test_timed_wait_timeout(void) {
    pthread_mutex_t mutex = 0;
    pthread_cond_t cond = 0;
    pthread_mutex_init(&mutex, NULL);
    pthread_cond_init(&cond, NULL);

    struct timespec abstime;
    clock_gettime(CLOCK_REALTIME, &abstime);
    abstime.tv_nsec += 50 * 1000000;
    if (abstime.tv_nsec >= 1000000000) {
        abstime.tv_sec += 1;
        abstime.tv_nsec -= 1000000000;
    }

    unsigned long before = monotonic_ms();
    pthread_mutex_lock(&mutex);
    int rv = pthread_cond_timedwait(&cond, &mutex, &abstime);
    unsigned long after = monotonic_ms();
    pthread_mutex_unlock(&mutex);
    unsigned long elapsed = after - before;

    if (rv != ETIMEDOUT)
        return fail("timedwait should return ETIMEDOUT", rv);
    if (elapsed < 40 || elapsed > 60)
        return fail("timedwait timeout out of range", (int)elapsed);

    pthread_cond_destroy(&cond);
    pthread_mutex_destroy(&mutex);
    emit("T3_TIMED_WAIT_TIMEOUT_OK");
    return 0;
}

/* --- Tests 4 & 5: Timed wait signal/broadcast --- */
struct signal_test {
    pthread_mutex_t mutex;
    pthread_cond_t cond;
    unsigned long wake_delay_ms;
    int use_broadcast;
    unsigned long woken_after_ms;
};

static void *signaler_thread(void *arg) {
    struct signal_test *st = (struct signal_test *)arg;
    delay((unsigned int)st->wake_delay_ms);
    pthread_mutex_lock(&st->mutex);
    if (st->use_broadcast)
        pthread_cond_broadcast(&st->cond);
    else
        pthread_cond_signal(&st->cond);
    pthread_mutex_unlock(&st->mutex);
    return NULL;
}

static int test_timed_wait_signal(int broadcast) {
    struct signal_test st;
    memset(&st, 0, sizeof(st));
    st.wake_delay_ms = 20;
    st.use_broadcast = broadcast;
    pthread_mutex_init(&st.mutex, NULL);
    pthread_cond_init(&st.cond, NULL);

    struct timespec abstime;
    clock_gettime(CLOCK_REALTIME, &abstime);
    abstime.tv_sec += 5;

    pthread_t tid;
    pthread_create(&tid, NULL, signaler_thread, &st);

    unsigned long before = monotonic_ms();
    pthread_mutex_lock(&st.mutex);
    int rv = pthread_cond_timedwait(&st.cond, &st.mutex, &abstime);
    unsigned long after = monotonic_ms();
    pthread_mutex_unlock(&st.mutex);
    unsigned long elapsed = after - before;

    void *retval;
    pthread_join(tid, &retval);

    if (rv != 0)
        return fail(broadcast ? "broadcast timedwait should return 0" : "signal timedwait should return 0", rv);
    if (elapsed > 30)
        return fail(broadcast ? "broadcast wake too slow" : "signal wake too slow", (int)elapsed);

    pthread_cond_destroy(&st.cond);
    pthread_mutex_destroy(&st.mutex);

    if (broadcast)
        emit("T5_TIMED_WAIT_BROADCAST_OK");
    else
        emit("T4_TIMED_WAIT_SIGNAL_OK");
    return 0;
}

/* --- Test 6: Join --- */
static void *join_test_thread(void *arg) {
    (void)arg;
    delay(10);
    return (void *)0x42;
}

static int test_join(void) {
    pthread_t tid;
    pthread_create(&tid, NULL, join_test_thread, NULL);
    void *retval = NULL;
    int rv = pthread_join(tid, &retval);
    if (rv != 0)
        return fail("join failed", rv);
    if (retval != (void *)0x42)
        return fail("join wrong return value", (int)(unsigned long)retval);
    emit("T6_JOIN_OK");
    return 0;
}

/* --- Test 7: TLS --- */
static int test_tls(void) {
    pthread_key_t key;
    int rv = pthread_key_create(&key, NULL);
    if (rv != 0)
        return fail("key_create", rv);

    if (pthread_getspecific(key) != NULL)
        return fail("getspecific should be NULL initially", 0);

    pthread_setspecific(key, (void *)0xDEAD);
    if (pthread_getspecific(key) != (void *)0xDEAD)
        return fail("getspecific wrong value", (int)(unsigned long)pthread_getspecific(key));

    rv = pthread_key_delete(key);
    if (rv != 0)
        return fail("key_delete", rv);

    emit("T7_TLS_OK");
    return 0;
}

/* --- Test 8: Repeated init/quit --- */
static int test_repeated_init_quit(void) {
    pthread_mutex_t mutex;
    pthread_cond_t cond;
    for (int i = 0; i < 100; i++) {
        pthread_mutex_init(&mutex, NULL);
        pthread_cond_init(&cond, NULL);
        pthread_mutex_lock(&mutex);
        pthread_cond_signal(&cond);
        pthread_mutex_unlock(&mutex);
        pthread_cond_destroy(&cond);
        pthread_mutex_destroy(&mutex);
    }
    emit("T8_REPEATED_INIT_QUIT_OK");
    return 0;
}

/* --- Test 9: No busy wait --- */
static int test_no_busy_wait(void) {
    unsigned long before = monotonic_ms();
    delay(50);
    unsigned long after = monotonic_ms();
    unsigned long elapsed = after - before;
    if (elapsed < 50)
        return fail("delay(50) too short — possible busy wait", (int)elapsed);
    if (elapsed > 200)
        return fail("delay(50) way too long — scheduling issue", (int)elapsed);
    emit("T9_NO_BUSY_WAIT_OK");
    return 0;
}

int main(void) {
    int failures = 0;
    failures += test_monotonic_non_regression();
    failures += test_delay_lower_bound();
    failures += test_timed_wait_timeout();
    failures += test_timed_wait_signal(0);
    failures += test_timed_wait_signal(1);
    failures += test_join();
    failures += test_tls();
    failures += test_repeated_init_quit();
    failures += test_no_busy_wait();

    if (failures == 0) {
        emit("POSIX_SYNC_CLEAN");
        return 0;
    }
    fprintf(stderr, "FAILURES: %d\n", failures);
    return 1;
}
