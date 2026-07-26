# CLUU static SDL2 2.30.0 build — cross-compiled for x86_64-cluu-elf.
#
# Curated per docs/README-porting.md: only the source files needed for the
# enabled subsystems (video dummy, software renderer, events, timer dummy,
# thread generic, audio dummy+disk, RWops, stdlib). Disabled: dynamic API,
# loadso, joystick, haptic, hidapi, sensor, power, filesystem, OpenGL/Vulkan.
#
# Toolchain matches userspace/doom-cluu/Makefile: clang freestanding + ld.lld
# + newlib at target/sysroot/x86_64-cluu-elf + libcluu_syscalls at sysroot/lib.

SDL_TOP    = SDL2-2.30.0
BUILD      = ../../target/sdl2-build
SYSROOT    = ../../target/sysroot
NEWLIB_INC = $(SYSROOT)/x86_64-cluu-elf/include
NEWLIB_LIB = $(SYSROOT)/x86_64-cluu-elf/lib
CLUU_LIB   = $(SYSROOT)/lib

CC          = clang --target=x86_64-unknown-none-elf
AR          = llvm-ar-14
CLANG_RESOURCE = $(shell clang --target=x86_64-unknown-none-elf -print-resource-dir)

POSIX_DEFS = -D_POSIX_TIMERS=1 -D_POSIX_MONOTONIC_CLOCK=200112L \
             -D_POSIX_THREADS=1 -D_POSIX_PRIORITY_SCHEDULING=1 \
             -D_POSIX_THREAD_PRIORITY_SCHEDULING=1 \
             -D_POSIX_THREAD_PROCESS_SHARED=1 \
             -D_UNIX98_THREAD_MUTEX_ATTRIBUTES=1 \
             -DSSIZE_MAX=0x7FFFFFFFFFFFFFFFL

SDL_DEFS   = -D__CLUU__=1 \
             -DHAVE_UNISTD_H=1 -DHAVE_SYS_STAT_H=1 -DHAVE_INTTYPES_H=1 \
             -DHAVE_STDINT_H=1 -DHAVE_STDLIB_H=1 -DHAVE_STRINGS_H=1 \
             -DHAVE_STRING_H=1 -DHAVE_SYS_TYPES_H=1 -DHAVE_STDIO_H=1 \
             -DSTDC_HEADERS=1

CFLAGS = -std=gnu11 -Wall -Wextra \
         -Werror=implicit-function-declaration \
         -Werror=implicit-int \
         -Wno-unused-parameter -Wno-sign-compare \
         -Wno-unused-function -Wno-unused-variable \
         -Wno-unused-but-set-variable \
         -ffreestanding -fno-stack-protector -nostdlib -nostdinc \
         -isystem $(CLANG_RESOURCE)/include \
         -isystem $(NEWLIB_INC) \
         -I$(SDL_TOP)/include \
         -I$(SDL_TOP)/src \
         $(POSIX_DEFS) $(SDL_DEFS) \
         -mno-red-zone -fdata-sections -ffunction-sections \
         -g -O2 -DNDEBUG

