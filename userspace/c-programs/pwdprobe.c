#include <stdio.h>
#include <string.h>
#include <unistd.h>

extern void debug_print(const char *msg);

int main(void) {
    char buf[1024];
    if (getcwd(buf, sizeof buf) == NULL) {
        debug_print("pwdprobe: FAIL getcwd returned NULL");
        printf("pwdprobe: FAIL getcwd returned NULL\n");
        fflush(stdout);
        return 1;
    }

    char line[1040];
    snprintf(line, sizeof line, "pwdprobe: cwd=%s", buf);
    debug_print(line);
    printf("%s\n", line);
    fflush(stdout);
    return 0;
}
