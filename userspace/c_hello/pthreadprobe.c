/*
 * pthreadprobe.c — Verify pthreads on CLUU.
 *
 * Tests:
 * 1. Create a thread that returns a value, join and check return value
 * 2. Create a thread that modifies shared data via pointer argument
 * 3. Create multiple threads and join them all
 * 4. __thread variables are per-thread (no cross-contamination)
 * 5. Mutex protects a shared counter from concurrent increments
 * 6. Condition variable: producer/consumer handshake
 * 7. pthread_once ensures init runs exactly once
 * 8. pthread_key: thread-specific data with destructors
 */
#include <stdio.h>
#include <stdint.h>

/* CLUU doesn't define _POSIX_THREADS so newlib's pthread.h is empty.
   Declare the functions we need directly (implemented in libcluu). */
typedef unsigned long pthread_t;
typedef struct { int __dummy; } pthread_attr_t;
typedef unsigned int pthread_mutex_t;
typedef unsigned int pthread_cond_t;
typedef unsigned int pthread_once_t;
typedef unsigned int pthread_key_t;

int pthread_create(pthread_t *thread, const pthread_attr_t *attr,
                   void *(*start_routine)(void *), void *arg);
int pthread_join(pthread_t thread, void **retval);
int pthread_mutex_init(pthread_mutex_t *mutex, const void *attr);
int pthread_mutex_destroy(pthread_mutex_t *mutex);
int pthread_mutex_lock(pthread_mutex_t *mutex);
int pthread_mutex_trylock(pthread_mutex_t *mutex);
int pthread_mutex_unlock(pthread_mutex_t *mutex);
int pthread_cond_init(pthread_cond_t *cond, const void *attr);
int pthread_cond_destroy(pthread_cond_t *cond);
int pthread_cond_wait(pthread_cond_t *cond, pthread_mutex_t *mutex);
int pthread_cond_signal(pthread_cond_t *cond);
int pthread_cond_broadcast(pthread_cond_t *cond);
int pthread_once(pthread_once_t *once, void (*init_routine)(void));
int pthread_key_create(pthread_key_t *key, void (*destructor)(void *));
int pthread_key_delete(pthread_key_t key);
void *pthread_getspecific(pthread_key_t key);
int pthread_setspecific(pthread_key_t key, const void *value);

extern void debug_print(const char *msg);

/* --- Test 1: Basic create/join with return value --- */
static void *return_42(void *arg) {
    (void)arg;
    return (void *)(uintptr_t)42;
}

/* --- Test 2: Thread modifies shared data --- */
static void *increment(void *arg) {
    int *p = (int *)arg;
    *p += 10;
    return NULL;
}

/* --- Test 3: Multiple threads --- */
#define N_THREADS 4
static void *thread_id_fn(void *arg) {
    int id = (int)(uintptr_t)arg;
    return (void *)(uintptr_t)(id * 100);
}

/* --- Test 4: TLS per-thread isolation --- */
__thread int per_thread_val = 0;

static void *set_tls(void *arg) {
    int id = (int)(uintptr_t)arg;
    per_thread_val = id;
    /* Yield to give other thread a chance to overwrite if broken */
    for (volatile int i = 0; i < 1000; i++) {}
    /* Check our value is still what we set */
    return (void *)(uintptr_t)per_thread_val;
}

/* --- Test 5: Mutex-protected counter --- */
static pthread_mutex_t counter_mutex;
static int counter = 0;
#define INCREMENTS_PER_THREAD 100

static void *mutex_incrementer(void *arg) {
    (void)arg;
    for (int i = 0; i < INCREMENTS_PER_THREAD; i++) {
        pthread_mutex_lock(&counter_mutex);
        counter++;
        pthread_mutex_unlock(&counter_mutex);
    }
    return NULL;
}

/* --- Test 6: Condition variable --- */
static pthread_mutex_t cond_mutex;
static pthread_cond_t cond_var;
static int cond_ready = 0;
static int cond_value = 0;

static void *cond_producer(void *arg) {
    int val = (int)(uintptr_t)arg;
    pthread_mutex_lock(&cond_mutex);
    cond_value = val;
    cond_ready = 1;
    pthread_cond_signal(&cond_var);
    pthread_mutex_unlock(&cond_mutex);
    return NULL;
}

static void *cond_consumer(void *arg) {
    (void)arg;
    pthread_mutex_lock(&cond_mutex);
    while (!cond_ready) {
        pthread_cond_wait(&cond_var, &cond_mutex);
    }
    int result = cond_value;
    pthread_mutex_unlock(&cond_mutex);
    return (void *)(uintptr_t)result;
}

/* --- Test 7: pthread_once --- */
static pthread_once_t once_ctrl = 0;
static int once_counter = 0;
static void once_init(void) {
    once_counter++;
}

