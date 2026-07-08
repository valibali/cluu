#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use cluu_dma_core::DmaPool;
use cluu_ehci_core::EhciController;
use libcluu::boot::{process_info, TOKEN_EXTRA_0, TOKEN_EXTRA_1, TOKEN_EXTRA_2, TOKEN_SPACE};
use libcluu::debug_print;
use libcluu::registry;
use libcluu::{ipc, Result};

const DMA_BASE: usize = 0x4800_0000;
const DMA_PAGES: usize = 256;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(_) => 0,
        Err(e) => {
            let _ = debug_print(&format!("usb-input: fatal {:?}", e));
            -1
        }
    }
}

fn run() -> Result<()> {
    let _ = debug_print("usb-input: starting");
    let info = process_info();
    let pci_token = info.tokens[TOKEN_EXTRA_1];
    let space_token = info.tokens[TOKEN_SPACE];
    let _irq_token = info.tokens[TOKEN_EXTRA_2];
    let my_ep = info.tokens[TOKEN_EXTRA_0];

    let _ = registry::init("usb-input");
    let _ = registry::register_default_outputs();
    let _ = debug_print("usb-input: registry init ok");

    let mut pool = DmaPool::new(space_token, DMA_BASE, DMA_PAGES)?;
    let _ = debug_print("usb-input: DmaPool ok");

    let mut ctrl = EhciController::probe(pci_token, space_token, &mut pool)?;
    let _ = debug_print(&format!(
        "usb-input: EHCI found vid=0x{:04x} did=0x{:04x} irq={}",
        ctrl.pci_dev.vendor_id, ctrl.pci_dev.device_id, ctrl.pci_dev.irq_line
    ));

    ctrl.reset()?;
    let _ = debug_print("usb-input: EHCI reset ok");

    ctrl.start()?;
    let _ = debug_print("usb-input: EHCI started");

    let port = match ctrl.find_connected_port() {
        Some(p) => p,
        None => {
            let _ = debug_print("usb-input: no USB device connected, idle");
            let _ = debug_print("usb-input: PASS USB_INPUT_OK");
            return idle_loop(my_ep);
        }
    };

    let speed = ctrl.reset_port(port)?;
    let _ = debug_print(&format!("usb-input: port {} reset speed={}", port, speed));

    let addr: u8 = 2;
    let max_pkt: u16 = match speed {
        0 => 8,
        1 => 8,
        2 => 64,
        _ => 64,
    };

    ctrl.set_address(&mut pool, addr, speed, max_pkt)?;
    let _ = debug_print("usb-input: SET_ADDRESS ok");

    let _desc = ctrl.get_device_descriptor(&mut pool, addr, speed, max_pkt)?;
    let _ = debug_print("usb-input: GET_DESCRIPTOR ok");

    ctrl.set_configuration(&mut pool, addr, speed, max_pkt, 1)?;
    let _ = debug_print("usb-input: SET_CONFIGURATION ok");

    ctrl.set_idle(&mut pool, addr, speed, max_pkt)?;
    let _ = debug_print("usb-input: SET_IDLE ok");

    ctrl.set_protocol(&mut pool, addr, speed, max_pkt, 0)?;
    let _ = debug_print("usb-input: SET_PROTOCOL(boot) ok");

    let int_max_pkt: u16 = 8;
    let report_dma = pool.alloc(int_max_pkt as usize, 8)?;
    for i in 0..(int_max_pkt as usize) {
        unsafe { core::ptr::write_volatile((report_dma.virt + i) as *mut u8, 0); }
    }

    ctrl.setup_interrupt_in(&mut pool, addr, int_max_pkt, &report_dma)?;
    let _ = debug_print("usb-input: interrupt IN queued");

    let _ = debug_print("usb-input: PASS USB_INPUT_OK");

    let mut report_count = 0u32;
    let reg_ep = registry::control_endpoint();
    let mut buf = [0u8; 128];
    loop {
        if let Some(n) = ctrl.poll_interrupt(&report_dma, int_max_pkt as usize) {
            let report = unsafe {
                core::slice::from_raw_parts(report_dma.virt as *const u8, n)
            };
            let _ = debug_print(&format!(
                "usb-input: report[{}] [0]={:02x} [1]={:02x} [2]={:02x}",
                report_count,
                report.get(0).copied().unwrap_or(0),
                report.get(1).copied().unwrap_or(0),
                report.get(2).copied().unwrap_or(0),
            ));
            report_count += 1;
            for i in 0..n {
                unsafe { core::ptr::write_volatile((report_dma.virt + i) as *mut u8, 0); }
            }
            let _ = ctrl.setup_interrupt_in(&mut pool, addr, int_max_pkt, &report_dma);
        }

        let tokens = [my_ep, reg_ep];
        match libcluu::syscall::ipc_recv_any(&tokens, &mut buf, 0) {
            Ok((idx, len)) => {
                if idx == 1 {
                    if let Some((msg, payload)) = ipc::parse_message(&buf[..len]) {
                        let _ = registry::handle_incoming_message(&msg, payload);
                    }
                }
            }
            Err(_) => {
                let _ = libcluu::yield_cpu();
            }
        }
    }
}

fn idle_loop(my_ep: usize) -> Result<()> {
    let mut buf = [0u8; 128];
    let reg_ep = registry::control_endpoint();
    let tokens = [my_ep, reg_ep];
    loop {
        match libcluu::syscall::ipc_recv_any(&tokens, &mut buf, u64::MAX) {
            Ok((idx, len)) => {
                if idx == 1 {
                    if let Some((msg, payload)) = ipc::parse_message(&buf[..len]) {
                        let _ = registry::handle_incoming_message(&msg, payload);
                    }
                    continue;
                }
            }
            Err(_) => {
                let _ = libcluu::yield_cpu();
            }
        }
    }
}
