use cluu_dma_core::{DmaPool, DmaRegion};
use libcluu::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TrbType {
    Normal = 1,
    SetupStage = 2,
    DataStage = 3,
    StatusStage = 4,
    Link = 6,
    EnableSlot = 9,
    AddressDevice = 11,
    ConfigureEndpoint = 12,
    Noop = 23,
    TransferEvent = 32,
    CommandCompletion = 33,
    PortStatusChange = 34,
}

#[derive(Debug, Clone, Copy)]
#[repr(C, align(16))]
pub struct Trb {
    pub param: u64,
    pub status: u32,
    pub control: u32,
}

impl Trb {
    pub const fn new() -> Self {
        Self { param: 0, status: 0, control: 0 }
    }

    pub fn with_type(self, ty: TrbType) -> Self {
        Self { control: (self.control & !(0x3F << 10)) | ((ty as u32) << 10), ..self }
    }

    pub fn with_cycle(self, c: bool) -> Self {
        let bit = if c { 1 } else { 0 };
        Self { control: (self.control & !1) | bit, ..self }
    }

    pub fn trb_type(&self) -> u8 {
        ((self.control >> 10) & 0x3F) as u8
    }

    pub fn cycle_bit(&self) -> bool {
        (self.control & 1) != 0
    }

    pub fn completion_code(&self) -> u8 {
        ((self.status >> 24) & 0xFF) as u8
    }

    pub fn slot_id(&self) -> u8 {
        ((self.control >> 24) & 0xFF) as u8
    }
}

pub struct TrbRing {
    pub dma: DmaRegion,
    pub count: usize,
    pub enqueue_idx: usize,
    pub cycle_bit: bool,
}

impl TrbRing {
    pub fn new(pool: &mut DmaPool, count: usize) -> Result<Self> {
        let size = count * core::mem::size_of::<Trb>();
        let dma = pool.alloc(size, 16)?;
        for i in 0..count {
            let ptr = (dma.virt + i * core::mem::size_of::<Trb>()) as *mut Trb;
            unsafe { core::ptr::write_volatile(ptr, Trb::new()); }
        }
        Ok(Self { dma, count, enqueue_idx: 0, cycle_bit: true })
    }

    pub fn phys(&self) -> u64 {
        self.dma.phys
    }

    pub fn enqueue_phys(&self) -> u64 {
        self.dma.phys + (self.enqueue_idx * core::mem::size_of::<Trb>()) as u64
    }

    pub fn enqueue(&mut self, trb: Trb) -> Result<u64> {
        if self.enqueue_idx >= self.count {
            return Err(Error::Overflow);
        }
        let trb = trb.with_cycle(self.cycle_bit);
        let ptr = (self.dma.virt + self.enqueue_idx * core::mem::size_of::<Trb>()) as *mut Trb;
        unsafe { core::ptr::write_volatile(ptr, trb); }
        let phys = self.dma.phys + (self.enqueue_idx * core::mem::size_of::<Trb>()) as u64;
        self.enqueue_idx += 1;
        if self.enqueue_idx == self.count {
            self.enqueue_idx = 0;
            self.cycle_bit = !self.cycle_bit;
        }
        Ok(phys)
    }
}

#[repr(C, align(16))]
pub struct ErstEntry {
    pub base: u64,
    pub size: u16,
    pub _rsvd: u16,
    pub _rsvd2: u32,
}

pub struct EventRing {
    pub dma: DmaRegion,
    pub erst: DmaRegion,
    pub count: usize,
    pub dequeue_idx: usize,
    pub cycle_bit: bool,
}

impl EventRing {
    pub fn new(pool: &mut DmaPool, count: usize) -> Result<Self> {
        let ring_size = count * core::mem::size_of::<Trb>();
        let dma = pool.alloc(ring_size, 16)?;
        for i in 0..count {
            let ptr = (dma.virt + i * core::mem::size_of::<Trb>()) as *mut Trb;
            unsafe { core::ptr::write_volatile(ptr, Trb::new()); }
        }

        let erst = pool.alloc(core::mem::size_of::<ErstEntry>(), 16)?;
        let entry = ErstEntry {
            base: dma.phys,
            size: count as u16,
            _rsvd: 0,
            _rsvd2: 0,
        };
        unsafe { core::ptr::write_volatile(erst.virt as *mut ErstEntry, entry); }

        Ok(Self { dma, erst, count, dequeue_idx: 0, cycle_bit: true })
    }

    pub fn ring_phys(&self) -> u64 {
        self.dma.phys
    }

    pub fn erst_phys(&self) -> u64 {
        self.erst.phys
    }

    pub fn erst_size(&self) -> u16 {
        self.count as u16
    }

    pub fn dequeue_phys(&self) -> u64 {
        self.dma.phys + (self.dequeue_idx * core::mem::size_of::<Trb>()) as u64
    }

    pub fn try_dequeue(&mut self) -> Option<Trb> {
        let ptr = (self.dma.virt + self.dequeue_idx * core::mem::size_of::<Trb>()) as *const Trb;
        let trb = unsafe { core::ptr::read_volatile(ptr) };
        if trb.cycle_bit() != self.cycle_bit {
            return None;
        }
        self.dequeue_idx += 1;
        if self.dequeue_idx == self.count {
            self.dequeue_idx = 0;
            self.cycle_bit = !self.cycle_bit;
        }
        Some(trb)
    }
}
