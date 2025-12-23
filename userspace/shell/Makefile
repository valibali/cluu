# CLUU Shell Build System

# Rust toolchain
CARGO := cargo

# Paths
RUST_TARGET := cluu-x86-64
C_SYSCALL_LIB := ../lib

# C syscall library objects needed
SYSCALL_OBJECTS := $(C_SYSCALL_LIB)/syscall.o $(C_SYSCALL_LIB)/ipc.o $(C_SYSCALL_LIB)/process.o

# Build RUSTFLAGS with linker script and C objects
RUSTFLAGS := -C link-arg=-T$(CURDIR)/linker.ld \
             -C link-arg=-nostdlib \
             $(foreach obj,$(SYSCALL_OBJECTS),-C link-arg=$(obj))

# Output binary
TARGET := shell
TARGET_DIR := target
TARGET_PATH := $(TARGET_DIR)/$(TARGET)
RUST_BINARY := target/$(RUST_TARGET)/debug/cluu-shell

.PHONY: all clean syscall_lib

all: $(TARGET_PATH)

# Build C syscall library if needed
syscall_lib:
	@if [ ! -f "$(C_SYSCALL_LIB)/syscall.o" ]; then \
		echo "Building C syscall library..."; \
		$(MAKE) -C $(C_SYSCALL_LIB); \
	fi

# Build Rust code and link with C syscall library
# Cargo will handle everything including linking
$(TARGET_PATH): src/main.rs Cargo.toml cluu-x86-64.json linker.ld syscall_lib | $(TARGET_DIR)
	@echo "Building Rust shell with C syscall library..."
	RUSTFLAGS="$(RUSTFLAGS)" $(CARGO) build
	@echo "Copying binary to $@..."
	@cp $(RUST_BINARY) $@
	@echo "Built shell binary: $(TARGET)"
	@file $@
	@size $@

# Create target directory
$(TARGET_DIR):
	@mkdir -p $(TARGET_DIR)

clean:
	$(CARGO) clean
	rm -rf $(TARGET_DIR)
	@echo "Cleaned shell build artifacts"

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
