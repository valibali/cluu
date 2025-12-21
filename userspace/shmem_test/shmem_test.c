/*
 * Shared Memory Test Program
 *
 * This program tests the shared memory syscalls:
 * - syscall_shmem_create: Create shared memory region
 * - syscall_shmem_map: Map into process address space
 * - syscall_shmem_unmap: Unmap from address space
 * - syscall_shmem_destroy: Destroy region
 */

#include "../lib/syscall.h"

/* Helper to convert number to string */
static void num_to_str(long num, char *buf, int size) {
    int i = 0;
    int is_negative = 0;

    if (num < 0) {
        is_negative = 1;
        num = -num;
    }

    /* Handle 0 case */
    if (num == 0) {
        buf[i++] = '0';
        buf[i] = '\0';
        return;
    }

    /* Convert digits in reverse */
    char temp[32];
    int j = 0;
    while (num > 0 && j < 31) {
        temp[j++] = '0' + (num % 10);
        num /= 10;
    }

    /* Add negative sign if needed */
    if (is_negative && i < size - 1) {
        buf[i++] = '-';
    }

    /* Reverse digits into output buffer */
    while (j > 0 && i < size - 1) {
        buf[i++] = temp[--j];
    }

    buf[i] = '\0';
}

/* Entry point */
int main(int argc, char **argv) {
    char buf[64];

    print("[SHMEM-TEST] Starting shared memory tests...\n");

    /* Test 1: Create shared memory region */
    print("[SHMEM-TEST] Test 1: Creating 4KB shared memory region...\n");
    long shmem_id = syscall_shmem_create(4096, SHMEM_READ | SHMEM_WRITE);

    if (shmem_id < 0) {
        print("[SHMEM-TEST] FAIL: shmem_create returned error ");
        num_to_str(shmem_id, buf, sizeof(buf));
        print(buf);
        print("\n");
        syscall_exit(1);
    }

    print("[SHMEM-TEST] SUCCESS: Created shmem ID ");
    num_to_str(shmem_id, buf, sizeof(buf));
    print(buf);
    print("\n");

    /* Test 2: Map shared memory */
    print("[SHMEM-TEST] Test 2: Mapping shared memory...\n");
    void *addr = syscall_shmem_map(shmem_id, (void *)0, SHMEM_READ | SHMEM_WRITE);

    if ((long)addr < 0) {
        print("[SHMEM-TEST] FAIL: shmem_map returned error ");
        num_to_str((long)addr, buf, sizeof(buf));
        print(buf);
        print("\n");
        syscall_exit(1);
    }

    print("[SHMEM-TEST] SUCCESS: Mapped at address 0x");
    /* Print address in hex */
    unsigned long addr_val = (unsigned long)addr;
    for (int i = 60; i >= 0; i -= 4) {
        int digit = (addr_val >> i) & 0xF;
        buf[0] = digit < 10 ? '0' + digit : 'a' + (digit - 10);
        buf[1] = '\0';
        print(buf);
    }
    print("\n");

    /* Test 3: Write to shared memory */
    print("[SHMEM-TEST] Test 3: Writing to shared memory...\n");
    char *shmem_ptr = (char *)addr;
    const char *test_msg = "Hello from shared memory!";
    int i;
    for (i = 0; test_msg[i] != '\0'; i++) {
        shmem_ptr[i] = test_msg[i];
    }
    shmem_ptr[i] = '\0';

    print("[SHMEM-TEST] SUCCESS: Wrote message to shared memory\n");

    /* Test 4: Read back from shared memory */
    print("[SHMEM-TEST] Test 4: Reading from shared memory...\n");
    print("[SHMEM-TEST] Message: ");
    print(shmem_ptr);
    print("\n");

    /* Test 5: Unmap shared memory */
    print("[SHMEM-TEST] Test 5: Unmapping shared memory...\n");
    int result = syscall_shmem_unmap(addr);

    if (result < 0) {
        print("[SHMEM-TEST] FAIL: shmem_unmap returned error ");
        num_to_str(result, buf, sizeof(buf));
        print(buf);
        print("\n");
        syscall_exit(1);
    }

    print("[SHMEM-TEST] SUCCESS: Unmapped shared memory\n");

    /* Test 6: Destroy shared memory */
    print("[SHMEM-TEST] Test 6: Destroying shared memory region...\n");
    result = syscall_shmem_destroy(shmem_id);

    if (result < 0) {
        print("[SHMEM-TEST] FAIL: shmem_destroy returned error ");
        num_to_str(result, buf, sizeof(buf));
        print(buf);
        print("\n");
        syscall_exit(1);
    }

    print("[SHMEM-TEST] SUCCESS: Destroyed shared memory region\n");

    /* All tests passed */
    print("[SHMEM-TEST] ==================================\n");
    print("[SHMEM-TEST] ALL TESTS PASSED!\n");
    print("[SHMEM-TEST] ==================================\n");

    syscall_exit(0);
    return 0;
}

/* Minimal _start entry point for freestanding binary */
void _start(void) {
    int argc = 0;
    char **argv = (char **)0;

    int ret = main(argc, argv);
    syscall_exit(ret);
}
