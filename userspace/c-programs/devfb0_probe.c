#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#ifndef MAP_FAILED
#define MAP_FAILED ((void *)-1)
#endif

#ifndef MAP_SHARED
#define MAP_SHARED 0x01
#endif

#ifndef PROT_READ
#define PROT_READ 0x1
#endif

#ifndef PROT_WRITE
#define PROT_WRITE 0x2
#endif

extern void *mmap(void *addr, size_t len, int prot, int flags, int fd, long off);

extern void debug_print(const char *msg);

int main(void) {
    int fd = open("/dev/fb0", O_RDWR);
    if (fd < 0) {
        debug_print("DEVFB0: FAIL open");
        return 1;
    }

    uint8_t hdr[40] = {0};
    ssize_t n = read(fd, hdr, sizeof hdr);
    if (n != (ssize_t)sizeof hdr) {
        debug_print("DEVFB0: FAIL short read");
        close(fd);
        return 2;
    }

    uint32_t magic = ((uint32_t*)hdr)[0];
    uint32_t w     = ((uint32_t*)hdr)[1];
    uint32_t h     = ((uint32_t*)hdr)[2];
    uint32_t pitch = ((uint32_t*)hdr)[3];
    uint32_t bpp   = ((uint32_t*)hdr)[4];
    uint64_t size  = *(uint64_t*)(hdr + 24);
    uint64_t phys  = *(uint64_t*)(hdr + 32);

    if (magic != 0x46424630u) {
        debug_print("DEVFB0: FAIL magic");
        close(fd);
        return 3;
    }
    if (w == 0 || h == 0 || pitch == 0 || bpp == 0 || size == 0 || phys == 0) {
        debug_print("DEVFB0: FAIL geom");
        close(fd);
        return 4;
    }
    printf("DEVFB0: geom %ux%u pitch=%u bpp=%u size=%llu phys=%llx\n",
           w, h, pitch, bpp,
           (unsigned long long)size, (unsigned long long)phys);

    void *mapped = mmap(NULL, (size_t)size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (mapped == MAP_FAILED) {
        debug_print("DEVFB0: FAIL mmap");
        close(fd);
        return 5;
    }

    /* Write a couple of cells. Don't read back -- WC reads aren't reliable
     * for this kind of correctness check. The PASS marker covers
     * "open + read header + mmap + write didn't fault". */
    volatile uint32_t *fb = (volatile uint32_t*)mapped;
    fb[0] = 0xCAFEBABEu;
    fb[1] = 0xDEADBEEFu;

    debug_print("DEVFB0: PASS");
    close(fd);
    return 0;
}
