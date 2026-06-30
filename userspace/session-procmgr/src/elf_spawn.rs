//! Production ELF-spawn primitive for session-procmgr (Phase 12.4b-2).
//!
//! Compiled only on the target (real x86-64 syscalls). Tests use `MockKernel`.
//!
//! 12.4a lays down the primitive but does NOT switch callers. Login still
//! routes through root-procmgr's legacy bypass. 12.4b flips the caller;
//! 12.4c deletes the bypass.

extern crate alloc;

use core::mem::size_of;

use libcluu::boot::{
    process_info, CWD_MAX, PARAM_ARGC, PARAM_ARGV_OFFSET, PARAM_CWD_LEN, PARAM_CWD_OFFSET,
    PARAM_ENVC, PARAM_ENV_OFFSET, PARAM_FD_VFS_LEN, PARAM_FD_VFS_OFFSET, PROCESS_INFO_ADDR,
    TOKEN_CLOCK, TOKEN_IPC, TOKEN_REGISTRY, TOKEN_SELF, TOKEN_SPACE, TOKEN_STDERR, TOKEN_STDIN,
    TOKEN_STDLOG, TOKEN_STDOUT, TOKEN_VFS_VIEW_MGR, ProcessInfo,
};
use libcluu::fs::VfsClient;
use libcluu::registry;
use libcluu::rights::Rights;
use libcluu::cap::CapProfile;
use libcluu::ipc::{send_msg_with_payload, VFS_DERIVE_CHILD_FD_LABEL, VFS_SET_VIEW_LABEL};
use libcluu::types::IpcFlags;
use libcluu::syscall::{
    space_create, space_map, space_map_range, thread_create, thread_get_id, thread_resume,
    thread_set_session, token_derive, THREAD_CREATE_START_SUSPENDED,
};
use libcluu::types::Message;
use procmgr_common::wire::SpawnReq;

use crate::dispatch::SessionState;

const CHILD_STACK_BASE: usize = 0x6d00_0000;
const CHILD_STACK_PAGES: usize = 32;
const CHILD_STACK_SIZE: usize = CHILD_STACK_PAGES * 4096;
const PROT_RW_USER: usize = 0x7;
const ANON_ZERO: usize = 0;
const PAGE_SIZE: usize = 4096;
const READ_ONLY: usize = 0x01;

#[derive(Debug)]
pub enum RealSpawnError {
    SpaceCreate,
    VfsOpen,
    MapElf,
    StackMap,
    ThreadCreate,
    TokenDerive,
    InfoPageBuild,
    InfoPageMap,
    VfsDeriveChildFd,
}

