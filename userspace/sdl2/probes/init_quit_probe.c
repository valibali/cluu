/* T14 stopping-condition probe: SDL_Init(SDL_INIT_VIDEO) -> SDL_Quit() × 100.
 *
 * Compiled and linked against the static libSDL2.a built in this directory.
 * Runs on the host (the build machine) under the CLUU userspace loader so we
 * can actually execute it; the constructor in cluu_init.c registers
 * SDL_SetMainReady() before main() runs, so SDL_Init() should succeed.
 *
 * Exit code 0 = pass (all 100 cycles returned 0 from SDL_Init and SDL_Quit
 * ran each cycle). Non-zero = fail (SDL_Init returned non-zero or SDL_Quit
 * was skipped). */

#include "SDL.h"

#include <stdio.h>

#define CYCLES 100

int main(int argc, char *argv[])
{
    (void)argc;
    (void)argv;

    int failures = 0;
    for (int i = 0; i < CYCLES; ++i) {
        if (SDL_Init(SDL_INIT_VIDEO) != 0) {
            fprintf(stderr, "cycle %d: SDL_Init failed: %s\n", i, SDL_GetError());
            ++failures;
            continue;
        }
        SDL_Quit();
    }

    if (failures == 0) {
        printf("SDL_INIT_QUIT_PROBE_OK cycles=%d\n", CYCLES);
        return 0;
    }
    fprintf(stderr, "SDL_INIT_QUIT_PROBE_FAIL cycles=%d failures=%d\n", CYCLES, failures);
    return 1;
}
