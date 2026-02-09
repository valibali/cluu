# Coreutils Porting Analysis for CLUU

**Date:** 2025-02-08
**Target:** uutils/coreutils (Rust implementation) and GNU coreutils
**OS:** CLUU x86_64 microkernel with newlib (x86_64-cluu-elf)

---

## Executive Summary

CLUU can run ~25 coreutils utilities **today** with its current POSIX layer. Another
~20 need minor syscall additions (getcwd, readdir, getenv, mkdir, unlink). Full
coreutils coverage (~77 utils) requires terminal control, signals, and process
management — roughly 3 tiers of incremental work.

**Recommendation:** Target **uutils/coreutils** (Rust), not GNU coreutils. Rationale:
- Same language as the kernel — single toolchain
- Individual utility compilation via Cargo features
- Redox OS (microkernel) has already done this — precedent exists
- No autoconf/configure headaches

---

## Current CLUU POSIX Inventory

Functions currently implemented in `userspace/libcluu/src/posix/`:

| Category | Implemented | Notes |
|----------|-------------|-------|
| **File I/O** | `_open`, `_close`, `_read`, `_write`, `_lseek` | VFS-backed via IPC |
| **Stat** | `_fstat`, `_stat`, `_isatty` | Correct newlib struct layout |
| **Process** | `_exit`, `_getpid`, `_kill`, `waitpid`, `posix_spawn` | No fork/exec |
| **Memory** | `_sbrk`, `brk` | Demand-paged heap 0x0080_0000–0x4000_0000 |
| **Time** | `_gettimeofday`, `clock_gettime`, `_times`, `time`, `sleep`, `usleep` | TSC-based, hardcoded 1GHz |
| **Stubs** | `_fork` (ENOSYS), `_execve` (ENOSYS), `_link` (ENOSYS), `_unlink` (ENOSYS) | Return errors |
| **Reentrant** | All `_*_r` forms | Delegate to non-reentrant versions |

**NOT implemented:** getcwd, readdir/opendir/closedir, getenv/setenv, environ,
mkdir, rmdir, unlink (real), rename, link, symlink, readlink, chmod, chown,
access, dup/dup2, pipe, fcntl, mmap, signals (sigaction), tcgetattr/tcsetattr,
ioctl, uname, gethostname, getpwuid/getgrnam, nanosleep (real — current sleep
is busy-yield).

---

## uutils/coreutils Build Requirements

1. **Rust `std` library** compiled for `x86_64-cluu-elf` target
   - Requires `cargo -Z build-std=std,core,alloc,panic_abort`
   - Requires a JSON target spec (`x86_64-cluu-elf.json`)
   - std needs: libc backing (newlib), thread primitives, fs/io/env support

2. **`libc` crate** — needs a fork with CLUU type definitions
   - Model after Redox OS's libc crate additions
   - Map to newlib's actual type sizes (see MEMORY.md §3)

3. **`nix` crate** — biggest dependency headache
   - Many utils use it for POSIX wrappers
   - Options: fork with `target_os = "cluu"` support, or patch uucore to bypass
   - Some utils gate `nix` behind cfg flags — check per-util

4. **`uucore`** — shared foundation all utils depend on
   - Error handling, arg parsing (clap), path utilities, platform abstractions
   - Has `#[cfg(unix)]` / `#[cfg(target_os = "...")]` conditionals

5. **Individual compilation works:**
   ```bash
   cargo build -p uu_echo                    # single util
   cargo build --no-default-features --features "feat_echo feat_cat"  # subset
   ```

---

## Tier Classification

### Tier 1: Works Today (~25 utilities)

Only needs: read, write, open, close, stat, isatty, exit — **all implemented**.

