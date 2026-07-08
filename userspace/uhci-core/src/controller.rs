use alloc::format;
use cluu_dma_core::{DmaPool, DmaRegion};
use cluu_driver_framework::pci::{self, PciDeviceInfo};
use libcluu::syscall;
use libcluu::{debug_print, Error, Result};

use crate::transfer::{setup_token_packet, UhciQueueHead, UhciTd, PID_IN, PID_OUT, PID_SETUP,
    REQ_GET_DESCRIPTOR, REQ_SET_ADDRESS, REQ_SET_CONFIGURATION, REQ_SET_IDLE,
    REQ_SET_PROTOCOL, REQ_TYPE_CLASS, REQ_TYPE_DEV_TO_HOST, REQ_TYPE_HOST_TO_DEV,
    REQ_TYPE_RECIPIENT_INTERFACE, DESC_DEVICE};

const USBCMD: u16 = 0;
const USBSTS: u16 = 2;
const USBINTR: u16 = 4;
const FRBASEADD: u16 = 8;
const SOFMOD: u16 = 12;
const PORTSC1: u16 = 16;
const PORTSC2: u16 = 18;

const USBCMD_RUN: u16 = 1 << 0;
const USBCMD_HCRESET: u16 = 1 << 1;
const USBCMD_MAXPACKET64: u16 = 1 << 6;
const USBCMD_CONFIGURE: u16 = 1 << 7;

const USBSTS_INT: u16 = 1 << 0;
const USBSTS_ERR: u16 = 1 << 1;
const USBSTS_HCH: u16 = 1 << 5;

const PORTSC_CCS: u16 = 1 << 0;
const PORTSC_CSC: u16 = 1 << 1;
const PORTSC_PED: u16 = 1 << 2;
const PORTSC_PEC: u16 = 1 << 3;
const PORTSC_RESET: u16 = 1 << 9;
const PORTSC_SUSPEND: u16 = 1 << 12;
const PORTSC_RD: u16 = 1 << 11;

const FRAME_LIST_SIZE: usize = 1024;

pub struct UhciController {
    pub io_base: u16,
    pub pci_dev: PciDeviceInfo,
    pub pci_token: usize,
    pub n_ports: u8,
    pub frame_list: DmaRegion,
    pub qh_pool: DmaRegion,
    pub td_pool: DmaRegion,
}

impl UhciController {
    pub fn probe(pci_token: usize, _space_token: usize, pool: &mut DmaPool) -> Result<Self> {
        let devs = pci::enumerate(pci_token)?;
        for d in &devs {
            let _ = debug_print(&format!(
                "uhci-core: PCI {:02x}:{:02x}.{} vid=0x{:04x} did=0x{:04x} class=0x{:06x}",
                d.bus, d.device, d.function, d.vendor_id, d.device_id, d.class_code
            ));
        }
        let uhci_dev = pci::find_by_class(&devs, 0xFFFFFF, 0x0C0300)
            .or_else(|| pci::find_by_class(&devs, 0xFFFFFF, 0x0C0310))
            .ok_or(Error::NotFound)?;
        let _ = debug_print(&format!(
            "uhci-core: found UHCI at {:02x}:{:02x}.{} class=0x{:06x}",
            uhci_dev.bus, uhci_dev.device, uhci_dev.function, uhci_dev.class_code
        ));

        pci::enable(pci_token, uhci_dev)?;

        let cmd_after = libcluu::pci::config_read_u32(
            pci_token, uhci_dev.bus, uhci_dev.device, uhci_dev.function, 0x04,
        )?;
        let _ = debug_print(&format!(
            "uhci-core: PCI cmd=0x{:08x} IO={} MEM={} BM={}",
            cmd_after,
            cmd_after & 1 != 0,
            cmd_after & 2 != 0,
            cmd_after & 4 != 0,
        ));

        let bar0_raw = libcluu::pci::config_read_u32(
            pci_token, uhci_dev.bus, uhci_dev.device, uhci_dev.function, 0x10,
        )?;
        let io_base = if bar0_raw == 0 || (bar0_raw & 1) == 0 {
            let new_bar = 0xC000u32;
            libcluu::pci::config_write_u32(
                pci_token, uhci_dev.bus, uhci_dev.device, uhci_dev.function, 0x10, new_bar,
            )?;
            (new_bar & 0xFFF0) as u16
        } else {
            (bar0_raw & 0xFFF0) as u16
        };
        let _ = debug_print(&format!("uhci-core: I/O base=0x{:04x}", io_base));

        let n_ports = 2u8;

        let frame_list = pool.alloc(FRAME_LIST_SIZE * 4, 4096)?;
        let qh_pool = pool.alloc(64 * core::mem::size_of::<UhciQueueHead>(), 16)?;
        let td_pool = pool.alloc(128 * core::mem::size_of::<UhciTd>(), 16)?;

        Ok(Self {
            io_base,
            pci_dev: uhci_dev.clone(),
            pci_token,
            n_ports,
            frame_list,
            qh_pool,
            td_pool,
        })
    }

