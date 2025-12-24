/*
 * Compiler Builtins for Userspace Programs
 *
 * Provides memcpy, memset, memcmp and other compiler intrinsics
 * required for no_std Rust programs.
 */

void *memcpy(void *dest, const void *src, unsigned long n) {
    unsigned char *d = dest;
    const unsigned char *s = src;
    while (n--) {
        *d++ = *s++;
    }
    return dest;
}

void *memset(void *dest, int c, unsigned long n) {
    unsigned char *d = dest;
    unsigned char byte = (unsigned char)c;
    while (n--) {
        *d++ = byte;
    }
    return dest;
}

int memcmp(const void *s1, const void *s2, unsigned long n) {
    const unsigned char *a = s1;
    const unsigned char *b = s2;
    while (n--) {
        if (*a != *b) {
            return *a - *b;
        }
        a++;
        b++;
    }
    return 0;
}

void rust_eh_personality(void) {
    /* Empty - we don't support unwinding */
}
