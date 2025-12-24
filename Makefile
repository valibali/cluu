#
# CLUU Project Master Build System
#
# This is the top-level Makefile that orchestrates the entire CLUU project build.
# It coordinates building the kernel, creating bootable images, and running
# the system in QEMU for testing and development.
#
# Why this is important:
# - Provides a single entry point for building the entire project
# - Coordinates multiple build subsystems (kernel, bootloader, utilities)
# - Automates the complex process of creating bootable disk images
# - Enables easy testing and development workflow with QEMU
# - Simplifies the build process for developers
#
# Build targets:
# - all: Build kernel and create bootable image
# - clean: Clean all build artifacts
# - qemu: Build and run in QEMU with debugging
# - qemu_nodebug: Build and run in QEMU without debugging
# - doc: Generate kernel documentation
#
# The build process involves:
# 1. Building the Rust kernel with embedded resources
# 2. Creating BOOTBOOT-compatible disk images
# 3. Setting up UEFI boot environment for testing

.PHONY: all clean qemu userspace

all: userspace
	@make -C ./kernel all
	@make -C ./bootboot_image all

userspace:
	@echo "Building userspace binaries..."
	@make -C ./userspace/vfs_server all
	@make -C ./userspace/shmem_server all
	@make -C ./userspace/shell all
	@echo "Copying userspace binaries to initrd..."
	@mkdir -p ./bootboot_image/initrd/bin
	@mkdir -p ./bootboot_image/initrd/sys
	@mkdir -p ./bootboot_image/initrd/utils
	@cp ./userspace/vfs_server/target/vfs_server ./bootboot_image/initrd/sys/vfs_server
	@cp ./userspace/shmem_server/target/shmem_server ./bootboot_image/initrd/sys/shmem_server
	@cp ./userspace/shell/target/shell ./bootboot_image/initrd/bin/shell
	@echo "Userspace binaries ready"

clean:
	@make -C ./kernel clean
	@make -C ./userspace/vfs_server clean
	@make -C ./userspace/shmem_server clean
	@make -C ./userspace/shell clean
	@make -C ./utilies/mkbootimg clean
	@make -C ./bootboot_image clean
	@rm -rf ./bootboot_image/initrd/bin
	@rm -rf ./bootboot_image/initrd/sys

qemu: clean all
	@make -C ./bootboot_image uefi

doc: clean
	@make -C ./kernel doc

qemu_nodebug: all
	@make -C ./bootboot_image uefi_nodebug
