#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

extern void debug_print(const char *msg);

#define PROT_READ   0x1
#define PROT_WRITE  0x2
#define MAP_SHARED  0x01
#define MAP_FAILED  ((void *)-1)
extern void *mmap(void *addr, size_t length, int prot, int flags, int fd, long offset);

int main(void) {
    int fd = open("/dev/fb0", O_RDWR);
    if (fd < 0) {
        debug_print("fbprobe: FAIL open");
        return 1;
    }

    uint8_t hdr[40] = {0};
    ssize_t n = read(fd, hdr, sizeof hdr);
    if (n != (ssize_t)sizeof hdr) {
        debug_print("fbprobe: FAIL short read");
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
        debug_print("fbprobe: FAIL magic");
        close(fd);
        return 3;
    }
    if (w == 0 || h == 0 || pitch == 0 || bpp == 0 || size == 0 || phys == 0) {
        debug_print("fbprobe: FAIL params zero");
        close(fd);
        return 4;
    }

    void *mapped = mmap(NULL, (size_t)size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (mapped == MAP_FAILED) {
        debug_print("fbprobe: FAIL mmap");
        close(fd);
        return 5;
    }

    /* Write a couple of cells to prove the mapping is writable.
     * Don't read back — WC-mapped fb reads are unreliable. */
    volatile uint32_t *fb = (volatile uint32_t*)mapped;
    fb[0] = 0xCAFEBABEu;
    fb[1] = 0xDEADBEEFu;

    debug_print("fbprobe: PASS");
    close(fd);
    return 0;
}
