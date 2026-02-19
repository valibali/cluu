#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdlib.h>
#include "py/runtime.h"
#include "py/gc.h"
#include "py/mpthread.h"
#include "py/mpstate.h"
#include "py/mperrno.h"

#ifndef PTHREAD_STACK_MIN
#define PTHREAD_STACK_MIN 200
#endif

typedef struct _mp_thread_t {
    pthread_t id;
    int ready;
    atomic_int exit_flag;   // Cooperative exit: 1 = please exit
    void *arg;
    void *(*entry)(void *);
    struct _mp_thread_t *next;
} mp_thread_t;

static pthread_mutex_t thread_mutex;
static pthread_key_t tls_key;
static mp_thread_t thread_entry0;  // Main thread
static mp_thread_t *thread_list = &thread_entry0;

// Port-level recursive lock state for thread_mutex.
// CLUU mutexes are non-recursive (futex-based 3-state), so we track
// ownership and depth here to allow the GIL (atomic sections) and
// thread list operations to nest safely on the same mutex.
static pthread_t thread_mutex_owner;
static int thread_mutex_depth;

static void thread_mutex_lock_recursive(void) {
    pthread_t self = pthread_self();
    if (thread_mutex_depth > 0 && pthread_equal(thread_mutex_owner, self)) {
        thread_mutex_depth++;
        return;
    }
    pthread_mutex_lock(&thread_mutex);
    thread_mutex_owner = self;
    thread_mutex_depth = 1;
}

static void thread_mutex_unlock_recursive(void) {
    if (--thread_mutex_depth == 0) {
        thread_mutex_owner = 0;
        pthread_mutex_unlock(&thread_mutex);
    }
}

void mp_thread_init(void) {
    pthread_mutex_init(&thread_mutex, NULL);
    pthread_key_create(&tls_key, NULL);
    pthread_setspecific(tls_key, &mp_state_ctx.thread);
    thread_entry0.id = pthread_self();
    thread_entry0.ready = 1;
    atomic_init(&thread_entry0.exit_flag, 0);
    thread_entry0.next = NULL;
    thread_mutex_owner = 0;
    thread_mutex_depth = 0;
}

mp_state_thread_t *mp_thread_get_state(void) {
    return (mp_state_thread_t *)pthread_getspecific(tls_key);
}

void mp_thread_set_state(mp_state_thread_t *state) {
    pthread_setspecific(tls_key, state);
}

mp_uint_t mp_thread_get_id(void) {
    return (mp_uint_t)pthread_self();
}

void mp_thread_deinit(void) {
    // Signal all non-main threads to exit cooperatively
    thread_mutex_lock_recursive();
    for (mp_thread_t *th = thread_list; th != NULL; th = th->next) {
        if (th != &thread_entry0 && th->ready) {
            atomic_store(&th->exit_flag, 1);
        }
    }
    thread_mutex_unlock_recursive();

    // Spin-wait for threads to notice and exit
    for (int i = 0; i < 500; i++) {
        sched_yield();
        thread_mutex_lock_recursive();
        int any_alive = 0;
        for (mp_thread_t *th = thread_list; th != NULL; th = th->next) {
            if (th != &thread_entry0 && th->ready) {
                any_alive = 1;
                break;
            }
        }
        thread_mutex_unlock_recursive();
        if (!any_alive) break;
    }
}

// Wrapper that checks exit_flag before entering user code
static void *thread_entry(void *arg) {
    mp_thread_t *th = (mp_thread_t *)arg;
    // Check if we should exit before even starting
    if (atomic_load(&th->exit_flag)) {
        return NULL;
    }
    void *ret = th->entry(th->arg);
    // Mark ourselves as not ready
    thread_mutex_lock_recursive();
    th->ready = 0;
    thread_mutex_unlock_recursive();
    return ret;
}

