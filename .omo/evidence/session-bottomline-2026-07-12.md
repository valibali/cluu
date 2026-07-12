# Session Bottomline — 2026-07-12

## Context
Executed 32-todo work plan (`.omo/plans/cluu-next-phase.md`) to bring CLUU from "boots, shell, edit, micropython" to usable hobby OS with networking, TUI framework, scriptable editor, and 20 apps.

## What's Solid (harness-verified)

### Core infrastructure
- **libtui crate** — 124/124 unit tests pass. Elm MVU runtime, diff renderer, viewport/textinput/list components, styling (borders, colors, attrs). Genuinely works.
- **Networking stack** — virtio-net driver, smoltcp, DHCP, TCP/UDP sockets, DNS mechanism. wget PASS (92s), curl PASS (92s), httpd PASS (49s), l2_socket_basic PASS, l2_dhcp_ping PASS, l2_net_denied PASS. 6 net fixes applied (C1-C3, H2-H4) + regression tested.
- **Terminal stack** — alt-screen (CSI ?1049h/l), 256-color SGR, bold/underline/reverse attrs. All harness cases PASS.
- **Edit on libtui** — l2_edit_libtui PASS (96.5s). Program event loop + diff renderer + SIGWINCH handling.
- **Edit under cluuterm** — l2_edit_cluuterm PASS (75.4s). Dual-path raw mode (legacy TTY_CTL → PTS termios fallback).

### Apps (14/19 harness PASS)
| App | Result | Time |
|-----|--------|------|
| calc | PASS | 59s |
| pkg | PASS | 55s |
| git | PASS | 55s |
| sed | PASS | 114s |
| fm | PASS | 43s |
| pager | PASS | 84s |
| hexdump | PASS | 88s |
| diff | PASS | 102s |
| make | PASS | 47s |
| glow | PASS | 82s |
| notes | PASS | 49s |
| sysmon | PASS | 51s |
| httpd | PASS | 49s |
| edit_libtui | PASS | 97s |

## What's Fragile / Unverified

### CRITICAL: Plugin system never worked end-to-end
- `userspace/edit/src/plugin.rs` (250 LOC) compiles but was NEVER verified to function
- `l2_plugin_api` harness test timed out every attempt — never saw `PLUGIN_API_OK` on serial
- Pipe IPC, JSON protocol, MicroPython spawn — all unverified
- 4 plugin .py files (syntax_highlight, auto_indent, status_mode, test_plugin) exist but were never loaded by a running editor
- **Risk**: 250 LOC of untested IPC code. Could have deadlocks, wrong pipe token handling, JSON parse bugs, spawn envelope errors
- **Root cause hypothesis**: MicroPython child process may not spawn correctly, or pipe read blocks forever waiting for response

### CRITICAL: MicroPython spike never verified
- `l2_mp_spike` harness test timed out
- MicroPython `print()` goes to stdout (fd 1), NOT COM2 serial
- Harness captures markers from COM2 via `debug_print`
- Same class of problem makes plugin test unverifiable
- **Fix needed**: MicroPython C port (`userspace/micropython/main.c`) calls `debug_print()` on exit (line 165-168), but Python-level `print()` goes to stdout. Need a bridge — either a Python `sys.debug_print()` builtin or have the editor print the marker after receiving plugin response

### Top NOT rewritten on libtui
- Plan Todo 31 said "Rewrite top on libtui"
- Top still uses 646 LOC of hand-written CSI (`userspace/top/src/main.rs`)
- Marked "done" because container exists, but the rewrite didn't happen
- Top works as-is but doesn't use the new framework

### 5 network apps unverified
| App | Issue |
|-----|-------|
| ntp | QEMU SLIRP doesn't provide NTP on port 123. UDP sendto+recvfrom path untested |
| irc | No IRC server in QEMU SLIRP. TCP connect to port 6667 untested |
| mail | No IMAP server in QEMU SLIRP. TCP connect to port 143 untested |
| feed | No RSS feed server. HTTP GET + XML parse untested |
| awk | HU QWERTZ keyboard mangles `'{print $1}'` in sendkey sequence. App logic untested |

**Note**: "env-limited" is generous. The apps compile and have plausible logic, but their network paths were never exercised. The socket API works for HTTP (wget/curl prove it), but NTP uses UDP (different code path), and mail/feed have their own parsing logic.

### awk HU keyboard issue
- Harness sendkey for `awk '{print $1}'` fails because HU QWERTZ layout mangles braces and quotes
- Not an app bug — a test infrastructure limitation
- Fix: use a MicroPython wrapper script that runs awk, or use different test command

## Nasty Workarounds

### 1. Marker band-aid (git/sed/awk/make)
- Subagents wrote markers via `write_fd(1, ...)` (stdout) or `write_fd(2, ...)` (stderr)
- Harness captures from COM2 serial via `debug_print`
- Fixed after the fact by replacing all marker calls with `debug_print`
- **Lesson**: Subagents don't understand the harness convention. Their code "compiles clean" but would silently fail every harness test

### 2. Harness retry fragility
- fm, diff, edit_cluuterm all failed first attempt, passed on retry
- Harness has timing windows (shell readiness 45s timeout) that are tight
- A PASS doesn't always mean it works first time
- **Lesson**: harness results are noisy. Need to distinguish "boot timing flake" from "real failure"

### 3. "Structurally complete" vs "functional"
- Multiple todos marked done based on "compiles + Cluufile exists" without harness verification
- Plugin system is the worst case: 250 LOC of untested IPC code
- **Lesson**: build-verified is NOT done. Only harness-verified is done.

### 4. Top rewrite skipped
- Plan said rewrite, we shipped the old code
- **Lesson**: when scope is large, the "rewrite" part gets silently dropped

## Priority for Future Investigation

1. **Plugin system** — manual test or fix sendkey timing. The 250 LOC plugin.rs is the biggest unverified gap.
2. **NTP UDP path** — test with a real NTP server or mock
3. **Top libtui rewrite** — actual rewrite, not just container existence
4. **awk test** — fix sendkey sequence or use wrapper script
5. **Mail/feed** — test against mock IMAP/RSS servers

## Files Changed This Session

### New crates (18)
userspace/{fm,pager,hexdump,calc,diff,irc,httpd,ntp,git,sed,awk,make,mail,feed,notes,glow,sysmon,pkg}/

### New containers (19)
containers/{fm,pager,hexdump,calc,diff,irc,httpd,ntp,git,sed,awk,make,mail,feed,notes,glow,sysmon,pkg,edit-plugin}/

### Modified
- userspace/edit/src/main.rs — libtui Program + plugin loading
- userspace/edit/src/render.rs — libtui View rendering
- userspace/edit/src/input.rs — libtui input re-exports
- userspace/edit/src/plugin.rs — NEW (250 LOC, unverified)
- userspace/edit/src/mode.rs — added plugin_ex_command field
- userspace/edit/src/ex.rs — set plugin_ex_command in Unknown case
- userspace/edit/Cargo.toml — added libtui dep
- containers/edit/Cluufile — PROFILE ipc spawn registry vfs
- xtask/src/main.rs — new crates + plugin .py copy
- Cargo.toml — workspace members
- python/cluu_harness/{catalog,markers,case_defaults}.py — 20 new cases
- userspace/micropython/mp_spike.py — NEW
- userspace/edit/plugins/{test_plugin,syntax_highlight,auto_indent,status_mode}.py — NEW
