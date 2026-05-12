//! `cluuterm` — CLUU terminal emulator binary.
//!
//! Task 14: scaffold quality — performs WIN_REGISTER with the compositor,
//! PTS_REGISTER with VFS, then posix_spawns /bin/login with fd 0/1/2 bound
//! to the pts node.  The real recv loop (PTS_READ/WRITE/CLOSED, FRAME_READY,
//! INPUT_FORWARD) is filled in by Tasks 15-17.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

mod input;
mod render;
mod tty_backend;

use libcluu::boot::{process_info, space_token, TOKEN_IPC};
use libcluu::ipc::{COMP_WIN_REGISTER_LABEL, COMP_WIN_REGISTER_REPLY, PTS_REGISTER_LABEL};
use libcluu::types::IpcFlags;
use libcluu::syscall::MAP_FRAME_TOKEN;
use libcluu::types::Message;
use libcluu::window_shm::WindowShm;
use libcluu::{debug_print, registry, syscall};

// ─── Layout constants ─────────────────────────────────────────────────────────

const COLS: usize = 80;
const ROWS: usize = 24;

// Request cell dimensions: terminal interior (COLS×ROWS) + 1-cell chrome on
// each side. The compositor's WIN_REGISTER protocol expects cell counts, not
// pixel dimensions. Chrome is 1 cell wide/tall on each edge.
const WIN_W: u32 = (COLS + 2) as u32;
const WIN_H: u32 = (ROWS + 2) as u32;

/// Virtual address at which the compositor SHM is mapped.
/// Must not overlap with other well-known regions (compdemo uses 0xD000_0000).
const SHM_VA: usize = 0xD100_0000;

const FLAGS_USER_RW: usize = 0x07;

// ─── Entry point ──────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    let _ = debug_print("cluuterm: start");

    // Registry init is required before any lookup_service calls.
    if registry::init("cluuterm").is_err() {
        let _ = debug_print("cluuterm: registry init failed");
        return 1;
    }

    // Allocate a long-lived endpoint for compositor pacing + VFS pts events.
    let info = process_info();
    let ipc_cap = info.tokens[TOKEN_IPC];
    let my_ep = match syscall::endpoint_create(ipc_cap) {
        Ok(ep) => ep,
        Err(_) => {
            let _ = debug_print("cluuterm: endpoint_create failed");
            return 1;
        }
    };

    // Phase 1: register window with compositor.
    let (window_id, comp_ep) = match register_window(my_ep) {
        Ok(p) => p,
        Err(code) => {
            let _ = debug_print("cluuterm: WIN_REGISTER failed");
            return code;
        }
    };
    let _ = debug_print("cluuterm: window registered");

    // Phase 2: register pts node with VFS.
    let pts_id = match register_pts(my_ep) {
        Ok(id) => id,
        Err(code) => {
            let _ = debug_print("cluuterm: PTS_REGISTER failed");
            return code;
        }
    };
    let _ = debug_print("cluuterm: pts registered");

    // Phase 3: spawn /bin/login with fd 0/1/2 wired to /dev/pts/<id>.
    if let Err(code) = spawn_login_with_pts(pts_id) {
        let _ = debug_print("cluuterm: spawn /bin/login failed");
        return code;
    }
    let _ = debug_print("cluuterm: /bin/login spawned");

    // Phase 4: run main loop (stub recv loop — Task 15 fills the real body).
    let shm_ptr = SHM_VA as *mut WindowShm;
    let mut term = tty_backend::Cluuterm::new(COLS, ROWS, shm_ptr, pts_id, window_id, my_ep, comp_ep);
    term.run();
    0
}

// ─── WIN_REGISTER ─────────────────────────────────────────────────────────────

/// Register a compositor window.
///
/// Protocol mirrors `compdemo/src/main.rs` exactly:
///   words[0] = payload_len (title bytes)
///   words[1] = req_w
///   words[2] = req_h
///   words[3] = app input/frame endpoint (my_ep)
///
/// Reply (label == COMP_WIN_REGISTER_REPLY):
///   words[0] = win_id
///   words[1] = shm_frame_token
///   words[2] = granted_w
///   words[3] = granted_h
///   words[4] = errno (0 = ok)
///
/// Returns `(win_id, comp_ep)` or an i32 exit code on error.
fn register_window(my_ep: usize) -> Result<(u32, usize), i32> {
    let comp_ep = match registry::lookup_service("compositor:client") {
        Some(ep) => ep,
        None => {
            let _ = debug_print("cluuterm: no compositor:client in registry");
            return Err(2);
        }
    };

    let title = b"cluuterm";
    let req = Message::new(
        COMP_WIN_REGISTER_LABEL,
        [
            title.len(),    // words[0] = payload_len
            WIN_W as usize, // words[1] = req_w
            WIN_H as usize, // words[2] = req_h
            my_ep,          // words[3] = app input/frame endpoint
            0,              // words[4] = reserved
            0,              // words[5] = reserved
        ],
        4,
    );
    let mut reply = Message::new(0, [0; 6], 0);
    if libcluu::ipc::call_with_payload(comp_ep, &req, title, &mut reply).is_err() {
        return Err(3);
    }
    if reply.tag.label != COMP_WIN_REGISTER_REPLY {
        return Err(4);
    }
    let win_id = reply.words[0] as u32;
    let shm_token = reply.words[1];
    let gw = reply.words[2];
    let gh = reply.words[3];
    let err = reply.words[4];
    if err != 0 {
        return Err(5);
    }

    // Map the SHM frame token into our address space.
    let cells_bytes = gw * gh * 8;
    let total = (32 + cells_bytes + 0xFFF) & !0xFFF;
    let num_pages = total / 0x1000;
    let space = space_token();
    if syscall::space_map_range(
        space,
        SHM_VA,
        shm_token,
        FLAGS_USER_RW | MAP_FRAME_TOKEN,
        num_pages,
        0,
    )
    .is_err()
    {
        return Err(6);
    }

    Ok((win_id, comp_ep))
}

