/*
  Simple DirectMedia Layer
  Copyright (C) 1997-2024 Sam Lantinga <slouken@libsdl.org>

  This software is provided 'as-is', without any express or implied
  warranty.  In no event will the authors be held liable for any damages
  arising from the use of this software.

  Permission is granted to anyone to use this software for any purpose,
  including commercial applications, and to alter it and redistribute it
  freely, subject to the following restrictions:

  1. The origin of this software must not be misrepresented; you must not
     claim that you wrote the original software. If you use this software
     in a product, an acknowledgment in the product documentation would be
     appreciated but is not required.
  2. Altered source versions must be plainly marked as such, and must not be
     misrepresented as being the original software.
  3. This notice may not be removed or altered from any source distribution.
*/

#ifndef SDL_config_cluu_h_
#define SDL_config_cluu_h_
#define SDL_config_h_

#include "SDL_platform.h"

/* CLUU userspace platform configuration.
 *
 * Built against the CLUU newlib toolchain (clang --target=x86_64-unknown-none-elf
 * + ld.lld + newlib + libcluu_syscalls). The platform exposes:
 *   - POSIX-ish libc (newlib): stdio, stdlib, string, setjmp/longjmp, malloc.
 *   - libcluu_syscalls: clock_gettime, gettimeofday, usleep, sleep, sched_yield,
 *     pthread_create/mutex/cond/key, signal, sigaction, _exit, file ops.
 * Compiled out for T14 (real CLUU backends come in T15-T18):
 *   - Dynamic API, dlopen, loadso.
 *   - Filesystem (SDL_GetBasePath / SDL_GetPrefPath return NULL).
 *   - Joystick, haptic, sensor, hidapi, power management.
 *   - OpenGL, Vulkan, EGL, KMS/DRM, Wayland, X11, etc.
 * Kept (dummy/generic backends for T14, real impls land in T15-T18):
 *   - Video (dummy), audio (dummy + disk), events, timer (dummy),
 *     threads (generic), software renderer, RWops, stdlib. */

#define HAVE_STDARG_H   1
#define HAVE_STDDEF_H   1
#define HAVE_STDINT_H   1
#define HAVE_INTTYPES_H 1
#define HAVE_STDLIB_H   1
#define HAVE_STRING_H   1
#define HAVE_STRINGS_H  1
#define HAVE_WCHAR_H    1
#define HAVE_LIMITS_H   1
#define HAVE_CTYPE_H    1
#define HAVE_MATH_H     1
#define HAVE_SIGNAL_H   1
#define HAVE_MALLOC_H   1
#define HAVE_ALLOCA_H   1
#define HAVE_UNISTD_H   1
#define HAVE_SYS_TYPES_H 1
#define HAVE_SYS_STAT_H 1
#define HAVE_GETPAGESIZE 1

#define HAVE_MALLOC 1
#define HAVE_CALLOC 1
#define HAVE_REALLOC 1
#define HAVE_FREE 1
#define HAVE_ALLOCA 1
#define HAVE_GETENV 1
#define HAVE_SETENV 1
#define HAVE_PUTENV 1
#define HAVE_UNSETENV 1
#define HAVE_QSORT 1
#define HAVE_BSEARCH 1
#define HAVE_ABS 1
#define HAVE_BCOPY 1
#define HAVE_MEMSET 1
#define HAVE_MEMCPY 1
#define HAVE_MEMMOVE 1
#define HAVE_MEMCMP 1
#define HAVE_STRLEN 1
#define HAVE_STRLCPY 1
#define HAVE_STRLCAT 1
#define HAVE_STRDUP 1
#define HAVE_STRCHR 1
#define HAVE_STRRCHR 1
#define HAVE_STRSTR 1
#define HAVE_STRTOL 1
#define HAVE_STRTOUL 1
#define HAVE_STRTOLL 1
#define HAVE_STRTOULL 1
#define HAVE_STRTOD 1
#define HAVE_ATOI 1
#define HAVE_ATOF 1
#define HAVE_STRCMP 1
#define HAVE_STRNCMP 1
#define HAVE_STRCASECMP 1
#define HAVE_STRNCASECMP 1
#define HAVE_SSCANF 1
#define HAVE_SNPRINTF 1
#define HAVE_VSNPRINTF 1
#define HAVE_M_PI 1
#define HAVE_ACOS 1
#define HAVE_ACOSF 1
#define HAVE_ASIN 1
#define HAVE_ASINF 1
#define HAVE_ATAN 1
#define HAVE_ATANF 1
#define HAVE_ATAN2 1
#define HAVE_ATAN2F 1
#define HAVE_CEIL 1
#define HAVE_CEILF 1
#define HAVE_COPYSIGN 1
#define HAVE_COPYSIGNF 1
#define HAVE_COS 1
#define HAVE_COSF 1
#define HAVE_EXP 1
#define HAVE_EXPF 1
#define HAVE_FABS 1
#define HAVE_FABSF 1
#define HAVE_FLOOR 1
#define HAVE_FLOORF 1
#define HAVE_LOG 1
#define HAVE_LOGF 1
#define HAVE_POW 1
#define HAVE_POWF 1
#define HAVE_SIN 1
#define HAVE_SINF 1
#define HAVE_SQRT 1
#define HAVE_SQRTF 1
#define HAVE_TAN 1
#define HAVE_TANF 1
#define HAVE_FMOD 1
#define HAVE_FMODF 1
#define HAVE_LOG10 1
#define HAVE_LOG10F 1
#define HAVE_LROUND 1
#define HAVE_LROUNDF 1
#define HAVE_TRUNC 1
#define HAVE_TRUNCF 1
#define HAVE_RINT 1
#define HAVE_RINTF 1

