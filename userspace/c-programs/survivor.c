/*
 * survivor - detached container survival test.
 *
 * Spawned with DETACH by detachprobe.  After the parent exits,
 * this process should NOT be killed by cascading cleanup because
 * it is detached (parent_container_id=0).  Waits briefly, then
 * prints PASS to confirm survival.
 */
#include <unistd.h>
#include <sched.h>

extern void debug_print(const char *msg);

int main(void) {
    debug_print("survivor: started");
    /* Wait for parent to exit and cascade to (not) fire */
    for (int i = 0; i < 8; i++) {
        usleep(250000);
    }
    debug_print("survivor: PASS survived parent exit");
    return 0;
}
