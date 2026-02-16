/*
 * pipecat - reads stdin until EOF, writes to stdout.
 *
 * Used by spawnpipeprobe to verify cross-process pipe fd inheritance.
 */
#include <unistd.h>

extern void debug_print(const char *msg);

int main(void) {
    debug_print("pipecat: main entered");
    char buf[256];
    ssize_t n;
    while ((n = read(STDIN_FILENO, buf, sizeof(buf))) > 0) {
        write(STDOUT_FILENO, buf, n);
    }
    debug_print("pipecat: done");
    return 0;
}
