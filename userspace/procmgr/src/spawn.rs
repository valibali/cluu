//! Procmgr-internal spawn function.
//!
//! `spawn(envelope, caller_pid)` is the single entry point called by:
//! - the unified IPC dispatch handler (Task 9)
//! - procmgr autostart (Task 14)
//! - SESSION_LOGIN internal spawns (Task 15)
//! - the PRIMORDIAL_SEED handler (Task 12)
//!
//! 10-step body per spec 1 §12.

use alloc::string::String;
use alloc::vec::Vec;

use cluu_proto::spawn::{
    FdInherit, FdSource, RestartPolicy, SpawnEnvelope, SpawnError, SpawnReply, ViewSource,
};
use cluu_proto::TokenHandle;

use crate::manifest_cache::{CachedManifest, MANIFEST_CACHE};
use crate::view_table::{verify_monotone, ViewObjectId, VIEW_TABLE};

/// True if `caller_pid` is the init pid or the procmgr itself (in-process call).
fn is_system_caller(caller_pid: u32, procmgr_self_pid: u32) -> bool {
    caller_pid == hooks::init_pid() || caller_pid == procmgr_self_pid
}

/// The single spawn entry point. Returns Ok(SpawnReply) on success or
/// Err(SpawnError) with a concrete discriminant on failure. No timeouts;
/// no waits on the child.
pub fn spawn(envelope: SpawnEnvelope, caller_pid: u32) -> Result<SpawnReply, SpawnError> {
    // Step 1: deserialize already happened at the IPC boundary; envelope is in.

    // Step 2: load manifest from cache (or VFS on miss).
    let manifest = MANIFEST_CACHE
        .get_or_load(&envelope.image, || load_manifest_from_vfs(&envelope.image))
        .ok_or(SpawnError::ImageNotFound)?;

    // Compute the process identity (basename of entrypoint), spec 1 §6.
    let comm: String = basename(&manifest.entrypoint).into();

    // Override argv[0] with comm (spec 1 §6).
    let mut argv = envelope.args.clone();
    if argv.is_empty() {
        argv.push(comm.clone());
    } else {
        argv[0] = comm.clone();
    }

    // Step 3: resolve & derive caps.
    let mut rollback = RollbackList::default();

    let view_id = match &envelope.view {
        ViewSource::Derive(parent_token) => {
            let parent_view_id = resolve_view_token(*parent_token, caller_pid)?;
            let child_view = hooks::narrow_for_manifest(parent_view_id, &manifest)
                .map_err(|_| SpawnError::ViewDeriveDenied)?;
            rollback.view = Some(child_view);
            child_view
        }
        ViewSource::BootstrapRoot => {
            if caller_pid != hooks::init_pid() {
                return Err(SpawnError::ViewDeriveDenied);
            }
            let v = hooks::build_root_view_for_primordial(&manifest)
                .map_err(|_| SpawnError::ViewDeriveDenied)?;
            rollback.view = Some(v);
            v
        }
    };

    let session_id = match envelope.session {
        None => {
            let procmgr_pid = hooks::procmgr_self_pid();
            if !is_system_caller(caller_pid, procmgr_pid)
                && !manifest.allow_sessionless
                && !hooks::caller_can_spawn_sessionless(caller_pid)
            {
                rollback_all(rollback);
                return Err(SpawnError::PermissionDenied);
            }
            None
        }
        Some(t) => {
            let resolved = crate::session_table::SESSION_TABLE.resolve(
                t,
                caller_pid,
                cluu_proto::session::RIGHT_SESSION_JOIN,
            );
            match resolved {
                Err(_) => {
                    rollback_all(rollback.clone());
                    return Err(SpawnError::SessionRevoked);
                }
                Ok((sid, _rights)) => {
                    rollback.session_id = Some(sid);
                    Some(sid)
                }
            }
        }
    };

    let notify_derived = match envelope.notify {
        None => None,
        Some(t) => {
            let raw = hooks::resolve_token(t, caller_pid).ok_or_else(|| {
                rollback_all(rollback.clone());
                SpawnError::NotifyTokenInvalid
            })?;
            let derived = hooks::derive_send(raw).ok_or_else(|| {
                rollback_all(rollback.clone());
                SpawnError::NotifyTokenInvalid
            })?;
            rollback.notify_token = Some(derived);
            Some(derived)
        }
    };

    // Step 4: allocate Space + initial Thread.
    let (space, child_tid) = hooks::alloc_child_space_and_thread().map_err(|_| {
        rollback_all(rollback.clone());
        SpawnError::OutOfMemory
    })?;
    rollback.space = Some(space);

    // Step 5: install fd_inherit entries.
    let mut inherited_for_pi: Vec<(u32, u64, u32)> = Vec::with_capacity(envelope.fd_inherit.len());
    for entry in &envelope.fd_inherit {
        match &entry.source {
            FdSource::VfsFd { vfs_client_id, vfs_remote_fd } => {
                hooks::vfs_derive_child_fd(*vfs_client_id, *vfs_remote_fd, child_tid, entry.child_fd)
                    .map_err(|_| {
                        rollback_all(rollback.clone());
                        SpawnError::FdInheritDeniedAt(entry.child_fd)
                    })?;
                rollback.installed_fds.push(entry.child_fd);
                inherited_for_pi.push((entry.child_fd, *vfs_client_id, *vfs_remote_fd));
            }
            FdSource::EndpointCap { endpoint_token } => {
                hooks::inherit_endpoint_cap(*endpoint_token, child_tid, entry.child_fd)
                    .map_err(|_| {
                        rollback_all(rollback.clone());
                        SpawnError::FdInheritDeniedAt(entry.child_fd)
                    })?;
                rollback.installed_fds.push(entry.child_fd);
                inherited_for_pi.push((entry.child_fd, 0, 0));
            }
        }
    }

    // Step 6: load ELF.
    let _entry = hooks::load_elf(&envelope.image, space).map_err(|_| {
        rollback_all(rollback.clone());
        SpawnError::ImageNotFound
    })?;

    // Step 7: write ProcessInfo page.
    hooks::write_process_info(space, &argv, &envelope.env, &inherited_for_pi).map_err(|_| {
        rollback_all(rollback.clone());
        SpawnError::Internal(0xE0000040u32)
    })?;

    // Step 8: insert ProcessEntry — first non-rollback-able step.
    let pid = hooks::insert_process_entry(
        child_tid,
        space,
        &envelope.image,
        &comm,
        caller_pid,
        session_id,
        view_id,
        notify_derived,
        manifest.restart_policy,
        envelope.clone(),
    )
    .map_err(|_| {
        rollback_all(rollback.clone());
        SpawnError::Internal(0xE0000050u32)
    })?;
    rollback.process_entry_pid = Some(pid);

    // Step 8b: if session-scoped, bump the session's refcount so the child
    // counts as a member. The exit path decrements it via SESSION_TABLE.
    if let Some(session_id) = session_id {
        if !crate::session_table::SESSION_TABLE.inc_refcount(session_id) {
            rollback_all(rollback.clone());
            return Err(SpawnError::SessionRevoked);
        }
    }

    // Step 9: start the thread.
    hooks::resume_thread(child_tid).map_err(|_| {
        SpawnError::Internal(0xE0000051u32)
    })?;

    // Step 10: derive a thread token for the caller.
    let child_thread_token = hooks::derive_thread_token_for_caller(child_tid, caller_pid)
        .map_err(|_| SpawnError::Internal(0xE0000052u32))?;

    Ok(SpawnReply { pid, child_thread_token })
}

