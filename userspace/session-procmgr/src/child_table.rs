extern crate alloc;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use procmgr_common::pid::{LOCAL_MAX, LocalPid, Pid, SessionId};

#[derive(Debug, Clone)]
pub struct ChildState {
    pub pid: Pid,
    pub local: LocalPid,
    pub thread_tok: u64,
    /// Raw address-space token returned by `space_create`; used for
    /// `space_destroy` on exit.  Zero means unknown (mock / legacy path).
    pub space_tok: u64,
    /// Kernel-authenticated sender_tid VFS uses as this child's client_id.
    /// Needed at exit to send VFS_SET_VIEW(empty) so VFS drops the child's
    /// fd refs (incl. PTS) and emits PTS_CLOSED to the cluuterm owner.
    pub child_tid: usize,
    pub cookie: u64,
    pub argv0: String,
    pub start_ticks: u64,
    pub minted_caps: Vec<u64>,
    pub pgid: Option<u32>,
    pub notify_ep: u64,
    /// PID of the parent process (the process that requested this spawn).
    /// 0 if the parent is not in this session's child_table (e.g. the session
    /// leader was spawned by login via session-procmgr, but login itself is a
    /// root-procmgr child). Used as `pcid` in /proc/<tid>/stat so top nests
    /// children under their parent.
    pub parent_pid: Pid,
}

pub struct ChildTable {
    sid: SessionId,
    pub next_local: LocalPid, // pub for tests setting exhaustion
    by_pid: BTreeMap<Pid, ChildState>,
    by_cookie: BTreeMap<u64, Pid>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ChildTableError {
    Exhausted,
    NotFound,
}

impl ChildTable {
    pub fn new(sid: SessionId) -> Self {
        Self {
            sid,
            next_local: 1,
            by_pid: BTreeMap::new(),
            by_cookie: BTreeMap::new(),
        }
    }

    pub fn alloc_pid(&mut self) -> Result<Pid, ChildTableError> {
        if self.next_local > LOCAL_MAX {
            return Err(ChildTableError::Exhausted);
        }
        let local = self.next_local;
        self.next_local += 1;
        procmgr_common::pid::encode(self.sid, local)
            .map_err(|_| ChildTableError::Exhausted)
    }

    pub fn insert(&mut self, child: ChildState) {
        self.by_cookie.insert(child.cookie, child.pid);
        self.by_pid.insert(child.pid, child);
    }

    pub fn lookup_by_pid(&self, pid: Pid) -> Option<&ChildState> {
        self.by_pid.get(&pid)
    }

    pub fn set_pgid(&mut self, pid: Pid, pgid: u32) {
        if let Some(child) = self.by_pid.get_mut(&pid) {
            child.pgid = Some(pgid);
        }
    }

    pub fn lookup_by_cookie(&self, cookie: u64) -> Option<&ChildState> {
        self.by_cookie
            .get(&cookie)
            .and_then(|p| self.by_pid.get(p))
    }

    pub fn remove(&mut self, pid: Pid) -> Result<ChildState, ChildTableError> {
        let child = self.by_pid.remove(&pid).ok_or(ChildTableError::NotFound)?;
        self.by_cookie.remove(&child.cookie);
        Ok(child)
    }

    pub fn iter(&self) -> impl Iterator<Item = &ChildState> {
        self.by_pid.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_pid_encodes_session() {
        let mut table = ChildTable::new(5);
        let pid1 = table.alloc_pid().expect("first alloc");
        let pid2 = table.alloc_pid().expect("second alloc");
        assert_eq!(pid1, procmgr_common::pid::encode(5, 1).unwrap());
        assert_eq!(pid2, procmgr_common::pid::encode(5, 2).unwrap());
        // Sanity: LOCAL_BITS=23 gives (5 << 23) | 1 = 0x2800001
        assert_eq!(pid1, 0x2800001);
        assert_eq!(pid2, 0x2800002);
    }

    #[test]
    fn insert_and_lookup() {
        let mut table = ChildTable::new(5);
        let pid = table.alloc_pid().unwrap();
        let cookie = 0xDEAD_BEEF;
        let child = ChildState {
            pid,
            local: 1,
            thread_tok: 0xAAAA,
            space_tok: 0,
            child_tid: 0,
            cookie,
            argv0: "ls".into(),
            start_ticks: 0,
            minted_caps: alloc::vec![],
            pgid: None,
            notify_ep: 0,
            parent_pid: 0,
        };
        table.insert(child);
        assert!(table.lookup_by_pid(pid).is_some());
        assert_eq!(table.lookup_by_pid(pid).unwrap().argv0, "ls");
        assert!(table.lookup_by_cookie(cookie).is_some());
        assert_eq!(table.lookup_by_cookie(cookie).unwrap().pid, pid);
    }

    #[test]
    fn exhaustion() {
        let mut table = ChildTable::new(1);
        table.next_local = LOCAL_MAX;
        // One more alloc should succeed (next_local == LOCAL_MAX is still valid)
        let first = table.alloc_pid();
        assert!(first.is_ok(), "LOCAL_MAX should be encodable");
        // Next alloc: next_local is now LOCAL_MAX + 1, which exceeds limit
        let second = table.alloc_pid();
        assert_eq!(second, Err(ChildTableError::Exhausted));
    }

    #[test]
    fn remove_unknown() {
        let mut table = ChildTable::new(3);
        let fake_pid = procmgr_common::pid::encode(3, 99).unwrap();
        assert_eq!(table.remove(fake_pid).unwrap_err(), ChildTableError::NotFound);
    }
}
