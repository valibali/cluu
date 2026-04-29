#![no_std]
#![no_main]

// CLUU vi-like editor — scaffold (T1).
//
// This binary will grow into a small modal editor over the next ~35 tasks.
// For now it is a hello-world stub that proves the crate builds, links
// against libcluu, and is staged into the userdisk image.
//
// Pre-flight findings (recorded during Phase 0 / Task 0; full notes in
// docs/superpowers/plans/2026-04-29-editor.md §"T0 Findings"):
//
// - Raw-mode setup/teardown: no native libcluu helper. The shell uses
//   private `tty_get_lflag` / `tty_set_lflag` helpers in
//   userspace/shell/src/commands.rs:1629-1641 — single TTY_CTL_LABEL
//   IPC, subcmd=0 read / subcmd=1 write, words[4] carries the lflag.
//   Recommendation: promote to `libcluu::posix::tty::enter_raw` /
//   `leave_raw` (~30 LOC) before T10 so future TUI binaries can reuse.
// - TTY constants: TTY_LFLAG_ICANON = 0x02, TTY_LFLAG_ECHO = 0x08;
//   "raw mode" clears both. Source: userspace/shell/src/commands.rs:35-37.
// - Single-byte input: TTY_READ_LABEL IPC payloads, same as the shell
//   main loop at userspace/shell/src/main.rs:142-162. In raw mode the
//   line discipline emits 0x1B, '[', 'A' as separate raw bytes — the
//   editor's CSI parser owns decoding.
// - Console SGR / cursor escapes (userspace/console/src/renderer.rs:269-366):
//   supported: CSI r;c H, CSI A/B/C/D, CSI K, CSI 2 J, CSI 0 m, CSI
//   30..37 / 40..47 / 90..97 / 100..107 m. NOT supported (silently
//   consumed): CSI 7 m (reverse), CSI ?25 l/h (cursor hide/show),
//   CSI 39/49 m (default fg/bg), CSI 1 m (bold), CSI 4 m (underline).
//   Status line should use a colored background (e.g. CSI 47;30m)
//   instead of reverse video.
// - VFS rename for atomic save: VfsClient::rename(old, new) -> Result<()>
//   at userspace/libcluu/src/fs/client.rs:317-332.
// - Whole-file read pattern: copy `read_file_via_vfs` from
//   userspace/shell/src/shellrc.rs:103-162 (4KB chunks via
//   `vfs.read_grant`); editor will use a 1MB cap.
// - Whole-file write: VfsClient::write(file, offset, data) at
//   client.rs:251-261. Atomic-save sequence:
//     open_with(tmp, O_WRONLY|O_CREAT|O_TRUNC, 0o644)
//       → write(file, 0, &bytes) → close(file) → rename(tmp, final).
// - Harness keystroke injection: KEYSTROKE_COMMANDS only types whole
//   lines + Enter; POST_SENDKEY sends a single key. There is no path
//   for sending raw escape-sequence byte streams. T34 must drive the
//   editor from a parent shell that injects bytes via
//   send_with_payload(child_stdin, TTY_READ_LABEL, ...) — same pattern
//   as SuBuiltin at userspace/shell/src/commands.rs:3027-3060.
// - Open follow-up: VFS open(O_WRONLY|O_CREAT) sometimes times out on
//   shell's MemFs /tmp (memory item #80). Editor save-path harness
//   cases should target ext2-backed paths under /home/root until
//   resolved.

extern crate alloc;

mod buffer;
mod input;
mod insert;
mod mode;
mod normal;
mod piece;
mod prompt;
mod tty;
mod undo;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use libcluu::{debug_print, Result};

fn main_result() -> Result<()> {
    debug_print("edit: starting up")?;
    debug_print("edit: scaffold only — exits immediately")?;
    Ok(())
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    if let Err(err) = main_result() {
        let _ = debug_print(&format!("edit: fatal {:?}", err));
        return 1;
    }
    0
}
