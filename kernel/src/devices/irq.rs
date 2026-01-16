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
use spin::Mutex;

const MAX_IRQS: usize = 16;
const KBD_EVENT_LABEL: u32 = 1;

static IRQ_ENDPOINTS: Mutex<[Option<EndpointId>; MAX_IRQS]> = Mutex::new([None; MAX_IRQS]);
static IRQ_MISS_COUNT: AtomicU64 = AtomicU64::new(0);
static IRQ_SEND_FAIL_COUNT: AtomicU64 = AtomicU64::new(0);
static IRQ_LOCK_BUSY_COUNT: AtomicU64 = AtomicU64::new(0);
static IRQ_DELIVERED_COUNT: AtomicU64 = AtomicU64::new(0);

pub fn attach(irq: u8, endpoint_id: EndpointId) -> Result<(), Error> {
    let irq_index = irq as usize;
    if irq_index >= MAX_IRQS {
        return Err(Error::InvalidArgument);
    }
    IRQ_ENDPOINTS.lock()[irq_index] = Some(endpoint_id);
    Ok(())
}

pub fn dispatch_scancode(irq: u8, scancode: u8) {
    let irq_index = irq as usize;
    if irq_index >= MAX_IRQS {
        return;
    }
    let endpoint_id = match IRQ_ENDPOINTS.try_lock() {
        Some(guard) => match guard[irq_index] {
            Some(id) => id,
            None => {
                if IRQ_MISS_COUNT.fetch_add(1, Ordering::Relaxed) == 0 {
                    klibcluu::warn("dispatch_scancode: no endpoint bound for IRQ");
                }
                return;
            }
        },
        None => {
            if IRQ_LOCK_BUSY_COUNT.fetch_add(1, Ordering::Relaxed) == 0 {
                klibcluu::warn("dispatch_scancode: irq endpoints busy");
            }
            return;
        }
    };

    let msg = UserMessage {
        tag: UserMessageTag {
            label: KBD_EVENT_LABEL,
            words: 1,
            extra: 0,
        },
        words: [scancode as usize, 0, 0, 0, 0, 0],
    };
    let msg_bytes = unsafe {
        core::slice::from_raw_parts(
            &msg as *const UserMessage as *const u8,
            core::mem::size_of::<UserMessage>(),
        )
    };

    match endpoint::try_send(endpoint_id, msg_bytes) {
        Ok(Some(thread_id)) => {
            if IRQ_DELIVERED_COUNT.fetch_add(1, Ordering::Relaxed) == 0 {
                klibcluu::trace("dispatch_scancode: endpoint_id=");
                klibcluu::log_dec(klibcluu::LogLevel::Trace, "", endpoint_id.as_u64());
                klibcluu::trace("dispatch_scancode: wake_thread_id=");
                klibcluu::log_dec(klibcluu::LogLevel::Trace, "", thread_id.as_u64());
            }
            ThreadManager::wake_thread(thread_id);
        }
        Ok(None) => {
            if IRQ_DELIVERED_COUNT.fetch_add(1, Ordering::Relaxed) == 0 {
                klibcluu::trace("dispatch_scancode: endpoint_id=");
                klibcluu::log_dec(klibcluu::LogLevel::Trace, "", endpoint_id.as_u64());
            }
        }
        Err(Error::WouldBlock) => {
            if IRQ_LOCK_BUSY_COUNT.fetch_add(1, Ordering::Relaxed) == 0 {
                klibcluu::warn("dispatch_scancode: endpoint busy");
            }
        }
        Err(_) => {
            if IRQ_SEND_FAIL_COUNT.fetch_add(1, Ordering::Relaxed) == 0 {
                klibcluu::warn("dispatch_scancode: send failed");
            }
        }
    }
}
