#![no_std]
#![no_main]

//! Init process for the CLUU userspace bootstrap.
//!
//! This binary is the first userspace program. It reads boot parameters,
//! spawns critical services (registry, procmgr, kbd, tty, console), and then
//! yields so the scheduler can switch to normal preemptive mode.

extern crate alloc;

mod boot;
mod context;
mod mappings;
mod services;
mod wiring;

use libcluu::{debug_print, yield_cpu, Result};

#[no_mangle]
/// Kernel entrypoint for the init process.
///
/// This keeps the signature C-compatible while deferring the main work to `run`.
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

/// Bootstraps core services and yields to the scheduler.
///
/// This centralizes the init flow while delegating policy and wiring to
/// dedicated modules so the sequence stays easy to audit.
fn run() -> Result<()> {
    // Init is the first userspace process: it spawns critical services and
    // then yields to the scheduler so preemptive mode can take over.
    debug_print("init: bootstrapping critical services")?;

    let boot = boot::capture_boot_snapshot()?;
    let initrd = boot::map_initrd_slice(boot.initrd_size);
    let ctx = context::InitContext::new(boot, initrd)?;

    // Launch services in the declared order; wiring policy is in wiring.rs.
    for (index, service) in services::SERVICE_LIST.iter().enumerate() {
        wiring::launch_service(&ctx, service, index)?;
    }

    debug_print("init: all critical services created; yielding to scheduler")?;
    yield_cpu()?;

    Ok(())
}
