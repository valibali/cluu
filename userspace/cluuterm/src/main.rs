//! `cluuterm` — CLUU terminal emulator binary.
//!
//! Registers with compositor (WIN_REGISTER), registers PTS with VFS,
//! then spawns /bin/shell with fd 0/1/2 bound to the pts node via
//! the unified spawn protocol (plan 1, `libcluu::spawn::spawn`).

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

mod input;
mod render;
mod tty_backend;

use libcluu::boot::{process_info, space_token, TOKEN_IPC};
use libcluu::ipc::{COMP_WIN_REGISTER_LABEL, COMP_WIN_REGISTER_REPLY};
use libcluu::syscall::MAP_FRAME_TOKEN;
use libcluu::types::Message;
use libcluu::window_shm::WindowShm;
use libcluu::{debug_print, registry, syscall};

use cluu_wire::pts::{VFS_REGISTER_PTS_LABEL, VfsRegisterPtsRequest, VfsRegisterPtsReply};
use cluu_wire::spawn::{FdInherit, FdRights, FdSource};
use procmgr_common::labels::SESSION_PROCMGR_SPAWN_LABEL;
use procmgr_common::wire::{FdInheritEntry, FdKind, SpawnReq, SpawnReply};

extern "C" {
    fn _open(path: *const u8, flags: i32, mode: u32) -> i32;
    fn _close(fd: i32) -> i32;
}

// ─── Layout constants ─────────────────────────────────────────────────────────

const COLS: usize = 80;
const ROWS: usize = 24;

const CHROME: usize = 1;
const PAD_TOP: usize = 0;
const PAD_BOT: usize = 1;
const PAD_LR: usize = 1;
const WIN_W: u32 = (COLS + 2 * (CHROME + PAD_LR)) as u32;
const WIN_H: u32 = (ROWS + (CHROME + PAD_TOP) + (CHROME + PAD_BOT)) as u32;

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
    let (window_id, comp_ep, gw, gh) = match register_window(my_ep) {
        Ok(p) => p,
        Err(code) => {
            let _ = debug_print("cluuterm: WIN_REGISTER failed");
            return code;
        }
    };
    let _ = debug_print("cluuterm: window registered");

    // Phase 2: register pts node with VFS.
    let (pts_id, vfs_ep) = match register_pts(my_ep) {
        Ok(pair) => pair,
        Err(code) => {
            let _ = debug_print("cluuterm: PTS_REGISTER failed");
            return code;
        }
    };
    let _ = debug_print("cluuterm: pts registered");

    // Phase 3: spawn /bin/shell with fd 0/1/2 wired to /dev/pts/<id>.
    if let Err(code) = spawn_shell_with_pts(pts_id) {
        let _ = debug_print("cluuterm: spawn /bin/shell failed");
        return code;
    }
    let _ = debug_print("cluuterm: /bin/shell spawned");

    // Phase 4: run main loop (stub recv loop — Task 15 fills the real body).
    let shm_ptr = SHM_VA as *mut WindowShm;
    let interior_cols = (gw as usize).saturating_sub(2 * (CHROME + PAD_LR)).max(1);
    let interior_rows = (gh as usize).saturating_sub((CHROME + PAD_TOP) + (CHROME + PAD_BOT)).max(1);
    let mut term = tty_backend::Cluuterm::new(interior_cols, interior_rows, shm_ptr, pts_id, window_id, my_ep, comp_ep, vfs_ep);
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
fn register_window(my_ep: usize) -> Result<(u32, usize, u32, u32), i32> {
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

    Ok((win_id, comp_ep, gw as u32, gh as u32))
}

// ─── PTS_REGISTER ─────────────────────────────────────────────────────────────

/// Query procmgr for the caller's session_id.
///
/// Until spec 3 wires sessions through spawn, this returns `None`
/// unconditionally. The per-session `/dev/pts/` overlay only activates
/// once spec 3 populates real session ids.
fn read_own_session_id() -> Option<u32> {
    None
}

