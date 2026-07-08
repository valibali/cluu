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
mod measured_boot;
mod attestation;
mod sealed_storage;
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

    // Collect SHA-256 hashes of each service binary for measured boot.
    let mut service_hashes: [([u8; 32], &str); 16] = [([0u8; 32], ""); 16];
    let mut hash_count = 0usize;

    // Launch services in the declared order; wiring policy is in wiring.rs.
    // Only primordial services get an exit cookie (non-zero) so init is
    // notified when they die.  Non-primordial services (e.g. tpmd) get
    // cookie 0 and may exit silently.
    for (index, service) in services::SERVICE_LIST.iter().enumerate() {
        let is_primordial = services::PRIMORDIAL_SERVICES.contains(&service.name);
        let exit_cookie = if is_primordial { index + 1 } else { 0 };
        let hash = wiring::launch_service(&ctx, service, index, Some(&manifest), exit_cookie)?;
        service_hashes[hash_count] = (hash, service.name);
        hash_count += 1;
        if is_primordial {
            cookie_to_name[num_primordials] = (exit_cookie, service.name);
            num_primordials += 1;
        }
    }

    debug_print("init: all critical services created; monitoring primordials")?;

    // Measured boot: extend TPM PCRs with service binary hashes.
    measured_boot::extend_measurements(
        ctx.registry_send,
        ctx.boot.root_token,
        ctx.initrd,
        &service_hashes[..hash_count],
    );

    // Sealed storage PoC: seal/unseal round-trip test.
    sealed_storage::run(ctx.registry_send, ctx.boot.root_token);

    // Remote attestation PoC: AIK creation + TPM Quote.
    attestation::run(ctx.registry_send, ctx.boot.root_token);

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
                        acpi_poweroff(&ctx);
                    }
                    43 => {
                        let _ = debug_print(&format!(
                            "init: procmgr '{}' requested reboot (code 43)", name
                        ));
                        let _ = libcluu::syscall::port_out8(ctx.pci_token, 0xCF9, 0x06);
                    }
                    _ => {
                        let _ = debug_print(&format!(
                            "init: FATAL — primordial '{}' exited (code {}), system halt",
                            name, exit_code
                        ));
    }
}

fn acpi_poweroff(ctx: &context::InitContext) {
    let rsdp_phys = ctx.boot.acpi_ptr;
    if rsdp_phys != 0 {
        let _ = debug_print("init: ACPI RSDP found, deriving S5 from FADT");
        if let Ok(fadt) = cluu_acpi::find_fadt_from_phys(ctx.boot.root_token, rsdp_phys) {
            let pm1a_cnt = fadt.pm1a_cnt_blk as u16;
            if pm1a_cnt != 0 {
                let _ = debug_print(&format!("init: PM1a_CNT=0x{:04x}", pm1a_cnt));
                let slp_typ: u16 = 0;
                let slp_en: u16 = 1 << 13;
                let val = slp_typ | slp_en;
                let _ = libcluu::syscall::port_out16(ctx.pci_token, pm1a_cnt as u16, val);
                return;
            }
        }
    }
    let _ = debug_print("init: FADT-derived S5 failed, falling back to QEMU 0x604");
    let _ = libcluu::syscall::port_out16(ctx.pci_token, 0x604, 0x2000);
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
