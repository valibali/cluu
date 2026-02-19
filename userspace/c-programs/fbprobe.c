#include <stdint.h>
#include <stdio.h>

extern void debug_print(const char *msg);

struct framebuffer_info {
    uint8_t *base;
    uint64_t phys;
    uint32_t size;
    uint32_t width;
    uint32_t height;
    uint32_t pitch;
    uint32_t bpp;
};

int framebuffer_acquire(struct framebuffer_info *info);
int framebuffer_release(const struct framebuffer_info *info);

int main(void) {
    struct framebuffer_info fb;

    int rc = framebuffer_acquire(&fb);
    if (rc != 0) {
        debug_print("fbprobe: FAIL acquire");
        return 1;
    }

    if (fb.base == 0 || fb.width == 0 || fb.height == 0 || fb.pitch == 0) {
        debug_print("fbprobe: FAIL params zero");
        return 2;
    }

    /* Write a 16x16 red square at top-left corner */
    uint32_t red = 0x00FF0000; /* BGRA: blue=0, green=0, red=FF, alpha=0 */
    for (int y = 0; y < 16 && y < (int)fb.height; y++) {
        uint32_t *row = (uint32_t *)(fb.base + y * fb.pitch);
        for (int x = 0; x < 16 && x < (int)fb.width; x++) {
            row[x] = red;
        }
    }

    /* Read back and verify the first pixel is red */
    uint32_t *pixel = (uint32_t *)fb.base;
    if (*pixel != red) {
        debug_print("fbprobe: FAIL readback");
        return 3;
    }

    /* Release the framebuffer mapping before exit to avoid kernel teardown
       issues with device-mapped pages. */
    framebuffer_release(&fb);

    debug_print("fbprobe: PASS");
    printf("fbprobe: PASS w=%u h=%u pitch=%u bpp=%u\n",
           fb.width, fb.height, fb.pitch, fb.bpp);
    return 0;
}
