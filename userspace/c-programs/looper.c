/*
 * looper - long-running child for cascading cleanup test.
 *
 * Loops for 30 seconds.  In the cascade test, the parent exits
 * immediately and procmgr's cascading cleanup should kill this
 * process before it finishes.
 */
#include <unistd.h>
#include <sched.h>

extern void debug_print(const char *msg);

int main(void) {
    debug_print("looper: started");
    for (int i = 0; i < 120; i++) {
        usleep(250000);
    }
    debug_print("looper: completed (should not reach here in cascade test)");
    return 0;
}
