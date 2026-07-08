use alloc::format;
use cluu_dma_core::DmaPool;
use cluu_driver_framework::mmio::{MmioAccess, MmioRegion};
use libcluu::{Error, Result};

use crate::context::*;
use crate::pci::XhciPciDevice;
use crate::regs::{XhciRegs, USBCMD_HCRST, USBCMD_RS, USBSTS_CNR, USBSTS_HCH,
    PORTSC_CCS, PORTSC_PED, PORTSC_PR, PORTSC_CSC, PORTSC_PRC, PORTSC_SPEED_MASK};
use crate::ring::{EventRing, Trb, TrbRing, TrbType};

#[derive(Debug)]
pub enum XhciError {
    PciNotFound,
    ResetTimeout,
    MmioMapFailed,
    RingAllocFailed,
}

pub struct XhciController {
    pub regs: XhciRegs,
    pub cmd_ring: TrbRing,
    pub event_ring: EventRing,
    pub dcbaa: DcbaaRegion,
    pub pci_dev: XhciPciDevice,
    pub max_slots: u8,
    pub max_ports: u8,
}

pub struct DcbaaRegion {
    pub dma: cluu_dma_core::DmaRegion,
}

impl DcbaaRegion {
    pub fn new(pool: &mut DmaPool, max_slots: u8) -> Result<Self> {
        let count = (max_slots as usize + 1) * 8;
        let dma = pool.alloc(count, 64)?;
        for i in 0..(count / 8) {
            unsafe { core::ptr::write_volatile((dma.virt + i * 8) as *mut u64, 0); }
        }
        Ok(Self { dma })
    }

    pub fn set(&self, slot: u8, phys: u64) {
        unsafe {
            core::ptr::write_volatile((self.dma.virt + (slot as usize) * 8) as *mut u64, phys);
        }
    }

    pub fn phys(&self) -> u64 {
        self.dma.phys
    }
}

pub struct UsbDevice {
    pub slot_id: u8,
    pub port: u8,
    pub speed: u8,
    pub dev_ctx_dma: cluu_dma_core::DmaRegion,
    pub input_ctx_dma: cluu_dma_core::DmaRegion,
    pub ep0_ring: TrbRing,
    pub ep1_in_ring: TrbRing,
}

impl XhciController {
    pub fn probe(pci_token: usize, space_token: usize, pool: &mut DmaPool) -> Result<Self> {
        let pci_dev = crate::pci::find_xhci_device(pci_token)?;
        let map_size = ((pci_dev.bar_size as usize) + 0xFFF) & !0xFFF;
        let mmio = MmioRegion::map(space_token, pci_dev.bar_phys, map_size.max(0x1000), MmioAccess::MAP_DEVICE)?;
        let regs = XhciRegs::new(mmio);
        let hcs1 = regs.hcsparams1();
        let hccparams = unsafe { regs.mmio.read32(0x10) };
        let max_slots = (hcs1 & 0xFF) as u8;
        let max_ports = ((hcs1 >> 24) & 0xFF) as u8;
        let _ = libcluu::debug_print(&format!(
            "xhci-core: HCIVERSION={:04x} max_slots={} max_ports={} HCCPARAMS=0x{:08x} AC64={}",
            regs.hciversion(), max_slots, max_ports, hccparams, hccparams & 1
        ));
        let cmd_ring = TrbRing::new(pool, 256)?;
        let event_ring = EventRing::new(pool, 256)?;
        let dcbaa = DcbaaRegion::new(pool, max_slots)?;
        Ok(Self { regs, cmd_ring, event_ring, dcbaa, pci_dev, max_slots, max_ports })
    }

    pub fn reset(&self) -> Result<()> {
        if self.regs.usbsts() & USBSTS_HCH == 0 {
            self.regs.set_usbcmd(0);
            for _ in 0..1000 {
                if self.regs.usbsts() & USBSTS_HCH != 0 { break; }
            }
        }
        self.regs.set_usbcmd(USBCMD_HCRST);
        for _ in 0..1000 {
            if self.regs.usbcmd() & USBCMD_HCRST == 0 { break; }
        }
        if self.regs.usbcmd() & USBCMD_HCRST != 0 {
            return Err(Error::Timeout);
        }
        for _ in 0..1000 {
            if self.regs.usbsts() & USBSTS_CNR == 0 { break; }
        }
        if self.regs.usbsts() & USBSTS_CNR != 0 {
            return Err(Error::Timeout);
        }
        let _ = libcluu::debug_print("xhci-core: controller reset complete");
        Ok(())
    }

