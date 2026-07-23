#![no_std]
#![no_main]

extern crate alloc;

mod context;
mod layout;

use alloc::format;
use alloc::vec::Vec;
use cluu_dma_core::{DmaPool, DmaRegion};
use cluu_ehci_core::EhciController;
use libcluu::boot::{process_info, TOKEN_CLOCK, TOKEN_EXTRA_1, TOKEN_EXTRA_2, TOKEN_SPACE};
use libcluu::debug_print;
use libcluu::ipc::{KBD_EVENT_LABEL, MOUSE_EVENT_LABEL};
use libcluu::types::Message;
use libcluu::{ipc, Result};

use context::UsbInputContext;
use layout::{
    hid_modifiers_to_kbd, hid_usage_to_extended, is_ctrl_alt, pack_mods_for_ipc, translate_scancode,
    hid_to_ps2_scancode, vt_switch_target, HID_USAGE_DELETE,
};

const DMA_BASE: usize = 0x4800_0000;
const DMA_PAGES: usize = 256;

const HID_CLASS: u8 = 0x03;
const HID_BOOT_SUBCLASS: u8 = 0x01;
const HID_PROTO_KBD: u8 = 0x01;
const HID_PROTO_MOUSE: u8 = 0x02;

const REPEAT_INITIAL_MS: u64 = 500;
const REPEAT_INTERVAL_MS: u64 = 50;

#[derive(Copy, Clone, PartialEq, Debug)]
enum DeviceKind {
    Keyboard,
    Mouse,
}

struct RepeatState {
    key: u8,
    press_tsc: u64,
    last_repeat_tsc: u64,
    scancode: u8,
    ascii: u8,
    extended: u8,
    mods: u8,
}

struct UsbDevice {
    slot: usize,
    addr: u8,
    kind: DeviceKind,
    report_dma: DmaRegion,
    report_len: usize,
    last_keys: [u8; 6],
    repeat: Option<RepeatState>,
}

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
    let clock_token = info.tokens[TOKEN_CLOCK];
    let tsc_hz = libcluu::syscall::clock_frequency(clock_token).unwrap_or(0);

    let mut ctx = UsbInputContext::new()?;
    let _ = debug_print("usb-input: registry init ok");

    ctx.ensure_subscriptions();

    let mut pool = DmaPool::new(space_token, DMA_BASE, DMA_PAGES)?;
    let _ = debug_print("usb-input: DmaPool ok");

    let mut ctrl = match EhciController::probe(pci_token, space_token, &mut pool) {
        Ok(c) => c,
        Err(e) => {
            let _ = debug_print(&format!(
                "usb-input: no EHCI controller ({:?}), entering idle mode",
                e
            ));
            let _ = debug_print("usb-input: PASS USB_INPUT_OK");
            return idle_loop(ctx);
        }
    };
    let _ = debug_print(&format!(
        "usb-input: EHCI found vid=0x{:04x} did=0x{:04x} irq={}",
        ctrl.pci_dev.vendor_id, ctrl.pci_dev.device_id, ctrl.pci_dev.irq_line
    ));

    if ctrl.reset().is_err() {
        let _ = debug_print("usb-input: EHCI reset failed, idle");
        let _ = debug_print("usb-input: PASS USB_INPUT_OK");
        return idle_loop(ctx);
    }
    let _ = debug_print("usb-input: EHCI reset ok");

    if ctrl.start().is_err() {
        let _ = debug_print("usb-input: EHCI start failed, idle");
        let _ = debug_print("usb-input: PASS USB_INPUT_OK");
        return idle_loop(ctx);
    }
    let _ = debug_print("usb-input: EHCI started");

    let ports = ctrl.find_connected_ports();
    if ports.is_empty() {
        let _ = debug_print("usb-input: no USB device connected, idle");
        let _ = debug_print("usb-input: PASS USB_INPUT_OK");
        return idle_loop(ctx);
    }

    let mut devices: Vec<UsbDevice> = Vec::new();
    let mut next_addr: u8 = 2;

    for (slot, &port) in ports.iter().enumerate() {
        if slot >= cluu_ehci_core::MAX_INTR_SLOTS {
            break;
        }
        match probe_device(&mut ctrl, &mut pool, port, slot, next_addr) {
            Ok(dev) => {
                let _ = debug_print(&format!(
                    "usb-input: port {} -> {:?} addr={} slot={}",
                    port, dev.kind, dev.addr, dev.slot
                ));
                next_addr += 1;
                devices.push(dev);
            }
            Err(e) => {
                let _ = debug_print(&format!(
                    "usb-input: port {} probe failed: {:?}", port, e
                ));
            }
        }
        pump_registry(&mut ctx);
    }

    if devices.is_empty() {
        let _ = debug_print("usb-input: no HID device initialized, idle");
        let _ = debug_print("usb-input: PASS USB_INPUT_OK");
        return idle_loop(ctx);
    }

    let _ = debug_print("usb-input: PASS USB_INPUT_OK");

    let mut buf = [0u8; 128];
    loop {
        ctx.ensure_subscriptions();

        poll_all(&mut ctrl, &mut pool, &mut devices, &ctx, clock_token, tsc_hz);

        let my_ep = ctx.endpoint;
        let reg_ep = ctx.registry_endpoint;
        let tokens = [my_ep, reg_ep];
        match libcluu::syscall::ipc_recv_any(&tokens, &mut buf, 10) {
            Ok((idx, len)) => {
                if idx == 1 {
                    if let Some((msg, payload)) = ipc::parse_message(&buf[..len]) {
                        ctx.handle_registry_message(&msg, payload);
                    }
                }
            }
            Err(_) => {}
        }
    }
}