/// Register a new `/dev/pts/<id>` slot with the VFS.
///
/// Wire format (sent to "vfs:main"): uses VFS_REGISTER_PTS_LABEL (111)
/// with a postcard-serialized VfsRegisterPtsRequest payload.
///
/// Reply is postcard-serialized VfsRegisterPtsReply with assigned_id.
///
/// Returns `(pts_id, vfs_ep)` so the caller can pass `vfs_ep` to
/// `Cluuterm::new` for `PTS_READ_DELIVER_LABEL` replies.
fn register_pts(my_ep: usize) -> Result<(u32, usize), i32> {
    let vfs_ep = match registry::lookup_service("vfs:main") {
        Some(ep) => ep,
        None => {
            let _ = debug_print("cluuterm: no vfs:main in registry");
            return Err(7);
        }
    };

    let req = VfsRegisterPtsRequest {
        session_id: read_own_session_id(),
        pts_endpoint: my_ep as u64,
        suggested_id: None,
    };

    let payload = postcard::to_allocvec(&req).map_err(|_| 10)?;

    let msg = libcluu::ipc::make_payload_message(
        VFS_REGISTER_PTS_LABEL,
        payload.len(),
        &[],
    );

    // Allocate reply buffer: Message header + up to 128 bytes for postcard reply.
    let mut reply_buf = [0u8; core::mem::size_of::<Message>() + 128];

    let (_reply_msg, reply_payload_len) =
        libcluu::ipc::call_with_reply_buf(vfs_ep, &msg, &payload, &mut reply_buf)
            .map_err(|_| 8)?;

    let reply_start = core::mem::size_of::<Message>();
    let reply_payload = &reply_buf[reply_start..reply_start + reply_payload_len];

    let reply: VfsRegisterPtsReply =
        postcard::from_bytes(reply_payload).map_err(|_| 9)?;

    Ok((reply.assigned_id, vfs_ep))
}

// ─── spawn /bin/shell ─────────────────────────────────────────────────────────

