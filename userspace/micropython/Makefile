MICROPYTHON_TOP = ../../external/micropython
PORT_DIR = .
BUILD = ../../target/micropython-build

SYSROOT = ../../target/sysroot
NEWLIB_INC = $(SYSROOT)/x86_64-cluu-elf/include
NEWLIB_LIB = $(SYSROOT)/x86_64-cluu-elf/lib
CLUU_LIB = $(SYSROOT)/lib

# CROSS_COMPILE must be empty before mkenv.mk
CROSS_COMPILE =

include $(MICROPYTHON_TOP)/py/mkenv.mk

# Override CC AFTER mkenv.mk (which sets CC=$(CROSS_COMPILE)gcc)
CC = clang --target=x86_64-unknown-none-elf
CPP = clang --target=x86_64-unknown-none-elf -E
LD = ld.lld

# qstr definitions (must come before including py.mk)
QSTR_DEFS = $(PORT_DIR)/qstrdefsport.h

# Disable ROM text compression (simpler build)
MICROPY_ROM_TEXT_COMPRESSION = 0

# include py core make definitions
include $(TOP)/py/py.mk
# include extmod (VFS, os, json, etc.) — adds to PY_O and SRC_QSTR
include $(TOP)/extmod/extmod.mk

# CLUU does not yet provide the full POSIX dirent/statvfs/poll surface
# expected by MicroPython's POSIX VFS helpers.
EXTMOD_UNSUPPORTED_C = \
	extmod/vfs_posix.c \
	extmod/vfs_posix_file.c
SRC_EXTMOD_C := $(filter-out $(EXTMOD_UNSUPPORTED_C),$(SRC_EXTMOD_C))
PY_O := $(filter-out $(addprefix $(BUILD)/, $(EXTMOD_UNSUPPORTED_C:.c=.o)),$(PY_O))
SRC_QSTR := $(filter-out $(EXTMOD_UNSUPPORTED_C),$(SRC_QSTR))

INC += -I$(PORT_DIR)
INC += -I$(TOP)
INC += -I$(BUILD)
INC += -I$(NEWLIB_INC)

# POSIX feature macros needed for newlib headers (clock_gettime, CLOCK_MONOTONIC, pthreads)
POSIX_DEFS = -D_POSIX_TIMERS=1 -D_POSIX_MONOTONIC_CLOCK=200112L \
             -D_POSIX_THREADS=1 -D_POSIX_PRIORITY_SCHEDULING=1 \
             -DSSIZE_MAX=0x7FFFFFFFFFFFFFFFL

CLANG_RESOURCE = $(shell clang --target=x86_64-unknown-none-elf -print-resource-dir)

CFLAGS += $(INC) $(POSIX_DEFS) -std=gnu11 -Wall \
          -ffreestanding -fno-stack-protector -nostdlib -nostdinc \
          -isystem $(CLANG_RESOURCE)/include \
          -mno-red-zone -fdata-sections -ffunction-sections \
          -Wno-unused-parameter -Wno-missing-field-initializers \
          -Wno-sign-compare -Wno-old-style-definition \
          -Wno-null-pointer-subtraction -Wno-unused-label

# Suppress additional warnings from MicroPython source
CFLAGS += -Wno-implicit-fallthrough -Wno-unreachable-code \
          -Wno-missing-prototypes -Wno-unused-variable \
          -Wno-tautological-constant-out-of-range-compare \
          -Wno-implicit-function-declaration -Wno-int-conversion

CFLAGS += -g -Os -DNDEBUG

CSUPEROPT = -O3

PROG = micropython

SRC_C = \
    main.c \
    mphalport.c \
    mpthreadport.c \
    gccollect.c \
    $(MICROPYTHON_TOP)/shared/runtime/pyexec.c \
    $(MICROPYTHON_TOP)/shared/readline/readline.c \
    $(MICROPYTHON_TOP)/shared/runtime/gchelper_generic.c \
    $(MICROPYTHON_TOP)/shared/runtime/stdout_helpers.c

SRC_QSTR += $(MICROPYTHON_TOP)/shared/readline/readline.c \
            $(MICROPYTHON_TOP)/shared/runtime/pyexec.c

# PY_O includes py core + extmod objects (from py.mk and extmod.mk)
OBJ = $(PY_O) $(addprefix $(BUILD)/, $(SRC_C:.c=.o))

# Include mkrules.mk first so we can override its link rule after
include $(TOP)/py/mkrules.mk

# Override mkrules.mk's link target — we link with ld.lld, not CC
$(BUILD)/$(PROG): $(OBJ)
	$(ECHO) "LINK $@"
	$(Q)$(LD) -T ../../userspace/user.ld \
		$(CLUU_LIB)/crt0.o \
		-o $@ $(OBJ) \
		-L$(CLUU_LIB) -lcluu_syscalls \
		-L$(NEWLIB_LIB) -lc -lm \
		--gc-sections
	@# Copy to standard output location for xtask to find
	@mkdir -p ../../target/x86_64-cluu-user/debug
	@cp $@ ../../target/x86_64-cluu-user/debug/$(PROG).elf
	$(ECHO) "Built $(PROG)"
