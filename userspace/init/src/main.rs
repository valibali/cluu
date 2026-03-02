#![no_std]
#![no_main]

//! Init process for the CLUU userspace bootstrap.
//!
//! This binary is the first userspace program. It reads boot parameters,
//! spawns critical services (registry, procmgr, kbd, tty:0, console:0,
//! vtmgr), and then idles forever.  On-demand VT spawning is handled by
//! vtmgr (kbd sends VTMGR_SWITCH_VT_LABEL to vtmgr).

extern crate alloc;

mod boot;
mod context;
mod mappings;
mod services;
mod wiring;

use alloc::format;
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, recv, yield_cpu, Result};

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

/// Bootstraps core services and idles.
///
/// Init spawns the boot-critical service set, then yields forever.
/// On-demand VT spawning is handled directly by procmgr.
fn run() -> Result<()> {
    debug_print("init: bootstrapping critical services")?;

    let boot = boot::capture_boot_snapshot()?;
    let initrd = boot::map_initrd_slice(boot.initrd_size);
    let manifest = boot::load_boot_manifest(initrd)?;
    if manifest.services.is_empty() {
        return Err(libcluu::Error::InvalidOperation);
    }
    debug_print("init: boot manifest parsed")?;
    let ctx = context::InitContext::new(boot, initrd)?;

    // Track primordial services by exit cookie for identification on death.
    let mut cookie_to_name: [(usize, &str); 8] = [(0, ""); 8];
    let mut num_primordials = 0usize;

    // Launch services in the declared order; wiring policy is in wiring.rs.
    for (index, service) in services::SERVICE_LIST.iter().enumerate() {
        let exit_cookie = index + 1; // cookies start at 1 (0 = no tracking)
        wiring::launch_service(&ctx, service, index, Some(&manifest), exit_cookie)?;
        cookie_to_name[num_primordials] = (exit_cookie, service.name);
        num_primordials += 1;
    }

    debug_print("init: all critical services created; monitoring primordials")?;

    // Monitor primordial exit endpoint — any message means a primordial died.
    let mut msg = Message::new(0, [0; 6], 0);
    loop {
        match recv(ctx.primordial_exit_recv, &mut msg, IpcFlags::empty()) {
            Ok(()) => {
                let cookie = msg.words[0];
                let exit_code = msg.words[1] as i32;
                let name = cookie_to_name[..num_primordials]
                    .iter()
                    .find(|(c, _)| *c == cookie)
                    .map(|(_, n)| *n)
                    .unwrap_or("unknown");

                match exit_code {
                    42 => {
                        let _ = debug_print(&format!(
                            "init: procmgr '{}' requested poweroff (code 42)", name
                        ));
                        // QEMU ACPI poweroff: write 0x2000 to port 0x604
                        let _ = libcluu::syscall::port_out16(ctx.pci_token, 0x604, 0x2000);
                    }
                    43 => {
                        let _ = debug_print(&format!(
                            "init: procmgr '{}' requested reboot (code 43)", name
                        ));
                        // x86 reset: write 0x06 to port 0xCF9
                        let _ = libcluu::syscall::port_out8(ctx.pci_token, 0xCF9, 0x06);
                    }
                    _ => {
                        let _ = debug_print(&format!(
                            "init: FATAL — primordial '{}' exited (code {}), system halt",
                            name, exit_code
                        ));
                    }
                }
                // Halt regardless — if ACPI/reset failed, spin forever
                loop { let _ = yield_cpu(); }
            }
            Err(_) => {
                let _ = yield_cpu();
            }
        }
    }
}
