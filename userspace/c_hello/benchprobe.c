#include <errno.h>
#include <signal.h>
#include <spawn.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

extern void debug_print(const char *msg);

static void log_line(const char *line) {
    debug_print(line);
    printf("%s\n", line);
}

static void logf_u64(const char *prefix, uint64_t a, uint64_t b, uint64_t c, uint64_t d, uint64_t e) {
    char buf[192];
    int n = snprintf(
        buf,
        sizeof(buf),
        "%s %llu %llu %llu %llu %llu",
        prefix,
        (unsigned long long)a,
        (unsigned long long)b,
        (unsigned long long)c,
        (unsigned long long)d,
        (unsigned long long)e);
    if (n > 0) {
        debug_print(buf);
    }
}

static inline uint64_t rdtsc_now(void) {
    uint32_t lo = 0;
    uint32_t hi = 0;
    __asm__ volatile("rdtsc" : "=a"(lo), "=d"(hi));
    return ((uint64_t)hi << 32) | (uint64_t)lo;
}

static int cmp_u64(const void *a, const void *b) {
    const uint64_t va = *(const uint64_t *)a;
    const uint64_t vb = *(const uint64_t *)b;
    if (va < vb) {
        return -1;
    }
    if (va > vb) {
        return 1;
    }
    return 0;
}

static uint64_t percentile_u64(uint64_t *samples, int n, int p) {
    if (n <= 0) {
        return 0;
    }
    if (p <= 0) {
        return samples[0];
    }
    if (p >= 100) {
        return samples[n - 1];
    }
    int idx = (n * p) / 100;
    if (idx >= n) {
        idx = n - 1;
    }
    return samples[idx];
}

static int bench_ipc_waitpoll(int iters) {
    uint64_t *samples = NULL;
    uint64_t total = 0;

    if (iters < 64) {
        iters = 64;
    }

    samples = (uint64_t *)malloc((size_t)iters * sizeof(uint64_t));
    if (samples == NULL) {
        debug_print("benchprobe: FAIL alloc ipc samples");
        return -1;
    }

    for (int i = 0; i < iters; i++) {
        int status = 0;
        uint64_t t0 = rdtsc_now();
        pid_t rc = waitpid(-1, &status, WNOHANG);
        uint64_t t1 = rdtsc_now();
        if (!(rc == 0 || rc == -1)) {
            free(samples);
            debug_print("benchprobe: FAIL ipc waitpoll");
            return -1;
        }
        samples[i] = (t1 > t0) ? (t1 - t0) : 0;
        total += samples[i];
    }

    qsort(samples, (size_t)iters, sizeof(uint64_t), cmp_u64);
    uint64_t p50 = percentile_u64(samples, iters, 50);
    uint64_t p95 = percentile_u64(samples, iters, 95);
    uint64_t p99 = percentile_u64(samples, iters, 99);
    uint64_t avg = (iters > 0) ? (total / (uint64_t)iters) : 0;

    logf_u64(
        "benchprobe: ipc_waitpoll iters/avg/p50/p95/p99",
        (uint64_t)iters,
        avg,
        p50,
        p95,
        p99);
    printf(
        "benchprobe: ipc_waitpoll iters=%d avg_cycles=%llu p50_cycles=%llu p95_cycles=%llu p99_cycles=%llu\n",
        iters,
        (unsigned long long)avg,
        (unsigned long long)p50,
        (unsigned long long)p95,
        (unsigned long long)p99);
    debug_print("benchprobe: ipc_clock");

    free(samples);
    return 0;
}

static int bench_spawn_wait(int iters) {
    uint64_t start = 0;
    uint64_t end = 0;
    int ok = 0;

    if (iters < 8) {
        iters = 8;
    }

    start = rdtsc_now();
    for (int i = 0; i < iters; i++) {
        pid_t child = 0;
        char *argv[] = {(char *)"noop", NULL};
        int rc = posix_spawn(&child, "/bin/noop", NULL, NULL, argv, NULL);
        if (rc != 0) {
            printf("benchprobe: spawn failed iter=%d rc=%d\n", i, rc);
            break;
        }
        int status = -1;
        pid_t done = waitpid(child, &status, 0);
        if (done != child || status != 0) {
            printf(
                "benchprobe: wait failed iter=%d pid=%d done=%d status=%d\n",
                i,
                (int)child,
                (int)done,
                status);
            break;
        }
        ok++;
    }
    end = rdtsc_now();

    uint64_t wall = (end > start) ? (end - start) : 1;
    uint64_t avg = (ok > 0) ? (wall / (uint64_t)ok) : 0;

    logf_u64(
        "benchprobe: spawn_wait iters/ok/avg",
        (uint64_t)iters,
        (uint64_t)ok,
        avg,
        0,
        0);
    printf("benchprobe: spawn_wait iters=%d ok=%d avg_cycles=%llu\n", iters, ok, (unsigned long long)avg);
    debug_print("benchprobe: spawn_wait");

    return (ok == iters) ? 0 : -1;
}

