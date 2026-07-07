# Debugging

CLUU runs under QEMU with two serial ports and an optional GDB stub. Debugging
splits into two paths: interactive single-stepping with `cargo xtask run --debug`,
and automated attach through the Python harness's GDB modes. Both use the same
QEMU ports and the same ELF artifacts.

## QEMU debug mode

`cargo xtask run --debug` (or the `make run-debug` alias) launches QEMU with
three additions over a normal run:

- `-s` opens a GDB server on `localhost:1234`.
- `-S` pauses the CPU at startup. Execution begins only when GDB sends
  `continue`.
- A second serial port is wired to `telnet:localhost:4321,server,nowait`, so
  you can watch kernel output without sharing GDB's terminal.

Add `--build` to rebuild images first: `cargo xtask run --build --debug`
(aliased `make run-debug-build`). Without `--build`, QEMU reuses the existing
`target/cluu.img` and `target/userdisk.img`.

Normal runs (`cargo xtask run`, no `--debug`) send serial to stdio only, start
immediately, and expose no GDB stub.

## Connecting GDB

The kernel ELF carries full symbols:

```bash
KERNEL=$(ls target/x86_64-cluu-kernel/debug/deps/kernel-*.elf)
gdb "$KERNEL"
```

Inside GDB:

```gdb
target remote :1234
break _start
continue
```

Userspace binaries live under `target/x86_64-cluu-user/debug/<name>.elf`. Load
the relevant ELF before continuing past kernel init if you want breakpoints in
a userspace service.

### Higher-half entry

The kernel is mapped at the top of the address space. The boot entry point sits
at `0xFFFFFFFF80000000`. To break before the Rust entry, set the breakpoint by
address:

```gdb
break *0xFFFFFFFF80000000
```

### Common GDB commands

```gdb
continue              # resume execution
step                  # step one source line, into calls
next                  # step one source line, over calls
finish                # run until current function returns
info registers        # dump GPRs
info frame            # current frame summary
backtrace             # call stack
x/10i $rip            # disassemble 10 instructions at $rip
x/16x $rsp            # examine 16 stack words
set disassembly-flavor intel
layout regs           # TUI: registers pane
layout split          # TUI: source + asm
```

Watchpoints (`watch <expr>`) catch memory-corruption bugs. A `.gdbinit` file
with `set disassembly-flavor intel` and your common breakpoints saves typing
across sessions.

## Reading the serial log

The kernel writes diagnostics to COM2 via `klibcluu::kprintln!`. Userspace
services write through `debug_print!`, which lands on the same serial line. Two
ways to read it:

- **Normal run**: serial is QEMU's stdio. Output appears inline in the
  terminal that launched QEMU.
- **Debug run**: COM2 is duplicated to the telnet server on port 4321. Connect
  with `telnet localhost 4321`. To capture a log, pipe telnet through tee:
  `telnet localhost 4321 | tee kernel.log`.

The harness writes the same stream to `/tmp/cluu-serial-com2.log` by default
(override with `SERIAL_LOG=path`). That file is the canonical source for
marker matching and post-run forensics.

Serial is a live stream, not a snapshot. A short `RUN_WAIT` can kill QEMU
mid-boot and leave the log truncated. Treat a missing marker as "did not run
long enough" first, then as a real failure.

## Harness GDB modes

The Python harness can drive GDB for you. Set `QEMU_GDB=1` to enable the
stub and pick a mode with `HARNESS_GDB_MODE`:

| Mode | Behavior |
|------|----------|
| `manual` | Print attach instructions, wait up to `HARNESS_GDB_MANUAL_TIMEOUT` (default 120s) for serial activity as the resume signal. |
| `auto-continue` | Attach, detach, quit. The target resumes on detach. Hands-off resume of a paused boot. |
| `script` | Run a GDB script file (set via `HARNESS_GDB_SCRIPT`) against the paused target, then quit. Requires `--batch` unless `HARNESS_GDB_BATCH=0`. |

Supporting env vars:

| Var | Default | Purpose |
|-----|---------|---------|
| `QEMU_GDB` | unset | Enable the GDB stub (`1` = on). |
| `QEMU_GDB_SERVER` | unset | Start QEMU with `-s` only, no `-S`. Attach to a running guest without pausing. |
| `HARNESS_GDB_BIN` | `gdb` | GDB binary path. |
| `HARNESS_GDB_TARGET` | `localhost:1234` | Stub address. |
| `HARNESS_GDB_TIMEOUT` | `20` | Seconds to wait for the stub port to accept a connection. |
| `HARNESS_GDB_SYMBOL` | unset | ELF path passed to GDB for symbols. |

Example: resume a paused boot without manual interaction.

```bash
QEMU_GDB=1 HARNESS_GDB_MODE=auto-continue \
  python -m cluu_harness --case l2_login --no-build
```

Example: run a GDB script that sets a breakpoint, continues, and dumps
registers on hit.

```bash
QEMU_GDB=1 HARNESS_GDB_MODE=script \
  HARNESS_GDB_SCRIPT=/path/to/dump_on_fault.gdb \
  python -m cluu_harness --case l2_login --no-build
```

## Debug scenarios

### Kernel initialization

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
(gdb) step               # walk through init one line at a time
```

The init order is documented in the Boot Flow chapter. UART comes first, so
serial output begins before the logger is wired.

### Page fault

Set a breakpoint on the fault entry, then inspect the faulting frame.

```gdb
break page_fault_handler
continue
# after the trap fires:
info registers
x/10i $rip
backtrace
```

The fault handler classifies the address against the per-process layout
(stack region, heap region, mmap region) and either resolves it or kills the
thread. `cr2` holds the faulting virtual address.

### Userspace service

Load the service ELF, set a breakpoint at its entry, and let the kernel boot
past init.

```gdb
symbol-file target/x86_64-cluu-user/debug/vfs.elf
break vfs_main
continue
```

For services spawned by procmgr after login, set the breakpoint before the
harness sends credentials so the breakpoint is armed before spawn.

### Boot hang

If the kernel stalls before serial comes up, break at the raw entry and
single-step:

```gdb
break *0xFFFFFFFF80000000
continue
layout asm
stepi                 # one instruction at a time
```

If serial never appears, check QEMU is actually running (`ps aux | grep qemu`)
and that OVMF firmware loaded. UEFI init can take a few seconds before the
kernel entry.

## Troubleshooting

**GDB can't connect.** Check the stub is listening: `ss -ltn | grep 1234` (or
`netstat -ltn | grep 1234`). Try `target remote localhost:1234` with the full
hostname. The stub starts only with `-s`, which `--debug` adds.

**Telnet connection refused.** QEMU must be running first. The telnet server
uses `nowait`, so it accepts connections as soon as QEMU starts. Check
`ss -ltn | grep 4321`.

**No serial output.** Confirm the kernel calls `kprintln!` (kernel) or
`debug_print!` (userspace). Both write to COM2. On a normal run, COM2 is stdio.
On a debug run, COM2 is the telnet port. If the log file is empty, the boot may
have died before UART init.

**QEMU window doesn't appear.** UEFI init takes a moment. If QEMU is paused
(`-S`), the window opens frozen until GDB sends `continue`. Check the QEMU
process is alive.

## VSCode

A `launch.json` entry for the cppdbg adapter wires the stub into the editor:

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

Pair it with a `tasks.json` that runs `cargo xtask run --debug` as the
`preLaunchTask`.

## Command reference

| Command | Effect |
|---------|--------|
| `cargo xtask run` | Normal run, serial to stdio. |
| `cargo xtask run --build` | Rebuild images, then normal run. |
| `cargo xtask run --debug` | Pause for GDB on :1234, telnet serial on :4321. |
| `cargo xtask run --build --debug` | Rebuild, then debug run. |
| `make run-debug` | Alias for `cargo xtask run --debug`. |
| `make run-debug-build` | Alias for `cargo xtask run --build --debug`. |
| `telnet localhost 4321` | Connect to the debug-run serial mirror. |
| `gdb <kernel.elf>` | Start GDB with kernel symbols. |
| `target remote :1234` | Connect GDB to the QEMU stub. |
| `QEMU_GDB=1 HARNESS_GDB_MODE=auto-continue python -m cluu_harness ...` | Harness-driven resume. |
