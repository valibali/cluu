# Task 21 — Validate portability with one independent NES emulator (fceux)

**Date:** 2026-07-27
**Status:** BLOCKED — escalates to GPT-5.6 Sol for architecture review
**Plan ref:** `.omo/plans/cluu-multimedia-stack.md` line 248

## Summary

Attempted to port fceux 2.6.5 (NES emulator) to CLUU as an independent
SDL2 portability validation. The task BLOCKS at the compilation stage:
fceux 2.6.5 is C++ and CLUU's newlib toolchain (clang
`--target=x86_64-unknown-none-elf` + newlib + libcluu_syscalls) does not
include a C++ standard library (no libc++ or libstdc++). Even if a C++
stdlib were added, the fceux 2.6.5 build system has four additional hard
dependencies unavailable on CLUU: Qt5/Qt6, OpenGL, PkgConfig-discovered
minizip, and `-ldl`. Per task instructions §MUST DO: "Do NOT silently
fall back to a different emulator. The task BLOCKS and escalates to
GPT-5.6 Sol for architecture review."

## Source provenance

| Field | Value |
|---|---|
| Emulator | fceux |
| Version | 2.6.5 |
| Upstream | https://github.com/TASEmulators/fceux |
| Tag (actual) | `v2.6.5` (commit `ea6ed69b874e3ae94072f1b4f14b9a8f0fdd774b`) |
| Tag (task-specified) | `fceux-2.6.5` — **does not exist** in the upstream repo. The `fceux-` prefix is used for ≤2.6.4; 2.6.5 and 2.6.6 use the bare `v` prefix. Downloaded from the actual `v2.6.5` tag instead. |
| Download URL used | `https://github.com/TASEmulators/fceux/archive/refs/tags/v2.6.5.tar.gz` |
| Archive SHA-256 | `78156f3685c55849351178773940871ed607bc4fc37f233fdab58c232e3208fa` |
| Archive size | 23,299,626 bytes (23 MB) |
| Extracted source | `userspace/fceux/` (trimmed: 8.5 MB, 690 files; removed icons, web docs, CI configs, Windows driver, doxygen) |
| License (actual) | **GPL-2.0** — per GitHub License API (`"key": "gpl-2.0"`) and `COPYING` file (GNU GPL v2, 339 lines). The COPYING file's LGPL mentions are standard GPL boilerplate suggesting LGPL *as an alternative* for libraries, not a declaration that fceux is LGPL. |
| License (task-specified) | LGPL-2.1+ — **incorrect**. fceux is GPL-2.0. Individual files (e.g. `src/boards/emu2413.c`) may carry different licenses, but the project-level license is GPL-2.0. |

## ROM fixture provenance

| Field | Value |
|---|---|
| ROM | nestest.nes |
| Author | Kevin Horton (kevtris) |
| License | Public domain (per `nestest.txt` by the author) |
| Canonical URL | `https://www.qmtpro.com/~nes/nestest/nestest.nes` — **blocked** by environment HTTP proxy (returns Bosch internal block page HTML instead of the ROM) |
| Mirror URL used | `https://raw.githubusercontent.com/christopherpow/nes-test-roms/master/other/nestest.nes` |
| ROM SHA-256 | `f67d55fd6b3cf0bad1cc85f1df0d739c65b53e79cecb7fea8f77ec0eadab0004` |
| ROM size | 24,592 bytes |
| Format | iNES: 1×16k PRG, 1×8k CHR, H-mirror (verified via `file`) |
| Reference log | `nestest.log` (859,167 bytes) — full CPU instruction trace from Kevin Horton, used for deterministic PC-state verification |
| Documentation | `nestest.txt` (17,774 bytes) — "The ultimate NES CPU test ROM" by Kevin Horton, V1.00 09/06/04 |
| Hash verification | Could not verify against the canonical source (qmtpro.com blocked by proxy). Cross-verified via: (1) valid iNES header `NES\x1a`, (2) correct size 24,592 bytes (16 header + 16384 PRG + 8192 CHR), (3) accompanying reference log + author documentation from the same mirror repo. |

## Files created

| Path | Purpose |
|---|---|
| `userspace/fceux/` | Vendored fceux v2.6.5 source (trimmed to 8.5 MB). Contains `src/`, `attic/`, `scripts/`, `CMakeLists.txt`, `COPYING`. |
| `containers/fceux/Cluufile` | Container manifest following the DOOM pattern (`PROFILE ipc registry`, no DEVICE). Build line is the INTENDED target — the build command does not exist in xtask yet because the port is blocked. |
| `.omo/evidence/task-21-cluu-multimedia-stack.md` | This file. |

## Files NOT modified

| Path | Reason |
|---|---|
| `Cargo.toml` | No Rust workspace member to add — fceux is C++, not Rust. |
| `xtask/src/main.rs` | No `build-fceux` xtask wired — the build is blocked at compilation, so there is no build step to wire. |
| `userspace/displayd/` | Not touched (verified: `git diff` empty). |
| `userspace/sdl2/` | Not touched (verified: `git diff` empty). |
| `userspace/audiod/` | Not touched (out of scope per MUST NOT). |

