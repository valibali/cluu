//! Futex wait queues keyed by (address space, user address).
//!
//! This module only tracks waiters and wakeups. Value validation is done in
//! syscall handlers before enqueueing a waiter.

use crate::sched::{ThreadId, ThreadManager};
use crate::token::scope::AddressSpaceId;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use lazy_static::lazy_static;
use spin::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct FutexKey {
    space_id: AddressSpaceId,
    user_addr: usize,
}

impl FutexKey {
    const fn new(space_id: AddressSpaceId, user_addr: usize) -> Self {
        Self {
            space_id,
            user_addr,
        }
    }
}

struct FutexQueues {
    waiters: BTreeMap<FutexKey, VecDeque<ThreadId>>,
}

impl FutexQueues {
    const fn new() -> Self {
        Self {
            waiters: BTreeMap::new(),
        }
    }

    fn enqueue(&mut self, key: FutexKey, thread_id: ThreadId) {
        self.waiters.entry(key).or_default().push_back(thread_id);
    }

    fn remove_waiter(&mut self, key: FutexKey, thread_id: ThreadId) {
        let mut empty = false;
        if let Some(queue) = self.waiters.get_mut(&key) {
            if let Some(pos) = queue.iter().position(|id| *id == thread_id) {
                queue.remove(pos);
            }
            empty = queue.is_empty();
        }
        if empty {
            self.waiters.remove(&key);
        }
    }

    fn pop_wake_candidates(&mut self, key: FutexKey, max_count: usize) -> Vec<ThreadId> {
        let mut out = Vec::new();
        let mut empty = false;
        if let Some(queue) = self.waiters.get_mut(&key) {
            for _ in 0..max_count {
                let Some(thread_id) = queue.pop_front() else {
                    break;
                };
                out.push(thread_id);
            }
            empty = queue.is_empty();
        }
        if empty {
            self.waiters.remove(&key);
        }
        out
    }
}

lazy_static! {
    static ref FUTEX_QUEUES: Mutex<FutexQueues> = Mutex::new(FutexQueues::new());
}

pub fn enqueue_waiter(space_id: AddressSpaceId, user_addr: usize, thread_id: ThreadId) {
    FUTEX_QUEUES
        .lock()
        .enqueue(FutexKey::new(space_id, user_addr), thread_id);
}

pub fn remove_waiter(space_id: AddressSpaceId, user_addr: usize, thread_id: ThreadId) {
    FUTEX_QUEUES
        .lock()
        .remove_waiter(FutexKey::new(space_id, user_addr), thread_id);
}

pub fn wake(space_id: AddressSpaceId, user_addr: usize, max_count: usize) -> usize {
    let key = FutexKey::new(space_id, user_addr);
    let candidates = FUTEX_QUEUES.lock().pop_wake_candidates(key, max_count);

    let mut woken = 0usize;
    let mut requeue = Vec::new();
    for thread_id in candidates {
        let blocked =
            ThreadManager::with_thread(thread_id, |thread| thread.is_blocked()).unwrap_or(false);
        if !blocked {
            requeue.push(thread_id);
            continue;
        }
        ThreadManager::wake_thread(thread_id);
        woken += 1;
    }
    if !requeue.is_empty() {
        let mut queues = FUTEX_QUEUES.lock();
        for thread_id in requeue {
            queues.enqueue(key, thread_id);
        }
    }
    woken
}