    pub fn enable_slots(&self) -> Result<()> {
        self.regs.set_config(self.max_slots as u32);
        let cfg_rb = self.regs.config();
        let _ = libcluu::debug_print(&format!(
            "xhci-core: CONFIG written={} readback={}", self.max_slots, cfg_rb
        ));
        let _ = libcluu::debug_print(&format!(
            "xhci-core: enabled {} slots", self.max_slots
        ));
        Ok(())
    }

    pub fn start(&self) -> Result<()> {
        let _ = libcluu::debug_print(&format!(
            "xhci-core: cap_off=0x{:x} dboff=0x{:x} rtsoff=0x{:x}",
            self.regs.cap_off, self.regs.dboff, self.regs.rtsoff
        ));
        let dcbaap = self.dcbaa.phys();
        self.regs.set_dcbaap_lo(dcbaap as u32);
        self.regs.set_dcbaap_hi((dcbaap >> 32) as u32);
        unsafe { core::ptr::write_volatile(self.dcbaa.dma.virt as *mut u64, 0xDEADBEEF12345678); }
        let dcaa_test = unsafe { core::ptr::read_volatile(self.dcbaa.dma.virt as *const u64) };
        let _ = libcluu::debug_print(&format!(
            "xhci-core: DCBAA[0]=0x{:016x} dcbaap_phys=0x{:x}", dcaa_test, dcbaap
        ));

        let erst = self.event_ring.erst_phys();
        self.regs.set_erstsz(self.event_ring.erst_size());
        self.regs.set_erstba_lo(erst as u32);
        self.regs.set_erstba_hi((erst >> 32) as u32);
        let erst_rb = unsafe { self.regs.mmio.read32(self.regs.rtsoff + 0x0C) };
        let _ = libcluu::debug_print(&format!(
            "xhci-core: ERSTBA=0x{:x} readback=0x{:08x}", erst, erst_rb
        ));
        let erdp = self.event_ring.dequeue_phys();
        self.regs.set_erdp_lo(erdp as u32);
        self.regs.set_erdp_hi((erdp >> 32) as u32);
        self.regs.set_iman(0x3);

        let cmd_phys = self.cmd_ring.phys();
        let usbsts_before = self.regs.usbsts();
        let _ = libcluu::debug_print(&format!(
            "xhci-core: before CRCR write usbsts=0x{:08x} HCH={}",
            usbsts_before, usbsts_before & 1
        ));
        let crcr_val = (cmd_phys & !0x3F) | 1;
        unsafe { self.regs.mmio.write64(self.regs.cap_off + 0x08, crcr_val); }
        let crcr_readback = unsafe { self.regs.mmio.read32(self.regs.cap_off + 0x08) };
        let crcr_hi_readback = unsafe { self.regs.mmio.read32(self.regs.cap_off + 0x0C) };
        let _ = libcluu::debug_print(&format!(
            "xhci-core: CRCR64 written=0x{:x} readback_lo=0x{:08x} readback_hi=0x{:08x}",
            crcr_val, crcr_readback, crcr_hi_readback
        ));
        self.regs.set_usbcmd(USBCMD_RS);
        for _ in 0..1000 {
            if self.regs.usbsts() & USBSTS_HCH == 0 { break; }
        }
        let _ = libcluu::debug_print("xhci-core: controller started");
        Ok(())
    }

