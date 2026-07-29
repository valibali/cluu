# DOOM Makefile — builds doomgeneric C engine + doom-cluu Rust platform backend.
#
# Based on userspace/micropython/Makefile. Compiles the doomgeneric C sources
# (minus SDL sound/video backends) with the CLUU newlib toolchain, links with
# the doom-cluu Rust staticlib + libcluu_syscalls + newlib.

DOOM_TOP = ../../external/doomgeneric/doomgeneric
PORT_DIR = .
BUILD = ../../target/doom-build

SYSROOT = ../../target/sysroot
NEWLIB_INC = $(SYSROOT)/x86_64-cluu-elf/include
NEWLIB_LIB = $(SYSROOT)/x86_64-cluu-elf/lib
CLUU_LIB = $(SYSROOT)/lib

CC = clang --target=x86_64-unknown-none-elf
LD = ld.lld

CLANG_RESOURCE = $(shell clang --target=x86_64-unknown-none-elf -print-resource-dir)

# POSIX feature macros for newlib headers
POSIX_DEFS = -D_POSIX_TIMERS=1 -D_POSIX_MONOTONIC_CLOCK=200112L \
             -D_POSIX_THREADS=1 -D_POSIX_PRIORITY_SCHEDULING=1 \
             -DSSIZE_MAX=0x7FFFFFFFFFFFFFFFL

# doomgeneric config: FEATURE_SOUND, SDL2 with CLUU video/events/audio backends.
# __CLUU__ selects SDL_config_cluu.h in SDL_config.h.
.DEFAULT_GOAL := all

DOOM_DEFS = -DDOOMGENERIC_RESX=640 -DDOOMGENERIC_RESY=400 \
            -DHAVE_UNISTD_H=1 -DHAVE_SYS_STAT_H=1 -DHAVE_INTTYPES_H=1 \
            -DHAVE_STDINT_H=1 -DHAVE_STDLIB_H=1 -DHAVE_STRINGS_H=1 \
            -DHAVE_STRING_H=1 -DHAVE_SYS_TYPES_H=1 \
            -DSTDC_HEADERS=1 \
            -D__CLUU__=1 \
          -DFEATURE_SOUND

CFLAGS = -std=gnu11 -Wall -Wno-unused-parameter -Wno-sign-compare \
         -Wno-old-style-definition -Wno-missing-prototypes \
         -Wno-unused-function -Wno-unused-variable -Wno-unused-label \
         -Wno-implicit-fallthrough -Wno-unreachable-code \
         -ffreestanding -fno-stack-protector -nostdlib -nostdinc \
         -isystem $(CLANG_RESOURCE)/include \
         -isystem $(NEWLIB_INC) \
         -I$(DOOM_TOP) \
         -I$(PORT_DIR) \
         -I$(PORT_DIR)/../sdl2/SDL2-2.30.0/include \
         $(POSIX_DEFS) $(DOOM_DEFS) \
         -mno-red-zone -fdata-sections -ffunction-sections \
         -g -O2 -DNDEBUG

LDFLAGS = -T $(PORT_DIR)/doom.ld --gc-sections --allow-multiple-definition

# Both SDL_cluuvideo.o and SDL_cluuaudio.o define a global named CLUU_bootstrap
# (VideoBootStrap vs AudioBootStrap — same name, different types).  With
# --allow-multiple-definition the linker silently picks one definition for all
# references; if the audio one wins, SDL_VideoInit calls CLUU_AudioInit (returns
# int 1) instead of CLUU_CreateDevice (returns SDL_VideoDevice *), and the
# stored _this=0x1 causes a PAGE_FAULT on the first write through it.
#
# Fix: rename the audio symbol to CLUUAUDIO_bootstrap via objcopy so both
# video and audio dispatchers resolve to their own backend's struct.  No SDL2
# source changes — pure build-time transformation.
SDL2_LIB_FIXED = $(BUILD)/libSDL2_fixed.a

$(SDL2_LIB_FIXED): $(CLUU_LIB)/libSDL2.a | $(BUILD)
	@echo "[Fixing SDL2 CLUU_bootstrap symbol collision]"
	cp $< $@
	cd $(BUILD) && ar x $(SDL2_LIB_FIXED) SDL_audio.o SDL_cluuaudio.o
	llvm-objcopy-14 --redefine-sym CLUU_bootstrap=CLUUAUDIO_bootstrap \
		$(BUILD)/SDL_audio.o $(BUILD)/SDL_audio.o
	llvm-objcopy-14 --redefine-sym CLUU_bootstrap=CLUUAUDIO_bootstrap \
		$(BUILD)/SDL_cluuaudio.o $(BUILD)/SDL_cluuaudio.o
	llvm-ar-14 r $(SDL2_LIB_FIXED) $(BUILD)/SDL_audio.o $(BUILD)/SDL_cluuaudio.o