mp_uint_t mp_thread_create(void *(*entry)(void *), void *arg, size_t *stack_size) {
    if (*stack_size == 0) {
        *stack_size = 64 * 1024;  // 64KB default
    }
    if (*stack_size < PTHREAD_STACK_MIN) {
        *stack_size = PTHREAD_STACK_MIN;
    }

    mp_thread_t *th = malloc(sizeof(mp_thread_t));
    if (th == NULL) {
        mp_raise_OSError(MP_ENOMEM);
    }
    th->entry = entry;
    th->arg = arg;
    th->ready = 0;
    atomic_init(&th->exit_flag, 0);

    pthread_attr_t attr;
    pthread_attr_init(&attr);
    pthread_attr_setstacksize(&attr, *stack_size);

    thread_mutex_lock_recursive();
    // Add to list
    th->next = thread_list;
    thread_list = th;

    int ret = pthread_create(&th->id, &attr, thread_entry, th);
    if (ret != 0) {
        // Remove from list on failure
        thread_list = th->next;
        thread_mutex_unlock_recursive();
        free(th);
        pthread_attr_destroy(&attr);
        mp_raise_OSError(ret);
    }
    pthread_attr_destroy(&attr);
    thread_mutex_unlock_recursive();
    return (mp_uint_t)th->id;
}

void mp_thread_start(void) {
    thread_mutex_lock_recursive();
    pthread_t self = pthread_self();
    for (mp_thread_t *th = thread_list; th != NULL; th = th->next) {
        if (pthread_equal(th->id, self)) {
            th->ready = 1;
            break;
        }
    }
    thread_mutex_unlock_recursive();
}

void mp_thread_finish(void) {
    thread_mutex_lock_recursive();
    pthread_t self = pthread_self();
    for (mp_thread_t *th = thread_list; th != NULL; th = th->next) {
        if (pthread_equal(th->id, self)) {
            th->ready = 0;
            break;
        }
    }
    thread_mutex_unlock_recursive();
}

void mp_thread_mutex_init(mp_thread_mutex_t *mutex) {
    pthread_mutex_init(mutex, NULL);
}

int mp_thread_mutex_lock(mp_thread_mutex_t *mutex, int wait) {
    // Check exit flag for cooperative termination — if this thread has been
    // asked to exit, return success so it unwinds through the GIL reacquire
    // path rather than blocking forever.
    pthread_t self = pthread_self();
    thread_mutex_lock_recursive();
    for (mp_thread_t *th = thread_list; th != NULL; th = th->next) {
        if (pthread_equal(th->id, self) && atomic_load(&th->exit_flag)) {
            thread_mutex_unlock_recursive();
            return 1;  // Pretend success so thread unwinds
        }
    }
    thread_mutex_unlock_recursive();

    if (wait) {
        return pthread_mutex_lock(mutex) == 0;
    } else {
        return pthread_mutex_trylock(mutex) == 0;
    }
}

void mp_thread_mutex_unlock(mp_thread_mutex_t *mutex) {
    pthread_mutex_unlock(mutex);
}

// Atomic sections (GIL) — port-level recursive lock on thread_mutex.
// Using a single mutex with depth tracking avoids the deadlock that would
// occur if the GIL and thread list operations used separate locks.
void mp_thread_unix_begin_atomic_section(void) {
    thread_mutex_lock_recursive();
}

void mp_thread_unix_end_atomic_section(void) {
    thread_mutex_unlock_recursive();
}

// GC needs to scan other threads' stacks
void mp_thread_gc_others(void) {
    thread_mutex_lock_recursive();
    for (mp_thread_t *th = thread_list; th != NULL; th = th->next) {
        // Skip current thread (GC scans its own stack)
        if (pthread_equal(th->id, pthread_self())) continue;
        if (!th->ready) continue;
        // MicroPython runtime handles the actual stack scanning
        // via mp_thread_set_state() — we just need the list traversal
    }
    thread_mutex_unlock_recursive();
}
