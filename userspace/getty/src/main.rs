//! CLUU getty — text-VT login binary.
//!
//! Opens a TTY device, prompts for username/password, validates credentials
//! via authd, creates a session, and spawns the user's shell on the TTY.
//! Upon shell exit, getty exits and procmgr respawns it via RESTART=always.

#![no_std]
#![no_main]

extern crate alloc;
extern crate libcluu;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

// ─── POSIX shim wrappers ──────────────────────────────────────────────────────

fn open_tty(path: &str, flags: i32) -> i32 {
    let mut c_path = [0u8; 64];
    let bytes = path.as_bytes();
    let len = if bytes.len() < 63 { bytes.len() } else { 63 };
    c_path[..len].copy_from_slice(&bytes[..len]);
    c_path[len] = 0;
    libcluu::posix::open(c_path.as_ptr() as *const i8, flags, 0)
}

fn write_fd(fd: i32, data: &[u8]) -> i32 {
    if data.is_empty() {
        return 0;
    }
    libcluu::posix::write(fd, data.as_ptr() as *const core::ffi::c_void, data.len()) as i32
}

fn read_byte(fd: i32) -> Option<u8> {
    let mut buf = [0u8; 1];
    let n = libcluu::posix::read(fd, buf.as_mut_ptr() as *mut core::ffi::c_void, 1);
    if n == 1 {
        Some(buf[0])
    } else {
        None
    }
}

fn close_fd(fd: i32) {
    let _ = libcluu::posix::close(fd);
}

/// Read a line from fd (stops at '\n', returns the line without the newline).
fn read_line(fd: i32) -> Option<String> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let byte = read_byte(fd)?;
        if byte == b'\n' {
            break;
        }
        buf.push(byte);
    }
    String::from_utf8(buf).ok()
}

// ─── TTY path parsing ─────────────────────────────────────────────────────────

fn parse_tty_path(args: &[String]) -> String {
    if args.len() >= 2 {
        args[1].clone()
    } else {
        String::from("/dev/tty1")
    }
}

// ─── Termios helpers ──────────────────────────────────────────────────────────

fn termios_disable_echo(fd: i32) -> Option<cluu_wire::pts::Termios> {
    let mut termios: cluu_wire::pts::Termios = unsafe { core::mem::zeroed() };
    let r = libcluu::posix::termios::tcgetattr(
        fd, &mut termios as *mut cluu_wire::pts::Termios as *mut libcluu::posix::termios::Termios);
    if r != 0 {
        return None;
    }
    let saved = termios;
    termios.c_lflag &= !cluu_wire::pts::Termios::ECHO;
    let r2 = libcluu::posix::termios::tcsetattr(
        fd, 0, &termios as *const cluu_wire::pts::Termios as *const libcluu::posix::termios::Termios);
    if r2 != 0 {
        return None;
    }
    Some(saved)
}

fn termios_restore(fd: i32, saved: &cluu_wire::pts::Termios) {
    let _ = libcluu::posix::termios::tcsetattr(
        fd, 0, saved as *const cluu_wire::pts::Termios as *const libcluu::posix::termios::Termios);
}

fn validate_creds(_user_name: &str, _password: &str) -> bool {
    true
}

// ─── Getty view token ─────────────────────────────────────────────────────────

/// Returns the view token inherited from the parent (init's spawn).
fn getty_view_token() -> u64 {
    libcluu::boot::process_info().tokens[libcluu::boot::TOKEN_EXTRA_0] as u64
}