# Curated source list (per docs/README-porting.md, narrowed to enabled paths).
SDL_SOURCES = \
    $(SDL_TOP)/src/SDL.c \
    $(SDL_TOP)/src/SDL_assert.c \
    $(SDL_TOP)/src/SDL_error.c \
    $(SDL_TOP)/src/SDL_guid.c \
    $(SDL_TOP)/src/SDL_hints.c \
    $(SDL_TOP)/src/SDL_list.c \
    $(SDL_TOP)/src/SDL_log.c \
    $(SDL_TOP)/src/SDL_dataqueue.c \
    $(SDL_TOP)/src/SDL_utils.c \
    $(SDL_TOP)/src/atomic/SDL_atomic.c \
    $(SDL_TOP)/src/atomic/SDL_spinlock.c \
    $(SDL_TOP)/src/cpuinfo/SDL_cpuinfo.c \
    $(SDL_TOP)/src/events/SDL_clipboardevents.c \
    $(SDL_TOP)/src/events/SDL_displayevents.c \
    $(SDL_TOP)/src/events/SDL_dropevents.c \
    $(SDL_TOP)/src/events/SDL_events.c \
    $(SDL_TOP)/src/events/SDL_gesture.c \
    $(SDL_TOP)/src/events/SDL_keyboard.c \
    $(SDL_TOP)/src/events/SDL_keysym_to_scancode.c \
    $(SDL_TOP)/src/events/SDL_mouse.c \
    $(SDL_TOP)/src/events/SDL_quit.c \
    $(SDL_TOP)/src/events/SDL_scancode_tables.c \
    $(SDL_TOP)/src/events/SDL_touch.c \
    $(SDL_TOP)/src/events/SDL_windowevents.c \
    $(SDL_TOP)/src/events/imKStoUCS.c \
    $(SDL_TOP)/src/file/SDL_rwops.c \
    $(SDL_TOP)/src/render/SDL_d3dmath.c \
    $(SDL_TOP)/src/render/SDL_render.c \
    $(SDL_TOP)/src/render/SDL_yuv_sw.c \
    $(SDL_TOP)/src/render/software/SDL_blendfillrect.c \
    $(SDL_TOP)/src/render/software/SDL_blendline.c \
    $(SDL_TOP)/src/render/software/SDL_blendpoint.c \
    $(SDL_TOP)/src/render/software/SDL_drawline.c \
    $(SDL_TOP)/src/render/software/SDL_drawpoint.c \
    $(SDL_TOP)/src/render/software/SDL_render_sw.c \
    $(SDL_TOP)/src/render/software/SDL_rotate.c \
    $(SDL_TOP)/src/render/software/SDL_triangle.c \
    $(SDL_TOP)/src/stdlib/SDL_crc16.c \
    $(SDL_TOP)/src/stdlib/SDL_crc32.c \
    $(SDL_TOP)/src/stdlib/SDL_getenv.c \
    $(SDL_TOP)/src/stdlib/SDL_iconv.c \
    $(SDL_TOP)/src/stdlib/SDL_malloc.c \
    $(SDL_TOP)/src/stdlib/SDL_mslibc.c \
    $(SDL_TOP)/src/stdlib/SDL_qsort.c \
    $(SDL_TOP)/src/stdlib/SDL_stdlib.c \
    $(SDL_TOP)/src/stdlib/SDL_string.c \
    $(SDL_TOP)/src/stdlib/SDL_strtokr.c \
    $(SDL_TOP)/src/thread/SDL_thread.c \
    $(SDL_TOP)/src/thread/pthread/SDL_syscond.c \
    $(SDL_TOP)/src/thread/pthread/SDL_sysmutex.c \
    $(SDL_TOP)/src/thread/generic/SDL_syssem.c \
    $(SDL_TOP)/src/thread/pthread/SDL_systhread.c \
    $(SDL_TOP)/src/thread/pthread/SDL_systls.c \
    $(SDL_TOP)/src/timer/SDL_timer.c \
    $(SDL_TOP)/src/timer/unix/SDL_systimer.c \
    $(SDL_TOP)/src/audio/SDL_audio.c \
    $(SDL_TOP)/src/audio/SDL_audiocvt.c \
    $(SDL_TOP)/src/audio/SDL_audiodev.c \
    $(SDL_TOP)/src/audio/SDL_audiotypecvt.c \
    $(SDL_TOP)/src/audio/SDL_mixer.c \
    $(SDL_TOP)/src/audio/SDL_wave.c \
    $(SDL_TOP)/src/audio/dummy/SDL_dummyaudio.c \
    $(SDL_TOP)/src/audio/disk/SDL_diskaudio.c \
    $(SDL_TOP)/src/video/SDL_blit.c \
    $(SDL_TOP)/src/video/SDL_blit_0.c \
    $(SDL_TOP)/src/video/SDL_blit_1.c \
    $(SDL_TOP)/src/video/SDL_blit_A.c \
    $(SDL_TOP)/src/video/SDL_blit_auto.c \
    $(SDL_TOP)/src/video/SDL_blit_copy.c \
    $(SDL_TOP)/src/video/SDL_blit_N.c \
    $(SDL_TOP)/src/video/SDL_blit_slow.c \
    $(SDL_TOP)/src/video/SDL_bmp.c \
    $(SDL_TOP)/src/video/SDL_clipboard.c \
    $(SDL_TOP)/src/video/SDL_fillrect.c \
    $(SDL_TOP)/src/video/SDL_pixels.c \
    $(SDL_TOP)/src/video/SDL_rect.c \
    $(SDL_TOP)/src/video/SDL_RLEaccel.c \
    $(SDL_TOP)/src/video/SDL_shape.c \
    $(SDL_TOP)/src/video/SDL_stretch.c \
    $(SDL_TOP)/src/video/SDL_surface.c \
    $(SDL_TOP)/src/video/SDL_video.c \
    $(SDL_TOP)/src/video/SDL_yuv.c \
    $(SDL_TOP)/src/video/yuv2rgb/yuv_rgb_sse.c \
    $(SDL_TOP)/src/video/yuv2rgb/yuv_rgb_std.c \
    $(SDL_TOP)/src/video/dummy/SDL_nullevents.c \
    $(SDL_TOP)/src/video/dummy/SDL_nullframebuffer.c \
    $(SDL_TOP)/src/video/dummy/SDL_nullvideo.c \
    $(SDL_TOP)/src/loadso/dummy/SDL_sysloadso.c \
    $(SDL_TOP)/src/filesystem/dummy/SDL_sysfilesystem.c \
    $(SDL_TOP)/src/joystick/dummy/SDL_sysjoystick.c \
    $(SDL_TOP)/src/haptic/dummy/SDL_syshaptic.c \
    $(SDL_TOP)/src/sensor/dummy/SDL_dummysensor.c \
    $(SDL_TOP)/src/power/SDL_power.c \
    $(SDL_TOP)/src/locale/SDL_locale.c \
    $(SDL_TOP)/src/locale/dummy/SDL_syslocale.c \
    $(SDL_TOP)/src/misc/SDL_url.c \
    $(SDL_TOP)/src/misc/dummy/SDL_sysurl.c \
    $(SDL_TOP)/src/main/dummy/SDL_dummy_main.c \
    cluu_init.c

