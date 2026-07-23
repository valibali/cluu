// DOOM entry point for CLUU.
//
// Calls doomgeneric_Create (engine init + DG_Init) then loops
// doomgeneric_Tick forever. The Rust staticlib (doom-cluu) provides
// the 6 DG_* platform functions.

#include "doomgeneric.h"

int main(int argc, char **argv) {
    doomgeneric_Create(argc, argv);
    while (1) {
        doomgeneric_Tick();
        DG_SleepMs(1000 / 35);
    }
    return 0;
}
