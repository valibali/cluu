use cluu_dma_core::DmaRegion;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct QtD {
    pub next_td: u32,
    pub alt_next_td: u32,
    pub token: u32,
    pub buffers: [u32; 5],
}

impl QtD {
    pub const TERMINATE: u32 = 1;

    pub fn new() -> Self {
        Self {
            next_td: Self::TERMINATE,
            alt_next_td: Self::TERMINATE,
            token: 0,
            buffers: [0; 5],
        }
    }

    pub fn set_pid(&mut self, pid: u8) {
        let val: u32 = match pid {
            PID_OUT => 0,
            PID_IN => 1,
            PID_SETUP => 2,
            _ => 0,
        };
        self.token = (self.token & !0x0300) | (val << 8);
    }

    pub fn set_total_bytes(&mut self, n: u32) {
        self.token = (self.token & !0x7FFF0000) | ((n & 0x7FFF) << 16);
    }

    pub fn set_ioc(&mut self) {
        self.token |= 1 << 15;
    }

    pub fn set_data_toggle(&mut self, toggle: u8) {
        self.token = (self.token & !(1 << 31)) | ((toggle as u32) << 31);
    }

    pub fn set_status(&mut self, status: u32) {
        self.token = (self.token & !0xFF) | (status & 0xFF);
    }

    pub fn set_active(&mut self) {
        self.token |= 0x80;
    }

    pub fn set_cerr(&mut self, n: u8) {
        self.token = (self.token & !0x0C00) | ((n as u32 & 0x3) << 10);
    }

    pub fn set_halt(&mut self) {
        self.token |= 0x40;
    }

    pub fn clear_active(&mut self) {
        self.token &= !0x80;
    }

    pub fn is_active(&self) -> bool {
        (self.token & 0x80) != 0
    }

    pub fn is_halted(&self) -> bool {
        (self.token & 0x40) != 0
    }

    pub fn is_data_buffer_error(&self) -> bool {
        (self.token & 0x20) != 0
    }

    pub fn is_babble(&self) -> bool {
        (self.token & 0x10) != 0
    }

    pub fn is_transaction_error(&self) -> bool {
        (self.token & 0x08) != 0
    }

    pub fn is_missed_microframe(&self) -> bool {
        (self.token & 0x04) != 0
    }

    pub fn is_split_transaction_state(&self) -> bool {
        (self.token & 0x02) != 0
    }

    pub fn is_periodic_status(&self) -> bool {
        (self.token & 0x01) != 0
    }

    pub fn remaining_bytes(&self) -> u32 {
        (self.token >> 16) & 0x7FFF
    }

    pub fn data_toggle(&self) -> u8 {
        ((self.token >> 31) & 1) as u8
    }

    pub fn set_buffer(&mut self, phys: u32) {
        self.buffers[0] = phys;
        let page = phys & 0xFFFF_F000;
        self.buffers[1] = page + 0x1000;
        self.buffers[2] = page + 0x2000;
        self.buffers[3] = page + 0x3000;
        self.buffers[4] = page + 0x4000;
    }

    pub fn set_next(&mut self, phys: u32) {
        self.next_td = phys & !0x1F;
    }

    pub fn terminate_next(&mut self) {
        self.next_td = Self::TERMINATE;
    }
}

pub const PID_SETUP: u8 = 0x2D;
pub const PID_IN: u8 = 0x69;
pub const PID_OUT: u8 = 0xE1;

#[repr(C, align(32))]
#[derive(Clone, Copy)]
pub struct QueueHead {
    pub next_qh: u32,
    pub charac: u32,
    pub cap: u32,
    pub cur_td: u32,
    pub overlay: QtD,
}

impl QueueHead {
    pub const TERMINATE: u32 = 1;

    pub fn new() -> Self {
        Self {
            next_qh: Self::TERMINATE,
            charac: 0,
            cap: 0,
            cur_td: 0,
            overlay: QtD::new(),
        }
    }

    pub fn set_next_qh(&mut self, phys: u32) {
        self.next_qh = (phys & !0x1F) | 0x2;
    }

    pub fn terminate_next(&mut self) {
        self.next_qh = Self::TERMINATE;
    }

    pub fn set_h_addr(&mut self, addr: u8) {
        self.charac = (self.charac & !0x7F) | (addr as u32 & 0x7F);
    }

    pub fn set_ep_number(&mut self, ep: u8) {
        self.charac = (self.charac & !(0xF << 8)) | ((ep as u32 & 0xF) << 8);
    }

    pub fn set_eps(&mut self, speed: u8) {
        self.charac = (self.charac & !(0x3 << 12)) | ((speed as u32 & 0x3) << 12);
    }

    pub fn set_max_packet_len(&mut self, len: u16) {
        self.charac = (self.charac & !(0x7FF << 16)) | ((len as u32 & 0x7FF) << 16);
    }

    pub fn set_control_endpoint(&mut self) {
        self.charac |= 1 << 27;
    }

    pub fn set_head_of_reclamation(&mut self) {
        self.charac |= 1 << 15;
    }

    pub fn clear_head_of_reclamation(&mut self) {
        self.charac &= !(1 << 15);
    }

    pub fn set_dtc(&mut self) {
        self.cap |= 1 << 14;
    }

    pub fn set_nak_reload(&mut self, n: u8) {
        self.cap = (self.cap & !(0xF << 8)) | ((n as u32 & 0xF) << 8);
    }

    pub fn set_qtd_ptr(&mut self, phys: u32) {
        self.overlay.set_next(phys);
    }

    pub fn terminate_qtd(&mut self) {
        self.overlay.terminate_next();
    }

    pub fn is_halted(&self) -> bool {
        self.overlay.is_halted()
    }

    pub fn is_active(&self) -> bool {
        self.overlay.is_active()
    }
}

pub struct EhciStructures {
    pub async_qh: DmaRegion,
    pub periodic_frame_list: DmaRegion,
    pub intr_qh: DmaRegion,
}

pub fn setup_token_packet(req_type: u8, req: u8, value: u16, index: u16, length: u16) -> [u8; 8] {
    [
        req_type,
        req,
        (value & 0xFF) as u8,
        ((value >> 8) & 0xFF) as u8,
        (index & 0xFF) as u8,
        ((index >> 8) & 0xFF) as u8,
        (length & 0xFF) as u8,
        ((length >> 8) & 0xFF) as u8,
    ]
}

pub const REQ_TYPE_HOST_TO_DEV: u8 = 0x00;
pub const REQ_TYPE_DEV_TO_HOST: u8 = 0x80;
pub const REQ_TYPE_CLASS: u8 = 0x20;
pub const REQ_TYPE_RECIPIENT_INTERFACE: u8 = 0x01;

pub const REQ_SET_ADDRESS: u8 = 0x05;
pub const REQ_GET_DESCRIPTOR: u8 = 0x06;
pub const REQ_SET_CONFIGURATION: u8 = 0x09;

pub const DESC_DEVICE: u8 = 0x01;
pub const DESC_CONFIGURATION: u8 = 0x02;

pub const REQ_SET_IDLE: u8 = 0x0A;
pub const REQ_SET_PROTOCOL: u8 = 0x0B;
