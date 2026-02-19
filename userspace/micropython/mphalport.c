#include <unistd.h>
#include <string.h>
#include <time.h>
#include "py/mpconfig.h"
#include "py/runtime.h"
#include "py/mphal.h"

// Interrupt char for Ctrl-C handling.
static int interrupt_char = -1;

// Blocking single-char read from stdin (fd 0)
int mp_hal_stdin_rx_chr(void) {
    unsigned char c;
    ssize_t n = read(0, &c, 1);
    if (n <= 0) return -1;
    if (interrupt_char >= 0 && c == (unsigned char)interrupt_char) {
        mp_sched_keyboard_interrupt();
    }
    // CLUU tty canonical reads return '\n'; MicroPython readline expects '\r'
    // as the line-commit character in friendly REPL mode.
    if (c == '\n') {
        c = '\r';
    }
    return (int)c;
}

// Write string to stdout (fd 1)
mp_uint_t mp_hal_stdout_tx_strn(const char *str, size_t len) {
    ssize_t ret;
    while (len > 0) {
        ret = write(1, str, len);
        if (ret <= 0) break;
        str += ret;
        len -= (size_t)ret;
    }
    return 0;
}

void mp_hal_delay_ms(mp_uint_t ms) {
    usleep(ms * 1000);
}

void mp_hal_set_interrupt_char(char c) {
    interrupt_char = (int)c;
}

mp_uint_t mp_hal_ticks_cpu(void) {
    // No cycle counter on CLUU — return microseconds as approximation
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (mp_uint_t)(ts.tv_sec * 1000000 + ts.tv_nsec / 1000);
}

int fsync(int fd) {
    (void)fd;
    return 0;
}
