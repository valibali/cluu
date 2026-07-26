/* CLUU SDL2 bootstrap registration.
 *
 * SDL expects either a real SDL_main() (when SDL_MAIN_NEEDED) or a call to
 * SDL_SetMainReady() before SDL_Init() (when SDL_MAIN_HANDLED). The CLUU
 * platform defines SDL_MAIN_HANDLED in SDL_config_cluu.h, so the only thing
 * the bootstrap has to do is call SDL_SetMainReady() once before any
 * SDL_Init() invocation.
 *
 * This file is compiled into libSDL2.a so that any CLUU program linking
 * against libSDL2.a gets the registration for free; the program is then free
 * to call SDL_Init() directly without worrying about main-callback plumbing.
 *
 * There is intentionally no CLUU-specific init here (no IPC endpoint, no
 * capability lookup). T14 is just the static-platform proof — real CLUU
 * video/audio/timer/thread backends land in T15-T18 and will hook their own
 * initialization through the SDL_Init bootstrap path. */

#include "SDL.h"
#include "SDL_main.h"

#include <errno.h>
#include <sys/unistd.h>

static int cluu_sdl_main_ready_registered = 0;

void SDL_CLUU_RegisterMainReady(void)
{
    if (!cluu_sdl_main_ready_registered) {
        SDL_SetMainReady();
        cluu_sdl_main_ready_registered = 1;
    }
}

/* Constructor: runs before main() so any SDL_Init() call from the program
 * body works without an explicit registration. The CLUU crt0 honours
 * constructors that the linker keeps. */
__attribute__((constructor))
static void SDL_CLUU_Bootstrap(void)
{
    SDL_CLUU_RegisterMainReady();
}

/* CLUU's newlib does not ship a sysconf() implementation. SDL_malloc.c uses
 * sysconf(_SC_PAGE_SIZE) to discover the page size; SDL_cpuinfo.c's sysconf
 * call sites are gated out because _SC_NPROCESSORS_ONLN / _SC_PHYS_PAGES are
 * not defined in the newlib headers. Provide the minimal honest sysconf()
 * here: known queries return the real CLUU value, unknown queries set
 * EINVAL and return -1 (per POSIX). */
long sysconf(int name)
{
    switch (name) {
    case _SC_PAGESIZE:
        return 4096L;
    default:
        errno = EINVAL;
        return -1L;
    }
}

