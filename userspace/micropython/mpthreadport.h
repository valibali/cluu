#ifndef MICROPY_INCLUDED_CLUU_MPTHREADPORT_H
#define MICROPY_INCLUDED_CLUU_MPTHREADPORT_H

#include <pthread.h>

typedef pthread_mutex_t mp_thread_mutex_t;

void mp_thread_init(void);
void mp_thread_deinit(void);
void mp_thread_gc_others(void);

#endif
