//! Virtqueue mechanics smoke test.
//!
//! Exercises five invariants of `cluu_virtio_core::Virtqueue` end-to-end
//! against the layout structures (no real device). Used as a regression
//! gate for the alloc/free/submit/pop_used implementation.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use cluu_virtio_core::{DmaPool, Virtqueue};
use libcluu::boot::{process_info, TOKEN_SPACE};

const POOL_BASE: usize = 0x4000_0000;
const POOL_PAGES: usize = 16;

fn fail(name: &str) -> i32 {
    let _ = libcluu::debug_print(&format!("vqprobe: [FAIL] {}", name));
    1
}

fn ok(name: &str) {
    let _ = libcluu::debug_print(&format!("vqprobe: ok {}", name));
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let info = process_info();
    let space_token = info.tokens[TOKEN_SPACE];

    let mut pool = match DmaPool::new(space_token, POOL_BASE, POOL_PAGES) {
        Ok(p) => p,
        Err(_) => return fail("DmaPool::new"),
    };

    // Build a 64-entry virtqueue.
    let mut vq = match Virtqueue::new(&mut pool, 64) {
        Ok(v) => v,
        Err(_) => return fail("Virtqueue::new"),
    };

    // Invariant 1: free_capacity == queue_size at start.
    if vq.free_capacity() != 64 {
        return fail("init free_capacity != 64");
    }
    ok("init capacity");

    // Invariant 2: alloc_chain(2) reduces free count by 2.
    let chain = match vq.alloc_chain(2) {
        Some(c) => c,
        None => return fail("alloc_chain(2) returned None"),
    };
    if vq.free_capacity() != 62 {
        return fail("after alloc_chain(2) capacity != 62");
    }
    ok("alloc_chain reduces capacity");

    // Invariant 3: free_chain restores capacity.
    vq.free_chain(chain);
    if vq.free_capacity() != 64 {
        return fail("after free_chain capacity != 64");
    }
    ok("free_chain restores");

    // Invariant 4: alloc_chain(N+1) returns None when N free.
    let big = vq.alloc_chain(64).unwrap();
    if vq.alloc_chain(1).is_some() {
        return fail("alloc_chain(1) succeeded with 0 free");
    }
    ok("alloc_chain returns None when full");
    vq.free_chain(big);

    // Invariant 5: submit + pop_used round-trip with cookie. Use a fake
    // "device" — write the used-ring entry by hand, then verify pop_used
    // returns our cookie.
    let chain = vq.alloc_chain(1).unwrap();
    let head = chain.head;
    vq.desc_set(head, 0xDEAD_BEEF, 4096, 0, 0); // single desc, no NEXT
    vq.submit(chain, 0xCAFE_BABE);

    // Pretend the device completed entry 0 with len=42.
    let used_va = vq.used_region.virt;
    unsafe {
        // Build VRingUsedElem at offset 4 (after flags/idx).
        let elem_ptr = (used_va + 4) as *mut u32;
        *elem_ptr = head as u32; // id
        *elem_ptr.add(1) = 42; // len
        // Bump device-side used.idx (offset 2 after flags).
        let idx_ptr = (used_va + 2) as *mut u16;
        *idx_ptr = 1;
    }

    match vq.pop_used() {
        Some((cookie, len)) => {
            if cookie != 0xCAFE_BABE {
                return fail("pop_used wrong cookie");
            }
            if len != 42 {
                return fail("pop_used wrong len");
            }
        }
        None => return fail("pop_used returned None"),
    }
    ok("submit + pop_used cookie roundtrip");

    if vq.free_capacity() != 64 {
        return fail("after pop_used capacity != 64");
    }
    ok("pop_used returns chain to free list");

    let _ = libcluu::debug_print("vqprobe: ALL OK");
    0
}