    fn port_in16(&self, offset: u16) -> u16 {
        syscall::port_in16(self.pci_token, self.io_base + offset).unwrap_or(0)
    }

    fn port_out16(&self, offset: u16, val: u16) {
        let _ = syscall::port_out16(self.pci_token, self.io_base + offset, val);
    }

    fn port_in32(&self, offset: u16) -> u32 {
        syscall::port_in32(self.pci_token, self.io_base + offset).unwrap_or(0)
    }

    fn port_out32(&self, offset: u16, val: u32) {
        let _ = syscall::port_out32(self.pci_token, self.io_base + offset, val);
    }

    pub fn reset(&self) -> Result<()> {
        let cmd_before = self.port_in16(USBCMD);
        let sts_before = self.port_in16(USBSTS);
        let _ = debug_print(&format!(
            "uhci-core: pre-reset USBCMD=0x{:04x} USBSTS=0x{:04x}", cmd_before, sts_before
        ));

        self.port_out16(USBCMD, 0);
        for _ in 0..1000 {
            if self.port_in16(USBSTS) & USBSTS_HCH != 0 {
                break;
            }
        }

        self.port_out16(USBCMD, USBCMD_HCRESET);
        for _ in 0..10000 {
            if self.port_in16(USBCMD) & USBCMD_HCRESET == 0 {
                break;
            }
        }
        let cmd_after = self.port_in16(USBCMD);
        let _ = debug_print(&format!("uhci-core: post-reset USBCMD=0x{:04x}", cmd_after));
        if cmd_after & USBCMD_HCRESET != 0 {
            return Err(Error::Timeout);
        }

        self.port_out16(USBINTR, 0);
        self.port_out16(USBSTS, 0xFFFF);

        let _ = debug_print("uhci-core: controller reset complete");
        Ok(())
    }

    pub fn start(&self) -> Result<()> {
        let frame_ptr = self.frame_list.virt as *mut u32;
        let term_qh_phys = self.alloc_qh(0)?;
        let term_qh = self.qh_at(0);
        term_qh.terminate();
        term_qh.set_td_terminate();

        for i in 0..FRAME_LIST_SIZE {
            unsafe {
                core::ptr::write_volatile(frame_ptr.add(i), term_qh_phys | 0x2);
            }
        }

        self.port_out32(FRBASEADD, self.frame_list.phys as u32);
        self.port_out16(FRBASEADD, self.frame_list.phys as u32 as u16);
        self.port_out16(FRBASEADD + 2, (self.frame_list.phys >> 16) as u32 as u16);

        self.port_out16(USBSTS, 0xFFFF);
        self.port_out16(USBCMD, USBCMD_RUN | USBCMD_MAXPACKET64 | USBCMD_CONFIGURE);

        let _ = debug_print(&format!(
            "uhci-core: started usbsts=0x{:04x}", self.port_in16(USBSTS)
        ));
        Ok(())
    }

