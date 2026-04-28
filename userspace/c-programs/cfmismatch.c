#include <stdio.h>

extern void debug_print(const char *msg);

int main(void) {
    /* If we ever reach here, strict Cluufile validation is broken — the
     * probe's Cluufile demands MOUNT /etc readwrite, which the user
     * envelope provides only as ro. Procmgr must reject the spawn before
     * main() runs. */
    debug_print("cfmismatch: ERROR: should not have started");
    return 0;
}
