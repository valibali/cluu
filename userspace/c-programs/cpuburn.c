/*
 * cpuburn - CPU + IPC load generator for stress testing.
 *
 * Modes:
 *   cpuburn cpu <iters>      — pure CPU burn (compute hash loop)
 *   cpuburn ipc <iters>      — IPC flood (getpid round-trips)
 *   cpuburn mixed <iters>    — alternating CPU + IPC
 *
 * Prints: cpuburn: PASS mode=<mode> iters=<n> ops=<total> avg_cycles=<avg>
 */
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <sys/syscall.h>

extern void debug_print(const char *msg);
extern int getpid(void);

static uint64_t rdtsc(void) {
    unsigned int lo, hi;
    __asm__ __volatile__("rdtsc" : "=a"(lo), "=d"(hi));
    return ((uint64_t)hi << 32) | lo;
}

static uint64_t cpu_burn(uint64_t iters) {
    volatile uint64_t acc = 0x1234567890ABCDEFULL;
    for (uint64_t i = 0; i < iters; i++) {
        acc ^= i * 0x9E3779B97F4A7C15ULL;
        acc = (acc << 17) | (acc >> 47);
        acc ^= acc >> 31;
        acc *= 0xBF58476D1CE4E5B9ULL;
    }
    return acc;
}

static int run_cpu(int iters) {
    uint64_t burn_iters = (uint64_t)iters * 100000ULL;
    uint64_t t0 = rdtsc();
    volatile uint64_t sink = cpu_burn(burn_iters);
    uint64_t t1 = rdtsc();
    (void)sink;
    uint64_t elapsed = t1 - t0;
    char buf[128];
    snprintf(buf, sizeof(buf), "cpuburn: PASS mode=cpu iters=%d ops=%llu avg_cycles=%llu",
             iters, (unsigned long long)burn_iters,
             (unsigned long long)(elapsed / (burn_iters > 0 ? burn_iters : 1)));
    debug_print(buf);
    printf("%s\n", buf);
    return 0;
}

static int run_ipc(int iters) {
    uint64_t total_cycles = 0;
    int ok = 0;
    for (int i = 0; i < iters; i++) {
        uint64_t t0 = rdtsc();
        int pid = getpid();
        uint64_t t1 = rdtsc();
        if (pid > 0) {
            ok++;
        }
        total_cycles += (t1 > t0) ? (t1 - t0) : 0;
    }
    char buf[128];
    snprintf(buf, sizeof(buf), "cpuburn: PASS mode=ipc iters=%d ops=%d avg_cycles=%llu",
             iters, ok,
             (unsigned long long)(ok > 0 ? total_cycles / ok : 0));
    debug_print(buf);
    printf("%s\n", buf);
    return 0;
}

static int run_mixed(int iters) {
    int ok = 0;
    uint64_t total_cycles = 0;
    uint64_t total_ops = 0;
    for (int i = 0; i < iters; i++) {
        uint64_t t0 = rdtsc();
        volatile uint64_t sink = cpu_burn(10000);
        int pid = getpid();
        uint64_t t1 = rdtsc();
        (void)sink;
        if (pid > 0) {
            ok++;
            total_ops += 10001;
        }
        total_cycles += (t1 > t0) ? (t1 - t0) : 0;
    }
    char buf[128];
    snprintf(buf, sizeof(buf), "cpuburn: PASS mode=mixed iters=%d ops=%llu avg_cycles=%llu",
             iters, (unsigned long long)total_ops,
             (unsigned long long)(ok > 0 ? total_cycles / ok : 0));
    debug_print(buf);
    printf("%s\n", buf);
    return 0;
}

int main(int argc, char *argv[]) {
    if (argc < 2) {
        fprintf(stderr, "usage: cpuburn <cpu|ipc|mixed> [iters]\n");
        return 1;
    }
    int iters = 100;
    if (argc >= 3) {
        iters = atoi(argv[2]);
        if (iters <= 0) iters = 100;
    }
    if (strcmp(argv[1], "cpu") == 0) return run_cpu(iters);
    if (strcmp(argv[1], "ipc") == 0) return run_ipc(iters);
    if (strcmp(argv[1], "mixed") == 0) return run_mixed(iters);
    fprintf(stderr, "unknown mode: %s\n", argv[1]);
    return 1;
}
