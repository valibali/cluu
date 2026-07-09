extern crate alloc;

use alloc::format;
use alloc::vec::Vec;
use cluu_dma_core::{DmaPool, DmaRegion};
use cluu_driver_framework::mmio::{MmioAccess, MmioRegion};
use cluu_driver_framework::pci::{self, PciDeviceInfo};
use libcluu::{debug_print, Error, Result};

use crate::queue::{
    setup_token_packet, PID_IN, PID_OUT, PID_SETUP, QueueHead, QtD,
    REQ_GET_DESCRIPTOR, REQ_SET_ADDRESS, REQ_SET_CONFIGURATION, REQ_SET_IDLE,
    REQ_SET_PROTOCOL, REQ_TYPE_CLASS, REQ_TYPE_DEV_TO_HOST, REQ_TYPE_HOST_TO_DEV,
    REQ_TYPE_RECIPIENT_INTERFACE, DESC_DEVICE, DESC_CONFIGURATION,
};
use crate::regs::{
    EhciRegs, PORTSC_CCS, PORTSC_CSC, PORTSC_PED, PORTSC_PEC, PORTSC_PP, PORTSC_RESET,
    USBCMD_ASEN, USBCMD_FLSIZE_SHIFT, USBCMD_HCREST, USBCMD_PSEN, USBCMD_RUN,
    USBSTS_AADV, USBSTS_ASI, USBSTS_HCH, USBSTS_INT,
};

const FRAME_LIST_SIZE: usize = 1024;
const FRAME_LIST_BYTES: usize = FRAME_LIST_SIZE * 4;

/// Maximum number of simultaneous interrupt-IN devices (e.g. kbd + mouse).
pub const MAX_INTR_SLOTS: usize = 2;

pub struct EhciController {
    pub regs: EhciRegs,
    pub pci_dev: PciDeviceInfo,
    pub n_ports: u8,
    pub async_head: DmaRegion,
    pub periodic_list: DmaRegion,
    pub intr_qhs: [DmaRegion; MAX_INTR_SLOTS],
}

impl EhciController {
    pub fn probe(pci_token: usize, space_token: usize, pool: &mut DmaPool) -> Result<Self> {
        let devs = pci::enumerate(pci_token)?;
        let ehci_dev = pci::find_by_class(&devs, 0xFFFFFF, 0x0C0320)
            .ok_or(Error::NotFound)?;
        let _ = debug_print(&format!(
            "ehci-core: found EHCI at {:02x}:{:02x}.{} class=0x{:06x}",
            ehci_dev.bus, ehci_dev.device, ehci_dev.function, ehci_dev.class_code
        ));

        pci::enable(pci_token, ehci_dev)?;

        let bar = &ehci_dev.bars[0];
        let bar_phys = bar.addr;
        let map_size = (((bar.size as usize).max(0x1000)) + 0xFFF) & !0xFFF;
        let mmio = MmioRegion::map(space_token, bar_phys, map_size, MmioAccess::MAP_DEVICE)?;
        let regs = EhciRegs::new(mmio);
        let n_ports = regs.n_ports();
        let _ = debug_print(&format!(
            "ehci-core: HCIVERSION={:04x} n_ports={}",
            regs.hciversion(), n_ports
        ));

        let async_head = pool.alloc(core::mem::size_of::<QueueHead>(), 32)?;
        let periodic_list = pool.alloc(FRAME_LIST_BYTES, 4096)?;
        let intr_qh0 = pool.alloc(core::mem::size_of::<QueueHead>(), 32)?;
        let intr_qh1 = pool.alloc(core::mem::size_of::<QueueHead>(), 32)?;

        Ok(Self {
            regs,
            pci_dev: ehci_dev.clone(),
            n_ports,
            async_head,
            periodic_list,
            intr_qhs: [intr_qh0, intr_qh1],
        })
    }

    pub fn reset(&self) -> Result<()> {
        if self.regs.usbsts() & USBSTS_HCH == 0 {
            self.regs.set_usbcmd(0);
            for _ in 0..1000 {
                if self.regs.usbsts() & USBSTS_HCH != 0 {
                    break;
                }
            }
        }

        self.regs.set_usbcmd(USBCMD_HCREST);
        for _ in 0..10000 {
            if self.regs.usbcmd() & USBCMD_HCREST == 0 {
                break;
            }
        }
        if self.regs.usbcmd() & USBCMD_HCREST != 0 {
            return Err(Error::Timeout);
        }

        self.regs.set_usbintr(0);
        self.regs.set_config_flag(1);

        let _ = debug_print("ehci-core: controller reset complete");
        Ok(())
    }