/// Spawn `/bin/shell` with fd 0, 1, 2 bound to `/dev/pts/<pts_id>`.
///
/// Opens the pts node, builds FdInherit entries referencing it, and calls
/// the unified spawn protocol (`libcluu::spawn::spawn`). The parent-side
/// pts fd is closed after spawn.
fn spawn_shell_with_pts(pts_id: u32) -> Result<(), i32> {
    let path_bytes = render::pts_path(pts_id);

    // O_RDONLY = 0; O_WRONLY = 1; we open once RW and build FdInherit entries.
    const O_RDONLY: i32 = 0;
    const O_WRONLY: i32 = 1;

    // Open the pts node for input (fd 0) and output (fd 1, 2).
    let pts_in: i32 = unsafe { _open(path_bytes.as_ptr(), O_RDONLY, 0) };
    if pts_in < 0 {
        let _ = debug_print("cluuterm: open /dev/pts for read failed");
        return Err(10);
    }
    let pts_out: i32 = unsafe { _open(path_bytes.as_ptr(), O_WRONLY, 0) };
    if pts_out < 0 {
        unsafe { _close(pts_in); }
        let _ = debug_print("cluuterm: open /dev/pts for write failed");
        return Err(11);
    }

    // Resolve VFS addresses for fd inheritance.
let (stdin_cid, stdin_rfd, stdout_cid, stdout_rfd) = {
            let fd_table = libcluu::fd_table::FD_TABLE.lock();
            let stdin_entry = fd_table.get(pts_in).ok_or(12)?;
            let stdout_entry = fd_table.get(pts_out).ok_or(13)?;
            (
                stdin_entry.client_id as u64, stdin_entry.remote_fd.unwrap_or(0) as u32,
                stdout_entry.client_id as u64, stdout_entry.remote_fd.unwrap_or(0) as u32,
            )
        };

    let fd_inherit = alloc::vec![
        FdInherit {
            child_fd: 0,
            source: FdSource::VfsFd {
                vfs_client_id: stdin_cid,
                vfs_remote_fd: stdin_rfd,
            },
            rights: FdRights::READ_ONLY,
        },
        FdInherit {
            child_fd: 1,
            source: FdSource::VfsFd {
                vfs_client_id: stdout_cid,
                vfs_remote_fd: stdout_rfd,
            },
            rights: FdRights::WRITE_ONLY,
        },
        FdInherit {
            child_fd: 2,
            source: FdSource::VfsFd {
                vfs_client_id: stdout_cid,
                vfs_remote_fd: stdout_rfd,
            },
            rights: FdRights::WRITE_ONLY,
        },
    ];

    // Translate FdInherit entries (cluu_wire) to FdInheritEntry (procmgr-common).
    // cap_token = VFS client endpoint (informational); parent_rfd = VFS-side
    // remote fd of the parent (this process). session-procmgr uses sender_tid
    // as parent_cid and parent_rfd here when calling VFS_DERIVE_CHILD_FD to
    // mint the child's VFS-backed fd.
    let fd_inherit_entries: alloc::vec::Vec<FdInheritEntry> = fd_inherit
        .iter()
        .map(|fi| {
            let (cap, rfd) = match &fi.source {
                FdSource::VfsFd { vfs_client_id, vfs_remote_fd } => {
                    (*vfs_client_id, *vfs_remote_fd)
                }
                _ => (0, 0),
            };
            FdInheritEntry {
                fd: fi.child_fd as i32,
                kind: FdKind::Pts,
                cap_token: cap,
                parent_rfd: rfd,
            }
        })
        .collect();

    // TODO(12.4b): read real sid from session envelope once TOKEN_EXTRA_0 carries
    // it. For now cluuterm is always spawned inside sid=1 (the only session that
    // login creates in tests).
    let sid: u32 = 1;

    let spawn_ep_name = alloc::format!("session-procmgr:spawn:{}", sid);
    let session_spawn_ep = match registry::lookup_service(&spawn_ep_name) {
        Some(ep) => ep,
        None => {
            let _ = debug_print(&alloc::format!(
                "cluuterm: {} not found in registry", spawn_ep_name));
            unsafe { _close(pts_in); _close(pts_out); }
            return Err(15);
        }
    };

    let spawn_req = SpawnReq {
        image_path: alloc::string::String::from("/bin/shell"),
        argv: alloc::vec![alloc::string::String::from("shell")],
        envp: alloc::vec![
            (alloc::string::String::from("TERM"),
             alloc::string::String::from("xterm-256color")),
            (alloc::string::String::from("CLUU_SESSION_ID"),
             alloc::format!("{}", sid)),
        ],
        cwd: alloc::string::String::from("/"),
        fd_inherit: fd_inherit_entries,
        notify: None,
    };

    let spawn_payload = match postcard::to_allocvec(&spawn_req) {
        Ok(b) => b,
        Err(_) => {
            let _ = debug_print("cluuterm: SpawnReq serialize failed");
            unsafe { _close(pts_in); _close(pts_out); }
            return Err(15);
        }
    };

    let spawn_msg = Message::new(
        SESSION_PROCMGR_SPAWN_LABEL,
        [spawn_payload.len(), 0usize, 0, 0, 0, 0],
        0,
    );
    let mut spawn_reply_buf = [0u8; 512];
    let (_srply_msg, srply_payload_len) = match libcluu::ipc::call_with_reply_buf(
        session_spawn_ep, &spawn_msg, &spawn_payload, &mut spawn_reply_buf,
    ) {
        Ok(r) => r,
        Err(e) => {
            let _ = debug_print(&alloc::format!(
                "cluuterm: session spawn IPC failed: {:?}", e));
            unsafe { _close(pts_in); _close(pts_out); }
            return Err(15);
        }
    };

    let hdr_sz = core::mem::size_of::<Message>();
    let srply_bytes = &spawn_reply_buf[hdr_sz..hdr_sz + srply_payload_len];
    match postcard::from_bytes::<SpawnReply>(srply_bytes) {
        Ok(r) => {
            let _ = debug_print(&alloc::format!(
                "cluuterm: spawned shell pid={}", r.pid));
        }
        Err(_) => {
            let _ = debug_print("cluuterm: SpawnReply deserialize failed");
            unsafe { _close(pts_in); _close(pts_out); }
            return Err(15);
        }
    }

    // Close parent-side pts fds.
    unsafe {
        _close(pts_in);
        _close(pts_out);
    }
    Ok(())
}
