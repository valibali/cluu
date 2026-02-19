#include <stdio.h>
#include <unistd.h>

void debug_print(const char *msg);

int main(void) {
    debug_print("sleepy: started");
    // Long enough to let harness inject Ctrl-C during foreground spawn wait.
    for (int i = 0; i < 20; i++) {
        usleep(250000);
    }
    debug_print("sleepy: completed");
    return 0;
}
