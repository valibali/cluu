//! Production ELF-spawn primitive for session-procmgr (Phase 12.4a).
//!
//! Compiled only on the target (real x86-64 syscalls). Tests use `MockKernel`.
//!
//! 12.4a lays down the primitive but does NOT switch callers. Login still
//! routes through root-procmgr's legacy bypass. 12.4b flips the caller;
//! 12.4c deletes the bypass.

extern crate alloc;

use libcluu::boot::{process_info, TOKEN_SPACE};
use libcluu::fs::VfsClient;
use libcluu::registry;
use libcluu::syscall::{space_create, space_map_range, thread_create};
use procmgr_common::wire::SpawnReq;

use crate::dispatch::SessionState;

const CHILD_STACK_BASE: usize = 0x6d00_0000;
const CHILD_STACK_PAGES: usize = 16;
const CHILD_STACK_SIZE: usize = CHILD_STACK_PAGES * 4096;
const PROT_RW_USER: usize = 0x7;
const ANON_ZERO: usize = 0;

#[derive(Debug)]
pub enum RealSpawnError {
    SpaceCreate,
    VfsOpen,
    MapElf,
    StackMap,
    ThreadCreate,
}

/// Perform a real per-session ELF spawn.
///
/// Steps: create address space, VFS map_elf via session vfs_cap, allocate
/// zero-filled stack, thread_create at the ELF entry point.
///
/// Returns `(thread_tok, cookie)` on success. Caller is responsible for
/// inserting the child into the session's `ChildTable` and revoking minted
/// caps on failure.
///
/// **Limitations (12.4a):** no `ProcessInfo` page handoff (argv, envp,
/// fd_inherit, token slots). The child will FATAL on startup until 12.4b
/// wires the full envelope. This primitive exists to validate the
/// `space_create + map_elf + thread_create` flow under the session's
/// `vfs_cap`.
pub fn real_spawn_user_process(
    state: &SessionState,
    pid: i32,
    req: &SpawnReq,
) -> Result<(u64, u64), RealSpawnError> {
    let info = process_info();
    let root_space = info.tokens[TOKEN_SPACE];

    let child_space = space_create(root_space).map_err(|_| RealSpawnError::SpaceCreate)?;

    let vfs = VfsClient::new(state.vfs_cap as usize, registry::control_endpoint());
    let file = vfs
        .open(&req.image_path)
        .map_err(|_| RealSpawnError::VfsOpen)?;
    let entry = vfs
        .map_elf(file, child_space)
        .map_err(|_| RealSpawnError::MapElf)?;

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

    let thread_tok = thread_create(child_space, entry, stack_top, 0, 0)
        .map_err(|_| RealSpawnError::ThreadCreate)?;

    let cookie = (pid as u64) ^ 0xC0DE_0000;
    Ok((thread_tok as u64, cookie))
}
