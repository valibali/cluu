#include <errno.h>
#include <stdio.h>
#include <string.h>

#ifndef MAP_FAILED
#define MAP_FAILED ((void *)-1)
#endif

#ifndef MAP_PRIVATE
#define MAP_PRIVATE 0x02
#endif

#ifndef MAP_ANONYMOUS
#define MAP_ANONYMOUS 0x20
#endif

#ifndef PROT_READ
#define PROT_READ 0x1
#endif

#ifndef PROT_WRITE
#define PROT_WRITE 0x2
#endif

extern void *mmap(void *addr, unsigned long len, int prot, int flags, int fd, long off);
extern int munmap(void *addr, unsigned long len);
extern int mprotect(void *addr, unsigned long len, int prot);

extern void debug_print(const char *msg);

static int fail(const char *msg, int code) {
    debug_print(msg);
    printf("%s errno=%d (%s)\n", msg, errno, strerror(errno));
    return code;
}

int main(void) {
    const size_t page = 4096;
    const size_t len = page * 2;

    void *region = mmap(NULL, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (region == MAP_FAILED) {
        return fail("mmapprobe: FAIL mmap initial", 1);
    }

    unsigned char *p = (unsigned char *)region;
    p[0] = 0xAA;
    p[page] = 0x55;
    debug_print("mmapprobe: PASS basic map/write");
    printf("mmapprobe: PASS basic map/write addr=%p\n", region);

    if (mprotect(region, len, PROT_READ) != 0) {
        return fail("mmapprobe: FAIL mprotect exact", 2);
    }

    if (mprotect((unsigned char *)region + page, page, PROT_READ) == 0) {
        debug_print("mmapprobe: FAIL mprotect partial unexpectedly succeeded");
        printf("mmapprobe: FAIL mprotect partial unexpectedly succeeded\n");
        return 3;
    }
    debug_print("mmapprobe: PASS mprotect exact");
    printf("mmapprobe: PASS mprotect exact addr=%p len=%zu\n", region, len);

    if (munmap(region, len) != 0) {
        return fail("mmapprobe: FAIL munmap initial", 4);
    }

    void *a = mmap(NULL, page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (a == MAP_FAILED) {
        return fail("mmapprobe: FAIL mmap a", 5);
    }
    void *b = mmap(NULL, page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (b == MAP_FAILED) {
        return fail("mmapprobe: FAIL mmap b", 6);
    }
    if (munmap(a, page) != 0) {
        return fail("mmapprobe: FAIL munmap a", 7);
    }
    void *c = mmap(NULL, page, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (c == MAP_FAILED) {
        return fail("mmapprobe: FAIL mmap c", 8);
    }
    if (c != a) {
        debug_print("mmapprobe: FAIL first-fit hole reuse");
        printf("mmapprobe: FAIL first-fit hole reuse a=%p c=%p\n", a, c);
        return 9;
    }
    debug_print("mmapprobe: PASS reuse hole");
    printf("mmapprobe: PASS reuse hole a=%p c=%p\n", a, c);

    if (munmap(b, page) != 0) {
        return fail("mmapprobe: FAIL munmap b", 10);
    }
    if (munmap(c, page) != 0) {
        return fail("mmapprobe: FAIL munmap c", 11);
    }

    debug_print("mmapprobe: PASS complete");
    printf("mmapprobe: PASS complete\n");
    return 0;
}
