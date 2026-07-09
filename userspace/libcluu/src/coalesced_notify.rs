//! Level-triggered notification coalescer for fire-and-forget fan-out.
//!
//! Problem: a broadcaster (e.g., compositor) that sends notifications to N
//! endpoints via `ipc_send` can pile up messages if a receiver is slow or
//! full. Each un-acked notification occupies a queue slot.
//!
//! Solution: `CoalescedNotify` tracks one pending bit per (endpoint, label).
//! `notify()` marks the bit and attempts a single `ipc_send`. `ack()` clears
//! it. If a second `notify()` arrives before `ack()`, it's coalesced — the
//! sender doesn't queue a second message. After `ack()`, a new `notify()`
//! can fire.
//!
//! This bounds fan-out memory to O(subscribers × labels) bits, not
//! O(subscribers × messages) queue slots.
//!
//! # Wire format
//!
//! The notification message carries `label` in `tag.label` and the coalescer
//! key in `words[0]`. The receiver acks by sending back the same label with
//! `words[0]` echoed.

extern crate alloc;
use alloc::collections::BTreeMap;
use crate::error::Result;
use crate::syscall;
use crate::types::Message;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct NotifyKey {
    endpoint: usize,
    label: u32,
}

pub struct CoalescedNotify {
    pending: BTreeMap<NotifyKey, bool>,
}

impl CoalescedNotify {
    pub const fn new() -> Self {
        Self {
            pending: BTreeMap::new(),
        }
    }

    pub fn notify(&mut self, endpoint: usize, label: u32, key: usize) -> Result<()> {
        let k = NotifyKey { endpoint, label };
        if self.pending.get(&k).copied().unwrap_or(false) {
            return Ok(());
        }
        let msg = Message::new(label, [key, 0, 0, 0, 0, 0], 0);
        match syscall::ipc_send(endpoint, msg.as_bytes()) {
            Ok(()) => {
                self.pending.insert(k, true);
                Ok(())
            }
            Err(crate::error::Error::WouldBlock) => {
                self.pending.insert(k, true);
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    pub fn ack(&mut self, endpoint: usize, label: u32) {
        let k = NotifyKey { endpoint, label };
        self.pending.remove(&k);
    }

    pub fn cancel_endpoint(&mut self, endpoint: usize) {
        self.pending.retain(|k, _| k.endpoint != endpoint);
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}
