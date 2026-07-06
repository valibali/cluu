//! Production ELF-spawn primitive for session-procmgr (Phase 12.4b-2).
//!
//! Compiled only on the target (real x86-64 syscalls). Tests use `MockKernel`.
//!
//! 12.4a lays down the primitive but does NOT switch callers. Login still
//! routes through root-procmgr's legacy bypass. 12.4b flips the caller;
//! 12.4c deletes the bypass.
//!
//! Async spawn: `begin_spawn` does the synchronous steps (space_create,
//! VFS open/map_elf, stack, thread_create suspended, token derivation,
//! legacy fd derivation) and collects VFS-backed fd-derive requests.
//! If any VFS-backed fds need derivation, it returns `NeedsAsync(PendingSpawn)`.
//! The main loop spawns `IpcCallFuture` tasks for each derive request and,
//! when all replies arrive, calls `finish_spawn` to complete the spawn.

extern crate alloc;

use core::mem::size_of;

use libcluu::boot::{
    process_info, CWD_MAX, PARAM_ARGC, PARAM_ARGV_OFFSET, PARAM_CWD_LEN, PARAM_CWD_OFFSET,
    PARAM_ENVC, PARAM_ENV_OFFSET, PARAM_FD_VFS_LEN, PARAM_FD_VFS_OFFSET, PARAM_SESSION_VFS_EP,
    PROCESS_INFO_ADDR,
    TOKEN_CLOCK, TOKEN_IPC, TOKEN_REGISTRY, TOKEN_SELF, TOKEN_SPACE, TOKEN_STDERR, TOKEN_STDIN,
    TOKEN_STDLOG, TOKEN_STDOUT, TOKEN_VFS_VIEW_MGR, ProcessInfo,
};
use libcluu::fs::VfsClient;
use libcluu::registry;
use libcluu::rights::Rights;
use libcluu::cap::CapProfile;
use libcluu::ipc::{send_msg_with_payload, VFS_SET_VIEW_LABEL};
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

#[derive(Debug, Clone, Copy)]
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

pub struct SpawnResult {
    pub thread_tok: u64,
    pub cookie: u64,
    pub space_tok: u64,
    pub child_tid: usize,
}

pub enum BeginSpawnResult {
    Complete(SpawnResult),
    NeedsAsync(PendingSpawn),
}

#[derive(Debug, Clone, Copy)]
pub struct FdDeriveRequest {
    pub fd: usize,
    pub vfs_ep: usize,
    pub parent_tid: usize,
    pub parent_rfd: usize,
    pub rights: usize,
    pub child_tid: usize,
}

#[derive(Debug)]
pub struct PendingSpawn {
    pub cookie: u64,
    pub pid: i32,
    pub req: SpawnReq,
    pub parent_tid: usize,
    pub parent_pid: i32,

    pub child_space: u64,
    pub thread_tok: u64,
    pub child_tid: usize,

    pub child_ipc: usize,
    pub child_self: usize,
    pub child_space_tok: usize,
    pub child_registry: usize,
    pub child_clock: usize,
    pub exit_token: usize,

    pub child_stdin: usize,
    pub child_stdout: usize,
    pub child_stderr: usize,
    pub child_stdlog: usize,

    pub fd_derive_requests: alloc::vec::Vec<FdDeriveRequest>,
    pub fd_derive_results: [Option<Result<(usize, usize, usize), RealSpawnError>>; 4],
    pub fd_derive_remaining: usize,

    pub minted: alloc::vec::Vec<u64>,

    pub reply_token: Option<usize>,
    pub async_reply: Option<(usize, usize)>,
}

#[derive(Debug)]
pub enum SpmCompletion {
    SpawnDeriveChildFdReply {
        cookie: u64,
        fd: usize,
        result: Result<(usize, usize, usize), RealSpawnError>,
    },
}