    pub fn start(&mut self) -> Result<()> {
        let async_qh = unsafe { &mut *(self.async_head.virt as *mut QueueHead) };
        async_qh.terminate_next();
        async_qh.terminate_qtd();
        async_qh.set_head_of_reclamation();
        let async_phys = self.async_head.phys as u32;
        self.regs.set_async_list_base(async_phys);

        let frame_list_ptr = self.periodic_list.virt as *mut u32;
        let intr_phys0 = self.intr_qhs[0].phys as u32;
        let intr_phys1 = self.intr_qhs[1].phys as u32;

        let intr_qh0 = unsafe { &mut *(self.intr_qhs[0].virt as *mut QueueHead) };
        intr_qh0.terminate_qtd();
        intr_qh0.set_h_addr(0);
        intr_qh0.set_ep_number(1);
        intr_qh0.set_eps(2);
        intr_qh0.set_max_packet_len(8);
        intr_qh0.set_next_qh(intr_phys1 | 0x2);

        let intr_qh1 = unsafe { &mut *(self.intr_qhs[1].virt as *mut QueueHead) };
        intr_qh1.terminate_next();
        intr_qh1.terminate_qtd();
        intr_qh1.set_h_addr(0);
        intr_qh1.set_ep_number(1);
        intr_qh1.set_eps(2);
        intr_qh1.set_max_packet_len(8);

        for i in 0..FRAME_LIST_SIZE {
            unsafe {
                core::ptr::write_volatile(frame_list_ptr.add(i), intr_phys0 | 0x2);
            }
        }
        self.regs.set_periodic_list_base(self.periodic_list.phys as u32);

        let cmd = USBCMD_RUN | USBCMD_ASEN | USBCMD_PSEN | (0 << USBCMD_FLSIZE_SHIFT);
        self.regs.set_usbcmd(cmd);

        for _ in 0..1000 {
            if self.regs.usbcmd() & USBCMD_RUN != 0 {
                break;
            }
        }

        let _ = debug_print(&format!(
            "ehci-core: started n_ports={} usbsts=0x{:08x}",
            self.n_ports,
            self.regs.usbsts()
        ));
        Ok(())
    }

    pub fn find_connected_port(&self) -> Option<u8> {
        for port in 0..self.n_ports {
            let sc = self.regs.portsc(port);
            if sc & PORTSC_CCS != 0 {
                let _ = debug_print(&format!(
                    "ehci-core: port {} connected (PORTSC=0x{:08x})",
                    port, sc
                ));
                return Some(port);
            }
        }
        None
    }

    pub fn find_connected_ports(&self) -> Vec<u8> {
        let mut ports = Vec::new();
        for port in 0..self.n_ports {
            let sc = self.regs.portsc(port);
            if sc & PORTSC_CCS != 0 {
                let _ = debug_print(&format!(
                    "ehci-core: port {} connected (PORTSC=0x{:08x})",
                    port, sc
                ));
                ports.push(port);
            }
        }
        ports
    }

    pub fn reset_port(&self, port: u8) -> Result<u8> {
        let sc = self.regs.portsc(port);
        let _ = debug_print(&format!(
            "ehci-core: port {} pre-reset PORTSC=0x{:08x}", port, sc
        ));

        let clear_mask = PORTSC_CSC | PORTSC_PEC | (1 << 4) | (1 << 5);
        let mut val = sc & !PORTSC_RESET;
        val &= !clear_mask;
        val |= PORTSC_RESET;
        val |= PORTSC_PP;
        self.regs.set_portsc(port, val);

        for _ in 0..100000 {
            if self.regs.portsc(port) & PORTSC_RESET == 0 {
                break;
            }
            let _ = libcluu::yield_cpu();
        }

        let sc = self.regs.portsc(port);
        self.regs.set_portsc(port, sc & !PORTSC_RESET);

        let mut ped = false;
        for _ in 0..100000 {
            let s = self.regs.portsc(port);
            if s & PORTSC_PED != 0 {
                ped = true;
                break;
            }
            if s & PORTSC_CCS == 0 {
                break;
            }
            let _ = libcluu::yield_cpu();
        }

        let sc = self.regs.portsc(port);
        if sc & PORTSC_CSC != 0 {
            self.regs.set_portsc(port, sc | PORTSC_CSC);
        }
        let sc = self.regs.portsc(port);
        if sc & PORTSC_PEC != 0 {
            self.regs.set_portsc(port, sc | PORTSC_PEC);
        }

        let sc = self.regs.portsc(port);
        let speed = if sc & PORTSC_PED != 0 {
            2u8
        } else if sc & (1 << 11) != 0 {
            if sc & (1 << 10) != 0 { 2 } else { 1 }
        } else {
            0
        };
        let _ = debug_print(&format!(
            "ehci-core: port {} reset complete speed={} PED={} PORTSC=0x{:08x}",
            port, speed, ped, sc
        ));
        Ok(speed)
    }

