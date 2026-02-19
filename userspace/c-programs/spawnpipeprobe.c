/*
 * spawnpipeprobe - test posix_spawn with pipe fd inheritance.
 *
 * Creates a pipe, spawns pipecat with stdin redirected to pipe read-end,
 * writes data through the pipe, closes write-end, waits for child exit.
 */
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <spawn.h>
#include <sys/wait.h>

extern void debug_print(const char *msg);
extern int pipe(int pipefd[2]);

int main(void) {
    debug_print("spawnpipeprobe: main entered");

    /* Step 1: Create a pipe */
    int pfd[2];
    if (pipe(pfd) != 0) {
        debug_print("spawnpipeprobe: FAIL pipe() error");
        printf("spawnpipeprobe: FAIL pipe() error\n");
        return 1;
    }
    debug_print("spawnpipeprobe: pipe created");

    /* Step 2: Set up file actions to redirect child stdin to pipe read-end */
    posix_spawn_file_actions_t fa;
    posix_spawn_file_actions_init(&fa);
    posix_spawn_file_actions_adddup2(&fa, pfd[0], STDIN_FILENO);

    /* Step 3: Spawn pipecat */
    pid_t child_pid;
    char *argv[] = { "pipecat", NULL };
    int rc = posix_spawn(&child_pid, "/bin/pipecat", &fa, NULL,
                         (char *const *)argv, NULL);
    posix_spawn_file_actions_destroy(&fa);

    if (rc != 0) {
        debug_print("spawnpipeprobe: FAIL posix_spawn error");
        printf("spawnpipeprobe: FAIL posix_spawn returned %d\n", rc);
        return 1;
    }
    debug_print("spawnpipeprobe: child spawned");

    /* Close read-end in parent (child has it) */
    close(pfd[0]);

    /* Step 4: Write data through pipe to child */
    const char *msg = "hello from parent";
    ssize_t written = write(pfd[1], msg, strlen(msg));
    if (written != (ssize_t)strlen(msg)) {
        debug_print("spawnpipeprobe: FAIL write error");
        printf("spawnpipeprobe: FAIL write returned %ld\n", (long)written);
        return 1;
    }
    debug_print("spawnpipeprobe: data written to pipe");

    /* Step 5: Close write-end (sends EOF to child) */
    close(pfd[1]);
    debug_print("spawnpipeprobe: write-end closed");

    /* Step 6: Wait for child to exit */
    int status = 0;
    pid_t waited = waitpid(child_pid, &status, 0);
    if (waited < 0) {
        debug_print("spawnpipeprobe: FAIL waitpid error");
        printf("spawnpipeprobe: FAIL waitpid error\n");
        return 1;
    }
    debug_print("spawnpipeprobe: child exited");

    debug_print("spawnpipeprobe: PASS");
    printf("spawnpipeprobe: PASS\n");
    return 0;
}
