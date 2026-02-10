# CLUU Build System - Thin wrapper around xtask
# For idiomatic Rust development, prefer: cargo xtask <command>

.PHONY: all build run test test-harness test-slo clean userspace kernel help

all: build

build:
	cargo xtask build

run:
	cargo xtask run

run-debug:
	cargo xtask run --debug

test:
	cargo xtask test

test-harness:
	cargo xtask harness-matrix

test-slo:
	cargo xtask harness-slo

clean:
	cargo xtask clean

userspace:
	cargo xtask userspace

kernel:
	cargo xtask kernel

help:
	@echo "CLUU Build System"
	@echo ""
	@echo "Usage:"
	@echo "  make build       - Build everything"
	@echo "  make run         - Build and run in QEMU"
	@echo "  make run-debug   - Build and run in QEMU with GDB + telnet serial"
	@echo "  make test        - Run all tests"
	@echo "  make test-harness - Run harness churn/leak/failpoint matrix"
	@echo "  make test-slo     - Run repeated fairness SLO sweep"
	@echo "  make clean       - Clean build artifacts"
	@echo "  make userspace   - Build only userspace"
	@echo "  make kernel      - Build only kernel"
	@echo ""
	@echo "Or use cargo directly:"
	@echo "  cargo xtask <command>"
	@echo ""
	@echo "Available xtask commands:"
	@echo "  cargo xtask build [--profile dev|release]"
	@echo "  cargo xtask run [--profile dev|release]"
	@echo "  cargo xtask run --debug              # Debug mode with GDB"
	@echo "  cargo xtask test"
	@echo "  cargo xtask harness-matrix [--no-build]"
	@echo "  cargo xtask harness-slo [--no-build] [--repeats N]"
	@echo "  cargo xtask clean"
	@echo "  cargo xtask userspace [--profile dev|release]"
	@echo "  cargo xtask kernel [--profile dev|release]"
	@echo ""
	@echo "Debug mode:"
	@echo "  Terminal 1: cargo xtask run --debug"
	@echo "  Terminal 2: telnet localhost 4321"
	@echo "  Terminal 3: gdb + target remote :1234"