    fn send_command(&mut self, trb: Trb) -> Result<Trb> {
        let phys = self.cmd_ring.enqueue(trb)?;
        let _ = libcluu::debug_print(&format!(
            "xhci-core: cmd enqueued at phys=0x{:x} trb_type={} control=0x{:08x}",
            phys, (trb.control >> 10) & 0x3F, trb.control
        ));
        unsafe { core::arch::asm!("mfence", options(nostack, preserves_flags)); };
        self.regs.ring_doorbell(0, 0);
        let _ = libcluu::debug_print("xhci-core: doorbell rung");
        for i in 0..100000 {
            if let Some(evt) = self.event_ring.try_dequeue() {
                let erdp = self.event_ring.dequeue_phys();
                self.regs.set_erdp_lo(erdp as u32 | 0x8);
                self.regs.set_erdp_hi((erdp >> 32) as u32);
                let _ = libcluu::debug_print(&format!(
                    "xhci-core: event type={} code={} slot={} after {} polls",
                    evt.trb_type(), evt.completion_code(), evt.slot_id(), i
                ));
                if evt.trb_type() == TrbType::CommandCompletion as u8 {
                    return Ok(evt);
                }
            }
        }
        let usbsts = self.regs.usbsts();
        let crcr_lo = unsafe { self.regs.mmio.read32(self.regs.cap_off + 0x08) };
        let iman = self.regs.iman();
        let evt0 = unsafe { self.event_ring.dma.virt as *const Trb };
        let evt0_val = unsafe { core::ptr::read_volatile(evt0) };
        let _ = libcluu::debug_print(&format!(
            "xhci-core: TIMEOUT usbsts=0x{:08x} crcr_lo=0x{:08x} iman=0x{:08x} evt[0]=0x{:08x}",
            usbsts, crcr_lo, iman, evt0_val.control
        ));
        Err(Error::Timeout)
    }

    pub fn wait_for_event(&mut self, timeout: usize) -> Option<Trb> {
        for _ in 0..timeout {
            if let Some(evt) = self.event_ring.try_dequeue() {
                let erdp = self.event_ring.dequeue_phys();
                self.regs.set_erdp_lo(erdp as u32 | 0x8);
                self.regs.set_erdp_hi((erdp >> 32) as u32);
                return Some(evt);
            }
        }
        None
    }

    pub fn find_connected_port(&self) -> Option<u8> {
        for port in 1..=self.max_ports {
            let portsc = self.regs.portsc(port);
            if portsc & PORTSC_CCS != 0 {
                let _ = libcluu::debug_print(&format!(
                    "xhci-core: port {} connected (PORTSC=0x{:08x})", port, portsc
                ));
                return Some(port);
            }
        }
        None
    }

    pub fn reset_port(&self, port: u8) -> Result<u8> {
        self.regs.set_portsc(port, PORTSC_PR | PORTSC_CSC);
        for _ in 0..10000 {
            let portsc = self.regs.portsc(port);
            if portsc & PORTSC_PR == 0 {
                let speed = ((portsc & PORTSC_SPEED_MASK) >> 10) as u8;
                let _ = libcluu::debug_print(&format!(
                    "xhci-core: port {} reset complete speed={} PED={}",
                    port, speed, (portsc & PORTSC_PED) != 0
                ));
                return Ok(speed);
            }
        }
        Err(Error::Timeout)
    }

