/*
 * CLUU C integration test - frame capabilities
 *
 * Tests: FrameAllocate, FrameGetPhys, SpaceMap with MAP_FRAME_TOKEN,
 *        write/read through mapped frame, SpaceUnmap, FrameFree.
 */

#include <stdio.h>
#include <stdint.h>
#include <string.h>

void debug_print(const char *msg);

/* ProcessInfo lives at this fixed address for all processes */
#define PROCESS_INFO_ADDR  0x7fe00100UL
#define TOKEN_SPACE        5

/* InvokeOp numbers */
#define OP_SPACE_MAP       12
#define OP_SPACE_UNMAP     13
#define OP_FRAME_ALLOCATE  70
#define OP_FRAME_FREE      71
#define OP_FRAME_GET_PHYS  72

/* SpaceMap flags */
#define MAP_FRAME_TOKEN    0x400

/* Raw syscall via SYSCALL instruction.
 * Returns negative errno on error, non-negative value on success. */
static inline long raw_syscall6(unsigned long nr,
                                unsigned long a1, unsigned long a2,
                                unsigned long a3, unsigned long a4,
                                unsigned long a5, unsigned long a6)
{
    long ret;
    register unsigned long r10 __asm__("r10") = a4;
    register unsigned long r8  __asm__("r8")  = a5;
    register unsigned long r9  __asm__("r9")  = a6;
    __asm__ volatile (
        "syscall"
        : "=a"(ret)
        : "a"(nr), "D"(a1), "S"(a2), "d"(a3),
          "r"(r10), "r"(r8), "r"(r9)
        : "rcx", "r11", "memory"
    );
    return ret;
}

/* sys_invoke(token, op, arg1, arg2, arg3, arg4) — syscall number 5 */
static inline long invoke(unsigned long token, unsigned long op,
                           unsigned long a1, unsigned long a2,
                           unsigned long a3, unsigned long a4)
{
    return raw_syscall6(5, token, op, a1, a2, a3, a4);
}

static unsigned long get_space_token(void)
{
    /* ProcessInfo layout: exit_token(8) + exit_cookie(8) + pid(8) + tokens[16] */
    const unsigned long *tokens = (const unsigned long *)(PROCESS_INFO_ADDR + 24);
    return tokens[TOKEN_SPACE];
}

static void print_hex(const char *prefix, unsigned long val)
{
    char buf[64];
    const char *hex = "0123456789abcdef";
    int i;

    /* Build "prefix0x<hex>\n" */
    int off = 0;
    for (const char *p = prefix; *p; p++)
        buf[off++] = *p;
    buf[off++] = '0';
    buf[off++] = 'x';
    for (i = 15; i >= 0; i--)
        buf[off++] = hex[(val >> (i * 4)) & 0xF];
    buf[off++] = '\n';
    buf[off] = '\0';
    debug_print(buf);
}

int main(void)
{
    debug_print("=== Frame capability test ===\n");

    unsigned long space_tok = get_space_token();
    print_hex("space_token=", space_tok);

    if (space_tok == 0) {
        debug_print("[FAIL] no space token\n");
        return 1;
    }

    /* 1. FrameAllocate — allocate a physical frame */
    long frame_tok = invoke(space_tok, OP_FRAME_ALLOCATE, 0, 0, 0, 0);
    if (frame_tok < 0) {
        debug_print("[FAIL] FrameAllocate failed\n");
        print_hex("  errno=", (unsigned long)(-frame_tok));
        return 1;
    }
    debug_print("[OK] FrameAllocate\n");
    print_hex("  frame_token=", (unsigned long)frame_tok);

    /* 2. FrameGetPhys — get physical address */
    long phys = invoke((unsigned long)frame_tok, OP_FRAME_GET_PHYS, 0, 0, 0, 0);
    if (phys < 0) {
        debug_print("[FAIL] FrameGetPhys failed\n");
        return 1;
    }
    debug_print("[OK] FrameGetPhys\n");
    print_hex("  phys=", (unsigned long)phys);

    /* 3. SpaceMap with MAP_FRAME_TOKEN — map frame at a free virtual address */
    unsigned long test_vaddr = 0x30000000UL; /* Pick an unused region */
    /* SpaceMap: invoke(space_tok, 12, virt_addr, data_ptr, perms, copy_len_or_frame_tok) */
    /* With MAP_FRAME_TOKEN: perms has 0x400, arg6 = frame token handle */
    long map_ret = invoke(space_tok, OP_SPACE_MAP,
                          test_vaddr,
                          0,                     /* data_ptr ignored */
                          0x02 | MAP_FRAME_TOKEN, /* writable + MAP_FRAME_TOKEN */
                          (unsigned long)frame_tok);
    if (map_ret < 0) {
        debug_print("[FAIL] SpaceMap MAP_FRAME_TOKEN failed\n");
        print_hex("  errno=", (unsigned long)(-map_ret));
        return 1;
    }
    debug_print("[OK] SpaceMap with MAP_FRAME_TOKEN\n");

    /* 4. Write to the mapped page and read back */
    volatile uint64_t *ptr = (volatile uint64_t *)test_vaddr;
    *ptr = 0xDEADBEEFCAFEBABEULL;
    uint64_t readback = *ptr;

    if (readback == 0xDEADBEEFCAFEBABEULL) {
        debug_print("[OK] Write/read through frame\n");
    } else {
        debug_print("[FAIL] Read-back mismatch\n");
        print_hex("  expected=", 0xDEADBEEFCAFEBABEULL);
        print_hex("  got=", readback);
        return 1;
    }

    /* 5. SpaceUnmap — unmap the page */
    long unmap_ret = invoke(space_tok, OP_SPACE_UNMAP, test_vaddr, 1, 0, 0);
    if (unmap_ret < 0) {
        debug_print("[FAIL] SpaceUnmap failed\n");
        return 1;
    }
    debug_print("[OK] SpaceUnmap\n");

    /* 6. FrameFree — free the frame (map_count should be 0 after unmap) */
    long free_ret = invoke((unsigned long)frame_tok, OP_FRAME_FREE, 0, 0, 0, 0);
    if (free_ret < 0) {
        debug_print("[FAIL] FrameFree failed\n");
        print_hex("  errno=", (unsigned long)(-free_ret));
        return 1;
    }
    debug_print("[OK] FrameFree\n");

    debug_print("=== Frame capability test PASSED ===\n");
    return 0;
}
