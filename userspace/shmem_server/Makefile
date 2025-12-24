# CLUU Shmem Server Build System

# Rust toolchain
CARGO := cargo

# Paths
RUST_TARGET := cluu-x86-64
C_SYSCALL_LIB := ../lib

# C syscall library objects needed
SYSCALL_OBJECTS := $(C_SYSCALL_LIB)/syscall.o $(C_SYSCALL_LIB)/ipc.o $(C_SYSCALL_LIB)/process.o $(C_SYSCALL_LIB)/builtins.o

# Build RUSTFLAGS with linker script and C objects
RUSTFLAGS := -C link-arg=-T$(CURDIR)/linker.ld \
             -C link-arg=-nostdlib \
             $(foreach obj,$(SYSCALL_OBJECTS),-C link-arg=$(obj))

# Output binary
TARGET := shmem_server
TARGET_DIR := target
TARGET_PATH := $(TARGET_DIR)/$(TARGET)
RUST_BINARY := target/debug/shmem_server

.PHONY: all clean syscall_lib

all: $(TARGET_PATH)

# Build C syscall library if needed
syscall_lib:
	@if [ ! -f "$(C_SYSCALL_LIB)/syscall.o" ]; then \
		echo "Building C syscall library..."; \
		$(MAKE) -C $(C_SYSCALL_LIB); \
	fi

# Build Rust code and link with C syscall library
$(TARGET_PATH): src/main.rs Cargo.toml cluu-x86-64.json linker.ld syscall_lib | $(TARGET_DIR)
	@echo "Building shmem_server with C syscall library..."
	RUSTFLAGS="$(RUSTFLAGS)" $(CARGO) build
	@echo "Copying binary to $@..."
	@cp $(RUST_BINARY) $@
	@echo "Built shmem_server binary: $(TARGET)"
	@file $@
	@size $@

# Create target directory
$(TARGET_DIR):
	@mkdir -p $(TARGET_DIR)

clean:
	$(CARGO) clean
	rm -rf $(TARGET_DIR)
	@echo "Cleaned shmem_server build artifacts"
