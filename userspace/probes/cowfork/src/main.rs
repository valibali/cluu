//! cowfork probe: M8 copy-on-write fork primitive validation.
//!
//! Tests the userspace COW primitive in `libcluu::process`:
//!   `cow_fork` creates a child space + fault endpoint, shares parent pages
//!   read-only via MAP_SHARE_PHYS. `cow_handle_fault` resolves write faults
//!   by giving the child a private copy (unmap shared → map fresh copy from
//!   parent's page), then replies RESUME. The parent's page is never modified.
//!
//! Modes (argv[1]):
//!   happy    (default) — child writes to shared page → gets private copy,
//!                        parent's page unchanged. Expects PASS.
//!   failure             — child faults on unmapped page → handler kills
//!                        child, parent survives. Expects PASS.
//!   refcount            — sequentially fork+destroy N children sharing one
//!                        page, verify parent's page intact after each cycle.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use libcluu::debug_print;
use libcluu::ipc::recv;
use libcluu::process::{cow_destroy, cow_fork, cow_handle_fault, CowFork, CowRegion};
use libcluu::syscall::{
    space_map_range, thread_create, thread_resume, thread_set_fault_endpoint,
    THREAD_CREATE_START_SUSPENDED,
};
use libcluu::types::{IpcFlags, Message};

const PAGE_SIZE: usize = 4096;

const CODE_BASE: usize = 0x400_000;
const CODE_PAGES: usize = 256;

const DATA_VA: usize = 0x5000_0000;
const PARENT_MAGIC: u64 = 0xDA7A_5455_DA7A_5455;
const CHILD_MAGIC: u64 = 0xC1D0_C0FF_C1D0_C0FF;

const CHILD_STACK_TOP: usize = 0x6F00_0000;
const CHILD_STACK_SIZE: usize = 2 * PAGE_SIZE;

const UNMAPPED_VA: usize = 0x6000_0000;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let args = libcluu::args::args();
    let mode = args.get(1).map(|s| s.as_str()).unwrap_or("happy");

    match mode {
        "failure" => run_failure(),
        "refcount" => run_refcount(3),
        _ => run_happy(),
    }
}

fn run_happy() -> i32 {
    let self_token = libcluu::boot::token_self();
    let space = libcluu::boot::space_token();

    let _ = libcluu::syscall::space_unmap(space, DATA_VA, 1);
    if let Err(e) = space_map_range(space, DATA_VA, 0, 0x03, 1, 0) {
        let _ = debug_print(&format!("cowfork: FAIL data map {:?}", e));
        return 1;
    }
    unsafe { core::ptr::write_volatile(DATA_VA as *mut u64, PARENT_MAGIC) };

    let regions = [
        CowRegion {
            parent_virt: CODE_BASE,
            child_virt: CODE_BASE,
            num_pages: CODE_PAGES,
        },
        CowRegion {
            parent_virt: DATA_VA,
            child_virt: DATA_VA,
            num_pages: 1,
        },
    ];

    let fork = match cow_fork(self_token, &regions) {
        Ok(f) => f,
        Err(e) => {
            let _ = debug_print(&format!("cowfork: FAIL cow_fork {:?}", e));
            return 1;
        }
    };

    if let Err(e) = space_map_range(
        fork.child_space_token,
        CHILD_STACK_TOP - CHILD_STACK_SIZE,
        0,
        0x02,
        2,
        0,
    ) {
        let _ = debug_print(&format!("cowfork: FAIL child stack {:?}", e));
        let _ = cow_destroy(&fork, 0);
        return 1;
    }

    let child_thread =
        match create_child_thread(&fork, child_cow_entry as *const () as usize, CHILD_STACK_TOP) {
            Ok(t) => t,
            Err(code) => return code,
        };

    let mut msg = Message::new(0, [0; 6], 0);
    let handled = match recv_and_handle(&fork, &mut msg, 1) {
        Some(h) => h,
        None => {
            let _ = cow_destroy(&fork, child_thread);
            return 1;
        }
    };

    let parent_val = unsafe { core::ptr::read_volatile(DATA_VA as *const u64) };
    let _ = cow_destroy(&fork, child_thread);

    if !handled {
        let _ = debug_print("cowfork: FAIL no COW fault handled");
        return 1;
    }
    if parent_val != PARENT_MAGIC {
        let _ = debug_print("cowfork: FAIL parent page corrupted");
        return 1;
    }

    let _ = debug_print("cowfork: PASS happy — child got private copy, parent unchanged");
    0
}

fn run_failure() -> i32 {
    let self_token = libcluu::boot::token_self();

    let regions = [CowRegion {
        parent_virt: CODE_BASE,
        child_virt: CODE_BASE,
        num_pages: CODE_PAGES,
    }];

    let fork = match cow_fork(self_token, &regions) {
        Ok(f) => f,
        Err(e) => {
            let _ = debug_print(&format!("cowfork: FAIL cow_fork {:?}", e));
            return 1;
        }
    };

    if let Err(e) = space_map_range(
        fork.child_space_token,
        CHILD_STACK_TOP - CHILD_STACK_SIZE,
        0,
        0x02,
        2,
        0,
    ) {
        let _ = debug_print(&format!("cowfork: FAIL child stack {:?}", e));
        let _ = cow_destroy(&fork, 0);
        return 1;
    }

    let child_thread =
        match create_child_thread(&fork, child_crash_entry as *const () as usize, CHILD_STACK_TOP) {
            Ok(t) => t,
            Err(code) => return code,
        };

    let mut msg = Message::new(0, [0; 6], 0);
    let killed = matches!(recv_and_handle(&fork, &mut msg, 1), Some(false));

    let _ = cow_destroy(&fork, child_thread);

    if !killed {
        let _ = debug_print("cowfork: FAIL child not killed on unmapped fault");
        return 1;
    }

    let _ = debug_print("cowfork: PASS failure — child killed, parent survived");
    0
}