static int bench_thread_control(int loops) {
    pid_t child = 0;
    char *argv[] = {(char *)"sleepy", NULL};
    uint64_t start = 0;
    uint64_t end = 0;
    int ok = 0;

    if (loops < 16) {
        loops = 16;
    }

    int rc = posix_spawn(&child, "/bin/sleepy", NULL, NULL, argv, NULL);
    if (rc != 0) {
        printf("benchprobe: sleepy spawn failed rc=%d\n", rc);
        return -1;
    }

    int mode = 0; // 0=sigstop/sigcont, 1=kill0, 2=getpid
    if (kill(child, SIGSTOP) != 0 || kill(child, SIGCONT) != 0) {
        mode = 1;
        if (kill(child, 0) != 0) {
            mode = 2;
            printf("benchprobe: thread_ctl fallback getpid errno=%d\n", errno);
        } else {
            printf("benchprobe: thread_ctl fallback kill0 errno=%d\n", errno);
        }
    }

    start = rdtsc_now();
    if (mode == 0) {
        for (int i = 0; i < loops; i++) {
            if (kill(child, SIGSTOP) != 0) {
                printf("benchprobe: SIGSTOP failed iter=%d errno=%d\n", i, errno);
                break;
            }
            if (kill(child, SIGCONT) != 0) {
                printf("benchprobe: SIGCONT failed iter=%d errno=%d\n", i, errno);
                break;
            }
            ok++;
        }
    } else if (mode == 1) {
        for (int i = 0; i < loops; i++) {
            if (kill(child, 0) != 0) {
                printf("benchprobe: kill0 failed iter=%d errno=%d\n", i, errno);
                break;
            }
            ok++;
        }
    } else {
        for (int i = 0; i < loops; i++) {
            if (getpid() <= 0) {
                printf("benchprobe: getpid failed iter=%d errno=%d\n", i, errno);
                break;
            }
            ok++;
        }
    }
    end = rdtsc_now();

    (void)kill(child, SIGINT);
    int status = -1;
    (void)waitpid(child, &status, 0);

    uint64_t wall = (end > start) ? (end - start) : 1;
    uint64_t ops = (mode == 0) ? ((uint64_t)ok * 2ull) : (uint64_t)ok;
    uint64_t avg = (ops > 0) ? (wall / ops) : 0;

    logf_u64(
        mode == 0 ? "benchprobe: thread_ctl sigstop_cont loops/ok/ops/avg"
                  : (mode == 1 ? "benchprobe: thread_ctl kill0 loops/ok/ops/avg"
                               : "benchprobe: thread_ctl getpid loops/ok/ops/avg"),
        (uint64_t)loops,
        (uint64_t)ok,
        ops,
        avg,
        0);
    printf(
        "benchprobe: thread_ctl mode=%s loops=%d ok=%d ops=%llu avg_cycles=%llu\n",
        mode == 0 ? "sigstop_cont" : (mode == 1 ? "kill0" : "getpid"),
        loops,
        ok,
        (unsigned long long)ops,
        (unsigned long long)avg);
    debug_print("benchprobe: thread_ctl");

    return (ok == loops) ? 0 : -1;
}

int main(int argc, char **argv) {
    int ipc_iters = 256;
    int spawn_iters = 32;
    int thread_loops = 256;
    int spawn_only = 0;

    if (argc >= 2) {
        if (strcmp(argv[1], "spawnonly") == 0) {
            spawn_only = 1;
        } else {
            ipc_iters = atoi(argv[1]);
            if (argc >= 3) {
                spawn_iters = atoi(argv[2]);
            }
            if (argc >= 4) {
                thread_loops = atoi(argv[3]);
            }
        }
    }

    log_line("benchprobe: start");
    printf("benchprobe: start ipc_iters=%d spawn_iters=%d thread_loops=%d\n", ipc_iters, spawn_iters, thread_loops);

    int rc1 = bench_ipc_waitpoll(ipc_iters);
    int rc2 = bench_spawn_wait(spawn_iters);
    int rc3 = spawn_only ? 0 : bench_thread_control(thread_loops);

    if (rc1 == 0 && rc2 == 0 && rc3 == 0) {
        log_line("benchprobe: PASS");
        printf("benchprobe: PASS\n");
        return 0;
    }

    log_line("benchprobe: FAIL");
    printf("benchprobe: FAIL rc_ipc=%d rc_spawn=%d rc_thread=%d\n", rc1, rc2, rc3);
    return 1;
}
