# CLUU (Compact Lightweight Unix Utopia)

[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE) 
[![Documentation](https://img.shields.io/badge/documentation-RUSTDOCS-blue.svg)](https://valibali.github.io/cluu/)
[![Build Status](https://img.shields.io/badge/build-passing-brightgreen.svg)]()

CLUU is a hobby operating system written in Rust, currently in active development. It targets the x86_64 architecture with future plans to support aarch64. The project emphasizes microkernel design, memory safety, and portability.

## 🎯 Project Goals

**Microkernel Architecture**: CLUU follows a microkernel design philosophy, keeping the kernel minimal while providing most system services through user-level processes or servers. This promotes modularity, extensibility, and maintainability.

**Memory Safety**: Built with Rust to leverage its ownership model and memory safety guarantees, minimizing common programming errors like null pointer dereferences, buffer overflows, and data races.

**Portability**: Designed to be portable across different platforms and architectures, starting with x86_64 and expanding to aarch64.

## 🚀 Current Features

- ✅ **BOOTBOOT Integration**: Uses the BOOTBOOT bootloader for system initialization
- ✅ **x86_64 Architecture Support**: Full support for 64-bit x86 processors
- ✅ **Framebuffer Graphics**: Hardware-accelerated graphics output
- ✅ **Serial Port Communication**: UART 16550 support for debugging and communication
- ✅ **Keyboard Input**: PS/2 keyboard driver with input buffering
- ✅ **TTY System**: Terminal implementation with line editing capabilities
- ✅ **Memory Management**: Physical frame allocation and management
- ✅ **Interrupt Handling**: Complete IDT and GDT setup with interrupt processing
- ✅ **System Utilities**: Reboot functionality, timers, and I/O operations
- ✅ **Console Output**: Text rendering with PSF2 font support

## 🏗️ Architecture Overview

### Kernel Components
```
kernel/
├── src/
│   ├── arch/           # Architecture-specific code (x86_64)
│   │   └── x86_64/     # GDT, IDT, interrupts
│   ├── drivers/        # Hardware drivers
│   │   ├── display/    # Framebuffer driver
│   │   ├── input/      # Keyboard driver
│   │   └── serial/     # UART 16550 driver
│   ├── memory/         # Memory management
│   ├── components/     # System components (TTY)
│   └── utils/          # Utilities and I/O operations
```

### Key Systems

**Boot Process**: Utilizes [BOOTBOOT](https://gitlab.com/bztsrc/bootboot) bootloader providing:
- High-half kernel support
- Memory map initialization
- Processor mode setup
- Framebuffer initialization
- Early UART debugging

**Memory Management**: Physical frame-based memory allocation with bitmap tracking for efficient memory usage.

**I/O System**: Comprehensive I/O framework supporting:
- Port-mapped I/O (PIO) for hardware communication
- Serial communication for debugging
- Console output with font rendering

## 🛠️ Development Environment

### Prerequisites

1. **Rust Toolchain**: Install from [rustup.rs](https://rustup.rs/)
2. **QEMU**: For system emulation
   ```bash
   # Ubuntu/Debian
   sudo apt-get install qemu-system-x86
   
   # macOS
   brew install qemu
   
   # Windows
   # Download from https://www.qemu.org/download/
   ```
3. **Build Tools**: Make and standard build utilities
4. **Development Tools** (Optional):
   - VSCode with CodeLLDB extension for debugging
   - Telnet or similar for serial communication

### Building and Running

1. **Clone the repository**:
   ```bash
   git clone https://github.com/valibali/cluu.git
   cd cluu
   ```

2. **Build the system**:
   ```bash
   make all
   ```

3. **Run in QEMU**:
   ```bash
   # With debugging support
   make qemu
   
   # Without debugging symbols
   make qemu_nodebug
   ```

4. **Connect to serial output**:
   ```bash
   # In another terminal
   telnet localhost 4321
   ```

5. **Debug with VSCode**:
   - Open the project in VSCode
   - Press F5 to start debugging session
   - The debugger will attach to the running QEMU instance

### Development Commands

```bash
# Clean build artifacts
make clean

# Build kernel only
make kernel

# Create bootable image
make image
```

## 🔧 Technical Details

### Supported Hardware
- **CPU**: x86_64 processors with long mode support
- **Graphics**: UEFI framebuffer (any resolution)
- **Input**: PS/2 keyboard
- **Serial**: 16550-compatible UART controllers
- **Boot**: UEFI-compatible systems

### System Requirements
- **Memory**: Minimum 64MB RAM
- **Storage**: Bootable from any UEFI-compatible media
- **Emulation**: QEMU 4.0+ recommended for development

## 🎨 Screenshots

![CLUU Framebuffer Output](https://github.com/valibali/cluu/assets/22941355/b5eae565-61e7-4137-bb40-46f66b731cb1)

## 🤝 Contributing

CLUU welcomes contributions from the community! Whether you're interested in:
- Adding new features
- Improving existing code
- Writing documentation
- Reporting bugs
- Suggesting enhancements

Please feel free to open issues or submit pull requests.

### Development Guidelines
- Follow Rust best practices and idioms
- Maintain memory safety guarantees
- Write comprehensive documentation
- Include tests where applicable
- Respect the microkernel architecture

## 📚 Inspiration and References

This project draws inspiration from several excellent operating systems and projects:

- **[Plan 9](https://github.com/plan9foundation/plan9)**: Distributed OS concepts and clean design
- **[FreeBSD](https://github.com/freebsd/freebsd)**: Robust Unix-like system architecture
- **[RedoxOS](https://github.com/redox-os/redox)**: Modern Rust-based microkernel OS
- **[blog_os](https://os.phil-opp.com/)**: Excellent Rust OS development tutorial
- **[k4dos](https://github.com/clstatham/k4dos)**: Hobby OS with userspace capabilities

## 📄 License

CLUU is licensed under the MIT License. See [LICENSE](LICENSE) for more information.

---

**Note**: CLUU is a hobby project in active development. While functional, it's not intended for production use. The project serves as an educational platform for operating system development in Rust.
