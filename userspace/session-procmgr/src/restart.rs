extern crate alloc;
use alloc::collections::BTreeMap;

const WINDOW_TICKS: u64 = 30 * 1_000_000;
const THRESHOLD: u32 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    Never,
    Always,
    OnFailure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    NoRestart,
    Restart,
    GiveUp,
}

#[derive(Default)]
pub struct RestartTracker {
    table: BTreeMap<u64, Entry>,
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    attempts: u32,
    first_attempt: u64,
    policy: Policy,
}

impl RestartTracker {
    pub fn new() -> Self {
        Self {
            table: BTreeMap::new(),
        }
    }

    pub fn register(&mut self, cookie: u64, policy: Policy) {
        self.table.insert(
            cookie,
            Entry {
                attempts: 0,
                first_attempt: 0,
                policy,
            },
        );
    }

    pub fn on_exit(&mut self, cookie: u64, exit_code: i32, now: u64) -> Decision {
        let e = match self.table.get_mut(&cookie) {
            Some(e) => e,
            None => return Decision::NoRestart,
        };
        let want_restart = match e.policy {
            Policy::Never => false,
            Policy::Always => true,
            Policy::OnFailure => exit_code != 0,
        };
        if !want_restart {
            return Decision::NoRestart;
        }
        if e.attempts == 0 {
            e.first_attempt = now;
        }
        e.attempts += 1;
        if now.saturating_sub(e.first_attempt) > WINDOW_TICKS {
            e.attempts = 1;
            e.first_attempt = now;
        }
        if e.attempts > THRESHOLD {
            e.policy = Policy::Never;
            return Decision::GiveUp;
        }
        Decision::Restart
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_policy_no_restart() {
        let mut t = RestartTracker::new();
        t.register(1, Policy::Never);
        assert_eq!(t.on_exit(1, 1, 0), Decision::NoRestart);
    }

    #[test]
    fn always_policy_restarts_until_threshold() {
        let mut t = RestartTracker::new();
        t.register(1, Policy::Always);
        for i in 0..THRESHOLD {
            assert_eq!(t.on_exit(1, 0, i as u64), Decision::Restart);
        }
        assert_eq!(t.on_exit(1, 0, (THRESHOLD as u64) + 1), Decision::GiveUp);
    }

    #[test]
    fn on_failure_only_on_nonzero() {
        let mut t = RestartTracker::new();
        t.register(1, Policy::OnFailure);
        assert_eq!(t.on_exit(1, 0, 0), Decision::NoRestart);
        assert_eq!(t.on_exit(1, 1, 1), Decision::Restart);
    }

    #[test]
    fn window_reset() {
        let mut t = RestartTracker::new();
        t.register(1, Policy::Always);
        for i in 0..THRESHOLD {
            assert_eq!(t.on_exit(1, 0, i as u64), Decision::Restart);
        }
        assert_eq!(t.on_exit(1, 0, WINDOW_TICKS + 1000), Decision::Restart);
    }

    #[test]
    fn unknown_cookie_no_restart() {
        let mut t = RestartTracker::new();
        assert_eq!(t.on_exit(99, 1, 0), Decision::NoRestart);
    }
}
