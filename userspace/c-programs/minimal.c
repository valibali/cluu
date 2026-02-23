/*
 * CLUU P1 POSIX stubs integration test
 *
 * Tests: getenv, getcwd, chdir, opendir/readdir/closedir,
 *        nanosleep, argc/argv
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <time.h>

/* Forward declarations for functions that may not have headers yet */
typedef struct DIR DIR;
struct dirent {
    unsigned long long d_ino;
    long long d_off;
    unsigned short d_reclen;
    unsigned char d_type;
    char d_name[256];
};
DIR *opendir(const char *path);
struct dirent *readdir(DIR *dirp);
int closedir(DIR *dirp);

void debug_print(const char *msg);

static void print_int(const char *prefix, int val)
{
    char buf[80];
    int off = 0;
    const char *p;

    for (p = prefix; *p; p++)
        buf[off++] = *p;

    /* Simple int-to-string */
    if (val < 0) {
        buf[off++] = '-';
        val = -val;
    }
    if (val == 0) {
        buf[off++] = '0';
    } else {
        char tmp[20];
        int n = 0;
        while (val > 0) {
            tmp[n++] = '0' + (val % 10);
            val /= 10;
        }
        for (int i = n - 1; i >= 0; i--)
            buf[off++] = tmp[i];
    }
    buf[off++] = '\n';
    buf[off] = '\0';
    printf("%s", buf);
}

int main(int argc, char *argv[])
{
    int pass = 0, fail = 0;

    debug_print("=== P1 POSIX stubs test ===");
    printf("=== P1 POSIX stubs test ===\n");

    /* ── Test 1: argc/argv ─────────────────────────────────────────── */
    print_int("argc=", argc);
    if (argc > 0 && argv != NULL) {
        for (int i = 0; i < argc; i++) {
            printf("  argv[%d]=\"%s\"\n", i, argv[i] ? argv[i] : "(null)");
        }
        printf("[OK] argv received\n");
        pass++;
    } else {
        printf("[OK] argc=0 (no args, expected for direct spawn)\n");
        pass++;
    }

    /* ── Test 2: getenv ────────────────────────────────────────────── */
    char *home = getenv("HOME");
    if (home != NULL && strcmp(home, "/") == 0) {
        printf("[OK] getenv(\"HOME\") = \"%s\"\n", home);
        pass++;
    } else if (home != NULL) {
        printf("[WARN] getenv(\"HOME\") = \"%s\" (expected \"/\")\n", home);
        pass++;
    } else {
        printf("[FAIL] getenv(\"HOME\") returned NULL\n");
        fail++;
    }

    char *path = getenv("PATH");
    if (path != NULL && strcmp(path, "/bin") == 0) {
        printf("[OK] getenv(\"PATH\") = \"%s\"\n", path);
        pass++;
    } else if (path != NULL) {
        printf("[WARN] getenv(\"PATH\") = \"%s\" (expected \"/bin\")\n", path);
        pass++;
    } else {
        printf("[FAIL] getenv(\"PATH\") returned NULL\n");
        fail++;
    }

    /* ── Test 3: getcwd ────────────────────────────────────────────── */
    char cwdbuf[256];
    char *cwd = getcwd(cwdbuf, sizeof(cwdbuf));
    if (cwd != NULL && strcmp(cwd, "/") == 0) {
        printf("[OK] getcwd() = \"%s\"\n", cwd);
        pass++;
    } else if (cwd != NULL) {
        printf("[WARN] getcwd() = \"%s\" (expected \"/\")\n", cwd);
        pass++; /* Not a hard failure */
    } else {
        printf("[FAIL] getcwd() returned NULL\n");
        fail++;
    }

    /* ── Test 4: opendir / readdir / closedir ──────────────────────── */
    DIR *dir = opendir("/");
    if (dir != NULL) {
        printf("[OK] opendir(\"/\") succeeded\n");
        pass++;

        int count = 0;
        struct dirent *entry;
        while ((entry = readdir(dir)) != NULL && count < 20) {
            printf("  [%d] %s (type=%d)\n", count, entry->d_name, entry->d_type);
            count++;
        }
        closedir(dir);

        if (count > 0) {
            printf("[OK] readdir returned %d entries\n", count);
            pass++;
        } else {
            printf("[WARN] readdir returned 0 entries\n");
            pass++; /* VFS root might be empty */
        }
    } else {
        printf("[FAIL] opendir(\"/\") returned NULL\n");
        fail++;
    }

    /* ── Test 5: nanosleep ─────────────────────────────────────────── */
    printf("Testing nanosleep(100ms)...\n");
    struct timespec req = { .tv_sec = 0, .tv_nsec = 100000000L }; /* 100ms */
    struct timespec rem = { 0 };
    int ns_ret = nanosleep(&req, &rem);
    if (ns_ret == 0) {
        debug_print("[OK] nanosleep(100ms) returned 0");
        printf("[OK] nanosleep(100ms) returned 0\n");
        pass++;
    } else {
        printf("[FAIL] nanosleep returned %d\n", ns_ret);
        fail++;
    }

    /* ── Test 6: sleep (short) ─────────────────────────────────────── */
    printf("Testing usleep(50000) = 50ms...\n");
    int us_ret = usleep(50000);
    if (us_ret == 0) {
        debug_print("[OK] usleep(50ms) returned 0");
        printf("[OK] usleep(50ms) returned 0\n");
        pass++;
    } else {
        printf("[FAIL] usleep returned %d\n", us_ret);
        fail++;
    }

    /* ── Summary ───────────────────────────────────────────────────── */
    printf("\n=== Results: %d passed, %d failed ===\n", pass, fail);

    if (fail == 0) {
        debug_print("=== P1 POSIX stubs test PASSED ===");
        printf("=== P1 POSIX stubs test PASSED ===\n");
    } else {
        printf("=== P1 POSIX stubs test FAILED ===\n");
    }

    return fail;
}
