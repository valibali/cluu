#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use libcluu::boot::{boot_info, process_info, TOKEN_SPACE};
use libcluu::debug_print;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let info = boot_info();
    let space_token = process_info().tokens[TOKEN_SPACE];
    let rsdp_phys = info.acpi_ptr;

    let _ = debug_print(&format!(
        "acpiprobe: root_token=0x{:x} fb_pitch={} acpi_ptr=0x{:x}",
        info.root_token, info.fb_pitch, rsdp_phys
    ));

    let raw_ptr = info as *const _ as *const u8;
    let val_at_88 = unsafe { core::ptr::read_volatile(raw_ptr.add(88) as *const u64) };
    let _ = debug_print(&format!("acpiprobe: raw offset[88]=0x{:x}", val_at_88));
    let val_at_84 = unsafe { core::ptr::read_volatile(raw_ptr.add(84) as *const u64) };
    let _ = debug_print(&format!("acpiprobe: raw offset[84]=0x{:x}", val_at_84));

    let rsdp_phys = if rsdp_phys != 0 {
        let _ = debug_print(&format!("acpiprobe: acpi_ptr=0x{:x}", rsdp_phys));
        rsdp_phys
    } else {
        let _ = debug_print("acpiprobe: acpi_ptr=0, falling back to RSDP scan");
        match cluu_acpi::find_rsdp(space_token) {
            Ok(rsdp) => {
                let _ = debug_print(&format!(
                    "acpiprobe: RSDP found via scan rev={} rsdt=0x{:x} xsdt=0x{:x}",
                    rsdp.revision, rsdp.rsdt_phys, rsdp.xsdt_phys
                ));
                match cluu_acpi::find_fadt_from_rsdp(space_token, &rsdp) {
                    Ok(fadt) => {
                        let _ = debug_print(&format!(
                            "acpiprobe: FADT found pm1a_cnt=0x{:x}",
                            fadt.pm1a_cnt_blk
                        ));
                        if fadt.pm1a_cnt_blk != 0 {
                            let _ = debug_print("acpiprobe: PASS ACPI_TABLES_OK");
                            return 0;
                        }
                        let _ = debug_print("acpiprobe: [FAIL] pm1a_cnt_blk == 0");
                        return 1;
                    }
                    Err(e) => {
                        let _ = debug_print(&format!(
                            "acpiprobe: [FAIL] find_fadt_from_rsdp: {:?}", e
                        ));
                        return 1;
                    }
                }
            }
            Err(e) => {
                let _ = debug_print(&format!("acpiprobe: [FAIL] find_rsdp: {:?}", e));
                return 1;
            }
        }
    };

    match cluu_acpi::find_fadt_from_phys(space_token, rsdp_phys) {
        Ok(fadt) => {
            let _ = debug_print(&format!(
                "acpiprobe: FADT found pm1a_cnt=0x{:x} pm1b_cnt=0x{:x}",
                fadt.pm1a_cnt_blk, fadt.pm1b_cnt_blk
            ));
            let _ = debug_print(&format!(
                "acpiprobe: sci_cmd=0x{:x} smi_cmd=0x{:x} reset_val=0x{:02x}",
                fadt.sci_command, fadt.smi_command_port, fadt.reset_value
            ));
            if fadt.pm1a_cnt_blk != 0 {
                let _ = debug_print("acpiprobe: PASS ACPI_TABLES_OK");
                return 0;
            } else {
                let _ = debug_print("acpiprobe: [FAIL] pm1a_cnt_blk == 0");
                return 1;
            }
        }
        Err(e) => {
            let _ = debug_print(&format!("acpiprobe: [FAIL] find_fadt: {:?}", e));
            return 1;
        }
    }
}
