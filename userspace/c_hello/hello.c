/*
 * CLUU C Hello World
 *
 * A simple test program to verify the C toolchain works.
 * This uses the POSIX syscall stubs provided by libcluu.
 */

/* For now, we use minimal headers since newlib may not be installed */

/* Syscall stubs provided by libcluu_syscalls */
extern int _write(int fd, const void *buf, unsigned long count);
extern void _exit(int status);
extern int _getpid(void);

/* Simple string length function */
static unsigned long strlen(const char *s) {
    unsigned long len = 0;
    while (*s++) len++;
    return len;
}

/* Print a string to stdout */
static void print(const char *s) {
    _write(1, s, strlen(s));
}

/* Print a number (simple implementation) */
static void print_num(int n) {
    char buf[16];
    int i = 0;
    int neg = 0;
    
    if (n < 0) {
        neg = 1;
        n = -n;
    }
    
    do {
        buf[i++] = '0' + (n % 10);
        n /= 10;
    } while (n > 0);
    
    if (neg) {
        buf[i++] = '-';
    }
    
    /* Reverse and print */
    while (i > 0) {
        char c = buf[--i];
        _write(1, &c, 1);
    }
}

/* Debug syscall to kernel log */
extern void debug_print(const char *msg);

int main(void) {
    print("Hello from C on CLUU!\n");
    print("PID: ");
    print_num(_getpid());
    print("\n");
    print("C runtime is working!\n");
    
    return 0;
}
