//! Process-group table for job control (Phase 4 Plan D).
//!
//! Maps pgid → [pid] and supports create/attach/detach/query operations.
//! This is a pure in-memory data structure; no kernel involvement.

extern crate alloc;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

/// In-memory mapping from process-group id → member pid list.
#[derive(Default)]
pub struct PgTable {
    next_pgid: usize,
    members: BTreeMap<usize, Vec<usize>>,
}

impl PgTable {
    pub const fn new() -> Self {
        Self {
            next_pgid: 1,
            members: BTreeMap::new(),
        }
    }

    /// Allocate a fresh pgid and initialise its (empty) member list.
    pub fn create(&mut self) -> usize {
        let id = self.next_pgid;
        self.next_pgid += 1;
        self.members.insert(id, Vec::new());
        id
    }

    /// Add `pid` to the member list of `pgid` (idempotent).
    pub fn attach(&mut self, pgid: usize, pid: usize) {
        if let Some(v) = self.members.get_mut(&pgid) {
            if !v.contains(&pid) {
                v.push(pid);
            }
        }
    }

    /// Remove `pid` from `pgid`; drops the group when it becomes empty.
    pub fn detach(&mut self, pgid: usize, pid: usize) {
        if let Some(v) = self.members.get_mut(&pgid) {
            v.retain(|&p| p != pid);
            if v.is_empty() {
                self.members.remove(&pgid);
            }
        }
    }

    /// Return a snapshot of the member pids for `pgid`.
    pub fn members(&self, pgid: usize) -> Vec<usize> {
        self.members.get(&pgid).cloned().unwrap_or_default()
    }

    /// True if the group exists (even if currently empty).
    pub fn exists(&self, pgid: usize) -> bool {
        self.members.contains_key(&pgid)
    }

    /// Return the pgid that contains `pid`, or `None`.
    pub fn pgid_of(&self, pid: usize) -> Option<usize> {
        for (pgid, members) in &self.members {
            if members.contains(&pid) {
                return Some(*pgid);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_attach_detach() {
        let mut t = PgTable::new();
        let p = t.create();
        t.attach(p, 100);
        t.attach(p, 100); // idempotent
        assert_eq!(t.members(p), vec![100]);
        t.detach(p, 100);
        assert!(!t.exists(p), "empty group dropped");
    }

    #[test]
    fn pgid_of_finds_member() {
        let mut t = PgTable::new();
        let p1 = t.create();
        let p2 = t.create();
        t.attach(p1, 10);
        t.attach(p2, 20);
        assert_eq!(t.pgid_of(10), Some(p1));
        assert_eq!(t.pgid_of(20), Some(p2));
        assert_eq!(t.pgid_of(99), None);
    }

    #[test]
    fn detach_unknown_pid_idempotent() {
        let mut t = PgTable::new();
        let p = t.create();
        // Pre-attach a real member so the group survives the detach of an unknown pid.
        t.attach(p, 1);
        t.detach(p, 99); // 99 not a member — no panic, group still alive
        assert!(t.exists(p));
        assert_eq!(t.members(p), vec![1]);
    }
}