## Blocker analysis: 5 independent architectural incompatibilities

### Blocker 1 (FATAL): No C++ standard library in CLUU newlib toolchain

CLUU's C toolchain is: `clang --target=x86_64-unknown-none-elf` + newlib
+ `libcluu_syscalls`. The sysroot at `target/sysroot/x86_64-cluu-elf/lib/`
contains `libc.a` (newlib) and `libm.a` but **no `libc++.a` or
`libstdc++.a`**. There are no C++ standard library headers
(`<iostream>`, `<fstream>`, `<string>`, `<vector>`, etc.) installed for
the target.

fceux 2.6.5 is written in C++ (all source files are `.cpp`). Core files
use C++ stdlib facilities:

```cpp
// src/drivers/sdl/sdl.cpp line 22-27
#include <unistd.h>
#include <csignal>
#include <cstring>
#include <cerrno>
#include <cstdio>
#include <cstdlib>
#include <climits>
#include <cmath>
#include <sys/types.h>
#include <sys/time.h>
#include <sys/stat.h>
#include <iostream>    // <-- requires C++ stdlib
#include <fstream>     // <-- requires C++ stdlib
```

Empirical verification:
```
$ echo '#include <iostream>
int main(){std::cout<<"hi";return 0;}' > /tmp/t.cpp
$ clang --target=x86_64-unknown-none-elf -c /tmp/t.cpp -o /tmp/t.o
/tmp/t.cpp:1:10: fatal error: 'iostream' file not found
1 error generated.
```

The DOOM port (T19) succeeded because doomgeneric is pure C. fceux is
C++. This is the fundamental blocker — compilation cannot begin, let
alone reach the static-link stage.

### Blocker 2: Qt5/Qt6 hard-required by the active build system

`src/CMakeLists.txt` (the active build, not the attic) line 13-25:
```cmake
if ( ${QT6} )
    find_package( Qt6 REQUIRED COMPONENTS Widgets OpenGL OpenGLWidgets ${QtHelpModule})
else()
    find_package( Qt5 REQUIRED COMPONENTS Widgets OpenGL ${QtHelpModule})
endif()
```

`find_package(... REQUIRED)` means CMake aborts if Qt is not found.
There is no Qt on CLUU (no display server, no widget toolkit). The
entire `SRC_DRIVERS_SDL` list (lines 514-562) is misnamed — every file
in it is `drivers/Qt/*.cpp`:

```cmake
set(SRC_DRIVERS_SDL
  ${CMAKE_CURRENT_SOURCE_DIR}/drivers/Qt/main.cpp
  ${CMAKE_CURRENT_SOURCE_DIR}/drivers/Qt/ConsoleWindow.cpp
  ${CMAKE_CURRENT_SOURCE_DIR}/drivers/Qt/ConsoleViewerGL.cpp
  ${CMAKE_CURRENT_SOURCE_DIR}/drivers/Qt/ConsoleViewerSDL.cpp
  ...44 more Qt/ files...
)
```

### Blocker 3: OpenGL hard-required

`src/CMakeLists.txt` line 78: `find_package(OpenGL REQUIRED)`.

CLUU's SDL2 config (`SDL_config_cluu.h` lines 175-182) explicitly
undefines all GL/EGL/Vulkan:
```c
#undef SDL_VIDEO_OPENGL
#undef SDL_VIDEO_OPENGL_EGL
#undef SDL_VIDEO_OPENGL_ES
#undef SDL_VIDEO_OPENGL_ES2
#undef SDL_VIDEO_VULKAN
#undef SDL_VIDEO_RENDER_OGL
#undef SDL_VIDEO_RENDER_OGL_ES
#undef SDL_VIDEO_RENDER_OGL_ES2
```

CLUU has no GPU and no OpenGL. The only video path is the software
renderer through the displayd surface protocol.

### Blocker 4: System libraries unavailable

`src/CMakeLists.txt` line 169: `set( SYS_LIBS  -lrt  -lpthread  -ldl)`.

- `-ldl`: dlopen is disabled in CLUU (`SDL_config_cluu.h` line 204:
  `#define HAVE_DLOPEN 0`). CLUU's newlib has no `dlfcn.h`.
- `-lrt`: POSIX realtime extensions. CLUU's newlib provides partial
  POSIX (clock_gettime, sched_yield) but not the full `-lrt` surface.
- `pkg_check_modules( MINIZIP REQUIRED minizip)` (line 106): no minizip
  for `x86_64-cluu-elf`. Internal zlib is available as a fallback, but
  minizip is hard-required with no internal fallback.
- `find_package(PkgConfig REQUIRED)` (line 77): no pkg-config cross-
  compilation setup for the CLUU target.

### Blocker 5: Legacy SDL driver (attic) requires GTK + X11 + GLX

