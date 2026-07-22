//! ACPI RSDP/FADT/MCFG/DSDT discovery for drivermgr (D1.3 + D6.2).
//!
//! Uses `cluu_acpi` to locate the RSDP, walk the RSDT/XSDT to the FADT,
//! and (if present) the MCFG.  D6.2 also reads the DSDT, parses it for
//! PNP `Device()` objects, and publishes each as a `DeviceNode` into
//! the shared `DeviceTree` under `/acpi/<HID>`.

extern crate alloc;

use alloc::format;

use cluu_acpi::{
    find_dsdt_bytes, find_fadt_from_rsdp, find_mcfg_from_rsdp, find_rsdp_with_phys,
    find_ssdt_bytes_from_rsdp, parse_devices,
};
use libcluu::{debug_print, Result};

use crate::device_tree::{acpi_path, DeviceNode, DeviceTree};

/// Discover ACPI tables, log their locations, and (D6.2) enumerate PNP
/// devices from the DSDT into `tree`.
pub fn scan(space_token: usize, tree: &mut DeviceTree) -> Result<()> {
    let (rsdp, rsdp_phys) = match find_rsdp_with_phys(space_token) {
        Ok(pair) => pair,
        Err(err) => {
            let _ = debug_print(&format!(
                "drivermgr: ACPI RSDP not found ({:?}); skipping ACPI scan",
                err
            ));
            return Ok(());
        }
    };

    let fadt = match find_fadt_from_rsdp(space_token, &rsdp) {
        Ok(fadt) => fadt,
        Err(err) => {
            let _ = debug_print(&format!(
                "drivermgr: ACPI RSDP at 0x{:x}, FADT lookup failed ({:?})",
                rsdp_phys, err
            ));
            return Ok(());
        }
    };

    let _ = debug_print(&format!(
        "drivermgr: ACPI RSDP at 0x{:x}, FADT pm1a=0x{:x}",
        rsdp_phys, fadt.pm1a_cnt_blk
    ));

    match find_mcfg_from_rsdp(space_token, &rsdp) {
        Ok(mcfg) => {
            if let Some(ecam) = mcfg.ecam_base() {
                let _ = debug_print(&format!(
                    "drivermgr: ACPI MCFG ECAM base=0x{:x}",
                    ecam
                ));
            } else {
                let _ = debug_print("drivermgr: ACPI MCFG present, no ECAM entry");
            }
        }
        Err(_) => {
            let _ = debug_print("drivermgr: ACPI MCFG not present (ok for minimal QEMU)");
        }
    }

    enumerate_dsdt(space_token, &fadt, &rsdp, tree);

    Ok(())
}

fn enumerate_dsdt(
    space_token: usize,
    fadt: &cluu_acpi::Fadt,
    rsdp: &cluu_acpi::Rsdp,
    tree: &mut DeviceTree,
) {
    let dsdt = match find_dsdt_bytes(space_token, fadt) {
        Ok(bytes) => bytes,
        Err(err) => {
            let _ = debug_print(&format!(
                "drivermgr: DSDT lookup failed ({:?}); no ACPI PNP devices",
                err
            ));
            return;
        }
    };
    let _ = debug_print(&format!(
        "drivermgr: DSDT read {} bytes, parsing for PNP devices",
        dsdt.len()
    ));

    let mut all_devices = parse_devices(&dsdt);

    let ssdts = find_ssdt_bytes_from_rsdp(space_token, rsdp);
    for (i, ssdt) in ssdts.iter().enumerate() {
        let _ = debug_print(&format!(
            "drivermgr: SSDT[{}] read {} bytes, parsing for PNP devices",
            i, ssdt.len()
        ));
        all_devices.extend(parse_devices(ssdt));
    }

    let mut published = 0usize;
    for dev in &all_devices {
        if dev.hid.is_empty() {
            continue;
        }
        let path = acpi_path(&dev.hid);
        let mut node = DeviceNode::new_acpi(path.clone(), dev.hid.clone());
        node.io_ports = dev.io_ports.clone();
        node.irq_line = dev.irq;
        let _ = debug_print(&format!(
            "drivermgr: ACPI device {} hid={} io_ports={} irq={:?}",
            path,
            dev.hid,
            dev.io_ports.len(),
            dev.irq
        ));
        tree.insert(path.clone(), node);
        published += 1;
    }
    let _ = debug_print(&format!(
        "drivermgr: ACPI PNP scan complete, {} devices published",
        published
    ));
}