fn run_refcount(n_cycles: usize) -> i32 {
    let self_token = libcluu::boot::token_self();
    let space = libcluu::boot::space_token();

    let _ = libcluu::syscall::space_unmap(space, DATA_VA, 1);
    if let Err(e) = space_map_range(space, DATA_VA, 0, 0x03, 1, 0) {
        let _ = debug_print(&format!("cowfork: FAIL data map {:?}", e));
        return 1;
    }
    unsafe { core::ptr::write_volatile(DATA_VA as *mut u64, PARENT_MAGIC) };

    for i in 0..n_cycles {
        let regions = [
            CowRegion {
                parent_virt: CODE_BASE,
                child_virt: CODE_BASE,
                num_pages: CODE_PAGES,
            },
            CowRegion {
                parent_virt: DATA_VA,
                child_virt: DATA_VA,
                num_pages: 1,
            },
        ];

        let fork = match cow_fork(self_token, &regions) {
            Ok(f) => f,
            Err(e) => {
                let _ = debug_print(&format!("cowfork: FAIL cow_fork[{}] {:?}", i, e));
                return 1;
            }
        };

        if let Err(e) = space_map_range(
            fork.child_space_token,
            CHILD_STACK_TOP - CHILD_STACK_SIZE,
            0,
            0x02,
            2,
            0,
        ) {
            let _ = debug_print(&format!("cowfork: FAIL child stack[{}] {:?}", i, e));
            let _ = cow_destroy(&fork, 0);
            return 1;
        }

        let child_thread =
            match create_child_thread(&fork, child_cow_entry as *const () as usize, CHILD_STACK_TOP) {
                Ok(t) => t,
                Err(code) => return code,
            };

        let mut msg = Message::new(0, [0; 6], 0);
        let handled = match recv_and_handle(&fork, &mut msg, 1) {
            Some(h) => h,
            None => {
                let _ = cow_destroy(&fork, child_thread);
                return 1;
            }
        };

        if !handled {
            let _ = debug_print(&format!("cowfork: FAIL no COW fault[{}]", i));
            let _ = cow_destroy(&fork, child_thread);
            return 1;
        }

        let _ = cow_destroy(&fork, child_thread);
    }

    let parent_val = unsafe { core::ptr::read_volatile(DATA_VA as *const u64) };
    if parent_val != PARENT_MAGIC {
        let _ = debug_print("cowfork: FAIL parent page corrupted after refcount test");
        return 1;
    }

    let _ = debug_print(&format!(
        "cowfork: PASS refcount — {} fork+destroy cycles, parent intact",
        n_cycles
    ));
    0
}

fn create_child_thread(
    fork: &CowFork,
    entry: usize,
    stack_top: usize,
) -> Result<usize, i32> {
    let child_thread = match thread_create(
        fork.child_space_token,
        entry,
        stack_top,
        0,
        THREAD_CREATE_START_SUSPENDED,
    ) {
        Ok(t) => t,
        Err(e) => {
            let _ = debug_print(&format!("cowfork: FAIL thread_create {:?}", e));
            let _ = cow_destroy(fork, 0);
            return Err(1);
        }
    };

    if let Err(e) = thread_set_fault_endpoint(child_thread, fork.fault_endpoint_token) {
        let _ = debug_print(&format!("cowfork: FAIL set_fault_ep {:?}", e));
        let _ = cow_destroy(fork, child_thread);
        return Err(1);
    }

    if let Err(e) = thread_resume(child_thread) {
        let _ = debug_print(&format!("cowfork: FAIL resume {:?}", e));
        let _ = cow_destroy(fork, child_thread);
        return Err(1);
    }

    Ok(child_thread)
}

fn recv_and_handle(
    fork: &CowFork,
    msg: &mut Message,
    max_faults: usize,
) -> Option<bool> {
    let mut handled_count = 0;
    for _ in 0..16 {
        if recv(fork.fault_endpoint_token, msg, IpcFlags::empty()).is_err() {
            return None;
        }
        match cow_handle_fault(fork, msg) {
            Ok(true) => {
                handled_count += 1;
                if handled_count >= max_faults {
                    return Some(true);
                }
            }
            Ok(false) => return Some(false),
            Err(_) => return None,
        }
    }
    None
}

#[no_mangle]
#[inline(never)]
extern "C" fn child_cow_entry() -> i32 {
    let page = DATA_VA as *mut u64;
    let before = unsafe { core::ptr::read_volatile(page) };
    unsafe { core::ptr::write_volatile(page, CHILD_MAGIC) };
    let after = unsafe { core::ptr::read_volatile(page) };
    let _ = (before, after);
    loop {
        unsafe { core::arch::asm!("hlt") }
    }
}

#[no_mangle]
#[inline(never)]
extern "C" fn child_crash_entry() -> i32 {
    let unmapped = UNMAPPED_VA as *mut u64;
    let _ = unsafe { core::ptr::read_volatile(unmapped) };
    loop {
        unsafe { core::arch::asm!("hlt") }
    }
}
