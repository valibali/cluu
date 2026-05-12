//! /bin/login — interactive username/password authentication, then spawn shell.
//!
//! Reads username (echoed) and password (masked with '*') from fd 0, sends
//! a `PROCMGR_SESSION_LOGIN_LABEL` IPC to procmgr, and on success
//! posix_spawns `/bin/sh`.  On failure it re-prompts.
//!
//! NOTE(post-pts): posix_spawn here does NOT carry the authenticated uid into
//! the child session.  libcluu's `posix_spawn` has no uid-override parameter;
//! the auth'd-uid wiring will be added when the legacy tty path migrates
//! (Task 12) after the pts namespace lands (Task 13).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;

#[allow(unused_imports)]
use libcluu::runtime as _;

use libcluu::ipc::PROCMGR_SESSION_LOGIN_LABEL;
use libcluu::posix::{_read, _write};
use libcluu::syscall::debug_print as dp;
use libcluu::types::Message;

// ─── I/O helpers ─────────────────────────────────────────────────────────────

fn write_stdout(data: &[u8]) {
    let _ = unsafe { _write(1, data.as_ptr() as *const _, data.len()) };
}

/// Read one byte from stdin; returns the byte, or loops on EINTR / empty read.
fn read_byte() -> u8 {
    let mut b = [0u8; 1];
    loop {
        let n = unsafe { _read(0, b.as_mut_ptr() as *mut _, 1) };
        if n == 1 {
            return b[0];
        }
        // n==0 (EOF-like) or n<0 (error/EINTR): yield and retry.
        let _ = libcluu::syscall::yield_cpu();
    }
}

/// Read a line from stdin.
///
/// If `mask` is true, echoes '*' for each printable character instead of the
/// character itself (password mode).  Supports backspace (0x7f/0x08) and
/// Ctrl-C (0x03) line-clear.  Terminates on CR or LF.
fn read_line(mask: bool) -> Vec<u8> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let b = read_byte();
        match b {
            b'\r' | b'\n' => {
                write_stdout(b"\r\n");
                return buf;
            }
            0x7f | 0x08 => {
                if buf.pop().is_some() {
                    write_stdout(b"\x08 \x08");
                }
            }
            0x03 => {
                // Ctrl-C: clear the line visually and restart.
                for _ in 0..buf.len() {
                    write_stdout(b"\x08 \x08");
                }
                buf.clear();
            }
            0x20..=0x7e => {
                buf.push(b);
                if mask {
                    write_stdout(b"*");
                } else {
                    write_stdout(&[b]);
                }
            }
            _ => {}
        }
    }
}

// ─── Auth IPC ────────────────────────────────────────────────────────────────

/// Send username + password to procmgr via PROCMGR_SESSION_LOGIN_LABEL.
///
/// Returns `Ok(())` on success, `Err(errno)` on failure.
///
/// The instance_id field (words[1]) is 0 here — /bin/login does not yet have
/// a pts/VT instance id to report.  The legacy tty path will remain the owner
/// of instance-scoped sessions until Task 12 wires /bin/login into a real pts.
fn try_login(username: &[u8], password: &[u8]) -> Result<(), usize> {
    let procmgr_ep = libcluu::registry::lookup_service("procmgr:spawn")
        .ok_or(libcluu::errno::ENOSYS as usize)?;

    let mut payload: Vec<u8> = Vec::new();
    payload.extend_from_slice(username);
    payload.push(0);
    payload.extend_from_slice(password);
    payload.push(0);

    let msg = Message::new(
        PROCMGR_SESSION_LOGIN_LABEL,
        [payload.len(), 0 /* instance_id — see NOTE above */, 0, 0, 0, 0],
        2,
    );

    let mut reply = Message::new(0, [0; 6], 0);
    libcluu::ipc::call_with_payload(procmgr_ep, &msg, &payload, &mut reply)
        .map_err(|_| libcluu::errno::EIO as usize)?;

    if reply.words[0] != 0 {
        Err(reply.words[0])
    } else {
        Ok(())
    }
}

// ─── Shell spawn ─────────────────────────────────────────────────────────────

/// posix_spawn /bin/sh with inherited fd 0/1/2.
///
/// NOTE(post-pts): no uid override is passed — libcluu::posix_spawn has no
/// uid-override parameter.  The authenticated uid will be wired in Task 12
/// once the pts namespace lands.
fn spawn_shell() {
    extern "C" {
        // From libcluu_syscalls / libcluu posix layer.
        fn posix_spawn(
            pid: *mut i32,
            path: *const u8,
            file_actions: *const core::ffi::c_void,
            attrp: *const core::ffi::c_void,
            argv: *const *const u8,
            envp: *const *const u8,
        ) -> i32;
    }

    let path = b"/bin/sh\0";
    let arg0 = b"sh\0";
    let argv: [*const u8; 2] = [arg0.as_ptr(), core::ptr::null()];
    let mut child_pid: i32 = 0;

    let rc = unsafe {
        posix_spawn(
            &mut child_pid,
            path.as_ptr(),
            core::ptr::null(),
            core::ptr::null(),
            argv.as_ptr(),
            core::ptr::null(),
        )
    };
    if rc != 0 {
        write_stdout(b"login: failed to spawn shell\r\n");
    }
}

// ─── Entry point ─────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn main() -> i32 {
    loop {
        write_stdout(b"\r\nlogin: ");
        let user = read_line(false);

        write_stdout(b"password: ");
        let pass = read_line(true);

        match try_login(&user, &pass) {
            Ok(()) => {
                let _ = dp("login: user authenticated");
                spawn_shell();
                return 0;
            }
            Err(_) => {
                write_stdout(b"login incorrect\r\n");
            }
        }
    }
}