static void *call_once(void *arg) {
    (void)arg;
    pthread_once(&once_ctrl, once_init);
    return (void *)(uintptr_t)once_counter;
}

/* --- Test 8: pthread_key --- */
static pthread_key_t test_key;
static volatile int dtor_called_count = 0;

static void key_destructor(void *val) {
    (void)val;
    /* Increment atomically (single-core so volatile is fine) */
    dtor_called_count++;
}

static void *use_key(void *arg) {
    int id = (int)(uintptr_t)arg;
    pthread_setspecific(test_key, (void *)(uintptr_t)(id * 10));
    void *val = pthread_getspecific(test_key);
    return val;
}

int main(void) {
    int pass = 1;
    pthread_t t;
    void *retval;
    int rc;

    debug_print("pthreadprobe: start");

    /* Test 1: Basic create + join with return value */
    rc = pthread_create(&t, NULL, return_42, NULL);
    if (rc != 0) {
        debug_print("pthreadprobe: FAIL create returned nonzero");
        printf("FAIL: pthread_create returned %d\n", rc);
        pass = 0;
    } else {
        rc = pthread_join(t, &retval);
        if (rc != 0) {
            debug_print("pthreadprobe: FAIL join returned nonzero");
            printf("FAIL: pthread_join returned %d\n", rc);
            pass = 0;
        } else if ((uintptr_t)retval != 42) {
            debug_print("pthreadprobe: FAIL wrong return value");
            printf("FAIL: return value = %lu (expected 42)\n", (unsigned long)(uintptr_t)retval);
            pass = 0;
        } else {
            printf("OK: create+join, return value = 42\n");
        }
    }

    /* Test 2: Thread modifies shared data */
    int shared = 5;
    rc = pthread_create(&t, NULL, increment, &shared);
    if (rc != 0) {
        debug_print("pthreadprobe: FAIL create for shared data test");
        pass = 0;
    } else {
        pthread_join(t, NULL);
        if (shared != 15) {
            debug_print("pthreadprobe: FAIL shared data wrong");
            printf("FAIL: shared = %d (expected 15)\n", shared);
            pass = 0;
        } else {
            printf("OK: shared data modified by thread\n");
        }
    }

    /* Test 3: Multiple threads */
    {
        pthread_t threads[N_THREADS];
        int all_ok = 1;
        for (int i = 0; i < N_THREADS; i++) {
            rc = pthread_create(&threads[i], NULL, thread_id_fn, (void *)(uintptr_t)i);
            if (rc != 0) {
                debug_print("pthreadprobe: FAIL multi-thread create");
                printf("FAIL: pthread_create[%d] returned %d\n", i, rc);
                all_ok = 0;
                pass = 0;
                break;
            }
        }
        if (all_ok) {
            for (int i = 0; i < N_THREADS; i++) {
                pthread_join(threads[i], &retval);
                int expected = i * 100;
                if ((int)(uintptr_t)retval != expected) {
                    debug_print("pthreadprobe: FAIL multi-thread wrong retval");
                    printf("FAIL: thread[%d] returned %d (expected %d)\n",
                           i, (int)(uintptr_t)retval, expected);
                    pass = 0;
                }
            }
            if (pass) {
                printf("OK: %d threads created and joined\n", N_THREADS);
            }
        }
    }

    /* Test 4: TLS isolation between threads */
    {
        per_thread_val = 999; /* main thread value */
        pthread_t t1, t2;
        void *r1, *r2;
        rc = pthread_create(&t1, NULL, set_tls, (void *)(uintptr_t)1);
        if (rc == 0) rc = pthread_create(&t2, NULL, set_tls, (void *)(uintptr_t)2);
        if (rc != 0) {
            debug_print("pthreadprobe: FAIL TLS isolation create");
            pass = 0;
        } else {
            pthread_join(t1, &r1);
            pthread_join(t2, &r2);
            if ((int)(uintptr_t)r1 != 1 || (int)(uintptr_t)r2 != 2) {
                debug_print("pthreadprobe: FAIL TLS not isolated");
                printf("FAIL: TLS t1=%d t2=%d (expected 1,2)\n",
                       (int)(uintptr_t)r1, (int)(uintptr_t)r2);
                pass = 0;
            } else if (per_thread_val != 999) {
                debug_print("pthreadprobe: FAIL main TLS corrupted");
                printf("FAIL: main TLS = %d (expected 999)\n", per_thread_val);
                pass = 0;
            } else {
                printf("OK: TLS per-thread isolation verified\n");
            }
        }
    }

    /* Test 5: Mutex-protected counter */
    {
        pthread_mutex_init(&counter_mutex, NULL);
        counter = 0;
        pthread_t mt[N_THREADS];
        int all_ok = 1;
        for (int i = 0; i < N_THREADS; i++) {
            rc = pthread_create(&mt[i], NULL, mutex_incrementer, NULL);
            if (rc != 0) {
                debug_print("pthreadprobe: FAIL mutex thread create");
                all_ok = 0;
                pass = 0;
                break;
            }
        }
        if (all_ok) {
            for (int i = 0; i < N_THREADS; i++) {
                pthread_join(mt[i], NULL);
            }
            int expected = N_THREADS * INCREMENTS_PER_THREAD;
            if (counter != expected) {
                debug_print("pthreadprobe: FAIL mutex counter wrong");
                printf("FAIL: counter=%d (expected %d)\n", counter, expected);
                pass = 0;
            } else {
                printf("OK: mutex-protected counter = %d\n", counter);
            }
        }
        pthread_mutex_destroy(&counter_mutex);
    }

    /* Test 6: Condition variable */
    {
        pthread_mutex_init(&cond_mutex, NULL);
        pthread_cond_init(&cond_var, NULL);
        cond_ready = 0;
        cond_value = 0;

        pthread_t consumer, producer;
        /* Start consumer first — it will block on cond_wait */
        rc = pthread_create(&consumer, NULL, cond_consumer, NULL);
        if (rc == 0) {
            rc = pthread_create(&producer, NULL, cond_producer, (void *)(uintptr_t)77);
        }
        if (rc != 0) {
            debug_print("pthreadprobe: FAIL condvar thread create");
            pass = 0;
        } else {
            void *consumer_result;
            pthread_join(producer, NULL);
            pthread_join(consumer, &consumer_result);
            if ((int)(uintptr_t)consumer_result != 77) {
                debug_print("pthreadprobe: FAIL condvar wrong value");
                printf("FAIL: condvar consumer got %d (expected 77)\n",
                       (int)(uintptr_t)consumer_result);
                pass = 0;
            } else {
                printf("OK: condvar producer/consumer = 77\n");
            }
        }
        pthread_cond_destroy(&cond_var);
        pthread_mutex_destroy(&cond_mutex);
    }

    /* Test 7: pthread_once */
    {
        once_ctrl = 0;
        once_counter = 0;
        pthread_t ot[N_THREADS];
        int all_ok = 1;
        for (int i = 0; i < N_THREADS; i++) {
            rc = pthread_create(&ot[i], NULL, call_once, NULL);
            if (rc != 0) {
                debug_print("pthreadprobe: FAIL once thread create");
                all_ok = 0;
                pass = 0;
                break;
            }
        }
        if (all_ok) {
            for (int i = 0; i < N_THREADS; i++) {
                pthread_join(ot[i], &retval);
            }
            if (once_counter != 1) {
                debug_print("pthreadprobe: FAIL once ran multiple times");
                printf("FAIL: once_counter=%d (expected 1)\n", once_counter);
                pass = 0;
            } else {
                printf("OK: pthread_once ran exactly once\n");
            }
        }
    }

    /* Test 8: pthread_key with destructor */
    {
        dtor_called_count = 0;
        rc = pthread_key_create(&test_key, key_destructor);
        if (rc != 0) {
            debug_print("pthreadprobe: FAIL key_create");
            printf("FAIL: pthread_key_create returned %d\n", rc);
            pass = 0;
        } else {
            pthread_t kt[2];
            rc = pthread_create(&kt[0], NULL, use_key, (void *)(uintptr_t)3);
            if (rc == 0) rc = pthread_create(&kt[1], NULL, use_key, (void *)(uintptr_t)7);
            if (rc != 0) {
                debug_print("pthreadprobe: FAIL key thread create");
                pass = 0;
            } else {
                void *r0, *r1;
                pthread_join(kt[0], &r0);
                pthread_join(kt[1], &r1);
                if ((int)(uintptr_t)r0 != 30 || (int)(uintptr_t)r1 != 70) {
                    debug_print("pthreadprobe: FAIL key getspecific wrong");
                    printf("FAIL: key values = %d, %d (expected 30, 70)\n",
                           (int)(uintptr_t)r0, (int)(uintptr_t)r1);
                    pass = 0;
                } else if (dtor_called_count != 2) {
                    debug_print("pthreadprobe: FAIL key destructor count wrong");
                    printf("FAIL: destructor called %d times (expected 2)\n",
                           dtor_called_count);
                    pass = 0;
                } else {
                    printf("OK: pthread_key get/set + destructors\n");
                }
            }
            pthread_key_delete(test_key);
        }
    }

    if (pass) {
        debug_print("pthreadprobe: PASS");
        printf("pthreadprobe: PASS\n");
    } else {
        debug_print("pthreadprobe: FAIL");
        printf("pthreadprobe: FAIL\n");
    }
    return pass ? 0 : 1;
}
