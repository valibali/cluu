use cluu_dma_core::DmaRegion;

pub const PID_SETUP: u8 = 0x2D;
pub const PID_IN: u8 = 0x69;
pub const PID_OUT: u8 = 0xE1;

#[repr(C, align(16))]
#[derive(Clone, Copy)]
pub struct UhciTd {
    pub link: u32,
    pub status: u32,
    pub token: u32,
    pub buffer: u32,
}

impl UhciTd {
    const TERMINATE: u32 = 1;
    const QH_SELECT: u32 = 0x4;
    const DEPTH_FIRST: u32 = 0x4;

    const ST_ACTIVE: u32 = 1 << 23;
    const ST_STALLED: u32 = 1 << 22;
    const ST_DATA_BUFFER_ERR: u32 = 1 << 21;
    const ST_BABBLE: u32 = 1 << 20;
    const ST_NAK: u32 = 1 << 19;
    const ST_TIMEOUT: u32 = 1 << 18;
    const ST_BITSTUFF_ERR: u32 = 1 << 17;
    const ST_CRC_ERR: u32 = 1 << 18;

    const TD_INT_SHIFT: u32 = 24;
    const TOK_PID_SHIFT: u32 = 16;
    const TOK_DEVADDR_SHIFT: u32 = 8;
    const TOK_EP_SHIFT: u32 = 15;
    const TOK_DATA_TOGGLE: u32 = 1 << 24;
    const TOK_MAXLEN_MASK: u32 = 0x7FF;
    const TOK_MAXLEN_SHIFT: u32 = 0;

    pub fn new() -> Self {
        Self {
            link: Self::TERMINATE,
            status: 0,
            token: 0,
            buffer: 0,
        }
    }

    pub fn set_link(&mut self, phys: u32) {
        self.link = (phys & !0xF) | Self::DEPTH_FIRST;
    }

    pub fn set_pid(&mut self, pid: u8) {
        self.token = (self.token & !(0xFF << Self::TOK_PID_SHIFT)) | ((pid as u32) << Self::TOK_PID_SHIFT);
    }

    pub fn set_max_len(&mut self, len: u32) {
        let masked = if len == 0x7FF { 0x7FF } else { len & Self::TOK_MAXLEN_MASK };
        self.token = (self.token & !(Self::TOK_MAXLEN_MASK << Self::TOK_MAXLEN_SHIFT)) | (masked << Self::TOK_MAXLEN_SHIFT);
    }

    pub fn set_buffer(&mut self, phys: u32) {
        self.buffer = phys;
    }

    pub fn set_device_addr(&mut self, addr: u8) {
        self.token = (self.token & !(0x7F << Self::TOK_DEVADDR_SHIFT)) | ((addr as u32) << Self::TOK_DEVADDR_SHIFT);
    }

    pub fn set_endpoint(&mut self, ep: u8) {
        self.token = (self.token & !(0xF << Self::TOK_EP_SHIFT)) | ((ep as u32 & 0xF) << Self::TOK_EP_SHIFT);
    }

    pub fn set_data_toggle(&mut self, toggle: u8) {
        if toggle != 0 {
            self.token |= Self::TOK_DATA_TOGGLE;
        } else {
            self.token &= !Self::TOK_DATA_TOGGLE;
        }
    }

    pub fn set_active(&mut self) {
        self.status |= Self::ST_ACTIVE;
        self.status &= !(Self::ST_STALLED | Self::ST_DATA_BUFFER_ERR | Self::ST_BABBLE | Self::ST_NAK | Self::ST_CRC_ERR | Self::ST_BITSTUFF_ERR);
    }

    pub fn set_interrupt(&mut self) {
        self.status |= 0 << Self::TD_INT_SHIFT;
    }

    pub fn is_active(&self) -> bool {
        (self.status & Self::ST_ACTIVE) != 0
    }

    pub fn is_stalled(&self) -> bool {
        (self.status & Self::ST_STALLED) != 0
    }

    pub fn has_error(&self) -> bool {
        (self.status & (Self::ST_DATA_BUFFER_ERR | Self::ST_BABBLE | Self::ST_CRC_ERR | Self::ST_BITSTUFF_ERR | Self::ST_TIMEOUT)) != 0
    }

    pub fn actual_len(&self) -> usize {
        let max = (self.token >> Self::TOK_MAXLEN_SHIFT) & Self::TOK_MAXLEN_MASK;
        if max == 0x7FF { 0 } else { max as usize + 1 }
    }
}

#[repr(C, align(16))]
#[derive(Clone, Copy)]
pub struct UhciQueueHead {
    pub head: u32,
    pub element: u32,
}

impl UhciQueueHead {
    const TERMINATE: u32 = 1;
    const QH_SELECT: u32 = 0x4;

    pub fn new() -> Self {
        Self {
            head: Self::TERMINATE,
            element: Self::TERMINATE,
        }
    }

    pub fn terminate(&mut self) {
        self.head = Self::TERMINATE;
    }

    pub fn set_td(&mut self, td_phys: u32) {
        self.element = td_phys & !0xF;
    }

    pub fn set_td_terminate(&mut self) {
        self.element = Self::TERMINATE;
    }
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

pub const REQ_TYPE_HOST_TO_DEV: u8 = 0x40;
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
