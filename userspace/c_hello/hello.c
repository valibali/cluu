/*
 * CLUU C integration test
 *
 * Verifies newlib headers, stdio, malloc, and VFS-backed file I/O.
 */

#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int cluu_take_rbx_change(uint64_t *before, uint64_t *after);
void debug_print(const char *msg);

static void write_str(const char *s) {
    (void)write(STDOUT_FILENO, s, strlen(s));
}

static void write_hex_u64(uint64_t value) {
    char buf[19];
    buf[0] = '0';
    buf[1] = 'x';
    for (int i = 0; i < 16; i++) {
        uint8_t nib = (value >> ((15 - i) * 4)) & 0xF;
        buf[2 + i] = (nib < 10) ? ('0' + nib) : ('a' + (nib - 10));
    }
    buf[18] = '\n';
    (void)write(STDOUT_FILENO, buf, sizeof(buf));
}

static void report_rbx_change(void) {
    uint64_t before = 0;
    uint64_t after = 0;
    if (cluu_take_rbx_change(&before, &after)) {
        debug_print("[RBX] syscall_raw changed RBX: before=");
        write_hex_u64(before);
        debug_print("[RBX] syscall_raw changed RBX: after=");
        write_hex_u64(after);
    }
}

static inline uint64_t read_rbx(void) {
    uint64_t value;
    __asm__ volatile("mov %%rbx, %0" : "=r"(value));
    return value;
}

static inline void write_rbx(uint64_t value) {
    __asm__ volatile("mov %0, %%rbx" : : "r"(value) : "rbx");
}

static int check_rbx_preserved(const char *label, void (*action)(void)) {
    const uint64_t sentinel = 0x1122334455667788ULL;
    write_rbx(sentinel);
    action();
    uint64_t after = read_rbx();
    if (after != sentinel) {
        debug_print("[FAIL] RBX changed\n");
        debug_print("  before=");
        write_hex_u64(sentinel);
        debug_print("  after =");
        write_hex_u64(after);
        report_rbx_change();
        return -1;
    }
    debug_print("[OK] RBX preserved\n");
    report_rbx_change();
    return 0;
}

static void action_write_syscall(void) {
    const char msg = '.';
    (void)write(STDOUT_FILENO, &msg, 1);
}

static void action_printf_buffered(void) {
    setvbuf(stdout, NULL, _IOFBF, 256);
    printf("printf buffered path: %d %s\n", 42, "hello");
    fflush(stdout);
}

static void action_snprintf_local(void) {
    char local[128];
    int len = snprintf(local, sizeof(local), "snprintf local path: %d %s\n", 7, "ok");
    if (len > 0) {
        (void)write(STDOUT_FILENO, local, (size_t)len);
    }
}

static void run_rbx_tests(void) {
    int failed = 0;
    failed |= check_rbx_preserved("syscall write", action_write_syscall);
    failed |= check_rbx_preserved("printf buffered", action_printf_buffered);
    failed |= check_rbx_preserved("snprintf local", action_snprintf_local);
    if (failed) {
        printf("[RBX TEST] FAILED\n");
    } else {
        printf("[RBX TEST] PASSED\n");
    }
}

static void dump_file(const char *path) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        printf("open('%s') failed\n", path);
        return;
    }

    char buf[128];
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    if (n > 0) {
        buf[n] = '\0';
        printf("read('%s'): %s\n", path, buf);
    } else {
        printf("read('%s') returned %ld\n", path, (long)n);
    }
    close(fd);
}

int main(int argc, char **argv) {
    (void)argc;
    (void)argv;

    debug_print("Hello from C on CLUU!\n");
    run_rbx_tests();
    // printf("PID: %d\n", getpid());

    // char *buf = malloc(64);
    // if (buf) {
    //     strcpy(buf, "malloc/free OK");
    //     printf("%s\n", buf);
    //     free(buf);
    // }

    // struct stat st;
    // if (stat("/dev/initrd/bin/hello", &st) == 0) {
    //     printf("stat: size=%ld mode=%o\n", (long)st.st_size, st.st_mode);
    // } else {
    //     printf("stat failed\n");
    // }

    // dump_file("/dev/initrd/bin/hello");

    // int wfd = open("/proc/uptime", O_WRONLY);
    // if (wfd >= 0) {
    //     const char *msg = "write-test\n";
    //     ssize_t written = write(wfd, msg, strlen(msg));
    //     printf("write('/proc/uptime') -> %ld\n", (long)written);
    //     close(wfd);
    // } else {
    //     printf("open('/proc/uptime', O_WRONLY) failed\n");
    // }

    // usleep(5000);
    // printf("sleep/usleep OK\n");
    return 0;
}