    pub fn enable_slot(&mut self, port: u8, speed: u8, pool: &mut DmaPool) -> Result<UsbDevice> {
        let cmd_trb = Trb {
            param: 0,
            status: 0,
            control: ((TrbType::EnableSlot as u32) << 10) | (1 << 5),
        };
        let evt = self.send_command(cmd_trb)?;
        let slot_id = evt.slot_id();
        if slot_id == 0 {
            return Err(Error::InvalidArgument);
        }
        let _ = libcluu::debug_print(&format!("xhci-core: enabled slot {}", slot_id));

        let dev_ctx_size = core::mem::size_of::<DeviceContext>();
        let dev_ctx_dma = pool.alloc(dev_ctx_size, 64)?;
        for i in 0..(dev_ctx_size / 4) {
            unsafe { core::ptr::write_volatile((dev_ctx_dma.virt + i * 4) as *mut u32, 0); }
        }
        self.dcbaa.set(slot_id, dev_ctx_dma.phys);

        let input_ctx_size = core::mem::size_of::<InputContext>();
        let input_ctx_dma = pool.alloc(input_ctx_size, 64)?;
        for i in 0..(input_ctx_size / 4) {
            unsafe { core::ptr::write_volatile((input_ctx_dma.virt + i * 4) as *mut u32, 0); }
        }

        let input_ctx = unsafe { &mut *(input_ctx_dma.virt as *mut InputContext) };
        input_ctx.icc.set_add_context(0);
        input_ctx.icc.set_add_context(1);
        input_ctx.slot.set_ctx_entries(1);
        input_ctx.slot.set_root_hub_port(port);
        input_ctx.slot.set_speed(speed);
        input_ctx.slot.set_route_string(0);

        let max_pkt = match speed {
            1 => 8u16,
            2 => 64u16,
            3 => 64u16,
            _ => 64u16,
        };
        input_ctx.ep0.set_ep_state(0);
        input_ctx.ep0.set_ep_type(4);
        input_ctx.ep0.set_max_packet_size(max_pkt);
        input_ctx.ep0.set_max_burst_size(0);
        input_ctx.ep0.set_interval(0);

        let ep0_ring = TrbRing::new(pool, 16)?;
        input_ctx.ep0.set_dequeue_ptr(ep0_ring.phys(), true);
        input_ctx.ep0.set_avg_trb_len(8);

        let addr_trb = Trb {
            param: input_ctx_dma.phys,
            status: 0,
            control: ((TrbType::AddressDevice as u32) << 10) | (1 << 5) | (1 << 8) | ((slot_id as u32) << 24),
        };
        let evt = self.send_command(addr_trb)?;
        let _ = libcluu::debug_print(&format!(
            "xhci-core: addressed slot {} code={}", slot_id, evt.completion_code()
        ));

        let ep1_in_ring = TrbRing::new(pool, 16)?;

        Ok(UsbDevice {
            slot_id,
            port,
            speed,
            dev_ctx_dma,
            input_ctx_dma,
            ep0_ring,
            ep1_in_ring,
        })
    }

    pub fn configure_interrupt_ep(
        &mut self,
        dev: &mut UsbDevice,
        ep_num: u8,
        max_pkt: u16,
        interval: u8,
    ) -> Result<()> {
        let input_ctx = unsafe { &mut *(dev.input_ctx_dma.virt as *mut InputContext) };
        for i in 0..(core::mem::size_of::<InputContext>() / 4) {
            unsafe { core::ptr::write_volatile((dev.input_ctx_dma.virt + i * 4) as *mut u32, 0); }
        }
        input_ctx.icc.set_add_context(0);
        input_ctx.icc.set_add_context(1);
        input_ctx.icc.set_add_context(ep_num as u8 + 1);
        input_ctx.slot.set_ctx_entries(ep_num as u8 + 2);
        input_ctx.slot.set_speed(dev.speed);
        input_ctx.slot.set_root_hub_port(dev.port);
        input_ctx.slot.set_route_string(0);

        input_ctx.ep0.set_ep_type(4);
        input_ctx.ep0.set_max_packet_size(match dev.speed { 1 => 8, _ => 64 });
        input_ctx.ep0.set_dequeue_ptr(dev.ep0_ring.phys(), true);
        input_ctx.ep0.set_avg_trb_len(8);

        let ep = &mut input_ctx.ep1_in;
        ep.set_ep_type(7);
        ep.set_max_packet_size(max_pkt);
        ep.set_max_burst_size(0);
        ep.set_interval(interval);
        ep.set_dequeue_ptr(dev.ep1_in_ring.phys(), true);
        ep.set_avg_trb_len(max_pkt as u16);

        let cfg_trb = Trb {
            param: dev.input_ctx_dma.phys,
            status: 0,
            control: ((TrbType::ConfigureEndpoint as u32) << 10) | (1 << 5) | (1 << 8) | ((dev.slot_id as u32) << 24),
        };
        let evt = self.send_command(cfg_trb)?;
        let _ = libcluu::debug_print(&format!(
            "xhci-core: configured EP{} code={}", ep_num, evt.completion_code()
        ));
        Ok(())
    }

