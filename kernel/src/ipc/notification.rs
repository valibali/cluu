//! Async Notification Objects (seL4-style)
//!
//! Lightweight signaling mechanism using a u64 bitmask with OR accumulation.
//! Single-waiter invariant: at most one thread can wait on a notification at a time.

use crate::error::Error;
use crate::sched::ThreadId;
use crate::token::NotificationId;
use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

/// A notification object: u64 pending word, optional single waiter, consumed word.
struct Notification {
    /// Accumulated signal bits (OR'd together by signal()).
    pending: u64,
    /// Thread currently waiting on this notification (at most one).
    waiter: Option<ThreadId>,
    /// Bits consumed by the waiter after a signal wakes it.
    /// Set under shard lock by signal() before clearing waiter.
    consumed_word: u64,
}

impl Notification {
    fn new() -> Self {
        Self {
            pending: 0,
            waiter: None,
            consumed_word: 0,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Sharded Notification Registry
// ═══════════════════════════════════════════════════════════════════════════

const NUM_NOTIFICATION_SHARDS: usize = 8;

struct NotificationShard {
    notifications: BTreeMap<NotificationId, Notification>,
}

impl NotificationShard {
    const fn new() -> Self {
        Self {
            notifications: BTreeMap::new(),
        }
    }
}

#[inline(always)]
fn hash_notification_id(id: NotificationId) -> usize {
    (id.0 as usize) % NUM_NOTIFICATION_SHARDS
}

static NOTIFICATION_SHARDS: [Mutex<NotificationShard>; NUM_NOTIFICATION_SHARDS] =
    [const { Mutex::new(NotificationShard::new()) }; NUM_NOTIFICATION_SHARDS];

#[inline(always)]
fn get_shard(id: NotificationId) -> &'static Mutex<NotificationShard> {
    &NOTIFICATION_SHARDS[hash_notification_id(id)]
}

static NEXT_NOTIFICATION_ID: AtomicU64 = AtomicU64::new(1);

/// Global live-notification counter.
static TOTAL_NOTIFICATION_COUNT: AtomicU64 = AtomicU64::new(0);

/// Hard cap on total live notifications system-wide.
const MAX_TOTAL_NOTIFICATIONS: u64 = 4096;

// ═══════════════════════════════════════════════════════════════════════════
// Public API
// ═══════════════════════════════════════════════════════════════════════════

/// Create a new notification object. Returns its ID.
pub fn try_create_notification() -> Result<NotificationId, Error> {
    loop {
        let current = TOTAL_NOTIFICATION_COUNT.load(Ordering::Relaxed);
        if current >= MAX_TOTAL_NOTIFICATIONS {
            return Err(Error::OutOfMemory);
        }
        if TOTAL_NOTIFICATION_COUNT
            .compare_exchange_weak(current, current + 1, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            break;
        }
    }
    let id = NotificationId::new(NEXT_NOTIFICATION_ID.fetch_add(1, Ordering::SeqCst));
    let shard = get_shard(id);
    shard.lock().notifications.insert(id, Notification::new());
    Ok(id)
}

/// Destroy a notification object. Wakes any blocked waiter with error.
pub fn destroy_notification(id: NotificationId) {
    let waiter = {
        let shard = get_shard(id);
        let mut guard = shard.lock();
        let notif = match guard.notifications.remove(&id) {
            Some(n) => n,
            None => return,
        };
        TOTAL_NOTIFICATION_COUNT.fetch_sub(1, Ordering::Relaxed);
        notif.waiter
    };

    // Wake blocked waiter outside shard lock
    if let Some(tid) = waiter {
        crate::sched::ThreadManager::with_thread_mut(tid, |t| {
            t.notification_wait = None;
            t.context.rax = crate::Error::NotFound.to_errno() as u64;
        });
        crate::sched::ThreadManager::wake_thread(tid);
    }
}

/// Signal a notification: OR `bits` into pending.
///
/// If a thread is waiting, atomically move pending into consumed_word,
/// clear the waiter, and wake the thread.
///
/// Returns `Ok(Some(tid))` if a waiter was woken, `Ok(None)` if bits accumulated.
pub fn signal(id: NotificationId, bits: u64) -> Result<Option<ThreadId>, Error> {
    let waiter_to_wake = {
        let shard = get_shard(id);
        let mut guard = shard.lock();
        let notif = guard.notifications.get_mut(&id).ok_or(Error::NotFound)?;

        notif.pending |= bits;

        if let Some(tid) = notif.waiter.take() {
            // Move pending into consumed_word under lock, then clear pending
            notif.consumed_word = notif.pending;
            notif.pending = 0;
            Some(tid)
        } else {
            None
        }
    };

    // Wake outside shard lock
    if let Some(tid) = waiter_to_wake {
        crate::sched::ThreadManager::with_thread_mut(tid, |t| {
            t.notification_wait = None;
        });
        crate::sched::ThreadManager::wake_thread(tid);
        Ok(Some(tid))
    } else {
        Ok(None)
    }
}

/// Try to consume pending bits without blocking.
///
/// If pending != 0, atomically takes pending and stores in consumed_word.
/// Returns `Ok((bits, true))` if bits were consumed.
/// Returns `Ok((0, false))` if nothing pending — caller should register as waiter and block.
pub fn try_wait(id: NotificationId, caller: ThreadId) -> Result<(u64, bool), Error> {
    let shard = get_shard(id);
    let mut guard = shard.lock();
    let notif = guard.notifications.get_mut(&id).ok_or(Error::NotFound)?;

    if notif.pending != 0 {
        let bits = notif.pending;
        notif.pending = 0;
        notif.consumed_word = bits;
        Ok((bits, true))
    } else {
        // Single-waiter invariant
        if notif.waiter.is_some() {
            return Err(Error::Busy);
        }
        notif.waiter = Some(caller);
        Ok((0, false))
    }
}

/// Non-blocking poll: return current pending bits without consuming.
pub fn poll(id: NotificationId) -> Result<u64, Error> {
    let shard = get_shard(id);
    let guard = shard.lock();
    let notif = guard.notifications.get(&id).ok_or(Error::NotFound)?;
    Ok(notif.pending)
}

/// Read consumed_word for a notification (called after waking from wait).
pub fn read_consumed(id: NotificationId) -> Result<u64, Error> {
    let shard = get_shard(id);
    let guard = shard.lock();
    let notif = guard.notifications.get(&id).ok_or(Error::NotFound)?;
    Ok(notif.consumed_word)
}

/// Clear a specific waiter (used during thread death cleanup).
pub fn clear_waiter(id: NotificationId, tid: ThreadId) {
    let shard = get_shard(id);
    let mut guard = shard.lock();
    if let Some(notif) = guard.notifications.get_mut(&id) {
        if notif.waiter == Some(tid) {
            notif.waiter = None;
        }
    }
}
