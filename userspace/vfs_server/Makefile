# VFS Server Build System for CLUU

# Toolchain configuration
CC := x86_64-elf-gcc
AS := x86_64-elf-as
LD := x86_64-elf-ld
OBJCOPY := x86_64-elf-objcopy

# If cross-compiler not available, try native compiler
ifeq ($(shell which $(CC) 2>/dev/null),)
    CC := gcc
    AS := as
    LD := ld
    OBJCOPY := objcopy
endif

# Compiler flags for freestanding userspace
CFLAGS := -std=c11 \
          -ffreestanding \
          -nostdlib \
          -fno-builtin \
          -fno-stack-protector \
          -fno-pic \
          -fno-pie \
          -m64 \
          -mno-red-zone \
          -O0 \
          -Wall \
          -Wextra

# Assembler flags
ASFLAGS := -64

# Linker flags
LDFLAGS := -T linker.ld \
           -nostdlib \
           -static \
           -no-pie

# Source files
C_SOURCES := vfs_server.c
ASM_SOURCES := ../lib/syscall.S ../lib/ipc.S

# Object files
C_OBJECTS := $(C_SOURCES:.c=.o)
ASM_OBJECTS := $(ASM_SOURCES:.S=.o)
ALL_OBJECTS := $(C_OBJECTS) $(ASM_OBJECTS)

# Output binary
TARGET := vfs_server
TARGET_DIR := target
TARGET_PATH := $(TARGET_DIR)/$(TARGET)

.PHONY: all clean

all: $(TARGET_PATH)

# Link all object files
$(TARGET_PATH): $(ALL_OBJECTS) linker.ld | $(TARGET_DIR)
	@echo "Linking $@..."
	$(LD) $(LDFLAGS) $(ALL_OBJECTS) -o $@
	@echo "Built VFS server binary: $(TARGET)"
	@file $@
	@size $@

# Create target directory
$(TARGET_DIR):
	@mkdir -p $(TARGET_DIR)

# Compile C sources
%.o: %.c
	@echo "Compiling $<..."
	$(CC) $(CFLAGS) -c $< -o $@

# Assemble assembly sources
%.o: %.S
	@echo "Assembling $<..."
	$(CC) $(CFLAGS) -c $< -o $@

clean:
	rm -f $(ALL_OBJECTS)
	rm -rf $(TARGET_DIR)
	@echo "Cleaned build artifacts"

# Display info about the built binary
info: $(TARGET_PATH)
	@echo "=== ELF Header ==="
	@readelf -h $(TARGET_PATH)
	@echo ""
	@echo "=== Program Headers ==="
	@readelf -l $(TARGET_PATH)
	@echo ""
	@echo "=== Section Headers ==="
	@readelf -S $(TARGET_PATH)