# doomgeneric engine sources (from Makefile.sdl, minus SDL backends + doomgeneric_sdl)
SRC_DOOM = \
    dummy.o am_map.o doomdef.o doomstat.o dstrings.o d_event.o d_items.o \
    d_iwad.o d_loop.o d_main.o d_mode.o d_net.o f_finale.o f_wipe.o \
    g_game.o hu_lib.o hu_stuff.o info.o i_cdmus.o i_endoom.o i_joystick.o \
    i_scale.o i_sound.o i_system.o i_timer.o memio.o m_argv.o m_bbox.o \
    m_cheat.o m_config.o m_controls.o m_fixed.o m_menu.o m_misc.o \
    m_random.o p_ceilng.o p_doors.o p_enemy.o p_floor.o p_inter.o \
    p_lights.o p_map.o p_maputl.o p_mobj.o p_plats.o p_pspr.o p_saveg.o \
    p_setup.o p_sight.o p_spec.o p_switch.o p_telept.o p_tick.o p_user.o \
    r_bsp.o r_data.o r_draw.o r_main.o r_plane.o r_segs.o r_sky.o \
    r_things.o sha1.o sounds.o statdump.o st_lib.o st_stuff.o s_sound.o \
    tables.o v_video.o wi_stuff.o w_checksum.o w_file.o w_main.o w_wad.o \
    z_zone.o w_file.o w_main.o w_wad.o \
    i_input.o i_video.o doomgeneric.o mus2mid.o

# w_file_stdc.o removed — replaced by port-local w_file_cluu.o (grant-based)
# i_sdlsound.o / i_sdlmusic.o removed — replaced by port-local i_cluu_sound.o
#   (in-process 8-channel SFX mixer with SDL2 audio output)
# doomgeneric_sdl.o removed — replaced by port-local doomgeneric_sdl_cluu.o
#   (minimal patch of upstream: software renderer, CLUU hint activation)
CLUU_OBJS = $(BUILD)/w_file_cluu.o $(BUILD)/i_cluu_sound.o $(BUILD)/doomgeneric_sdl_cluu.o

OBJS = $(addprefix $(BUILD)/, $(SRC_DOOM)) $(CLUU_OBJS)

PROG = doom

all: $(BUILD)/$(PROG)

clean:
	rm -rf $(BUILD)

$(BUILD)/$(PROG): $(OBJS) $(CLUU_LIB)/libdoom_cluu.a $(SDL2_LIB_FIXED)
	@echo "[Linking $@]"
	$(LD) $(LDFLAGS) \
		$(CLUU_LIB)/crt0.o \
		-o $@ $(OBJS) \
		$(SDL2_LIB_FIXED) \
		$(CLUU_LIB)/libdoom_cluu.a \
		-L$(CLUU_LIB) -lcluu_syscalls \
		-L$(NEWLIB_LIB) -lc -lm
	@mkdir -p ../../target/x86_64-cluu-user/debug
	@llvm-strip-14 -o ../../target/x86_64-cluu-user/debug/$(PROG).elf $@
	@echo "Built $(PROG)"

$(OBJS): | $(BUILD)

$(BUILD):
	mkdir -p $(BUILD)

$(BUILD)/%.o: $(DOOM_TOP)/%.c
	@echo "[Compiling $<]"
	$(CC) $(CFLAGS) -c $< -o $@

$(BUILD)/main.o: $(PORT_DIR)/main.c
	@echo "[Compiling $<]"
	$(CC) $(CFLAGS) -c $< -o $@

$(BUILD)/doomgeneric_sdl_cluu.o: $(PORT_DIR)/doomgeneric_sdl_cluu.c
	@echo "[Compiling $<]"
	$(CC) $(CFLAGS) -c $< -o $@

$(BUILD)/w_file_cluu.o: $(PORT_DIR)/w_file_cluu.c
	@echo "[Compiling $<]"
	$(CC) $(CFLAGS) -c $< -o $@

$(BUILD)/i_cluu_sound.o: $(PORT_DIR)/i_cluu_sound.c
	@echo "[Compiling $<]"
	$(CC) $(CFLAGS) -c $< -o $@
