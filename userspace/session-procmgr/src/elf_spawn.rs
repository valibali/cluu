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
    PARAM_ENVC, PARAM_ENV_OFFSET, PROCESS_INFO_ADDR, TOKEN_CLOCK, TOKEN_IPC, TOKEN_REGISTRY,
    TOKEN_SELF, TOKEN_SPACE, TOKEN_STDERR, TOKEN_STDIN, TOKEN_STDLOG, TOKEN_STDOUT,
    ProcessInfo,
};
use libcluu::fs::VfsClient;
use libcluu::registry;
use libcluu::rights::Rights;
use libcluu::syscall::{space_create, space_map, space_map_range, thread_create, token_derive};
use procmgr_common::wire::SpawnReq;

use crate::dispatch::SessionState;

const CHILD_STACK_BASE: usize = 0x6d00_0000;
const CHILD_STACK_PAGES: usize = 16;
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
/// Returns `(thread_tok, cookie)` on success. Caller is responsible for
/// inserting the child into the session's `ChildTable` and revoking minted
/// caps on failure.
pub fn real_spawn_user_process(
    state: &SessionState,
    pid: i32,
    req: &SpawnReq,
) -> Result<(u64, u64), RealSpawnError> {
    // ── 1. Create child address space ──────────────────────────────────────
    let info = process_info();
    let our_space = info.tokens[TOKEN_SPACE];

    let child_space = space_create(our_space).map_err(|_| RealSpawnError::SpaceCreate)?;

    // ── 2. Map ELF via VFS ─────────────────────────────────────────────────
    let vfs = VfsClient::new(state.vfs_cap as usize, registry::control_endpoint());
    let file = vfs
        .open(&req.image_path)
        .map_err(|_| RealSpawnError::VfsOpen)?;
    let entry = vfs
        .map_elf(file, child_space)
        .map_err(|_| RealSpawnError::MapElf)?;

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

    // ── 4. Derive child capability tokens ──────────────────────────────────

    // TOKEN_IPC: children may create endpoints + call procmgr/vfs
    let ipc_rights = (Rights::IPC_SEND
        | Rights::IPC_RECV
        | Rights::IPC_CALL
        | Rights::CREATE
        | Rights::GRANT)
        .bits() as usize;
    let child_ipc = token_derive(info.tokens[TOKEN_IPC], ipc_rights, u64::MAX)
        .map_err(|_| RealSpawnError::TokenDerive)?;

    // TOKEN_SELF: children may spawn threads + grant
    let self_rights =
        (Rights::CREATE | Rights::GRANT | Rights::THREAD_CONTROL).bits() as usize;
    let child_self = token_derive(info.tokens[TOKEN_SELF], self_rights, u64::MAX)
        .map_err(|_| RealSpawnError::TokenDerive)?;

    // TOKEN_SPACE: derived from the new child_space
    let space_rights =
        (Rights::SPACE_MAP | Rights::SPACE_GRANT | Rights::CREATE | Rights::THREAD_CONTROL)
            .bits() as usize;
    let child_space_tok = token_derive(child_space, space_rights, u64::MAX)
        .map_err(|_| RealSpawnError::TokenDerive)?;

    // TOKEN_REGISTRY: derive from state.registry_cap
    let registry_rights =
        (Rights::IPC_SEND | Rights::IPC_CALL | Rights::GRANT).bits() as usize;
    let child_registry = token_derive(state.registry_cap as usize, registry_rights, u64::MAX)
        .map_err(|_| RealSpawnError::TokenDerive)?;

    // TOKEN_CLOCK: derive from state.timeserver_cap
    let clock_rights = (Rights::IPC_CALL | Rights::IPC_SEND).bits() as usize;
    let child_clock = token_derive(state.timeserver_cap as usize, clock_rights, u64::MAX)
        .map_err(|_| RealSpawnError::TokenDerive)?;

    // TOKEN_STDIN/STDOUT/STDERR/STDLOG: derive from fd_inherit entries
    // stdin needs IPC_RECV, stdout/stderr/stdlog need IPC_SEND|IPC_CALL
    let stdin_rights = (Rights::IPC_SEND | Rights::IPC_RECV).bits() as usize;
    let stdout_rights = (Rights::IPC_SEND | Rights::IPC_CALL).bits() as usize;

    let mut child_stdin: usize = 0;
    let mut child_stdout: usize = 0;
    let mut child_stderr: usize = 0;
    let mut child_stdlog: usize = 0;

    for entry in &req.fd_inherit {
        let raw = entry.cap_token as usize;
        if raw == 0 {
            continue;
        }
        match entry.fd {
            0 => {
                child_stdin =
                    token_derive(raw, stdin_rights, u64::MAX).unwrap_or(0);
            }
            1 => {
                child_stdout =
                    token_derive(raw, stdout_rights, u64::MAX).unwrap_or(0);
            }
            2 => {
                child_stderr =
                    token_derive(raw, stdout_rights, u64::MAX).unwrap_or(0);
            }
            3 => {
                child_stdlog =
                    token_derive(raw, stdout_rights, u64::MAX).unwrap_or(0);
            }
            _ => {}
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

    // ── 5. Build ProcessInfo page ──────────────────────────────────────────
    let mut tokens = [0usize; 16];
    tokens[TOKEN_STDIN] = child_stdin;
    tokens[TOKEN_STDOUT] = child_stdout;
    tokens[TOKEN_STDERR] = child_stderr;
    tokens[TOKEN_STDLOG] = child_stdlog;
    tokens[TOKEN_SELF] = child_self;
    tokens[TOKEN_SPACE] = child_space_tok;
    tokens[TOKEN_IPC] = child_ipc;
    tokens[TOKEN_CLOCK] = child_clock;
    tokens[TOKEN_REGISTRY] = child_registry;
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

    // ── 6. Map ProcessInfo page read-only into child ───────────────────────
    space_map(
        child_space,
        page_base,
        page.as_ptr() as usize,
        READ_ONLY,
        PAGE_SIZE,
    )
    .map_err(|_| RealSpawnError::InfoPageMap)?;

    // ── 7. Create child thread at entry point ──────────────────────────────
    let thread_tok = thread_create(child_space, entry, stack_top, 0, 0)
        .map_err(|_| RealSpawnError::ThreadCreate)?;

    Ok((thread_tok as u64, cookie))
}
