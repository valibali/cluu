//! Spawn protocol types — see spec 1.
//!
//! Wire envelope for `PROCMGR_SPAWN_UNIFIED_LABEL = 50`.

use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::TokenHandle;

/// Wire label for the unified spawn IPC verb.
pub const PROCMGR_SPAWN_UNIFIED_LABEL: u32 = 50;

/// ProcessInfo.params slot carrying the session-scoped displayd endpoint.
///
/// Installed at session spawn by root-procmgr (the sole holder of the global
/// displayd control endpoint). Session binaries resolve `displayd:main` via
/// this parameter — `subscribe_output("displayd", "main")` returns the
/// session-scoped endpoint, NOT a global one. Analogous to
/// `PARAM_SESSION_VFS_EP` (libcluu::boot, slot 18). The global endpoint never
/// appears in any compositor or descendant envelope (AGENTS.md §2, §3, §5,
/// §6). No runtime ACL — authority is possession of the scoped endpoint.
pub const PARAM_DISPLAYD_EP: usize = 19;

/// One spawn call's payload. Postcard-serialized into the IPC payload buffer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpawnEnvelope {
    /// Image name (1:1 with manifest under `/var/images/<image>/manifest.toml`).
    pub image: String,

    /// argv list. Procmgr overrides `args[0]` with `basename(manifest.entrypoint)`
    /// regardless of value (process-identity rule, spec 1 §6).
    pub args: Vec<String>,

    /// Environment as (key, value) pairs. Newlib `posix_spawn` shim joins to
    /// `KEY=VAL` for the C runtime.
    pub env: Vec<(String, String)>,

    /// View source (parent-derive or bootstrap-root for init primordials).
    pub view: ViewSource,

    /// FD inheritance manifest — sole fd-wiring mechanism on the wire.
    pub fd_inherit: Vec<FdInherit>,

    /// Optional session-token cap. `None` permitted only for sessionless
    /// callers (init, procmgr-internal, or manifests declaring
    /// `RIGHT_SESSIONLESS_SPAWN`).
    pub session: Option<TokenHandle>,

    /// Optional notify endpoint cap. `None` = silent exit; otherwise
    /// procmgr derives IPC_SEND into its own table and fires PROC_EXIT_LABEL
    /// on child exit.
    pub notify: Option<TokenHandle>,
}

/// View origin discriminator. Steady-state uses `Derive`; init bootstrap uses
/// `BootstrapRoot` (rejected for any caller other than init's pid during the
/// one-shot primordial seed handler).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ViewSource {
    Derive(TokenHandle),
    BootstrapRoot,
}

/// One fd-inheritance entry.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FdInherit {
    /// Slot in the child's fd table.
    pub child_fd: u32,
    /// Where the inherited fd comes from.
    pub source: FdSource,
    /// Rights subset — must be ≤ caller's rights on the source fd.
    pub rights: FdRights,
}

/// Where the inherited fd lives. Currently VFS-backed only.
/// Extending to `PipeCap` / `EndpointCap` later is additive.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum FdSource {
    VfsFd {
        vfs_client_id: u64,
        vfs_remote_fd: u32,
    },
    EndpointCap {
        endpoint_token: u64,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct FdRights {
    pub read: bool,
    pub write: bool,
}

impl FdRights {
    pub const READ_ONLY: Self = Self { read: true, write: false };
    pub const WRITE_ONLY: Self = Self { read: false, write: true };
    pub const READ_WRITE: Self = Self { read: true, write: true };

    pub fn is_subset_of(self, other: Self) -> bool {
        (!self.read || other.read) && (!self.write || other.write)
    }
}

/// Restart policy. Lives in this crate so manifest parsing and procmgr storage
/// share the type, but is NOT a `SpawnEnvelope` field — manifest is the source
/// of truth per spec 1 §11.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum RestartPolicy {
    Never,
    Always,
    OnFailure { max: u32, window_ms: u64 },
}

impl Default for RestartPolicy {
    fn default() -> Self {
        RestartPolicy::Never
    }
}

/// Successful reply from `procmgr::spawn`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpawnReply {
    pub pid: u32,
    pub child_thread_token: TokenHandle,
}

/// All ways spawn can fail.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SpawnError {
    /// `envelope.image` not found in `/var/images/`.
    ImageNotFound,
    /// Manifest read but parse failed; payload is the parse error message.
    ManifestInvalid(String),
    /// View token resolution failed, OR derive would widen.
    ViewDeriveDenied,
    /// FD inheritance failed at the given child_fd index.
    FdInheritDeniedAt(u32),
    /// Session token resolution failed (revoked / dying).
    SessionRevoked,
    /// Notify token resolution failed.
    NotifyTokenInvalid,
    /// Caller's manifest does not declare the rights to spawn.
    PermissionDenied,
    /// Kernel resource exhaustion (Space alloc, Thread alloc).
    OutOfMemory,
    /// Diagnostic. Should be rare; if seen, file a bug.
    Internal(u32),
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn sample_envelope() -> SpawnEnvelope {
        SpawnEnvelope {
            image: String::from("shell"),
            args: vec![String::from("shell"), String::from("-c"), String::from("echo hi")],
            env: vec![
                (String::from("HOME"), String::from("/home/dave")),
                (String::from("TERM"), String::from("xterm-256color")),
            ],
            view: ViewSource::Derive(0xDEAD_BEEF_u64),
            fd_inherit: vec![FdInherit {
                child_fd: 0,
                source: FdSource::VfsFd { vfs_client_id: 7, vfs_remote_fd: 3 },
                rights: FdRights::READ_ONLY,
            }],
            session: Some(0xCAFE_F00D_u64),
            notify: Some(0xFACE_BEEF_u64),
        }
    }

    #[test]
    fn envelope_roundtrip() {
        let env = sample_envelope();
        let bytes = postcard::to_allocvec(&env).expect("serialize");
        let decoded: SpawnEnvelope = postcard::from_bytes(&bytes).expect("deserialize");
        assert_eq!(decoded.image, env.image);
        assert_eq!(decoded.args, env.args);
        assert_eq!(decoded.env, env.env);
        assert_eq!(decoded.fd_inherit.len(), env.fd_inherit.len());
        assert_eq!(decoded.session, env.session);
        assert_eq!(decoded.notify, env.notify);
    }

    #[test]
    fn fd_rights_subset() {
        let ro = FdRights::READ_ONLY;
        let rw = FdRights::READ_WRITE;
        assert!(ro.is_subset_of(rw));
        assert!(!rw.is_subset_of(ro));
        assert!(ro.is_subset_of(ro));
    }

    #[test]
    fn spawn_error_roundtrip() {
        let err = SpawnError::FdInheritDeniedAt(2);
        let bytes = postcard::to_allocvec(&err).expect("serialize");
        let decoded: SpawnError = postcard::from_bytes(&bytes).expect("deserialize");
        assert_eq!(decoded, err);
    }

    #[test]
    fn bootstrap_root_roundtrip() {
        let env = SpawnEnvelope {
            view: ViewSource::BootstrapRoot,
            ..sample_envelope()
        };
        let bytes = postcard::to_allocvec(&env).expect("serialize");
        let decoded: SpawnEnvelope = postcard::from_bytes(&bytes).expect("deserialize");
        match decoded.view {
            ViewSource::BootstrapRoot => (),
            _ => panic!("expected BootstrapRoot"),
        }
    }
}