fn poll_all(
    ctrl: &mut EhciController,
    pool: &mut DmaPool,
    devices: &mut Vec<UsbDevice>,
    ctx: &UsbInputContext,
    clock_token: usize,
    tsc_hz: u64,
) {
    for dev in devices.iter_mut() {
        if let Some(n) = ctrl.poll_interrupt(dev.slot, &dev.report_dma, dev.report_len) {
            let report = unsafe {
                core::slice::from_raw_parts(dev.report_dma.virt as *const u8, n)
            };
            handle_report(ctx, dev, report, clock_token, tsc_hz);
            for i in 0..dev.report_len {
                unsafe {
                    core::ptr::write_volatile((dev.report_dma.virt + i) as *mut u8, 0);
                }
            }
            let _ = ctrl.setup_interrupt_in(
                pool,
                dev.slot,
                dev.addr,
                dev.report_len as u16,
                &dev.report_dma,
            );
        }
    }
}

fn probe_device(
    ctrl: &mut EhciController,
    pool: &mut DmaPool,
    port: u8,
    slot: usize,
    addr: u8,
) -> Result<UsbDevice> {
    let speed = ctrl.reset_port(port)?;
    let _ = debug_print(&format!("usb-input: port {} reset speed={}", port, speed));

    let max_pkt: u16 = match speed {
        0 => 8,
        1 => 8,
        2 => 64,
        _ => 64,
    };

    ctrl.set_address(pool, addr, speed, max_pkt)?;
    let _ = debug_print("usb-input: SET_ADDRESS ok");

    let _desc = ctrl.get_device_descriptor(pool, addr, speed, max_pkt)?;
    let _ = debug_print("usb-input: GET_DESCRIPTOR ok");

    ctrl.set_configuration(pool, addr, speed, max_pkt, 1)?;
    let _ = debug_print("usb-input: SET_CONFIGURATION ok");

    let config = ctrl.get_config_descriptor(pool, addr, speed, max_pkt)?;
    let proto = parse_hid_protocol(&config);
    let kind = match proto {
        Some(HID_PROTO_KBD) => DeviceKind::Keyboard,
        Some(HID_PROTO_MOUSE) => DeviceKind::Mouse,
        _ => {
            let _ = debug_print(&format!(
                "usb-input: port {} not a boot HID device (proto={:?}), skipping",
                port, proto
            ));
            return Err(libcluu::Error::NotFound);
        }
    };

    ctrl.set_idle(pool, addr, speed, max_pkt)?;
    let _ = debug_print("usb-input: SET_IDLE ok");

    ctrl.set_protocol(pool, addr, speed, max_pkt, 0)?;
    let _ = debug_print("usb-input: SET_PROTOCOL(boot) ok");

    let (report_len, alloc_len) = match kind {
        DeviceKind::Keyboard => (8, 8),
        DeviceKind::Mouse => (4, 4),
    };
    let report_dma = pool.alloc(alloc_len, 8)?;
    for i in 0..alloc_len {
        unsafe { core::ptr::write_volatile((report_dma.virt + i) as *mut u8, 0); }
    }

    ctrl.setup_interrupt_in(pool, slot, addr, report_len as u16, &report_dma)?;

    Ok(UsbDevice {
        slot,
        addr,
        kind,
        report_dma,
        report_len,
        last_keys: [0; 6],
        repeat: None,
    })
}

