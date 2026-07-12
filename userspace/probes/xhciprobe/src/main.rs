#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use cluu_dma_core::DmaPool;
use cluu_xhci_core::XhciController;
use libcluu::boot::process_info;
use libcluu::debug_print;


const PCI_TOKEN_SLOT: usize = 9;
const DMA_BASE: usize = 0x4200_0000;
const DMA_PAGES: usize = 64;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let info = process_info();
    let pci_token = info.tokens[PCI_TOKEN_SLOT];
    let space_token = info.tokens[5];

    let _ = debug_print("xhciprobe: starting");

    let mut pool = match DmaPool::new(space_token, DMA_BASE, DMA_PAGES) {
        Ok(p) => p,
        Err(e) => {
            let _ = debug_print(&format!("xhciprobe: [FAIL] DmaPool: {:?}", e));
            return 1;
        }
    };
    let _ = debug_print("xhciprobe: DmaPool ok");

    let mut ctrl = match XhciController::probe(pci_token, space_token, &mut pool) {
        Ok(c) => c,
        Err(e) => {
            let _ = debug_print(&format!("xhciprobe: [FAIL] XhciController::probe: {:?}", e));
            return 1;
        }
    };
    let _ = debug_print(&format!(
        "xhciprobe: xHCI found vendor=0x{:04x} device=0x{:04x} irq={}",
        ctrl.pci_dev.vendor_id, ctrl.pci_dev.device_id, ctrl.pci_dev.irq_line
    ));
    let _ = debug_print("xhciprobe: XHCI_PCI_OK");

    match ctrl.reset() {
        Ok(()) => {
            let _ = debug_print("xhciprobe: RESET_OK");
        }
        Err(e) => {
            let _ = debug_print(&format!("xhciprobe: [FAIL] reset: {:?}", e));
            return 1;
        }
    }

    match ctrl.enable_slots() {
        Ok(()) => {
            let _ = debug_print(&format!("xhciprobe: SLOT_OK max_slots={}", ctrl.max_slots));
        }
        Err(e) => {
            let _ = debug_print(&format!("xhciprobe: [FAIL] enable_slots: {:?}", e));
            return 1;
        }
    }

    let _ = debug_print("xhciprobe: PASS XHCI_PROBE_OK");
    0
}
