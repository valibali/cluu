//! Verifies the kernel honors THREAD_CREATE_START_SUSPENDED:
//! a thread created with that flag must not run until thread_resume is called.
//!
//! Probe creates a child thread in its own address space with the SUSPENDED
//! flag, yields several times to give the scheduler a chance, asserts the
//! child has NOT written its "ran" marker. Then calls thread_resume, yields
//! again, asserts the marker IS set.

#![no_std]
#![no_main]
#![allow(static_mut_refs)]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use core::sync::atomic::{AtomicU32, Ordering};
use libcluu::boot;
use libcluu::debug_print;
use libcluu::syscall::{
    thread_create, thread_destroy, thread_resume, yield_cpu,
    THREAD_CREATE_START_SUSPENDED,
};

static RAN: AtomicU32 = AtomicU32::new(0);

const STACK_BYTES: usize = 16 * 1024;
#[repr(C, align(16))]
struct Stack([u8; STACK_BYTES]);
static mut CHILD_STACK: Stack = Stack([0; STACK_BYTES]);

extern "C" fn child_entry() -> ! {
    RAN.store(1, Ordering::SeqCst);
    loop {
        let _ = yield_cpu();
    }
}

fn yield_some(times: u32) {
    for _ in 0..times {
        let _ = yield_cpu();
    }
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let space = boot::space_token();
    let stack_top = unsafe {
        (CHILD_STACK.0.as_ptr() as usize).wrapping_add(STACK_BYTES)
    };

    let child = match thread_create(
        space,
        child_entry as *const () as usize,
        stack_top,
        128,
        THREAD_CREATE_START_SUSPENDED,
    ) {
        Ok(t) => t,
        Err(e) => {
            let _ = debug_print(&alloc::format!(
                "suspendprobe: FAIL thread_create {:?}", e
            ));
            return 1;
        }
    };

    // Yield several times. If the kernel honored SUSPENDED, child has not run.
    yield_some(8);
    if RAN.load(Ordering::SeqCst) != 0 {
        let _ = debug_print("suspendprobe: FAIL ran before resume");
        let _ = thread_destroy(child);
        return 1;
    }

    if let Err(e) = thread_resume(child) {
        let _ = debug_print(&alloc::format!(
            "suspendprobe: FAIL thread_resume {:?}", e
        ));
        let _ = thread_destroy(child);
        return 1;
    }

    yield_some(8);
    if RAN.load(Ordering::SeqCst) != 1 {
        let _ = debug_print("suspendprobe: FAIL did not run after resume");
        let _ = thread_destroy(child);
        return 1;
    }

    let _ = debug_print("suspendprobe: PASS suspended-thread did not run before resume");
    let _ = thread_destroy(child);
    0
}
