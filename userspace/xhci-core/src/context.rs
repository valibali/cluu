//! xHCI context structures: Slot, Endpoint, Input Context, Device Context.

#[repr(C, align(64))]
pub struct SlotContext {
    pub dw0: u32,
    pub dw1: u32,
    pub dw2: u32,
    pub dw3: u32,
    pub _rsvd: [u32; 4],
}

impl SlotContext {
    pub const fn new() -> Self {
        Self { dw0: 0, dw1: 0, dw2: 0, dw3: 0, _rsvd: [0; 4] }
    }

    pub fn set_route_string(&mut self, route: u32) {
        self.dw0 = (self.dw0 & !0xFFFFF) | (route & 0xFFFFF);
    }

    pub fn set_speed(&mut self, speed: u8) {
        self.dw0 = (self.dw0 & !(0x7 << 20)) | ((speed as u32 & 0x7) << 20);
    }

    pub fn set_ctx_entries(&mut self, n: u8) {
        self.dw0 = (self.dw0 & !(0x1F << 27)) | ((n as u32 & 0x1F) << 27);
    }

    pub fn set_root_hub_port(&mut self, port: u8) {
        self.dw1 = (self.dw1 & !0xFF) | (port as u32 & 0xFF);
    }

    pub fn set_max_exit_latency(&mut self, latency: u16) {
        self.dw2 = (self.dw2 & !0xFFFF) | (latency as u32 & 0xFFFF);
    }
}

#[repr(C, align(64))]
pub struct EndpointContext {
    pub dw0: u32,
    pub dw1: u32,
    pub dw2: u32,
    pub trb_dequeue_lo: u32,
    pub trb_dequeue_hi: u32,
    pub _rsvd: [u32; 3],
}

impl EndpointContext {
    pub const fn new() -> Self {
        Self {
            dw0: 0, dw1: 0, dw2: 0,
            trb_dequeue_lo: 0, trb_dequeue_hi: 0,
            _rsvd: [0; 3],
        }
    }

    pub fn set_ep_state(&mut self, state: u8) {
        self.dw0 = (self.dw0 & !0x7) | (state as u32 & 0x7);
    }

    pub fn set_ep_type(&mut self, ty: u8) {
        self.dw1 = (self.dw1 & !(0x7 << 3)) | ((ty as u32 & 0x7) << 3);
    }

    pub fn set_max_packet_size(&mut self, size: u16) {
        self.dw1 = (self.dw1 & !0xFFFF) | (size as u32 & 0xFFFF);
    }

    pub fn set_max_burst_size(&mut self, b: u8) {
        self.dw1 = (self.dw1 & !(0xFF << 16)) | ((b as u32 & 0xFF) << 16);
    }

    pub fn set_interval(&mut self, interval: u8) {
        self.dw0 = (self.dw0 & !(0xFF << 16)) | ((interval as u32 & 0xFF) << 16);
    }

    pub fn set_dequeue_ptr(&mut self, phys: u64, dcs: bool) {
        self.trb_dequeue_lo = (phys as u32) & 0xFFFF_FFF0 | if dcs { 1 } else { 0 };
        self.trb_dequeue_hi = (phys >> 32) as u32;
    }

    pub fn set_avg_trb_len(&mut self, len: u16) {
        self.dw2 = (self.dw2 & !0xFFFF) | (len as u32 & 0xFFFF);
    }
}

#[repr(C, align(64))]
pub struct InputControlContext {
    pub dw0: u32,
    pub dw1: u32,
    pub _rsvd: [u32; 6],
}

impl InputControlContext {
    pub const fn new() -> Self {
        Self { dw0: 0, dw1: 0, _rsvd: [0; 6] }
    }

    pub fn set_add_context(&mut self, bit: u8) {
        self.dw0 |= 1u32 << bit;
    }

    pub fn set_drop_context(&mut self, bit: u8) {
        self.dw1 |= 1u32 << bit;
    }
}

#[repr(C, align(64))]
pub struct InputContext {
    pub icc: InputControlContext,
    pub slot: SlotContext,
    pub ep0: EndpointContext,
    pub ep1_in: EndpointContext,
}

impl InputContext {
    pub const fn new() -> Self {
        Self {
            icc: InputControlContext::new(),
            slot: SlotContext::new(),
            ep0: EndpointContext::new(),
            ep1_in: EndpointContext::new(),
        }
    }
}

#[repr(C, align(64))]
pub struct DeviceContext {
    pub slot: SlotContext,
    pub ep0: EndpointContext,
    pub ep1_in: EndpointContext,
    pub _padding: [u32; 20 * 3],
}

impl DeviceContext {
    pub const fn new() -> Self {
        Self {
            slot: SlotContext::new(),
            ep0: EndpointContext::new(),
            ep1_in: EndpointContext::new(),
            _padding: [0; 60],
        }
    }
}

#[repr(C, align(64))]
pub struct DcbaaEntry {
    pub phys: u64,
}