fn parse_hid_protocol(config: &[u8]) -> Option<u8> {
    let mut offset = 0;
    while offset + 9 <= config.len() {
        let desc_len = config[offset] as usize;
        let desc_type = config[offset + 1];
        if desc_len == 0 {
            break;
        }
        if desc_type == 0x04 {
            let iface_class = config[offset + 5];
            let iface_subclass = config[offset + 6];
            let iface_proto = config[offset + 7];
            if iface_class == HID_CLASS && iface_subclass == HID_BOOT_SUBCLASS {
                return Some(iface_proto);
            }
        }
        offset += desc_len;
    }
    None
}

fn handle_report(
    ctx: &UsbInputContext,
    dev: &mut UsbDevice,
    report: &[u8],
    clock_token: usize,
    tsc_hz: u64,
) {
    match dev.kind {
        DeviceKind::Keyboard => handle_kbd_report(ctx, dev, report, clock_token, tsc_hz),
        DeviceKind::Mouse => handle_mouse_report(ctx, report),
    }
}

fn tsc_to_ms(delta_tsc: u64, tsc_hz: u64) -> u64 {
    if tsc_hz == 0 {
        return 0;
    }
    delta_tsc * 1000 / tsc_hz
}

fn handle_kbd_report(
    ctx: &UsbInputContext,
    dev: &mut UsbDevice,
    report: &[u8],
    clock_token: usize,
    tsc_hz: u64,
) {
    if report.len() < 3 {
        return;
    }
    let hid_mods = report[0];
    let kbd_mods = hid_modifiers_to_kbd(hid_mods);
    let ctrl_alt = is_ctrl_alt(kbd_mods);

    let mut new_keys: [u8; 6] = [0; 6];
    let count = (report.len() - 2).min(6);
    new_keys[..count].copy_from_slice(&report[2..2 + count]);

    // Forward key-release events for keys that were in last_keys but are
    // no longer pressed. kind=2 signals a release; scancode gets the PS/2
    // set-1 break bit (0x80) set. ascii=0 so legacy consumers that only
    // react to ascii!=0 (tty, login) ignore these. Games like DOOM read
    // kind to distinguish press/release.
    for &old in dev.last_keys.iter() {
        if old == 0 {
            continue;
        }
        if new_keys.contains(&old) {
            continue;
        }
        let extended = hid_usage_to_extended(old);
        let scancode = hid_to_ps2_scancode(old).unwrap_or(0);
        let msg_mods = pack_mods_for_ipc(kbd_mods);
        let msg = Message::new(
            KBD_EVENT_LABEL,
            [
                0,
                0,
                msg_mods as usize,
                (scancode | 0x80) as usize,
                extended as usize,
                2, // kind=2: key release
            ],
            5,
        );
        ctx.forward(&msg);
    }

    for &key in new_keys.iter() {
        if key == 0 {
            continue;
        }
        if dev.last_keys.contains(&key) {
            if let Some(ref mut rep) = dev.repeat {
                if rep.key == key {
                    let now_tsc = libcluu::syscall::clock_now(clock_token).unwrap_or(0);
                    let elapsed_ms = tsc_to_ms(now_tsc.saturating_sub(rep.press_tsc), tsc_hz);
                    if elapsed_ms >= REPEAT_INITIAL_MS {
                        let since_last =
                            tsc_to_ms(now_tsc.saturating_sub(rep.last_repeat_tsc), tsc_hz);
                        if since_last >= REPEAT_INTERVAL_MS {
                            rep.last_repeat_tsc = now_tsc;
                            let msg_mods = pack_mods_for_ipc(rep.mods);
                            let msg = Message::new(
                                KBD_EVENT_LABEL,
                                [
                                    0,
                                    rep.ascii as usize,
                                    msg_mods as usize,
                                    rep.scancode as usize,
                                    rep.extended as usize,
                                    0,
                                ],
                                5,
                            );
                            ctx.forward(&msg);
                        }
                    }
                }
            }
            continue;
        }

        if ctrl_alt {
            if let Some(target) = vt_switch_target(key) {
                ctx.request_vt_switch(target);
                continue;
            }
            if key == HID_USAGE_DELETE {
                ctx.send_shutdown();
                continue;
            }
        }

        let extended = hid_usage_to_extended(key);
        let scancode = hid_to_ps2_scancode(key).unwrap_or(0);
        let ascii = if extended != 0 {
            0u8
        } else {
            translate_scancode(scancode, kbd_mods).unwrap_or(0)
        };

        if ascii != 0 || extended != 0 {
            static FIRST_KEY_LOGGED: core::sync::atomic::AtomicBool =
                core::sync::atomic::AtomicBool::new(false);
            if !FIRST_KEY_LOGGED.swap(true, core::sync::atomic::Ordering::Relaxed) {
                let _ = debug_print(&format!(
                    "usb-input: first key forwarded usage=0x{:02x} scancode=0x{:02x} ascii=0x{:02x} extended={} mods=0x{:02x}",
                    key, scancode, ascii, extended, kbd_mods
                ));
            }
            let msg_mods = pack_mods_for_ipc(kbd_mods);
            let msg = Message::new(
                KBD_EVENT_LABEL,
                [0, ascii as usize, msg_mods as usize, scancode as usize, extended as usize, 0],
                5,
            );
            ctx.forward(&msg);

            if !ctrl_alt {
                let now_tsc = libcluu::syscall::clock_now(clock_token).unwrap_or(0);
                dev.repeat = Some(RepeatState {
                    key,
                    press_tsc: now_tsc,
                    last_repeat_tsc: now_tsc,
                    scancode,
                    ascii,
                    extended,
                    mods: kbd_mods,
                });
            }
        }
    }

    if let Some(ref rep) = dev.repeat {
        if !new_keys.contains(&rep.key) {
            dev.repeat = None;
        }
    }

    dev.last_keys = new_keys;
}