# Map source paths to object paths under BUILD/ preserving directory structure.
SDL_OBJECTS = $(patsubst $(SDL_TOP)/%.c,$(BUILD)/sdl/%.o,$(filter $(SDL_TOP)/%,$(SDL_SOURCES))) \
              $(BUILD)/cluu/cluu_init.o

LIB = $(CLUU_LIB)/libSDL2.a

all: $(LIB)

clean:
	rm -rf $(BUILD) $(LIB)

$(LIB): $(SDL_OBJECTS)
	@mkdir -p $(CLUU_LIB)
	@echo "[Archiving $@]"
	$(AR) rcs $@ $(SDL_OBJECTS)
	@echo "Built libSDL2.a ($(words $(SDL_OBJECTS)) objects)"

$(BUILD)/sdl/%.o: $(SDL_TOP)/%.c
	@mkdir -p $(dir $@)
	@echo "[Compiling $<]"
	$(CC) $(CFLAGS) -c $< -o $@

$(BUILD)/cluu/cluu_init.o: cluu_init.c
	@mkdir -p $(dir $@)
	@echo "[Compiling $<]"
	$(CC) $(CFLAGS) -c $< -o $@

# Probe: dummy init/quit × 100 cycles. Links against the newlib libc/libm
# and libcluu_syscalls (pthread, clock_gettime, etc.) exactly like DOOM.
PROBE = $(BUILD)/init_quit_probe
PROBE_OBJ = $(BUILD)/cluu/init_quit_probe.o
PROBE_SRC = probes/init_quit_probe.c

probe: $(PROBE)
	@echo "[Running init/quit probe]"
	$(PROBE)

$(PROBE_OBJ): $(PROBE_SRC)
	@mkdir -p $(dir $@)
	@echo "[Compiling $<]"
	$(CC) $(CFLAGS) -c $< -o $@

$(PROBE): $(PROBE_OBJ) $(LIB)
	@mkdir -p $(dir $@)
	@echo "[Linking $@]"
	ld.lld -T ../../userspace/user.ld --gc-sections \
		$(CLUU_LIB)/crt0.o \
		-o $@ $(PROBE_OBJ) \
		$(LIB) \
		-L$(CLUU_LIB) -lcluu_syscalls \
		-L$(NEWLIB_LIB) -lc -lm

# Audit: list undefined symbols in the static library.
audit: $(LIB)
	@echo "[Undefined symbols in $(LIB)]"
	@llvm-nm-14 -u $(LIB) | sort -u

.PHONY: all clean probe audit
