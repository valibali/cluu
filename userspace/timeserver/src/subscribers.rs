// userspace/timeserver/src/subscribers.rs
//! Periodic-tick subscriber table.
//!
//! Capped at MAX_SUBSCRIBERS slots. Identified by sender_tid; re-subscribing
//! with the same tid replaces the prior slot.

#![allow(dead_code)]

const MAX_SUBSCRIBERS: usize = 64;
const MIN_PERIOD_MS: u32 = 10;
const MAX_PERIOD_MS: u32 = 60_000;
const MAX_CONSECUTIVE_FAILS: u8 = 3;

#[derive(Copy, Clone)]
pub struct Subscriber {
    pub tid: usize,
    pub notify_ep: usize,
    pub period_ms: u32,
    pub next_deadline_ms: u64,
    pub tick_count: u64,
    pub fail_count: u8,
}

pub struct SubscriberTable {
    slots: [Option<Subscriber>; MAX_SUBSCRIBERS],
    len: usize,
}

impl SubscriberTable {
    pub const fn new() -> Self {
        Self { slots: [None; MAX_SUBSCRIBERS], len: 0 }
    }
    pub fn len(&self) -> usize { self.len }
    pub fn insert(&mut self, tid: usize, notify_ep: usize, period_ms: u32, now_ms: u64) -> Result<(), u64> {
        if period_ms == 0 || period_ms > MAX_PERIOD_MS { return Err(22); /* EINVAL */ }
        let period = period_ms.max(MIN_PERIOD_MS);
        for slot in self.slots.iter_mut() {
            if let Some(s) = slot {
                if s.tid == tid {
                    *s = Subscriber {
                        tid, notify_ep, period_ms: period,
                        next_deadline_ms: now_ms.saturating_add(period as u64),
                        tick_count: 0, fail_count: 0,
                    };
                    return Ok(());
                }
            }
        }
        for slot in self.slots.iter_mut() {
            if slot.is_none() {
                *slot = Some(Subscriber {
                    tid, notify_ep, period_ms: period,
                    next_deadline_ms: now_ms.saturating_add(period as u64),
                    tick_count: 0, fail_count: 0,
                });
                self.len += 1;
                return Ok(());
            }
        }
        Err(28) /* ENOSPC */
    }
    pub fn remove(&mut self, tid: usize) -> Result<(), u64> {
        for slot in self.slots.iter_mut() {
            if let Some(s) = slot {
                if s.tid == tid { *slot = None; self.len -= 1; return Ok(()); }
            }
        }
        Err(2) /* ENOENT */
    }
    pub fn next_deadline_ms(&self) -> u64 {
        self.slots.iter().filter_map(|s| s.as_ref().map(|x| x.next_deadline_ms))
            .min().unwrap_or(u64::MAX)
    }
    pub fn iter_due_mut<'a>(&'a mut self, now_ms: u64) -> impl Iterator<Item = &'a mut Subscriber> {
        self.slots.iter_mut().filter_map(move |s| s.as_mut()).filter(move |s| s.next_deadline_ms <= now_ms)
    }
    pub fn remove_tid(&mut self, tid: usize) { let _ = self.remove(tid); }
}

impl Subscriber {
    pub fn advance_deadline(&mut self, now_ms: u64) {
        let next = self.next_deadline_ms.saturating_add(self.period_ms as u64);
        if next <= now_ms {
            self.next_deadline_ms = now_ms.saturating_add(self.period_ms as u64);
        } else {
            self.next_deadline_ms = next;
        }
    }
    pub fn record_send(&mut self, ok: bool) -> bool {
        if ok { self.fail_count = 0; self.tick_count = self.tick_count.saturating_add(1); false }
        else { self.fail_count = self.fail_count.saturating_add(1); self.fail_count >= MAX_CONSECUTIVE_FAILS }
    }
    pub fn tick_count(&self) -> u64 { self.tick_count }
    pub fn tick_count_for_next(&self) -> u64 { self.tick_count.saturating_add(1) }
}