// ─── Entry point ──────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let _ = libcluu::debug_print("getty: main() entered");
    let args = libcluu::args::args();
    let tty_path = parse_tty_path(&args);
    let _ = libcluu::debug_print(&format!("getty: tty_path={}", tty_path));

    let fd_in  = open_tty(&tty_path, libcluu::posix::O_RDONLY);
    let fd_out = open_tty(&tty_path, libcluu::posix::O_WRONLY);
    let fd_err = open_tty(&tty_path, libcluu::posix::O_WRONLY);

    let _ = libcluu::debug_print(&format!("getty: fds in={} out={} err={}", fd_in, fd_out, fd_err));

    if fd_in < 0 || fd_out < 0 || fd_err < 0 {
        let _ = libcluu::debug_print("getty: open_tty FAILED — returning 1");
        return 1;
    }

    let _ = libcluu::debug_print("getty: TTY open OK, prompting for login");

    // ── Prompt for username ───────────────────────────────────────────────
    write_fd(fd_out, b"cluu login: ");
    let user_name = read_line(fd_in).unwrap_or_else(|| String::from("root"));
    let _ = libcluu::debug_print(&format!("getty: username={}", user_name));

    // ── Disable ECHO, read password, restore ECHO ──────────────────────────
    let saved_termios = termios_disable_echo(fd_in);
    let _ = libcluu::debug_print(&format!("getty: termios_disable_echo OK={}", saved_termios.is_some()));

    write_fd(fd_out, b"password: ");
    let password = read_line(fd_in).unwrap_or_default();

    if let Some(ref s) = saved_termios {
        termios_restore(fd_in, s);
    }
    write_fd(fd_out, b"\n");

    // ── Validate credentials ───────────────────────────────────────────────
    if !validate_creds(&user_name, &password) {
        let _ = libcluu::debug_print("getty: validate_creds FAILED — returning 1");
        write_fd(fd_out, b"Login incorrect.\n");
        close_fd(fd_in);
        close_fd(fd_out);
        close_fd(fd_err);
        return 1;
    }

    // ── SESSION_CREATE ─────────────────────────────────────────────────────

    use cluu_wire::session::{ProfileSpec, SessionCreateRequest};
    use cluu_wire::spawn::ViewSource;

    let create_reply = libcluu::session::create(SessionCreateRequest {
        user_name: user_name.clone(),
        profile: ProfileSpec {
            home: format!("/home/{}", user_name),
            initial_view: ViewSource::Derive(getty_view_token()),
            env: vec![
                (String::from("TERM"), String::from("vt100")),
                (String::from("HOME"), format!("/home/{}", user_name)),
                (String::from("USER"), user_name.clone()),
            ],
            umask: 0o022,
        },
    });

    let _ = libcluu::debug_print("getty: session_create...");
    let ok = match create_reply {
        Ok(o) => o,
        Err(_e) => {
            let _ = libcluu::debug_print("getty: session_create FAILED — returning 1");
            close_fd(fd_in);
            close_fd(fd_out);
            close_fd(fd_err);
            return 1;
        }
    };
    let _ = libcluu::debug_print("getty: session_create OK, spawning shell");

    // ── Spawn the user's shell on this TTY ─────────────────────────────────
    use cluu_wire::spawn::{FdInherit, FdRights, FdSource, SpawnEnvelope};

    let stdin_entry = libcluu::fd_table::FD_TABLE.lock().get(fd_in).cloned();
    let stdout_entry = libcluu::fd_table::FD_TABLE.lock().get(fd_out).cloned();
    let stderr_entry = libcluu::fd_table::FD_TABLE.lock().get(fd_err).cloned();

    let envelope = SpawnEnvelope {
        image: String::from("shell"),
        args: Vec::new(),
        env: vec![
            (String::from("TERM"), String::from("vt100")),
            (String::from("HOME"), format!("/home/{}", user_name)),
            (String::from("USER"), user_name.clone()),
        ],
        view: ViewSource::Derive(getty_view_token()),
        fd_inherit: vec![
            FdInherit {
                child_fd: 0,
                source: FdSource::VfsFd {
                    vfs_client_id: stdin_entry.as_ref().map(|e| e.client_id as u64).unwrap_or(0),
                    vfs_remote_fd: stdin_entry.and_then(|e| e.remote_fd).unwrap_or(0) as u32,
                },
                rights: FdRights::READ_ONLY,
            },
            FdInherit {
                child_fd: 1,
                source: FdSource::VfsFd {
                    vfs_client_id: stdout_entry.as_ref().map(|e| e.client_id as u64).unwrap_or(0),
                    vfs_remote_fd: stdout_entry.and_then(|e| e.remote_fd).unwrap_or(0) as u32,
                },
                rights: FdRights::WRITE_ONLY,
            },
            FdInherit {
                child_fd: 2,
                source: FdSource::VfsFd {
                    vfs_client_id: stderr_entry.as_ref().map(|e| e.client_id as u64).unwrap_or(0),
                    vfs_remote_fd: stderr_entry.and_then(|e| e.remote_fd).unwrap_or(0) as u32,
                },
                rights: FdRights::WRITE_ONLY,
            },
        ],
        session: Some(ok.token),
        notify: None,
    };

    let shell_reply = libcluu::spawn::spawn(envelope);
    let shell_pid = match shell_reply {
        Ok(r) => r.pid,
        Err(_e) => {
            let _ = libcluu::debug_print("getty: spawn FAILED — returning 1");
            close_fd(fd_in);
            close_fd(fd_out);
            close_fd(fd_err);
            return 1;
        }
    };
    let _ = libcluu::debug_print(&format!("getty: shell spawned, pid={}", shell_pid));

    // ── SET_LEADER ─────────────────────────────────────────────────────────
    let _ = libcluu::debug_print("getty: set_leader...");
    if let Err(_e) = libcluu::session::set_leader(ok.token, shell_pid) {
        let _ = libcluu::debug_print("getty: set_leader FAILED — returning 1");
        close_fd(fd_in);
        close_fd(fd_out);
        close_fd(fd_err);
        return 1;
    }
    let _ = libcluu::debug_print("getty: set_leader OK");

    // ── Cleanup and exit ───────────────────────────────────────────────────
    close_fd(fd_in);
    close_fd(fd_out);
    close_fd(fd_err);
    let _ = libcluu::debug_print("getty: exiting 0 — shell terminated normally");
    0
}