#define HAVE_SIGACTION 1
#define HAVE_SETJMP 1
#define HAVE_NANOSLEEP 1
#define HAVE_GETHOSTNAME 1
#define HAVE_SYSCONF 1
#define HAVE_CLOCK_GETTIME 1
#define HAVE_PTHREAD_H 1

#define HAVE_GCC_SYNC_LOCK_TEST_AND_SET 1
#define HAVE_GCC_ATOMICS 1

/* Threading: real CLUU pthread backend (libcluu/posix/pthread.rs).
 * SDL_THREAD_PTHREAD_RECURSIVE_MUTEX intentionally undefined — CLUU's
 * mutex is non-recursive; SDL's FAKE_RECURSIVE_MUTEX path tracks
 * ownership via pthread_self(). */
#define SDL_THREAD_PTHREAD 1

/* Timer: unix backend (clock_gettime + nanosleep from libcluu/posix/time.rs). */
#define SDL_TIMER_UNIX 1

/* Audio: CLUU audiod backend (T18) + dummy + disk fallbacks. */
#define SDL_AUDIO_DRIVER_CLUU 1
#define SDL_AUDIO_DRIVER_DUMMY 1
#define SDL_AUDIO_DRIVER_DISK 1

/* Video: CLUU backend (displayd surface protocol + compositor input).
 * Dummy kept as fallback for headless/host-test paths. */
#define SDL_VIDEO_DRIVER_CLUU 1
#define SDL_VIDEO_DRIVER_DUMMY 1
/* GL/EGL/Vulkan/render-gl left undefined — `#ifdef` checks in SDL source
 * treat any definition (even to 0) as enabled, so they must be left
 * undefined to fully compile out. */
#undef SDL_VIDEO_OPENGL
#undef SDL_VIDEO_OPENGL_EGL
#undef SDL_VIDEO_OPENGL_ES
#undef SDL_VIDEO_OPENGL_ES2
#undef SDL_VIDEO_VULKAN
#undef SDL_VIDEO_RENDER_OGL
#undef SDL_VIDEO_RENDER_OGL_ES
#undef SDL_VIDEO_RENDER_OGL_ES2

/* Software renderer always available — the T16 CLUU video backend will
 * use it as the only honest renderer (no accelerated claims). */
#define SDL_VIDEO_RENDER_SW 1

/* RWops from stdio. */
#define HAVE_STDIO_H 1

/* Disabled subsystems. */
#define SDL_JOYSTICK_DISABLED 1
#define SDL_HAPTIC_DISABLED 1
#define SDL_HIDAPI_DISABLED 1
#define SDL_SENSOR_DISABLED 1
#define SDL_LOADSO_DISABLED 1
#define SDL_POWER_DISABLED 1
#define SDL_FILESYSTEM_DUMMY 1

/* No dynamic API, no shared library loading. */
/* SDL_DYNAMIC_API is set to 0 in src/dynapi/SDL_dynapi.h for __CLUU__. */

/* CLUU newlib has no dlfcn.h. */
#define HAVE_DLOPEN 0

/* SDL_main: dummy (no SDL_MainIsReady gate). The CLUU bootstrap registers
 * via SDL_SetMainReady() — see userspace/sdl2/cluu_init.c. */
#define SDL_MAIN_HANDLED 1

#endif /* SDL_config_cluu_h_ */