    pub fn control_transfer(
        &mut self,
        dev: &mut UsbDevice,
        bm_request_type: u8,
        b_request: u8,
        w_value: u16,
        w_index: u16,
        w_length: u16,
        data: Option<&mut [u8]>,
        data_dma: Option<&cluu_dma_core::DmaRegion>,
    ) -> Result<()> {
        let setup = Trb {
            param: (bm_request_type as u64)
                | ((b_request as u64) << 8)
                | ((w_value as u64) << 16)
                | ((w_index as u64) << 32)
                | ((w_length as u64) << 48),
            status: 8,
            control: ((TrbType::SetupStage as u32) << 10) | (1 << 5) | (1 << 6) | (3 << 16),
        };
        dev.ep0_ring.enqueue(setup)?;

        let dir_in = (bm_request_type & 0x80) != 0;
        if let (Some(dma), Some(_data)) = (data_dma, data) {
            let data_trb = Trb {
                param: dma.phys,
                status: w_length as u32,
                control: ((TrbType::DataStage as u32) << 10) | (1 << 5) | (if dir_in { 1 << 16 } else { 0 }) | (1 << 2),
            };
            dev.ep0_ring.enqueue(data_trb)?;
        }

        let status_dir = if dir_in { 0 } else { 1 << 16 };
        let status = Trb {
            param: 0,
            status: 0,
            control: ((TrbType::StatusStage as u32) << 10) | (1 << 5) | (1 << 6) | status_dir | (1 << 2),
        };
        dev.ep0_ring.enqueue(status)?;

        self.regs.ring_doorbell(dev.slot_id, 1);

        for _ in 0..10000 {
            if let Some(evt) = self.event_ring.try_dequeue() {
                let erdp = self.event_ring.dequeue_phys();
                self.regs.set_erdp_lo(erdp as u32 | 0x8);
                self.regs.set_erdp_hi((erdp >> 32) as u32);
                if evt.trb_type() == TrbType::TransferEvent as u8 {
                    let code = evt.completion_code();
                    let _ = libcluu::debug_print(&format!(
                        "xhci-core: control transfer code={}", code
                    ));
                    if code == 1 { return Ok(()); }
                }
            }
        }
        Err(Error::Timeout)
    }

    pub fn set_idle(&mut self, dev: &mut UsbDevice, iface: u8) -> Result<()> {
        self.control_transfer(dev, 0x21, 0x0A, 0, iface as u16, 0, None, None)
    }

    pub fn set_protocol(&mut self, dev: &mut UsbDevice, iface: u8, protocol: u8) -> Result<()> {
        self.control_transfer(dev, 0x21, 0x0B, protocol as u16, iface as u16, 0, None, None)
    }

    pub fn enqueue_interrupt_in(
        &mut self,
        dev: &mut UsbDevice,
        data_dma: &cluu_dma_core::DmaRegion,
        len: usize,
    ) -> Result<()> {
        let trb = Trb {
            param: data_dma.phys,
            status: len as u32,
            control: ((TrbType::Normal as u32) << 10) | (1 << 5) | (1 << 2) | (2 << 16),
        };
        dev.ep1_in_ring.enqueue(trb)?;
        self.regs.ring_doorbell(dev.slot_id, 2);
        Ok(())
    }

    pub fn poll_interrupt(&mut self, dev: &mut UsbDevice) -> Option<u32> {
        if let Some(evt) = self.event_ring.try_dequeue() {
            let erdp = self.event_ring.dequeue_phys();
            self.regs.set_erdp_lo(erdp as u32 | 0x8);
            self.regs.set_erdp_hi((erdp >> 32) as u32);
            if evt.trb_type() == TrbType::TransferEvent as u8 {
                if evt.slot_id() == dev.slot_id {
                    return Some(evt.status & 0xFFFFFF);
                }
            }
        }
        None
    }

    pub fn enqueue_nop(&mut self) -> Result<u64> {
        let trb = Trb::new().with_type(TrbType::Noop).with_cycle(self.cmd_ring.cycle_bit);
        self.cmd_ring.enqueue(trb)
    }
}