fn resolve_view_token(_token: TokenHandle, _caller_pid: u32) -> Result<ViewObjectId, SpawnError> {
    // The engineer wires this to the procmgr-side mapping of TokenHandle
    // → ViewObjectId.
    Err(SpawnError::ViewDeriveDenied)
}

fn load_manifest_from_vfs(_image: &str) -> Option<CachedManifest> {
    None
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[derive(Clone, Default)]
struct RollbackList {
    view: Option<ViewObjectId>,
    session_id: Option<u32>,
    notify_token: Option<TokenHandle>,
    space: Option<u64>,
    installed_fds: Vec<u32>,
    process_entry_pid: Option<u32>,
}

fn rollback_all(_rb: RollbackList) {
    // The engineer wires rollback actions.
}

/// Hook stubs that the engineer wires to existing procmgr helpers in main.rs.
/// Once all hooks are wired to existing procmgr helpers, no `unimplemented!()` will remain.
mod hooks {
    use cluu_proto::TokenHandle;
    use crate::view_table::ViewObjectId;
    use crate::manifest_cache::CachedManifest;

    pub fn resolve_token(_token: TokenHandle, _caller_pid: u32) -> Option<u64> {
        None
    }

    pub fn derive_send(_raw_endpoint: u64) -> Option<TokenHandle> {
        None
    }

    pub fn vfs_derive_child_fd(
        _vfs_client_id: u64,
        _vfs_remote_fd: u32,
        _child_tid: u64,
        _child_fd: u32,
    ) -> Result<TokenHandle, ()> {
        Err(())
    }

    pub fn inherit_endpoint_cap(
        _endpoint_token: u64,
        _child_tid: u64,
        _child_fd: u32,
    ) -> Result<TokenHandle, ()> {
        Err(())
    }

    pub fn alloc_child_space_and_thread() -> Result<(u64, u64), ()> {
        Err(())
    }

    pub fn load_elf(_image: &str, _space: u64) -> Result<u64, ()> {
        Err(())
    }

    pub fn write_process_info(
        _space: u64,
        _argv: &[alloc::string::String],
        _env: &[(alloc::string::String, alloc::string::String)],
        _inherited_fds: &[(u32, u64, u32)],
    ) -> Result<(), ()> {
        Err(())
    }

    pub fn insert_process_entry(
        _tid: u64,
        _space: u64,
        _image: &str,
        _comm: &str,
        _parent_pid: u32,
        _session_id: Option<u32>,
        _view_id: u32,
        _notify: Option<TokenHandle>,
        _restart_policy: cluu_proto::spawn::RestartPolicy,
        _restart_envelope: cluu_proto::spawn::SpawnEnvelope,
    ) -> Result<u32, ()> {
        Err(())
    }

    pub fn resume_thread(_tid: u64) -> Result<(), ()> {
        Err(())
    }

    pub fn derive_thread_token_for_caller(
        _child_tid: u64,
        _caller_pid: u32,
    ) -> Result<TokenHandle, ()> {
        Err(())
    }

    pub fn resolve_session_token(
        _token: TokenHandle,
        _caller_pid: u32,
    ) -> Result<u32, ()> {
        Err(())
    }

    pub fn dec_session_refcount(_session_id: u32) {}

    pub fn revoke_procmgr_token(_token: TokenHandle) {}

    pub fn destroy_space(_space: u64) {}

    pub fn caller_can_spawn_sessionless(_caller_pid: u32) -> bool {
        false
    }

    pub fn init_pid() -> u32 {
        0
    }

    pub fn procmgr_self_pid() -> u32 {
        0
    }

    pub fn build_root_view_for_primordial(_manifest: &CachedManifest) -> Result<ViewObjectId, ()> {
        Err(())
    }

    pub fn narrow_for_manifest(
        _parent_view_id: ViewObjectId,
        _manifest: &CachedManifest,
    ) -> Result<ViewObjectId, ()> {
        Err(())
    }
}