    fn alloc_qh(&self, _idx: usize) -> Result<u32> {
        Ok(self.qh_pool.phys as u32)
    }

    fn qh_at(&self, idx: usize) -> &mut UhciQueueHead {
        unsafe {
            &mut *((self.qh_pool.virt + idx * core::mem::size_of::<UhciQueueHead>()) as *mut UhciQueueHead)
        }
    }

    fn td_at(&self, idx: usize) -> &mut UhciTd {
        unsafe {
            &mut *((self.td_pool.virt + idx * core::mem::size_of::<UhciTd>()) as *mut UhciTd)
        }
    }

    pub fn find_connected_port(&self) -> Option<u8> {
        for port in 0..self.n_ports {
            let sc = self.port_in16(PORTSC1 + (port as u16) * 2);
            if sc & PORTSC_CCS != 0 {
                let _ = debug_print(&format!(
                    "uhci-core: port {} connected (PORTSC=0x{:04x})", port, sc
                ));
                return Some(port);
            }
        }
        None
    }

    pub fn reset_port(&self, port: u8) -> Result<u8> {
        let port_reg = PORTSC1 + (port as u16) * 2;
        let sc = self.port_in16(port_reg);
        self.port_out16(port_reg, sc & !(PORTSC_PED | PORTSC_RESET));
        let _ = libcluu::yield_cpu();

        self.port_out16(port_reg, PORTSC_RESET);
        for _ in 0..100 {
            let _ = libcluu::yield_cpu();
        }

        self.port_out16(port_reg, self.port_in16(port_reg) & !PORTSC_RESET);

        for _ in 0..1000 {
            let s = self.port_in16(port_reg);
            if s & PORTSC_CCS == 0 {
                break;
            }
            if s & PORTSC_PED != 0 {
                break;
            }
            let _ = libcluu::yield_cpu();
        }

        let sc = self.port_in16(port_reg);
        let speed = if sc & (1 << 8) != 0 { 0u8 } else { 1u8 };
        let _ = debug_print(&format!(
            "uhci-core: port {} reset complete speed={} PED={} PORTSC=0x{:04x}",
            port, speed, sc & PORTSC_PED != 0, sc
        ));
        Ok(speed)
    }