| Utility | Syscalls Used |
|---------|---------------|
| echo | write |
| yes | write |
| true / false | exit |
| printf | write |
| base32 / base64 | read, write |
| basename / dirname | write (pure string ops) |
| seq | write (arithmetic) |
| factor | write (arithmetic) |
| expr | read, write |
| rev | read, write |
| wc | read, write |
| head | read, write |
| cat (simple) | read, write, open, close |
| tee | read, write, open |
| fold | read, write |
| nl | read, write |
| expand / unexpand | read, write |
| paste | read, write, open |
| sum / cksum / md5sum / sha*sum | read, write |
| tac | read, write (+ lseek or buffering) |
| truncate | ftruncate (if implemented) |

### Tier 2: Minor Additions (~20 utilities)

Needs: **getcwd, opendir/readdir/closedir, getenv, lseek, access, mkdir, rmdir,
unlink, readlink, dup/dup2, utimensat**

| Utility | Additional Syscalls |
|---------|--------------------|
| ls (basic) | readdir, stat, isatty, getcwd, getenv(COLUMNS) |
| pwd | getcwd |
| env / printenv | getenv, environ |
| test / [ | stat, access |
| cut / tr / sort / uniq | getenv (locale) |
| comm / join | basic file I/O |
| shuf | read, /dev/urandom |
| numfmt | arithmetic |
| tail (no -f) | lseek |
| split / csplit | open (multiple outputs) |
| mkdir | mkdir() |
| rmdir | rmdir() |
| rm | unlink, readdir (for -r) |
| touch | utimensat, open(O_CREAT) |
| realpath / readlink | readlink, getcwd |
| mktemp | open(O_EXCL), getenv(TMPDIR) |
| dd (basic) | lseek, open |

**Implementation effort:** ~2-3 days. Most are straightforward VFS operations
routed through IPC. `readdir` is the biggest piece (needs VFS protocol extension
and newlib `dirent.h` support).

### Tier 3: Moderate Work (~20 utilities)

Needs: **rename, link/symlink, chmod/chown, signals (sigaction/kill), terminal
ioctl (tcgetattr/tcsetattr/TIOCGWINSZ), pipe, nanosleep, uname, statvfs**

| Utility | Additional Syscalls |
|---------|--------------------|
| ls -l (full) | getpwuid, getgrgid, TIOCGWINSZ |
| cp (with -p) | chmod, chown, symlink, utimensat |
| mv | rename (cross-device: cp+rm) |
| ln | link, symlink |
| chmod | chmod/fchmod |
| stat (command) | full stat fields, getpwuid |
| date | clock_gettime, strftime, settimeofday |
| stty | tcgetattr, tcsetattr |
| tty | ttyname |
| dd (full) | signals (SIGUSR1 for progress) |
| tail -f | inotify or poll |
| sleep (real) | nanosleep |
| sync | sync/fsync |
| uname / arch | uname() |
| hostname | gethostname() |
| nproc | sysconf or /proc/cpuinfo |
| df / du | statvfs, recursive readdir+stat |
| install | cp + mkdir + chmod + chown |
| kill (command) | kill() with signal mapping |
| mkfifo / mknod | mkfifo, mknod |

**Implementation effort:** ~2-4 weeks. Signal handling and terminal control are
the two biggest subsystems. Both require kernel-side support (signal delivery
mechanism, TTY ioctl forwarding).

### Tier 4: Major Work or N/A (~12 utilities)

Needs: **fork/exec, user/group database, chroot, scheduler integration**

| Utility | Why It's Hard |
|---------|---------------|
| timeout | fork + exec + waitpid + signals + timer |
| nohup | fork + exec + signal manipulation |
| nice / renice | scheduler priority (kernel integration) |
| su | setuid/setgid, PAM |
| chown/chgrp (by name) | getpwnam/getgrnam (/etc/passwd) |
| id / whoami / groups | getuid, getpwuid, group enumeration |
| who / w / users | utmp/wtmp database |
| chroot | chroot() + process creation |
| pinky / logname | utmp, getlogin |
| runcon | SELinux (irrelevant) |
| stdbuf | LD_PRELOAD (irrelevant) |

**Note:** CLUU's microkernel has threads, not processes. fork/exec requires the
procmgr's posix_spawn path. Several of these utils (su, who, runcon, stdbuf)
are irrelevant for a hobby OS.

---

## Summary Table

| Tier | Count | Status | Effort |
|------|-------|--------|--------|
| Tier 1 (Today) | ~25 | All syscalls exist | Rust std port only |
| Tier 2 (Minor) | ~20 | Need ~10 new POSIX functions | 2-3 days |
| Tier 3 (Moderate) | ~20 | Need signals + terminal + filesystem ops | 2-4 weeks |
| Tier 4 (Major/N/A) | ~12 | Need fork/exec, user DB, etc. | Months or skip |
| **Total** | **~77** | | |

---

## Blockers (Ordered by Priority)

### 1. Rust `std` for x86_64-cluu-elf (CRITICAL)

This is the single biggest blocker. Without Rust `std`, no uutils code compiles.

**Steps:**
1. Create `x86_64-cluu-elf.json` Rust target spec
2. Build std with `cargo -Z build-std` against newlib sysroot
3. Implement missing std backing: `std::fs` → VFS IPC, `std::env` → getenv,
   `std::io` → fd I/O, `std::process` → posix_spawn

**Reference:** Redox OS's std port (`relibc` + custom target spec).

### 2. `libc` Crate Fork (CRITICAL)

The Rust `libc` crate needs CLUU-specific type definitions:
- struct stat, dirent, termios, etc.
- Type aliases matching newlib's sizes (dev_t=short, etc.)
- Function declarations for all implemented POSIX functions

### 3. `nix` Crate (HIGH)

Many utils depend on `nix` for safe POSIX wrappers. Options:
- Fork with `target_os = "cluu"` support
- Patch uucore to use raw libc calls
- Feature-gate nix usage per utility

### 4. Directory Operations (MEDIUM)

`readdir`/`opendir`/`closedir` are needed by ls, rm -r, cp -r, du, etc.
Requires VFS protocol extension for `READDIR` operation.

### 5. Environment Variables (MEDIUM)

`getenv`/`setenv`/`environ` — newlib provides these but `environ` must be
populated at process startup. Currently not implemented.

---

## Recommended Approach

1. **Phase A — Rust std port** (prerequisite for everything)
   - Target spec, build-std, libc crate fork
   - Validate with `uu_echo` as first utility

2. **Phase B — Tier 1 utilities** (~25 utils, no new syscalls)
   - Build and test: echo, cat, head, wc, base64, seq, factor, etc.
   - Fix any std/libc integration issues discovered

3. **Phase C — Tier 2 syscalls** (getcwd, readdir, getenv, mkdir, unlink, etc.)
   - Implement in libcluu posix layer
   - Build and test: ls, pwd, env, mkdir, rm, touch, test

4. **Phase D — Tier 3 subsystems** (signals, terminal, filesystem mutation)
   - Kernel-side signal delivery
   - TTY raw mode + ioctl support
   - Build and test: cp, mv, chmod, date, stty

5. **Skip Tier 4** unless specifically needed — most are irrelevant for a
   hobby microkernel.

---

## GNU Coreutils Comparison

GNU coreutils would be **significantly harder** to port:
- C codebase with heavy autoconf/automake dependency
- Assumes glibc — many GNU extensions (getopt_long, etc.)
- Cross-compilation requires a full toolchain (gcc cross-compiler for cluu)
- No modular build — all-or-nothing configure/make
- Harder to debug (C vs Rust safety)

uutils is the clear winner for CLUU.

---

## Redox OS Precedent

Redox OS (microkernel, Rust) has successfully ported uutils. Key differences:
- Redox has `relibc` (custom libc), CLUU has newlib
- Redox has a more complete POSIX layer (signals, pipes, etc.)
- Redox's `libc` crate fork is the closest reference implementation
- Study: https://gitlab.redox-os.org/redox-os/libc (their libc crate fork)

---

## Shell Built-ins Already in CLUU

The CLUU shell already implements these as built-ins, reducing the urgency
for their coreutils equivalents:

- echo, cat, ls, env, pwd, mkdir, sleep, time, help, ps, cd, exit, clear,
  history, spawn, kill