The `src/drivers/sdl/` directory exists with a standalone SDL driver
(`sdl.cpp`, `sdl-video.cpp`, `glxwin.cpp`, etc.), but it is NOT
referenced by the active `src/CMakeLists.txt`. The only build file that
references it is `attic/cmake-stuff/cmake/fceux.cmake` (retired).

Even if resurrected from the attic, the SDL driver has its own hard
dependencies:

```cpp
// src/drivers/sdl/glxwin.cpp lines 1-15
#include <X11/X.h>
#include <X11/Xlib.h>
#include <GL/gl.h>
#include <GL/glx.h>
#include <GL/glu.h>
#include <gtk/gtk.h>
#include <gdk/gdkkeysyms.h>
```

CLUU has no X11, no GTK, no GLX. The SDL driver's video path
(`sdl-video.cpp`) has `#ifdef OPENGL` guards for GL, but the window
creation path in `glxwin.cpp` is unconditionally X11+GLX+GTK.

## What would be needed to unblock (for Sol's review)

1. **C++ standard library**: Add `libc++` (or `libstdc++`) built for
   `x86_64-unknown-none-elf` to the CLUU sysroot, plus C++ ABI
   support (libc++abi or libsupc++). This is a toolchain-level change.
2. **Non-Qt frontend**: Write a new `drivers/cluu/` frontend that uses
   SDL2 CLUU video/audio backends directly (no Qt, no OpenGL, no X11).
   This is a substantial port — the Qt frontend is ~60 files. The
   closest reference is the retired `drivers/sdl/` driver, but it
   needs its GLX/GTK/X11 dependencies stripped and replaced with pure
   SDL2 surface rendering.
3. **minizip**: Either vendor minizip into `userspace/fceux/` or provide
   it in the CLUU sysroot.
4. **Build system**: Replace the Qt+PkgConfig CMake with a curated
   Makefile (matching the DOOM/SDL2 pattern: `userspace/doom-cluu/Makefile`
   and `userspace/sdl2/Makefile`).

Alternatively: choose a C-based NES emulator instead of fceux (which is
C++). The task explicitly forbids this: "Do NOT silently fall back to a
different emulator." This is a decision for Sol.

## Verification

### Source hash verification
```
$ sha256sum /tmp/fceux-2.6.5.tar.gz
78156f3685c55849351178773940871ed607bc4fc37f233fdab58c232e3208fa  /tmp/fceux-2.6.5.tar.gz
```

### ROM hash verification
```
$ sha256sum /tmp/nestest.nes
f67d55fd6b3cf0bad1cc85f1df0d739c65b53e79cecb7fea8f77ec0eadab0004  /tmp/nestest.nes
$ file /tmp/nestest.nes
/tmp/nestest.nes: NES ROM image (iNES): 1x16k PRG, 1x8k CHR [H-mirror]
```

### C++ compilation failure (the blocker)
```
$ echo '#include <iostream>' | clang --target=x86_64-unknown-none-elf -x c++ -c - -o /dev/null
<stdin>:1:10: fatal error: 'iostream' file not found
1 error generated.
```

### No protocol changes
```
$ git diff userspace/displayd/ userspace/sdl2/
(empty — no displayd or sdl2 changes)
```

## License and ROM policy

- **fceux source**: GPL-2.0 (per GitHub License API and `COPYING` file).
  The task description's "LGPL-2.1+" is incorrect. Vendored source
  includes the original `COPYING` file at `userspace/fceux/COPYING`.
- **nestest.nes**: Public domain, authored by Kevin Horton. Per the
  author's documentation (`nestest.txt`): "This here is a pretty much
  all inclusive test suite for a NES CPU." No license restrictions —
  public-domain CPU test ROM, freely redistributable.
- **Reference log**: `nestest.log` is the canonical CPU instruction
  trace from Kevin Horton, used for deterministic PC-state verification.
  Not included in the commit (evidence-only artifact).

## Escalation

Per task §MUST DO: "If fceux cannot static-link against CLUU's newlib
toolchain: STOP. Do NOT silently fall back to a different emulator.
Document the exact link failure in `.omo/evidence/task-21-cluu-multimedia-stack.md`.
The task BLOCKS and escalates to GPT-5.6 Sol for architecture review."

**This task is BLOCKED and escalates to GPT-5.6 Sol.**

The blocker is not a link failure per se — it is a compilation failure
that precedes any possible link step. CLUU's newlib toolchain has no C++
standard library, and fceux 2.6.5 is C++. The four additional blockers
(Qt, OpenGL, system libs, legacy SDL driver's X11/GTK deps) would each
independently prevent the port even if the C++ stdlib were added.

Decision points for Sol:
1. Add a C++ stdlib to the CLUU toolchain and write a new `drivers/cluu/`
   frontend (substantial work), OR
2. Choose a C-based NES emulator for the portability validation instead
   of fceux (requires task scope change), OR
3. Defer T21 until CLUU has a C++ toolchain and a windowing system that
   can host Qt or a Qt-equivalent frontend.