pub fn begin_spawn(
    state: &SessionState,
    pid: i32,
    req: &SpawnReq,
    parent_tid: usize,
) -> Result<BeginSpawnResult, RealSpawnError> {
    let info = process_info();
    let our_space = info.tokens[TOKEN_SPACE];

    let child_space = space_create(our_space).map_err(|_| RealSpawnError::SpaceCreate)?;

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

    let thread_tok = thread_create(
        child_space,
        entry,
        stack_top,
        0,
        THREAD_CREATE_START_SUSPENDED,
    )
    .map_err(|_| RealSpawnError::ThreadCreate)?;
    let child_tid = thread_get_id(thread_tok).map_err(|_| RealSpawnError::ThreadCreate)?;

    let _ = libcluu::debug_print(&alloc::format!(
        "session-procmgr: tokens IPC={} SELF={} SPACE={} REG={} CLK={}",
        info.tokens[TOKEN_IPC], info.tokens[TOKEN_SELF], info.tokens[TOKEN_SPACE],
        info.tokens[TOKEN_REGISTRY], info.tokens[TOKEN_CLOCK],
    ));

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

    let self_rights =
        (Rights::CREATE | Rights::GRANT | Rights::THREAD_CONTROL).bits() as usize;
    let child_self = token_derive(info.tokens[TOKEN_SELF], self_rights, u64::MAX)
        .map_err(|_| {
            let _ = libcluu::debug_print("session-procmgr: TOKEN_SELF derive FAILED");
            RealSpawnError::TokenDerive
        })?;

    let space_rights =
        (Rights::SPACE_MAP | Rights::SPACE_GRANT | Rights::CREATE | Rights::THREAD_CONTROL)
            .bits() as usize;
    let child_space_tok = token_derive(child_space, space_rights, u64::MAX)
        .map_err(|_| {
            let _ = libcluu::debug_print("session-procmgr: TOKEN_SPACE derive FAILED");
            RealSpawnError::TokenDerive
        })?;

    let registry_rights =
        (Rights::IPC_SEND | Rights::IPC_CALL | Rights::GRANT).bits() as usize;
    let child_registry = token_derive(state.registry_cap as usize, registry_rights, u64::MAX)
        .map_err(|_| {
            let _ = libcluu::debug_print("session-procmgr: TOKEN_REGISTRY derive FAILED");
            RealSpawnError::TokenDerive
        })?;

    let child_clock = state.timeserver_cap as usize;

    let stdin_rights = (Rights::IPC_SEND | Rights::IPC_RECV | Rights::IPC_CALL).bits() as usize;
    let stdout_rights = (Rights::IPC_SEND | Rights::IPC_CALL).bits() as usize;

    let mut child_stdin: usize = 0;
    let mut child_stdout: usize = 0;
    let mut child_stderr: usize = 0;
    let mut child_stdlog: usize = 0;
    let mut fd_derive_requests: alloc::vec::Vec<FdDeriveRequest> = alloc::vec::Vec::new();

    for entry in &req.fd_inherit {
        let raw = entry.cap_token as usize;
        if raw == 0 {
            continue;
        }
        let fd = entry.fd as usize;
        let rights = if fd == 0 { stdin_rights } else { stdout_rights };

        if entry.parent_rfd != 0 && state.vfs_cap != 0 {
            let derive_vfs = if state.session_vfs_cap != 0 {
                state.session_vfs_cap as usize
            } else {
                state.vfs_cap as usize
            };
            fd_derive_requests.push(FdDeriveRequest {
                fd,
                vfs_ep: derive_vfs,
                parent_tid,
                parent_rfd: entry.parent_rfd as usize,
                rights,
                child_tid,
            });
        } else {
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

    let exit_rights = (Rights::IPC_SEND).bits() as usize;
    let exit_token = if state.spawn_ep != 0 {
        token_derive(state.spawn_ep as usize, exit_rights, u64::MAX).unwrap_or(0)
    } else {
        0
    };

    let cookie = (pid as u64) ^ 0xC0DE_0000;

    if fd_derive_requests.is_empty() {
        let result = finish_spawn(state, PendingSpawn {
            cookie,
            pid,
            req: req.clone(),
            parent_tid,
            parent_pid: 0,
            child_space: child_space as u64,
            thread_tok: thread_tok as u64,
            child_tid,
            child_ipc,
            child_self,
            child_space_tok,
            child_registry,
            child_clock,
            exit_token,
            child_stdin,
            child_stdout,
            child_stderr,
            child_stdlog,
            fd_derive_requests: alloc::vec::Vec::new(),
            fd_derive_results: [None; 4],
            fd_derive_remaining: 0,
            minted: alloc::vec::Vec::new(),
            reply_token: None,
            async_reply: None,
        })?;
        Ok(BeginSpawnResult::Complete(result))
    } else {
        let remaining = fd_derive_requests.len();
        Ok(BeginSpawnResult::NeedsAsync(PendingSpawn {
            cookie,
            pid,
            req: req.clone(),
            parent_tid,
            parent_pid: 0,
            child_space: child_space as u64,
            thread_tok: thread_tok as u64,
            child_tid,
            child_ipc,
            child_self,
            child_space_tok,
            child_registry,
            child_clock,
            exit_token,
            child_stdin,
            child_stdout,
            child_stderr,
            child_stdlog,
            fd_derive_requests,
            fd_derive_results: [None; 4],
            fd_derive_remaining: remaining,
            minted: alloc::vec::Vec::new(),
            reply_token: None,
            async_reply: None,
        }))
    }
}

pub fn finish_spawn(
    state: &SessionState,
    mut pending: PendingSpawn,
) -> Result<SpawnResult, RealSpawnError> {
    let mut fd_vfs_meta: [(usize, usize); 4] = [(0, 0); 4];

    for req in &pending.fd_derive_requests {
        if req.fd >= 4 {
            continue;
        }
        match &pending.fd_derive_results[req.fd] {
            Some(Ok((derived_tok, child_cid, child_rfd))) => {
                let _ = libcluu::debug_print(&alloc::format!(
                    "session-procmgr: vfs_derive_child_fd fd={} OK child_cid={} child_rfd={}",
                    req.fd, child_cid, child_rfd,
                ));
                fd_vfs_meta[req.fd] = (*child_cid, *child_rfd);
                match req.fd {
                    0 => pending.child_stdin = *derived_tok,
                    1 => pending.child_stdout = *derived_tok,
                    2 => pending.child_stderr = *derived_tok,
                    3 => pending.child_stdlog = *derived_tok,
                    _ => {}
                }
            }
            Some(Err(_)) => {
                let _ = libcluu::debug_print(&alloc::format!(
                    "session-procmgr: vfs_derive_child_fd FAILED fd={}",
                    req.fd,
                ));
                return Err(RealSpawnError::VfsDeriveChildFd);
            }
            None => {
                return Err(RealSpawnError::VfsDeriveChildFd);
            }
        }
    }

    let pid = pending.pid;
    let cookie = pending.cookie;
    let child_space = pending.child_space as usize;
    let thread_tok = pending.thread_tok as usize;
    let child_tid = pending.child_tid;

    let mut tokens = [0usize; 17];
    tokens[TOKEN_STDIN] = pending.child_stdin;
    tokens[TOKEN_STDOUT] = pending.child_stdout;
    tokens[TOKEN_STDERR] = pending.child_stderr;
    tokens[TOKEN_STDLOG] = pending.child_stdlog;
    tokens[TOKEN_SELF] = pending.child_self;
    tokens[TOKEN_SPACE] = pending.child_space_tok;
    tokens[TOKEN_IPC] = pending.child_ipc;
    tokens[TOKEN_CLOCK] = pending.child_clock;
    tokens[TOKEN_REGISTRY] = pending.child_registry;
    tokens[TOKEN_VFS_VIEW_MGR] = state.view_mgr_token as usize;

    let mut argv_payload: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    for arg in &pending.req.argv {
        argv_payload.extend_from_slice(arg.as_bytes());
        argv_payload.push(0u8);
    }
    let argc = pending.req.argv.len();

    let mut env_payload: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    for (k, v) in &pending.req.envp {
        env_payload.extend_from_slice(k.as_bytes());
        env_payload.push(b'=');
        env_payload.extend_from_slice(v.as_bytes());
        env_payload.push(0u8);
    }
    let envc = pending.req.envp.len();

    let cwd_bytes = pending.req.cwd.as_bytes();
    let cwd_clamped_len = cwd_bytes.len().min(CWD_MAX);

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
    if state.session_vfs_cap != 0 {
        params[PARAM_SESSION_VFS_EP] = state.session_vfs_cap;
    }

    let child_info = ProcessInfo {
        exit_token: pending.exit_token,
        exit_cookie: cookie as usize,
        pid: pid as usize,
        tokens,
        params,
    };

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
    if fd_vfs_trailer_fits {
        let trailer_end = fd_vfs_trailer_offset + FD_VFS_TRAILER_SIZE;
        let dst = &mut page[fd_vfs_trailer_offset..trailer_end];
        for (i, &(cid, rfd)) in fd_vfs_meta.iter().enumerate() {
            let off = i * 16;
            dst[off..off + 8].copy_from_slice(&(cid as u64).to_le_bytes());
            dst[off + 8..off + 16].copy_from_slice(&(rfd as u64).to_le_bytes());
        }
    }

    space_map(
        child_space,
        page_base,
        page.as_ptr() as usize,
        READ_ONLY,
        PAGE_SIZE,
    )
    .map_err(|_| RealSpawnError::InfoPageMap)?;

    let _ = libcluu::debug_print(&alloc::format!(
        "session-procmgr: elf_spawn VFS_SET_VIEW vfs_cap={} child_tid={}",
        state.vfs_cap, child_tid,
    ));
    if state.session_vfs_cap != 0 {
        let home = alloc::format!("/home/{}", state.user_name);
        let default_mounts = libcluu::vfs_view::default_mounts_for_profile_and_home(
            CapProfile::USER,
            &home,
        );
        let mut payload = alloc::vec::Vec::new();
        for (src, dst, writable) in &default_mounts {
            let src_bytes = src.as_bytes();
            let dst_bytes = dst.as_bytes();
            payload.extend_from_slice(&(src_bytes.len() as u16).to_le_bytes());
            payload.extend_from_slice(&(dst_bytes.len() as u16).to_le_bytes());
            payload.push(if *writable { 1u8 } else { 0u8 });
            payload.extend_from_slice(&0u64.to_le_bytes());
            payload.extend_from_slice(src_bytes);
            payload.extend_from_slice(dst_bytes);
        }
        let mut msg = Message::new(VFS_SET_VIEW_LABEL, [0; 6], 6);
        msg.words[0] = payload.len();
        msg.words[1] = child_tid;
        msg.words[2] = default_mounts.len();
        msg.words[3] = CapProfile::USER.bits() as usize;
        msg.words[4] = 0usize;
        msg.words[5] = state.view_mgr_token as usize;
        match send_msg_with_payload(state.session_vfs_cap as usize, &msg, &payload) {
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

    let _ = libcluu::debug_print(&alloc::format!(
        "session-procmgr: elf_spawn resuming thread_tok={}",
        thread_tok
    ));
    let _ = thread_set_session(thread_tok, state.sid as u64);
    thread_resume(thread_tok).map_err(|_| RealSpawnError::ThreadCreate)?;

    Ok(SpawnResult {
        thread_tok: thread_tok as u64,
        cookie,
        space_tok: child_space as u64,
        child_tid,
    })
}
