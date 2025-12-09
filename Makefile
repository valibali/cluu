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

.PHONY: all clean qemu

all:
	@make -C ./kernel all
	@make -C ./bootboot_image all

clean:
	@make -C ./kernel clean
	@make -C ./utilies/mkbootimg clean
	@make -C ./bootboot_image clean

qemu: clean all
	@make -C ./bootboot_image uefi

doc: clean
	@make -C ./kernel doc

qemu_nodebug: all
	@make -C ./bootboot_image uefi_nodebug