/// Perform a real per-session ELF spawn with full ProcessInfo handoff.
///
/// Steps:
///   1. Create address space.
///   2. VFS map_elf via session vfs_cap.
///   3. Allocate zero-filled stack.
///   4. Derive child capability tokens from our own ProcessInfo.
///   5. Build the 4 KiB ProcessInfo page (tokens, argv, envp, cwd).
///   6. Map the page read-only into the child at PROCESS_INFO_ADDR.
///   7. thread_create at the ELF entry point.
///
/// Returns `(thread_tok, cookie, space_tok, child_tid)` on success. Caller is
/// responsible for inserting the child into the session's `ChildTable` and
/// revoking minted caps on failure.
pub fn real_spawn_user_process(
    state: &SessionState,
    pid: i32,
    req: &SpawnReq,
    parent_tid: usize,
) -> Result<(u64, u64, u64, usize), RealSpawnError> {
    // ── 1. Create child address space ──────────────────────────────────────
    let info = process_info();
    let our_space = info.tokens[TOKEN_SPACE];

    let child_space = space_create(our_space).map_err(|_| RealSpawnError::SpaceCreate)?;

    // ── 2. Map ELF via VFS ─────────────────────────────────────────────────
    let ctrl_ep = registry::control_endpoint();
    let _ = libcluu::debug_print(&alloc::format!(
        "session-procmgr: elf_spawn vfs_ep={} ctrl_ep={} path={}",
        state.vfs_cap, ctrl_ep, req.image_path
    ));
    let vfs = VfsClient::new(state.vfs_cap as usize, ctrl_ep);
    let file = vfs
        .open(&req.image_path)
        .map_err(|e| {
            let _ = libcluu::debug_print(&alloc::format!(
                "session-procmgr: VfsOpen failed: {:?}", e
            ));
            RealSpawnError::VfsOpen
        })?;
    let entry = vfs
        .map_elf(file, child_space)
        .map_err(|e| {
            let _ = libcluu::debug_print(&alloc::format!(
                "session-procmgr: map_elf failed: {:?}", e
            ));
            RealSpawnError::MapElf
        })?;
    let _ = libcluu::debug_print(&alloc::format!(
        "session-procmgr: map_elf OK entry=0x{:x}", entry
    ));

    // ── 3. Allocate stack ──────────────────────────────────────────────────
    space_map_range(
        child_space,
        CHILD_STACK_BASE,
        ANON_ZERO,
        PROT_RW_USER,
        CHILD_STACK_PAGES,
        0,
    )
    .map_err(|_| RealSpawnError::StackMap)?;
    let stack_top = CHILD_STACK_BASE + CHILD_STACK_SIZE;

    // ── 4. Create child thread suspended (need child_tid before fd derives) ──
    // child_tid is VFS's new client_id for the child; VFS authenticates by
    // kernel-supplied sender_tid on the child's first request.
    let thread_tok = thread_create(
        child_space,
        entry,
        stack_top,
        0,
        THREAD_CREATE_START_SUSPENDED,
    )
    .map_err(|_| RealSpawnError::ThreadCreate)?;
    let child_tid = thread_get_id(thread_tok).map_err(|_| RealSpawnError::ThreadCreate)?;

    // ── 5. Derive child capability tokens ──────────────────────────────────
    let _ = libcluu::debug_print(&alloc::format!(
        "session-procmgr: tokens IPC={} SELF={} SPACE={} REG={} CLK={}",
        info.tokens[TOKEN_IPC], info.tokens[TOKEN_SELF], info.tokens[TOKEN_SPACE],
        info.tokens[TOKEN_REGISTRY], info.tokens[TOKEN_CLOCK],
    ));

    // TOKEN_IPC: children may create endpoints + call procmgr/vfs
    let ipc_rights = (Rights::IPC_SEND
        | Rights::IPC_RECV
        | Rights::IPC_CALL
        | Rights::CREATE
        | Rights::GRANT)
        .bits() as usize;
    let child_ipc = token_derive(info.tokens[TOKEN_IPC], ipc_rights, u64::MAX)
        .map_err(|_| {
            let _ = libcluu::debug_print("session-procmgr: TOKEN_IPC derive FAILED");
            RealSpawnError::TokenDerive
        })?;

    // TOKEN_SELF: children may spawn threads + grant
    let self_rights =
        (Rights::CREATE | Rights::GRANT | Rights::THREAD_CONTROL).bits() as usize;
    let child_self = token_derive(info.tokens[TOKEN_SELF], self_rights, u64::MAX)
        .map_err(|_| {
            let _ = libcluu::debug_print("session-procmgr: TOKEN_SELF derive FAILED");
            RealSpawnError::TokenDerive
        })?;

    // TOKEN_SPACE: derived from the new child_space
    let space_rights =
        (Rights::SPACE_MAP | Rights::SPACE_GRANT | Rights::CREATE | Rights::THREAD_CONTROL)
            .bits() as usize;
    let child_space_tok = token_derive(child_space, space_rights, u64::MAX)
        .map_err(|_| {
            let _ = libcluu::debug_print("session-procmgr: TOKEN_SPACE derive FAILED");
            RealSpawnError::TokenDerive
        })?;

    // TOKEN_REGISTRY: derive from state.registry_cap.  GRANT included so
    // grandchildren can also re-derive narrower handles via FdInherit.
    let registry_rights =
        (Rights::IPC_SEND | Rights::IPC_CALL | Rights::GRANT).bits() as usize;
    let child_registry = token_derive(state.registry_cap as usize, registry_rights, u64::MAX)
        .map_err(|_| {
            let _ = libcluu::debug_print("session-procmgr: TOKEN_REGISTRY derive FAILED");
            RealSpawnError::TokenDerive
        })?;

    // TOKEN_CLOCK: kernel-minted clock object (Rights::READ only) used by
    // clock_now syscall.  Not an IPC endpoint — pass through raw, do not
    // derive with IPC rights.  Timeserver IPC is discovered via registry
    // ("timeserver:main") by clients that need pushmode subscriptions.
    let child_clock = state.timeserver_cap as usize;

    // ── 6. Derive fd tokens; VFS-backed fds go through VFS_DERIVE_CHILD_FD ──
    // stdin needs IPC_RECV, stdout/stderr/stdlog need IPC_SEND|IPC_CALL
    let stdin_rights = (Rights::IPC_SEND | Rights::IPC_RECV).bits() as usize;
    let stdout_rights = (Rights::IPC_SEND | Rights::IPC_CALL).bits() as usize;

    let mut child_stdin: usize = 0;
    let mut child_stdout: usize = 0;
    let mut child_stderr: usize = 0;
    let mut child_stdlog: usize = 0;
    // Per-fd VFS metadata: (child_client_id, child_remote_fd); zero = not VFS-backed.
    let mut fd_vfs_meta: [(usize, usize); 4] = [(0, 0); 4];

    for entry in &req.fd_inherit {
        let raw = entry.cap_token as usize;
        if raw == 0 {
            continue;
        }
        let fd = entry.fd as usize;
        let rights = if fd == 0 { stdin_rights } else { stdout_rights };

        if entry.parent_rfd != 0 && state.vfs_cap != 0 {
            // VFS-backed fd: ask VFS to clone the parent's open file to child_tid.
            // parent_tid is the kernel-authenticated sender_tid VFS uses as parent's cid.
            match vfs_derive_child_fd(
                state.vfs_cap as usize,
                parent_tid,
                entry.parent_rfd as usize,
                rights,
                child_tid,
            ) {
                Ok((derived_tok, child_cid, child_rfd)) => {
                    let _ = libcluu::debug_print(&alloc::format!(
                        "session-procmgr: vfs_derive_child_fd fd={} OK child_cid={} child_rfd={}",
                        fd, child_cid, child_rfd,
                    ));
                    if fd < 4 {
                        fd_vfs_meta[fd] = (child_cid, child_rfd);
                    }
                    match fd {
                        0 => child_stdin = derived_tok,
                        1 => child_stdout = derived_tok,
                        2 => child_stderr = derived_tok,
                        3 => child_stdlog = derived_tok,
                        _ => {}
                    }
                }
                Err(e) => {
                    let _ = libcluu::debug_print(&alloc::format!(
                        "session-procmgr: vfs_derive_child_fd FAILED fd={} parent_rfd={} err={:?}",
                        fd, entry.parent_rfd, e,
                    ));
                    // Propagate: missing fd 0 → child FATALs per loud-fail rule.
                    return Err(RealSpawnError::VfsDeriveChildFd);
                }
            }
        } else {
            // Legacy path: direct token_derive (pipes, tty endpoints, parent_rfd==0).
            match fd {
                0 => {
                    child_stdin = token_derive(raw, stdin_rights, u64::MAX).unwrap_or(0);
                }
                1 => {
                    child_stdout = token_derive(raw, stdout_rights, u64::MAX).unwrap_or(0);
                }
                2 => {
                    child_stderr = token_derive(raw, stdout_rights, u64::MAX).unwrap_or(0);
                }
                3 => {
                    child_stdlog = token_derive(raw, stdout_rights, u64::MAX).unwrap_or(0);
                }
                _ => {}
            }
        }
    }

    // exit_token: child sends exit notifications to our spawn_ep
    let exit_rights = (Rights::IPC_SEND).bits() as usize;
    let exit_token = if state.spawn_ep != 0 {
        token_derive(state.spawn_ep as usize, exit_rights, u64::MAX).unwrap_or(0)
    } else {
        0
    };

    let cookie = (pid as u64) ^ 0xC0DE_0000;

    // ── 7. Build ProcessInfo page ──────────────────────────────────────────
    let mut tokens = [0usize; 17];
    tokens[TOKEN_STDIN] = child_stdin;
    tokens[TOKEN_STDOUT] = child_stdout;
    tokens[TOKEN_STDERR] = child_stderr;
    tokens[TOKEN_STDLOG] = child_stdlog;
    tokens[TOKEN_SELF] = child_self;
    tokens[TOKEN_SPACE] = child_space_tok;
    tokens[TOKEN_IPC] = child_ipc;
    tokens[TOKEN_CLOCK] = child_clock;
    tokens[TOKEN_REGISTRY] = child_registry;
    tokens[TOKEN_VFS_VIEW_MGR] = state.view_mgr_token as usize;
    // TOKEN_EXTRA_0 = 0 (cluuterm vt; out of scope for 12.4b-2)

    // Build argv payload: each arg as null-terminated bytes packed contiguously
    let mut argv_payload: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    for arg in &req.argv {
        argv_payload.extend_from_slice(arg.as_bytes());
        argv_payload.push(0u8);
    }
    let argc = req.argv.len();

    // Build envp payload: KEY=VALUE\0 strings packed contiguously
    let mut env_payload: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    for (k, v) in &req.envp {
        env_payload.extend_from_slice(k.as_bytes());
        env_payload.push(b'=');
        env_payload.extend_from_slice(v.as_bytes());
        env_payload.push(0u8);
    }
    let envc = req.envp.len();

    // cwd bytes (clamped to CWD_MAX)
    let cwd_bytes = req.cwd.as_bytes();
    let cwd_clamped_len = cwd_bytes.len().min(CWD_MAX);

    // Layout within 4 KiB page:
    //   [info_offset .. info_offset+info_size] = ProcessInfo struct
    //   [argv_data_offset ..]                  = argv payload
    //   [env_data_offset ..]                   = env payload
    //   [cwd_data_offset ..]                   = cwd bytes
    //   [fd_vfs_trailer_offset ..+64]           = VFS fd trailer (4×16 bytes)
    let page_base = PROCESS_INFO_ADDR & !(PAGE_SIZE - 1);
    let info_offset = PROCESS_INFO_ADDR - page_base;
    let info_size = size_of::<ProcessInfo>();

    let argv_data_offset = info_offset + info_size;
    let env_data_offset = argv_data_offset + argv_payload.len();
    let cwd_data_offset = env_data_offset + env_payload.len();

    let argv_end = argv_data_offset + argv_payload.len();
    let env_end = env_data_offset + env_payload.len();
    let cwd_end = cwd_data_offset + cwd_clamped_len;

    let argv_fits = argc > 0 && !argv_payload.is_empty() && argv_end <= PAGE_SIZE;
    let env_fits = envc > 0 && !env_payload.is_empty() && env_end <= PAGE_SIZE;
    let cwd_fits = cwd_clamped_len > 0 && cwd_end <= PAGE_SIZE;

    // VFS fd trailer: 4 × 16 bytes = 64 bytes, placed after cwd.
    const FD_VFS_TRAILER_SIZE: usize = 64;
    let fd_vfs_trailer_offset = cwd_data_offset + cwd_clamped_len;
    let any_vfs = fd_vfs_meta.iter().any(|&(cid, _)| cid != 0);
    let fd_vfs_trailer_fits = any_vfs && fd_vfs_trailer_offset + FD_VFS_TRAILER_SIZE <= PAGE_SIZE;

    let mut params = [0u64; 32];
    if argv_fits {
        params[PARAM_ARGC] = argc as u64;
        params[PARAM_ARGV_OFFSET] = argv_data_offset as u64;
    }
    if env_fits {
        params[PARAM_ENVC] = envc as u64;
        params[PARAM_ENV_OFFSET] = env_data_offset as u64;
    }
    if cwd_fits {
        params[PARAM_CWD_OFFSET] = cwd_data_offset as u64;
        params[PARAM_CWD_LEN] = cwd_clamped_len as u64;
    }
    if fd_vfs_trailer_fits {
        params[PARAM_FD_VFS_OFFSET] = fd_vfs_trailer_offset as u64;
        params[PARAM_FD_VFS_LEN] = FD_VFS_TRAILER_SIZE as u64;
    }

    let child_info = ProcessInfo {
        exit_token,
        exit_cookie: cookie as usize,
        pid: pid as usize,
        tokens,
        params,
    };

    // Assemble the page
    let mut page = [0u8; PAGE_SIZE];
    let info_bytes = unsafe {
        core::slice::from_raw_parts(&child_info as *const ProcessInfo as *const u8, info_size)
    };
    let info_end = info_offset + info_bytes.len();
    if info_end > PAGE_SIZE {
        return Err(RealSpawnError::InfoPageBuild);
    }
    page[info_offset..info_end].copy_from_slice(info_bytes);

    if argv_fits {
        page[argv_data_offset..argv_end].copy_from_slice(&argv_payload);
    }
    if env_fits {
        page[env_data_offset..env_end].copy_from_slice(&env_payload);
    }
    if cwd_fits {
        page[cwd_data_offset..cwd_end].copy_from_slice(&cwd_bytes[..cwd_clamped_len]);
    }
    // Write VFS fd trailer: each entry (vfs_client_id u64 LE, vfs_remote_fd u64 LE)
    if fd_vfs_trailer_fits {
        let trailer_end = fd_vfs_trailer_offset + FD_VFS_TRAILER_SIZE;
        let dst = &mut page[fd_vfs_trailer_offset..trailer_end];
        for (i, &(cid, rfd)) in fd_vfs_meta.iter().enumerate() {
            let off = i * 16;
            dst[off..off + 8].copy_from_slice(&(cid as u64).to_le_bytes());
            dst[off + 8..off + 16].copy_from_slice(&(rfd as u64).to_le_bytes());
        }
    }

    // ── 8. Map ProcessInfo page read-only into child ───────────────────────
    space_map(
        child_space,
        page_base,
        page.as_ptr() as usize,
        READ_ONLY,
        PAGE_SIZE,
    )
    .map_err(|_| RealSpawnError::InfoPageMap)?;

    // ── 9. Install VFS view before resuming ───────────────────────────────
    // child_tid already computed above; reuse it here.
    let _ = libcluu::debug_print(&alloc::format!(
        "session-procmgr: elf_spawn VFS_SET_VIEW vfs_cap={} child_tid={}",
        state.vfs_cap, child_tid,
    ));
    if state.vfs_cap != 0 {
        // Use the same default user-profile mount set as root-procmgr so that
        // children spawned via session-procmgr have a writable /dev/pts (needed
        // for cluuterm → shell pts wiring). Hardcoded "/dev" read-only would
        // make /dev/pts/<id> O_WRONLY opens fail with PermissionDenied.
        let default_mounts = libcluu::vfs_view::default_mounts_for_profile(CapProfile::USER);
        let mut payload = alloc::vec::Vec::new();
        for (src, dst, writable) in default_mounts {
            let src_bytes = src.as_bytes();
            let dst_bytes = dst.as_bytes();
            payload.extend_from_slice(&(src_bytes.len() as u16).to_le_bytes());
            payload.extend_from_slice(&(dst_bytes.len() as u16).to_le_bytes());
            payload.push(if *writable { 1u8 } else { 0u8 });
            payload.extend_from_slice(&0u64.to_le_bytes()); // memfs_cid
            payload.extend_from_slice(src_bytes);
            payload.extend_from_slice(dst_bytes);
        }
        let mut msg = Message::new(VFS_SET_VIEW_LABEL, [0; 6], 6);
        msg.words[0] = payload.len();
        msg.words[1] = child_tid;
        msg.words[2] = default_mounts.len();
        msg.words[3] = CapProfile::USER.bits() as usize;
        msg.words[4] = 0usize; // container_id
        msg.words[5] = state.view_mgr_token as usize;
        match send_msg_with_payload(state.vfs_cap as usize, &msg, &payload) {
            Ok(()) => {
                let _ = libcluu::debug_print("session-procmgr: VFS_SET_VIEW OK");
            }
            Err(e) => {
                let _ = libcluu::debug_print(&alloc::format!(
                    "session-procmgr: VFS_SET_VIEW FAILED {:?} — resuming anyway",
                    e
                ));
            }
        }
    }

    // ── 9. Resume child ────────────────────────────────────────────────────
    let _ = libcluu::debug_print(&alloc::format!(
        "session-procmgr: elf_spawn resuming thread_tok={}",
        thread_tok
    ));
    let _ = thread_set_session(thread_tok, state.sid as u64);
    thread_resume(thread_tok).map_err(|_| RealSpawnError::ThreadCreate)?;

    Ok((thread_tok as u64, cookie, child_space as u64, child_tid))
}

/// Ask VFS to clone the parent's open file into the child's client_id slot
/// and mint a narrowed VFS-scoped token from VFS's own endpoint.
///
/// Wire format: see [`VFS_DERIVE_CHILD_FD_LABEL`] in libcluu::ipc.
///
/// Returns `(derived_token, child_client_id, child_remote_fd)`.
fn vfs_derive_child_fd(
    vfs_endpoint: usize,
    parent_cid: usize,
    parent_rfd: usize,
    child_rights: usize,
    child_tid: usize,
) -> Result<(usize, usize, usize), RealSpawnError> {
    if vfs_endpoint == 0 {
        return Err(RealSpawnError::VfsDeriveChildFd);
    }
    let mut msg = Message::new(
        VFS_DERIVE_CHILD_FD_LABEL,
        [parent_cid, parent_rfd, child_rights, child_tid, 0, 0],
        4,
    );
    libcluu::ipc::call(vfs_endpoint, &mut msg, IpcFlags::empty())
        .map_err(|_| RealSpawnError::VfsDeriveChildFd)?;
    if msg.words[0] != 0 {
        return Err(RealSpawnError::VfsDeriveChildFd);
    }
    Ok((msg.words[1], msg.words[2], msg.words[3]))
}
