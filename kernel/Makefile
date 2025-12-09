#
# CLUU Kernel Build System
#
# This Makefile orchestrates the build process for the CLUU microkernel.
# It handles font embedding, Rust compilation, and final ELF generation
# for the x86_64 target architecture.
#
# Why this is important:
# - Automates the complex kernel build process
# - Integrates font resources into the kernel binary
# - Coordinates Rust compilation with custom target
# - Produces bootable ELF binary compatible with BOOTBOOT
# - Provides documentation generation and cleanup targets
#
# Build process:
# 1. Convert PSF font to object file for embedding
# 2. Compile Rust kernel with custom target specification
# 3. Generate final ELF binary ready for bootloader
#
# Copyright (C) 2017 - 2021 Vinay Chandra
# Modified for CLUU kernel
#
# Permission is hereby granted, free of charge, to any person
# obtaining a copy of this software and associated documentation
# files (the "Software"), to deal in the Software without
# restriction, including without limitation the rights to use, copy,
# modify, merge, publish, distribute, sublicense, and/or sell copies
# of the Software, and to permit persons to whom the Software is
# furnished to do so, subject to the following conditions:
#
# The above copyright notice and this permission notice shall be
# included in all copies or substantial portions of the Software.
#
# THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
# EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
# MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
# NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT
# HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY,
# WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
# OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
# DEALINGS IN THE SOFTWARE.
#
# This file is part of the BOOTBOOT Protocol package.
#

all: cluu.x86_64.elf

# Kernel build
cluu.x86_64.elf: src/**
	objcopy -O elf64-x86-64 -B i386 -I binary font.psf font.o
	cargo build 
	cp ./target/cluu-x86-64/debug/cluu-kernel-rust cluu-kernel-rust.x86_64.elf

doc:
	cargo doc --target-dir ../docs

clean:
	rm -rf ./target