    pub fn control_transfer(
        &mut self,
        pool: &mut DmaPool,
        addr: u8,
        ep_speed: u8,
        max_pkt: u16,
        setup: &[u8; 8],
        data: Option<&mut [u8]>,
        data_in: bool,
    ) -> Result<usize> {
        let qh_dma = pool.alloc(core::mem::size_of::<QueueHead>(), 32)?;
        let qh = unsafe { &mut *(qh_dma.virt as *mut QueueHead) };
        *qh = QueueHead::new();
        qh.set_h_addr(addr);
        qh.set_ep_number(0);
        qh.set_eps(ep_speed);
        qh.set_max_packet_len(max_pkt);
        qh.set_control_endpoint();
        qh.set_dtc();
        qh.set_nak_reload(4);

        let setup_dma = pool.alloc(8, 8)?;
        unsafe {
            core::ptr::copy_nonoverlapping(setup.as_ptr(), setup_dma.virt as *mut u8, 8);
        }

        let setup_td_dma = pool.alloc(core::mem::size_of::<QtD>(), 32)?;
        let setup_td = unsafe { &mut *(setup_td_dma.virt as *mut QtD) };
        *setup_td = QtD::new();
        setup_td.set_pid(PID_SETUP);
        setup_td.set_total_bytes(8);
        setup_td.set_cerr(3);
        setup_td.set_data_toggle(0);
        setup_td.set_active();
        setup_td.set_buffer(setup_dma.phys as u32);

        let data_len = data.as_ref().map(|d| d.len()).unwrap_or(0);
        let (data_dma, data_td_dma) = if data_len > 0 {
            let dma = pool.alloc(data_len, 8)?;
            let td_dma = pool.alloc(core::mem::size_of::<QtD>(), 32)?;
            if !data_in {
                let data_ref = data.as_ref().unwrap();
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        data_ref.as_ptr(),
                        dma.virt as *mut u8,
                        data_len,
                    );
                }
            }
            (Some(dma), Some(td_dma))
        } else {
            (None, None)
        };

        if let Some(ref td_dma) = data_td_dma {
            let data_td = unsafe { &mut *(td_dma.virt as *mut QtD) };
            *data_td = QtD::new();
            data_td.set_pid(if data_in { PID_IN } else { PID_OUT });
            data_td.set_total_bytes(data_len as u32);
            data_td.set_cerr(3);
            data_td.set_data_toggle(1);
            data_td.set_active();
            data_td.set_buffer(data_dma.as_ref().unwrap().phys as u32);
        }

        let status_td_dma = pool.alloc(core::mem::size_of::<QtD>(), 32)?;
        let status_td = unsafe { &mut *(status_td_dma.virt as *mut QtD) };
        *status_td = QtD::new();
        status_td.set_pid(if data_in { PID_OUT } else { PID_IN });
        status_td.set_total_bytes(0);
        status_td.set_cerr(3);
        status_td.set_data_toggle(1);
        status_td.set_ioc();
        status_td.set_active();

        let mut prev_phys = setup_td_dma.phys as u32;
        qh.set_qtd_ptr(prev_phys);

        if let Some(td_dma) = data_td_dma {
            let setup_td = unsafe { &mut *(setup_td_dma.virt as *mut QtD) };
            setup_td.set_next(td_dma.phys as u32);
            prev_phys = td_dma.phys as u32;
        }

        let setup_td = unsafe { &mut *(setup_td_dma.virt as *mut QtD) };
        if data_td_dma.is_none() {
            setup_td.set_next(status_td_dma.phys as u32);
        } else {
            let data_td = unsafe { &mut *(data_td_dma.as_ref().unwrap().virt as *mut QtD) };
            data_td.set_next(status_td_dma.phys as u32);
        }

        let async_head = unsafe { &mut *(self.async_head.virt as *mut QueueHead) };
        async_head.set_next_qh(qh_dma.phys as u32);
        qh.set_next_qh(self.async_head.phys as u32);
        unsafe { core::arch::asm!("mfence", options(nostack, preserves_flags)); }
        let _ = debug_print(&format!(
            "ehci-core: async_head next=0x{:08x} transfer_qh phys=0x{:x}",
            async_head.next_qh, qh_dma.phys
        ));

        let _ = debug_print(&format!(
            "ehci-core: async linked USBCMD=0x{:08x} USBSTS=0x{:08x}",
            self.regs.usbcmd(), self.regs.usbsts()
        ));
        let _ = debug_print(&format!(
            "ehci-core: qh phys=0x{:x} next=0x{:08x} charac=0x{:08x} cap=0x{:08x} overlay_next=0x{:08x} overlay_token=0x{:08x}",
            qh_dma.phys, qh.next_qh, qh.charac, qh.cap, qh.overlay.next_td, qh.overlay.token
        ));
        let _ = debug_print(&format!(
            "ehci-core: setup_td phys=0x{:x} next=0x{:08x} token=0x{:08x} buf=0x{:08x}",
            setup_td_dma.phys, setup_td.next_td, setup_td.token, setup_td.buffers[0]
        ));

        for _ in 0..500000 {
            let status_td = unsafe { &*(status_td_dma.virt as *const QtD) };
            if !status_td.is_active() {
                break;
            }
            let qh_now = unsafe { &*(qh_dma.virt as *const QueueHead) };
            if qh_now.is_halted() {
                break;
            }
            let sts = self.regs.usbsts();
            if sts & (1 << 1) != 0 {
                self.regs.set_usbsts(sts);
                let _ = debug_print(&format!(
                    "ehci-core: async error sts=0x{:08x}", sts
                ));
                async_head.terminate_next();
                return Err(Error::InvalidOperation);
            }
            let _ = libcluu::yield_cpu();
        }

        let sts = self.regs.usbsts();
        if sts & USBSTS_INT != 0 {
            self.regs.set_usbsts(sts);
        }

        let qh_after = unsafe { &*(qh_dma.virt as *const QueueHead) };
        let status_td = unsafe { &*(status_td_dma.virt as *const QtD) };
        if qh_after.is_halted() || status_td.is_halted() {
            let setup_after = unsafe { &*(setup_td_dma.virt as *const QtD) };
            let _ = debug_print(&format!(
                "ehci-core: transfer halted (stall) overlay_token=0x{:08x} setup_td token=0x{:08x} status_td token=0x{:08x}",
                qh_after.overlay.token, setup_after.token, status_td.token
            ));
            async_head.terminate_next();
            return Err(Error::InvalidOperation);
        }
        if status_td.is_active() {
            let setup_after = unsafe { &*(setup_td_dma.virt as *const QtD) };
            let _ = debug_print(&format!(
                "ehci-core: timeout qh cur_td=0x{:08x} overlay_next=0x{:08x} overlay_token=0x{:08x}",
                qh_after.cur_td, qh_after.overlay.next_td, qh_after.overlay.token
            ));
            let _ = debug_print(&format!(
                "ehci-core: timeout setup_td token=0x{:08x} status_td token=0x{:08x}",
                setup_after.token, status_td.token
            ));
            async_head.terminate_next();
            return Err(Error::Timeout);
        }

        async_head.terminate_next();

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
                let data_td = unsafe { &*(data_td_dma.as_ref().unwrap().virt as *const QtD) };
                data_len - data_td.remaining_bytes() as usize
            } else {
                0
            }
        } else {
            0
        };

        Ok(actual)
    }

    pub fn set_address(&mut self, pool: &mut DmaPool, addr: u8, ep_speed: u8, max_pkt: u16) -> Result<()> {
        let setup = setup_token_packet(REQ_TYPE_HOST_TO_DEV, REQ_SET_ADDRESS, (addr as u16) << 0, 0, 0);
        self.control_transfer(pool, 0, ep_speed, max_pkt, &setup, None, false)?;
        let _ = debug_print(&format!("ehci-core: SET_ADDRESS({}) ok", addr));
        Ok(())
    }

    pub fn get_device_descriptor(
        &mut self,
        pool: &mut DmaPool,
        addr: u8,
        ep_speed: u8,
        max_pkt: u16,
    ) -> Result<[u8; 18]> {
        let setup = setup_token_packet(
            REQ_TYPE_DEV_TO_HOST,
            REQ_GET_DESCRIPTOR,
            (DESC_DEVICE as u16) << 8,
            0,
            18,
        );
        let mut buf = [0u8; 18];
        let n = self.control_transfer(pool, addr, ep_speed, max_pkt, &setup, Some(&mut buf), true)?;
        let _ = debug_print(&format!("ehci-core: GET_DESCRIPTOR device, got {} bytes", n));
        Ok(buf)
    }

    pub fn get_config_descriptor(
        &mut self,
        pool: &mut DmaPool,
        addr: u8,
        ep_speed: u8,
        max_pkt: u16,
    ) -> Result<[u8; 64]> {
        let setup = setup_token_packet(
            REQ_TYPE_DEV_TO_HOST,
            REQ_GET_DESCRIPTOR,
            (DESC_CONFIGURATION as u16) << 8,
            0,
            64,
        );
        let mut buf = [0u8; 64];
        let n = self.control_transfer(pool, addr, ep_speed, max_pkt, &setup, Some(&mut buf), true)?;
        let _ = debug_print(&format!("ehci-core: GET_DESCRIPTOR config, got {} bytes", n));
        Ok(buf)
    }

    pub fn set_configuration(
        &mut self,
        pool: &mut DmaPool,
        addr: u8,
        ep_speed: u8,
        max_pkt: u16,
        config: u8,
    ) -> Result<()> {
        let setup = setup_token_packet(REQ_TYPE_HOST_TO_DEV, REQ_SET_CONFIGURATION, config as u16, 0, 0);
        self.control_transfer(pool, addr, ep_speed, max_pkt, &setup, None, false)?;
        let _ = debug_print(&format!("ehci-core: SET_CONFIGURATION({}) ok", config));
        Ok(())
    }

    pub fn set_idle(&mut self, pool: &mut DmaPool, addr: u8, ep_speed: u8, max_pkt: u16) -> Result<()> {
        let setup = setup_token_packet(
            REQ_TYPE_HOST_TO_DEV | REQ_TYPE_CLASS | REQ_TYPE_RECIPIENT_INTERFACE,
            REQ_SET_IDLE,
            0,
            0,
            0,
        );
        self.control_transfer(pool, addr, ep_speed, max_pkt, &setup, None, false)?;
        let _ = debug_print("ehci-core: SET_IDLE ok");
        Ok(())
    }

    pub fn set_protocol(
        &mut self,
        pool: &mut DmaPool,
        addr: u8,
        ep_speed: u8,
        max_pkt: u16,
        protocol: u8,
    ) -> Result<()> {
        let setup = setup_token_packet(
            REQ_TYPE_HOST_TO_DEV | REQ_TYPE_CLASS | REQ_TYPE_RECIPIENT_INTERFACE,
            REQ_SET_PROTOCOL,
            protocol as u16,
            0,
            0,
        );
        self.control_transfer(pool, addr, ep_speed, max_pkt, &setup, None, false)?;
        let _ = debug_print(&format!("ehci-core: SET_PROTOCOL({}) ok", protocol));
        Ok(())
    }

    pub fn setup_interrupt_in(
        &mut self,
        pool: &mut DmaPool,
        slot: usize,
        addr: u8,
        max_pkt: u16,
        report_dma: &DmaRegion,
    ) -> Result<()> {
        if slot >= MAX_INTR_SLOTS {
            return Err(Error::InvalidArgument);
        }
        let td_dma = pool.alloc(core::mem::size_of::<QtD>(), 32)?;
        let td = unsafe { &mut *(td_dma.virt as *mut QtD) };
        *td = QtD::new();
        td.set_pid(PID_IN);
        td.set_total_bytes(max_pkt as u32);
        td.set_data_toggle(0);
        td.set_active();
        td.set_ioc();
        td.set_buffer(report_dma.phys as u32);

        let intr_qh = unsafe { &mut *(self.intr_qhs[slot].virt as *mut QueueHead) };
        intr_qh.set_h_addr(addr);
        intr_qh.set_ep_number(1);
        intr_qh.set_eps(2);
        intr_qh.set_max_packet_len(max_pkt);
        intr_qh.set_qtd_ptr(td_dma.phys as u32);

        Ok(())
    }

    pub fn poll_interrupt(&self, slot: usize, _report_dma: &DmaRegion, _max_len: usize) -> Option<usize> {
        if slot >= MAX_INTR_SLOTS {
            return None;
        }
        let sts = self.regs.usbsts();
        if sts & USBSTS_INT != 0 {
            self.regs.set_usbsts(sts);
        }
        let intr_qh = unsafe { &*(self.intr_qhs[slot].virt as *const QueueHead) };
        if intr_qh.is_active() {
            return None;
        }
        Some(_max_len)
    }
}
