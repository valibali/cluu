# CLUU Debug Guide

## Debug Mode with GDB and Telnet Serial

### Quick Start

**Terminal 1: Start QEMU in debug mode**
```bash
cargo xtask run --build --debug
# or
make run-debug-build
```

This will:
- ✅ Build/update disk images
- ✅ Start QEMU with UEFI
- ✅ **Pause CPU** - waiting for GDB
- ✅ Start **GDB server** on `localhost:1234`
- ✅ Start **telnet serial** on `localhost:4321`

**Terminal 2: Connect to serial output**
```bash
telnet localhost 4321
```

**Terminal 3: Debug with GDB**
```bash
# Find the kernel binary
KERNEL=$(ls target/x86_64-cluu-kernel/debug/deps/kernel-*.elf)

# Start GDB
gdb $KERNEL

# In GDB:
(gdb) target remote :1234
(gdb) break _start
(gdb) continue
```

---

## Debug Workflow

### 1. Normal Run (No Debugging)

```bash
cargo xtask run
```

- Serial output to **stdio** (your terminal)
- No pause, starts immediately
- Uses existing images (no rebuild)
- Good for: Testing, watching output

If you need a fresh image first:
```bash
cargo xtask run --build
```

### 2. Debug Run (With GDB)

```bash
cargo xtask run --debug
```

- Serial output to **stdio** + **telnet:4321**
- **Pauses at startup** - waits for GDB
- GDB server on port **1234**
- Uses existing images (no rebuild)
- Good for: Debugging, step-through, breakpoints

---

## GDB Commands

### Connect to QEMU
```gdb
target remote :1234
```

### Set Breakpoints
```gdb
# Break at kernel entry
break _start

# Break at a function
break kmain

# Break at address (higher-half kernel)
break *0xFFFFFFFF80000000
```

### Control Execution
```gdb
continue        # Continue execution
step            # Step one instruction
next            # Step over function calls
finish          # Run until function returns
```

### Inspect State
```gdb
info registers  # Show all registers
info frame      # Current stack frame
backtrace       # Call stack
x/10i $rip      # Disassemble at current location
x/10x $rsp      # Examine stack
```

### Useful Settings
```gdb
# Show disassembly in Intel syntax
set disassembly-flavor intel

# Auto-display registers
layout regs

# Show source + assembly
layout split
```

---

## QEMU Configuration

### Debug Mode
```bash
qemu-system-x86_64 \
  -s                                           # GDB server :1234
  -S                                           # Pause CPU
  -bios /usr/share/ovmf/OVMF.fd               # UEFI
  -m 256M \
  -drive file=target/cluu.img,format=raw \
  -serial stdio \
  -serial telnet:localhost:4321,server,nowait # Telnet serial
  -display gtk \
  -no-reboot \
  -no-shutdown
```

### Normal Mode
```bash
qemu-system-x86_64 \
  -bios /usr/share/ovmf/OVMF.fd
  -m 256M \
  -drive file=target/cluu.img,format=raw \
  -serial stdio \
  -display gtk \
  -no-reboot \
  -no-shutdown
```

---

## Debugging Scenarios

### Scenario 1: Debug Kernel Initialization

```bash
# Terminal 1
cargo xtask run --debug

# Terminal 2
telnet localhost 4321

# Terminal 3
gdb target/x86_64-cluu-kernel/debug/deps/kernel-*.elf
(gdb) target remote :1234
(gdb) break _start
(gdb) continue
(gdb) step      # Step through initialization
```

### Scenario 2: Debug Page Fault

```bash
# Terminal 3 (GDB)
(gdb) break page_fault_handler
(gdb) continue
# When page fault occurs:
(gdb) info registers
(gdb) x/10i $rip
(gdb) backtrace
```

### Scenario 3: Debug Boot Process

```bash
# Set breakpoint at BOOTBOOT entry
(gdb) break *0xFFFFFFFF80000000  # Adjust to your entry point
(gdb) continue
(gdb) layout asm
(gdb) stepi     # Step one instruction at a time
```

### Scenario 4: Watch Serial Output

```bash
# Terminal 1
cargo xtask run --debug

# Terminal 2 - Watch kernel debug output
telnet localhost 4321

# Kernel uses klibcluu::kprintln!() which outputs here
```

---

## Troubleshooting

### GDB Can't Connect
```bash
# Check if QEMU GDB server is listening
netstat -ln | grep 1234

# Try connecting with full hostname
(gdb) target remote localhost:1234
```

### Telnet Connection Refused
```bash
# QEMU needs to be running first
# Check telnet server
netstat -ln | grep 4321
```

### No Serial Output
- Make sure kernel uses `kprintln!()` from klibcluu
- Check debug output is initialized
- Verify serial port configuration in kernel

### QEMU Window Doesn't Appear
- Check if QEMU is actually paused (it should be)
- UEFI might take time to initialize
- Check QEMU process is running: `ps aux | grep qemu`

---

## VSCode Debug Configuration

Create `.vscode/launch.json`:

```json
{
    "version": "0.2.0",
    "configurations": [
        {
            "name": "Debug CLUU Kernel",
            "type": "cppdbg",
            "request": "launch",
            "program": "${workspaceFolder}/target/x86_64-cluu-kernel/debug/deps/kernel-${command:pickKernelHash}.elf",
            "miDebuggerServerAddress": "localhost:1234",
            "miDebuggerPath": "/usr/bin/gdb",
            "cwd": "${workspaceFolder}",
            "setupCommands": [
                {
                    "description": "Enable pretty-printing",
                    "text": "-enable-pretty-printing",
                    "ignoreFailures": true
                }
            ],
            "preLaunchTask": "Start QEMU Debug"
        }
    ]
}
```

---

## Tips

1. **Use two monitors**: QEMU on one, GDB + telnet on the other
2. **Save GDB commands**: Create `.gdbinit` with common commands
3. **Log serial output**: `telnet localhost 4321 | tee kernel.log`
4. **Multiple breakpoints**: Set breakpoints in critical paths
5. **Watchpoints**: Use GDB watchpoints for memory corruption bugs

---

## Command Reference

| Command | Description |
|---------|-------------|
| `cargo xtask run` | Normal run (serial to stdio) |
| `cargo xtask run --debug` | Debug run (pause, GDB, telnet) |
| `make run` | Same as cargo xtask run |
| `make run-debug` | Same as cargo xtask run --debug |
| `telnet localhost 4321` | Connect to serial output |
| `gdb kernel.elf` | Start GDB with kernel |
| `target remote :1234` | Connect GDB to QEMU |

---

**Happy Debugging! 🐛🔍**
