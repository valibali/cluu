#ifndef MICROPY_INCLUDED_CLUU_MPHALPORT_H
#define MICROPY_INCLUDED_CLUU_MPHALPORT_H

#include <errno.h>
#include <time.h>
#include <unistd.h>

#ifndef CHAR_CTRL_C
#define CHAR_CTRL_C (3)
#endif

// PEP 475: retry syscalls on EINTR
#define MP_HAL_RETRY_SYSCALL(ret, syscall, raise) \
    { \
        for (;;) { \
            MP_THREAD_GIL_EXIT(); \
            ret = syscall; \
            MP_THREAD_GIL_ENTER(); \
            if (ret == -1) { \
                int err = errno; \
                if (err == EINTR) { \
                    mp_handle_pending(true); \
                    continue; \
                } \
                raise; \
            } \
            break; \
        } \
    }

void mp_hal_set_interrupt_char(char c);

#define mp_hal_stdio_poll unused  // not implemented, not needed

// Use POSIX versions
static inline mp_uint_t mp_hal_ticks_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (mp_uint_t)(ts.tv_sec * 1000 + ts.tv_nsec / 1000000);
}

static inline mp_uint_t mp_hal_ticks_us(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (mp_uint_t)(ts.tv_sec * 1000000 + ts.tv_nsec / 1000);
}

static inline void mp_hal_delay_us(mp_uint_t us) {
    usleep(us);
}

#endif