    pub fn control_transfer(
        &mut self,
        _pool: &mut DmaPool,
        addr: u8,
        _ep_speed: u8,
        max_pkt: u16,
        setup: &[u8; 8],
        data: Option<&mut [u8]>,
        data_in: bool,
    ) -> Result<usize> {
        let qh = self.qh_at(1);
        *qh = UhciQueueHead::new();
        qh.terminate();
        let qh_phys = self.qh_pool.phys as u32 + core::mem::size_of::<UhciQueueHead>() as u32;

        let setup_dma = self.alloc_data(8, 8)?;
        unsafe {
            core::ptr::copy_nonoverlapping(setup.as_ptr(), setup_dma.virt as *mut u8, 8);
        }

        let setup_td = self.td_at(0);
        *setup_td = UhciTd::new();
        setup_td.set_pid(PID_SETUP);
        setup_td.set_max_len(8);
        setup_td.set_buffer(setup_dma.phys as u32);
        setup_td.set_device_addr(addr);
        setup_td.set_endpoint(0);
        setup_td.set_active();
        let setup_td_phys = self.td_pool.phys as u32;

        let data_len = data.as_ref().map(|d| d.len()).unwrap_or(0);
        let (data_dma, data_td_phys) = if data_len > 0 {
            let dma = self.alloc_data(data_len, 8)?;
            if !data_in {
                let data_ref = data.as_ref().unwrap();
                unsafe {
                    core::ptr::copy_nonoverlapping(data_ref.as_ptr(), dma.virt as *mut u8, data_len);
                }
            }
            let td = self.td_at(1);
            *td = UhciTd::new();
            td.set_pid(if data_in { PID_IN } else { PID_OUT });
            td.set_max_len(data_len as u32);
            td.set_buffer(dma.phys as u32);
            td.set_device_addr(addr);
            td.set_endpoint(0);
            td.set_data_toggle(1);
            td.set_active();
            (Some(dma), self.td_pool.phys as u32 + core::mem::size_of::<UhciTd>() as u32)
        } else {
            (None, 0)
        };

        let status_td = self.td_at(2);
        *status_td = UhciTd::new();
        status_td.set_pid(if data_in { PID_OUT } else { PID_IN });
        status_td.set_max_len(0x7FF);
        status_td.set_device_addr(addr);
        status_td.set_endpoint(0);
        status_td.set_data_toggle(1);
        status_td.set_interrupt();
        status_td.set_active();
        let status_td_phys = self.td_pool.phys as u32 + 2 * core::mem::size_of::<UhciTd>() as u32;

        if data_td_phys != 0 {
            setup_td.set_link(data_td_phys | 0x4);
            let data_td = self.td_at(1);
            data_td.set_link(status_td_phys | 0x4);
        } else {
            setup_td.set_link(status_td_phys | 0x4);
        }

        qh.set_td(setup_td_phys);

        let frame_ptr = self.frame_list.virt as *mut u32;
        unsafe {
            core::ptr::write_volatile(frame_ptr, qh_phys | 0x2);
        }

        for _ in 0..200000 {
            let sts = self.port_in16(USBSTS);
            if sts & (USBSTS_INT | USBSTS_ERR) != 0 {
                self.port_out16(USBSTS, 0xFFFF);
                break;
            }
            let _ = libcluu::yield_cpu();
        }

        unsafe {
            core::ptr::write_volatile(frame_ptr, self.qh_pool.phys as u32 | 0x2);
        }

        let status_td = self.td_at(2);
        if status_td.is_active() {
            let _ = debug_print("uhci-core: control transfer timeout");
            return Err(Error::Timeout);
        }
        if status_td.is_stalled() || status_td.has_error() {
            let _ = debug_print(&format!(
                "uhci-core: control transfer error status=0x{:08x}", status_td.status
            ));
            return Err(Error::InvalidOperation);
        }

        let actual = if data_len > 0 && data_in {
            if let Some(ref dma) = data_dma {
                let data_ref = data.as_ref().unwrap();
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        dma.virt as *const u8,
                        data_ref.as_ptr() as *mut u8,
                        data_len,
                    );
                }
                let data_td = self.td_at(1);
                let act_len = data_td.actual_len();
                let _ = debug_print(&format!("uhci-core: IN actual={} bytes", act_len));
                act_len
            } else { 0 }
        } else { 0 };

        let _ = max_pkt;
        Ok(actual)
    }

    fn alloc_data(&self, len: usize, _align: usize) -> Result<DmaRegion> {
        let off = 0x3000;
        if len > 0x1000 {
            return Err(Error::InvalidArgument);
        }
        Ok(DmaRegion {
            virt: self.frame_list.virt + off,
            phys: self.frame_list.phys + off as u64,
            len,
        })
    }

    pub fn set_address(&mut self, pool: &mut DmaPool, addr: u8, speed: u8, max_pkt: u16) -> Result<()> {
        let setup = setup_token_packet(REQ_TYPE_HOST_TO_DEV, REQ_SET_ADDRESS, (addr as u16) << 0, 0, 0);
        self.control_transfer(pool, 0, speed, max_pkt, &setup, None, false)?;
        let _ = debug_print(&format!("uhci-core: SET_ADDRESS({}) ok", addr));
        Ok(())
    }

    pub fn get_device_descriptor(
        &mut self, pool: &mut DmaPool, addr: u8, speed: u8, max_pkt: u16,
    ) -> Result<[u8; 18]> {
        let setup = setup_token_packet(REQ_TYPE_DEV_TO_HOST, REQ_GET_DESCRIPTOR, (DESC_DEVICE as u16) << 8, 0, 18);
        let mut buf = [0u8; 18];
        let n = self.control_transfer(pool, addr, speed, max_pkt, &setup, Some(&mut buf), true)?;
        let _ = debug_print(&format!("uhci-core: GET_DESCRIPTOR device, got {} bytes", n));
        Ok(buf)
    }

    pub fn set_configuration(&mut self, pool: &mut DmaPool, addr: u8, speed: u8, max_pkt: u16, config: u8) -> Result<()> {
        let setup = setup_token_packet(REQ_TYPE_HOST_TO_DEV, REQ_SET_CONFIGURATION, config as u16, 0, 0);
        self.control_transfer(pool, addr, speed, max_pkt, &setup, None, false)?;
        let _ = debug_print(&format!("uhci-core: SET_CONFIGURATION({}) ok", config));
        Ok(())
    }

    pub fn set_idle(&mut self, pool: &mut DmaPool, addr: u8, speed: u8, max_pkt: u16) -> Result<()> {
        let setup = setup_token_packet(
            REQ_TYPE_HOST_TO_DEV | REQ_TYPE_CLASS | REQ_TYPE_RECIPIENT_INTERFACE,
            REQ_SET_IDLE, 0, 0, 0,
        );
        self.control_transfer(pool, addr, speed, max_pkt, &setup, None, false)?;
        let _ = debug_print("uhci-core: SET_IDLE ok");
        Ok(())
    }

    pub fn set_protocol(&mut self, pool: &mut DmaPool, addr: u8, speed: u8, max_pkt: u16, protocol: u8) -> Result<()> {
        let setup = setup_token_packet(
            REQ_TYPE_HOST_TO_DEV | REQ_TYPE_CLASS | REQ_TYPE_RECIPIENT_INTERFACE,
            REQ_SET_PROTOCOL, protocol as u16, 0, 0,
        );
        self.control_transfer(pool, addr, speed, max_pkt, &setup, None, false)?;
        let _ = debug_print(&format!("uhci-core: SET_PROTOCOL({}) ok", protocol));
        Ok(())
    }

    pub fn setup_interrupt_in(&mut self, _pool: &mut DmaPool, addr: u8, max_pkt: u16, report_dma: &DmaRegion) -> Result<()> {
        let td = self.td_at(3);
        *td = UhciTd::new();
        td.set_pid(PID_IN);
        td.set_max_len(max_pkt as u32);
        td.set_buffer(report_dma.phys as u32);
        td.set_device_addr(addr);
        td.set_endpoint(1);
        td.set_interrupt();
        td.set_active();
        let td_phys = self.td_pool.phys as u32 + 3 * core::mem::size_of::<UhciTd>() as u32;

        let qh = self.qh_at(2);
        *qh = UhciQueueHead::new();
        qh.set_td(td_phys);
        let qh_phys = self.qh_pool.phys as u32 + 2 * core::mem::size_of::<UhciQueueHead>() as u32;

        let frame_ptr = self.frame_list.virt as *mut u32;
        for i in 0..FRAME_LIST_SIZE {
            unsafe {
                core::ptr::write_volatile(frame_ptr.add(i), qh_phys | 0x2);
            }
        }
        let _ = debug_print("uhci-core: interrupt IN queued");
        Ok(())
    }

    pub fn poll_interrupt(&self, _report_dma: &DmaRegion) -> Option<usize> {
        let td = self.td_at(3);
        if td.is_active() {
            return None;
        }
        let sts = self.port_in16(USBSTS);
        if sts & (USBSTS_INT | USBSTS_ERR) != 0 {
            self.port_out16(USBSTS, 0xFFFF);
        }
        if td.has_error() || td.is_stalled() {
            return None;
        }
        let actual = td.actual_len();
        td.set_active();
        Some(actual)
    }
}