fn handle_mouse_report(ctx: &UsbInputContext, report: &[u8]) {
    if report.len() < 3 {
        return;
    }
    let buttons = report[0] & 0x07;
    let dx = report[1] as i8 as i32;
    let dy = report[2] as i8 as i32;
    let msg = Message::new(
        MOUSE_EVENT_LABEL,
        [0, dx as usize, dy as usize, buttons as usize, 0, 0],
        4,
    );
    ctx.forward(&msg);
}

fn pump_registry(ctx: &mut UsbInputContext) {
    let mut buf = [0u8; 128];
    let tokens = [ctx.endpoint, ctx.registry_endpoint];
    if let Ok((idx, len)) = libcluu::syscall::ipc_recv_any(&tokens, &mut buf, 0) {
        if idx == 1 {
            if let Some((msg, payload)) = ipc::parse_message(&buf[..len]) {
                ctx.handle_registry_message(&msg, payload);
            }
        }
    }
    ctx.ensure_subscriptions();
}

fn idle_loop(mut ctx: UsbInputContext) -> Result<()> {
    let mut buf = [0u8; 128];
    loop {
        ctx.ensure_subscriptions();
        let tokens = [ctx.endpoint, ctx.registry_endpoint];
        match libcluu::syscall::ipc_recv_any(&tokens, &mut buf, u64::MAX) {
            Ok((idx, len)) => {
                if idx == 1 {
                    if let Some((msg, payload)) = ipc::parse_message(&buf[..len]) {
                        ctx.handle_registry_message(&msg, payload);
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
