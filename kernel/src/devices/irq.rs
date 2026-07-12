//! IRQ endpoint routing (minimal).
//!
//! Delivers hardware IRQ payloads to a userspace endpoint without
//! maintaining a dedicated device buffer.

use crate::error::Error;
use crate::ipc::endpoint;
#[repr(C)]
struct UserMessageTag {
    label: u32,
    words: u8,
    extra: u8,
}

#[repr(C)]
struct UserMessage {
    tag: UserMessageTag,
    words: [usize; 6],
}
use crate::sched::ThreadManager;
use crate::token::EndpointId;
use core::sync::atomic::{AtomicU64, Ordering};

const MAX_IRQS: usize = 16;
const MAX_ENDPOINTS_PER_IRQ: usize = 4;
pub const KBD_RAW_LABEL: u32 = 0x600;

/// Lock-free IRQ endpoint array with shared IRQ support.
///
/// Each IRQ line can have up to MAX_ENDPOINTS_PER_IRQ endpoints attached
/// (PCI shared IRQ). dispatch_irq delivers to ALL attached endpoints;
/// each driver checks its own device ISR to decide if the IRQ is its own.
///
/// Flat array [MAX_IRQS * MAX_ENDPOINTS_PER_IRQ] because AtomicU64 is not
/// Copy, so 2D array initialization isn't allowed. Index: irq * MAX_ENDPOINTS_PER_IRQ + slot.
/// Each slot stores EndpointId as u64, where 0 = None/free.
static IRQ_ENDPOINTS: [AtomicU64; MAX_IRQS * MAX_ENDPOINTS_PER_IRQ] =
    [const { AtomicU64::new(0) }; MAX_IRQS * MAX_ENDPOINTS_PER_IRQ];
static IRQ_MISS_COUNT: AtomicU64 = AtomicU64::new(0);
static IRQ_SEND_FAIL_COUNT: AtomicU64 = AtomicU64::new(0);
static IRQ_LOCK_BUSY_COUNT: AtomicU64 = AtomicU64::new(0);
static IRQ_DELIVERED_COUNT: AtomicU64 = AtomicU64::new(0);
static IRQ_SHARED_FULL_COUNT: AtomicU64 = AtomicU64::new(0);

pub fn attach(irq: u8, endpoint_id: EndpointId) -> Result<(), Error> {
    let irq_index = irq as usize;
    if irq_index >= MAX_IRQS {
        return Err(Error::InvalidArgument);
    }
    let raw = endpoint_id.as_u64();
    let base = irq_index * MAX_ENDPOINTS_PER_IRQ;
    for slot in base..base + MAX_ENDPOINTS_PER_IRQ {
        match IRQ_ENDPOINTS[slot].compare_exchange(0, raw, Ordering::Release, Ordering::Relaxed) {
            Ok(_) => return Ok(()),
            Err(existing) if existing == raw => return Ok(()),
            Err(_) => continue,
        }
    }
    IRQ_SHARED_FULL_COUNT.fetch_add(1, Ordering::Relaxed);
    Err(Error::NoSpace)
}

pub fn dispatch_irq(irq: u8, label: u32, data: u8) {
    let irq_index = irq as usize;
    if irq_index >= MAX_IRQS {
        return;
    }

    let mut delivered = false;
    let base = irq_index * MAX_ENDPOINTS_PER_IRQ;

    for slot in base..base + MAX_ENDPOINTS_PER_IRQ {
        let endpoint_raw = IRQ_ENDPOINTS[slot].load(Ordering::Acquire);
        if endpoint_raw == 0 {
            continue;
        }

        let endpoint_id = EndpointId::new(endpoint_raw);

        let msg = UserMessage {
            tag: UserMessageTag {
                label,
                words: 1,
                extra: 0,
            },
            words: [data as usize, 0, 0, 0, 0, 0],
        };
        let msg_bytes = unsafe {
            core::slice::from_raw_parts(
                &msg as *const UserMessage as *const u8,
                core::mem::size_of::<UserMessage>(),
            )
        };

        match endpoint::try_send(endpoint_id, msg_bytes) {
            Ok(Some(thread_id)) => {
                IRQ_DELIVERED_COUNT.fetch_add(1, Ordering::Relaxed);
                ThreadManager::wake_thread(thread_id);
                delivered = true;
            }
            Ok(None) => {
                IRQ_DELIVERED_COUNT.fetch_add(1, Ordering::Relaxed);
                delivered = true;
            }
            Err(Error::WouldBlock) => {
                IRQ_LOCK_BUSY_COUNT.fetch_add(1, Ordering::Relaxed);
            }
            Err(_) => {
                IRQ_SEND_FAIL_COUNT.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    if !delivered {
        if IRQ_MISS_COUNT.fetch_add(1, Ordering::Relaxed) == 0 {
            klibcluu::warn("dispatch_irq: no endpoint bound for IRQ");
        }
    }
}

pub fn dispatch_scancode(irq: u8, scancode: u8) {
    dispatch_irq(irq, KBD_RAW_LABEL, scancode);
}
