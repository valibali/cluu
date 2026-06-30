//! IPC wire types serialised via postcard.
//! Keep payloads ≤ 4 KiB (matches kernel inline IPC limit).

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::pid::{Pid, SessionId};

/// Session lifetime envelope from root-procmgr → session-procmgr at spawn.
#[derive(Debug, Serialize, Deserialize)]
pub struct SessionEnvelope {
    pub sid: SessionId,
    pub generation: u32,
    pub user_name: String,
    pub profile: String,            // ProfileSpec serialised
    pub pid_base: i32,              // sid << 23
    /// Caps minted by root for this session (handles by name → token).
    pub caps: Vec<(String, u64)>,
    pub env_defaults: Vec<(String, String)>,
    pub view_spec: String,          // serialised view (mount table)
}

/// Spawn request (session-procmgr child spawn).
#[derive(Debug, Serialize, Deserialize)]
pub struct SpawnReq {
    pub image_path: String,
    pub argv: Vec<String>,
    pub envp: Vec<(String, String)>,
    pub cwd: String,
    pub fd_inherit: Vec<FdInheritEntry>,
    pub notify: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FdInheritEntry {
    pub fd: i32,
    pub kind: FdKind,
    pub cap_token: u64,
    /// VFS-side remote fd of the *parent* (only meaningful for VfsFile/Pts).
    /// session-procmgr passes this to VFS_DERIVE_CHILD_FD when minting the
    /// child's VFS-backed fd.  0 for legacy (tty/pipe) entries.
    pub parent_rfd: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum FdKind {
    VfsFile,
    VfsPipe,
    Pts,
    Tty,
    Null,
    Zero,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SpawnReply {
    pub pid: Pid,
    pub cookie: u64,
}

/// Exit notification (crt0 → session-procmgr).
#[derive(Debug, Serialize, Deserialize)]
pub struct ExitNotif {
    pub cookie: u64,
    pub exit_code: i32,
}

/// Proc query local (root → session-procmgr).
#[derive(Debug, Serialize, Deserialize)]
pub struct ProcQueryLocalReq {
    /// Empty = all procs in this session.
    pub pids: Vec<Pid>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProcQueryLocalReply {
    pub procs: Vec<ProcInfo>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProcInfo {
    pub pid: Pid,
    pub ppid: Pid,
    pub state: u8,
    pub command: String,
    pub argv0: String,
    pub start_ticks: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use postcard::{from_bytes, to_allocvec};

    #[test]
    fn spawn_req_postcard_roundtrip() {
        let req = SpawnReq {
            image_path: "/bin/ls".into(),
            argv: vec!["ls".into(), "-l".into()],
            envp: vec![("PATH".into(), "/bin".into())],
            cwd: "/".into(),
            fd_inherit: vec![FdInheritEntry { fd: 0, kind: FdKind::Pts, cap_token: 42, parent_rfd: 4 }],
            notify: None,
        };
        let bytes = to_allocvec(&req).unwrap();
        let back: SpawnReq = from_bytes(&bytes).unwrap();
        assert_eq!(back.image_path, "/bin/ls");
        assert_eq!(back.fd_inherit[0].cap_token, 42);
    }
}
