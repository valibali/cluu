# CLUU Build System - Thin wrapper around xtask
# For idiomatic Rust development, prefer: cargo xtask <command>

.PHONY: all build build-rich build-release run run-rich run-release run-debug test test-harness test-slo \
	clean full-clean pristine rebuild-full rebuild-full-release userspace kernel setup-c doctor logs repo-hygiene help

all: build

build:
	cargo xtask build

build-rich:
	cargo xtask build --ui rich

build-release:
	cargo xtask build --profile release

run:
	cargo xtask run

run-rich:
	cargo xtask run --ui rich

run-release:
	cargo xtask run --profile release

run-debug:
	cargo xtask run --debug

test:
	cargo xtask test

test-harness:
	cargo xtask harness-matrix

test-slo:
	cargo xtask harness-slo

clean:
	cargo xtask clean-full

full-clean:
	cargo xtask clean-full

pristine: full-clean

rebuild-full:
	cargo xtask rebuild-full

rebuild-full-release:
	cargo xtask rebuild-full --profile release

userspace:
	cargo xtask userspace

kernel:
	cargo xtask kernel

setup-c:
	cargo xtask setup-c

doctor:
	cargo xtask doctor

logs:
	cargo xtask logs

repo-hygiene:
	./scripts/repo_hygiene_check.sh

help:
	@echo "CLUU Build System"
	@echo ""
	@echo "Recommended flows:"
	@echo "  make doctor       - Check host tools + key artifacts"
	@echo "  make logs         - List latest rich-build task logs"
	@echo "  make repo-hygiene - Verify repository structure and clean invariants"
	@echo "  make build-rich   - Rich UI build (parallel-safe stages + progress + per-task logs)"
	@echo "  make build        - Rich build (default UI)"
	@echo "  make run-debug    - Build + run paused for GDB"
	@echo "  make clean        - Full workspace clean (equivalent build reset)"
	@echo "  make full-clean   - Remove all generated artifacts"
	@echo "  make pristine     - Alias for full-clean"
	@echo "  make rebuild-full - Deterministic from-scratch rebuild"
	@echo ""
	@echo "Common targets:"
	@echo "  make build            - Build everything (dev profile, rich UI default)"
	@echo "  make build-rich       - Build everything (dev profile, rich UI)"
	@echo "  make build-release    - Build everything (release profile)"
	@echo "  make run              - Build and run in QEMU (rich UI default)"
	@echo "  make run-rich         - Build and run in QEMU (rich UI)"
	@echo "  make run-release      - Build+run with release profile"
	@echo "  make run-debug        - Run with GDB + telnet serial"
	@echo "  make test             - Run tests"
	@echo "  make test-harness     - Harness churn/leak/failpoint matrix"
	@echo "  make test-slo         - Repeated fairness SLO sweep"
	@echo "  make clean            - Full clean (target/tmp/external caches/build outputs)"
	@echo "  make full-clean       - Full clean (same as make clean)"
	@echo "  make pristine         - Alias for full-clean"
	@echo "  make rebuild-full     - Full clean + rebuild toolchain/images"
	@echo "  make rebuild-full-release - Full clean + rebuild in release profile"
	@echo "  make setup-c          - Build newlib/syscalls/crt0 toolchain bits"
	@echo "  make userspace        - Build only userspace"
	@echo "  make kernel           - Build only kernel"
	@echo ""
	@echo "Or use cargo directly:"
	@echo "  cargo xtask <command>"
	@echo ""
	@echo "Available xtask commands:"
	@echo "  cargo xtask doctor"
	@echo "  cargo xtask build [--profile dev|release] [--ui linear|rich]"
	@echo "  cargo xtask run [--profile dev|release] [--ui linear|rich]"
	@echo "  cargo xtask run --debug"
	@echo "  cargo xtask test"
	@echo "  cargo xtask harness-matrix [--no-build]"
	@echo "  cargo xtask harness-slo [--no-build] [--repeats N]"
	@echo "  cargo xtask clean"
	@echo "  cargo xtask clean-full"
	@echo "  cargo xtask rebuild-full [--profile dev|release]"
	@echo "  cargo xtask logs [--run <id|path>] [--task <name>] [--lines N] [--follow]"
	@echo "  cargo xtask setup-c"
	@echo "  cargo xtask userspace [--profile dev|release]"
	@echo "  cargo xtask kernel [--profile dev|release]"
	@echo ""
	@echo "Debug mode:"
	@echo "  Terminal 1: cargo xtask run --debug"
	@echo "  Terminal 2: telnet localhost 4321"
	@echo "  Terminal 3: gdb + target remote :1234"