// ─── PTS_REGISTER ─────────────────────────────────────────────────────────────

/// Register a new `/dev/pts/<id>` slot with the VFS.
///
/// Wire format (sent to "vfs:main"):
///   label   = PTS_REGISTER_LABEL
///   words[0] = notify_endpoint — VFS fires PTS_CLOSED_LABEL here when the
///              last fd on the pts is closed.
///   nwords  = 1
///
/// Reply:
///   words[0] = errno (0 = ok)
///   words[1] = id (u32)
fn register_pts(my_ep: usize) -> Result<u32, i32> {
    let vfs_ep = match registry::lookup_service("vfs:main") {
        Some(ep) => ep,
        None => {
            let _ = debug_print("cluuterm: no vfs:main in registry");
            return Err(7);
        }
    };

    let mut msg = Message::new(
        PTS_REGISTER_LABEL,
        [
            my_ep, // words[0] = notify_endpoint for PTS_CLOSED_LABEL
            0,
            0,
            0,
            0,
            0,
        ],
        1,
    );

    if libcluu::ipc::call(vfs_ep, &mut msg, IpcFlags::empty()).is_err() {
        return Err(8);
    }

    // reply is in-place in msg after call().
    let errno = msg.words[0];
    if errno != 0 {
        return Err(9);
    }
    let id = msg.words[1] as u32;
    Ok(id)
}

// ─── spawn /bin/login ─────────────────────────────────────────────────────────

/// Spawn `/bin/login` with fd 0, 1, 2 bound to `/dev/pts/<pts_id>`.
///
/// Strategy: open /dev/pts/<id> three times (for stdin, stdout, stderr),
/// then use posix_spawn_file_actions_adddup2 to redirect the child's
/// fd 0/1/2 to those open file descriptors.
///
/// The pts fds opened here are closed after spawn; the child inherits them
/// through the FDAC mechanism baked into libcluu's posix_spawn.
///
/// NOTE: The file_actions adddup2 only accepts newfd in 0-3 (libcluu
/// constraint: `!(0..=3).contains(&newfd)` → EINVAL).  We open the pts fd
/// once and dup2 it to 0, 1, and 2 separately.
fn spawn_login_with_pts(pts_id: u32) -> Result<(), i32> {
    extern "C" {
        fn posix_spawn(
            pid: *mut i32,
            path: *const u8,
            file_actions: *const core::ffi::c_void,
            attrp: *const core::ffi::c_void,
            argv: *const *const u8,
            envp: *const *const u8,
        ) -> i32;

        fn posix_spawn_file_actions_init(
            actions: *mut *mut core::ffi::c_void,
        ) -> i32;

        fn posix_spawn_file_actions_destroy(
            actions: *mut *mut core::ffi::c_void,
        ) -> i32;

        fn posix_spawn_file_actions_adddup2(
            actions: *mut *mut core::ffi::c_void,
            fd: i32,
            newfd: i32,
        ) -> i32;

        fn _open(path: *const u8, flags: i32, mode: u32) -> i32;
        fn _close(fd: i32) -> i32;
    }

    // Build the /dev/pts/<id> path.
    let path_bytes = render::pts_path(pts_id);

    // O_RDWR = 2 on this target.
    const O_RDWR: i32 = 2;

    // Open the pts node.
    let pts_fd = unsafe { _open(path_bytes.as_ptr(), O_RDWR, 0) };
    if pts_fd < 0 {
        let _ = debug_print("cluuterm: open /dev/pts failed");
        return Err(10);
    }

    // Set up file_actions: dup2 pts_fd → 0, 1, 2.
    let mut fa_ptr: *mut core::ffi::c_void = core::ptr::null_mut();
    let fa_init_rc = unsafe { posix_spawn_file_actions_init(&mut fa_ptr) };
    if fa_init_rc != 0 {
        unsafe { _close(pts_fd); }
        return Err(11);
    }

    let mut ok = true;
    for newfd in [0i32, 1, 2] {
        let rc = unsafe {
            posix_spawn_file_actions_adddup2(
                &mut fa_ptr,
                pts_fd,
                newfd,
            )
        };
        if rc != 0 {
            ok = false;
            break;
        }
    }

    if !ok {
        unsafe {
            posix_spawn_file_actions_destroy(&mut fa_ptr);
            _close(pts_fd);
        }
        return Err(12);
    }

    let login_path = b"/bin/login\0";
    let arg0 = b"login\0";
    let argv: [*const u8; 2] = [arg0.as_ptr(), core::ptr::null()];
    // Empty-but-non-null envp so posix_spawn does not fall back to `environ`.
    let envp: [*const u8; 1] = [core::ptr::null()];
    let mut child_pid: i32 = 0;

    let rc = unsafe {
        posix_spawn(
            &mut child_pid,
            login_path.as_ptr(),
            &fa_ptr as *const _ as *const core::ffi::c_void,
            core::ptr::null(),
            argv.as_ptr(),
            envp.as_ptr(),
        )
    };

    unsafe {
        posix_spawn_file_actions_destroy(&mut fa_ptr);
        _close(pts_fd);
    }

    if rc != 0 {
        return Err(13);
    }
    Ok(())
}
