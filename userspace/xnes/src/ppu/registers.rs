use crate::mapper::Mapper;

use super::{Ppu, VBLANK_START};

impl Ppu {
    pub fn read_status(&mut self) -> u8 {
        let s = self.status;
        self.status &= !0x80;
        self.w = 0;
        let result = (s & 0xE0) | (self.get_open_bus() & 0x1F);
        self.last_bus_value = result;
        if self.scanline == VBLANK_START && self.cycle == 1 && (s & 0x80) == 0 {
            self.vbl_suppressed = true;
        }
        self.update_nmi_edge(false);
        result
    }

    pub fn read_data(&mut self, mapper: &mut Mapper) -> u8 {
        let addr = self.v & 0x3FFF;
        let val = if addr < 0x2000 {
            self.chr_read(addr, mapper)
        } else {
            self.ppu_read_nt(addr, mapper)
        };
        let result = if addr < 0x3F00 {
            self.data_buffer
        } else {
            (self.get_open_bus() & 0xC0) | (val & 0x3F)
        };
        if addr < 0x3F00 {
            self.data_buffer = val;
        } else {
            self.data_buffer = self.ppu_read_nt(addr & 0x2FFF, mapper);
        }
        self.set_last_bus_value(result);
        self.v = self
            .v
            .wrapping_add(if self.ctrl & 0x04 != 0 { 32 } else { 1 });
        result
    }

    pub fn write_ctrl(&mut self, val: u8) {
        self.set_last_bus_value(val);
        self.ctrl = val;
        self.t = (self.t & 0xF3FF) | ((val as u16 & 3) << 10);
        self.update_nmi_edge(false);
        if val & 0x80 == 0 {
            self.nmi_latched = false;
        }
    }

    pub fn write_mask(&mut self, val: u8) {
        self.set_last_bus_value(val);
        self.mask = val;
    }

    pub fn write_oam_addr(&mut self, val: u8) {
        self.set_last_bus_value(val);
        self.oam_addr = val;
    }

    pub fn write_oam_data(&mut self, val: u8) {
        self.set_last_bus_value(val);
        self.oam[self.oam_addr as usize] = val;
        self.oam_addr = self.oam_addr.wrapping_add(1);
    }

    pub fn read_oam_data(&mut self) -> u8 {
        let mut val = self.oam[self.oam_addr as usize];
        // Bits 2-4 of sprite attribute bytes are always 0 when read via $2004
        if self.oam_addr & 0x03 == 2 {
            val &= 0xE3;
        }
        self.set_last_bus_value(val);
        val
    }

    pub fn write_scroll(&mut self, val: u8) {
        self.set_last_bus_value(val);
        if self.w == 0 {
            self.t = (self.t & 0xFFE0) | ((val >> 3) as u16);
            self.fine_x = val & 7;
            self.w = 1;
        } else {
            self.t =
                (self.t & 0xFC1F) | (((val as u16) & 0x07) << 12) | (((val as u16) & 0xF8) << 2);
            self.w = 0;
        }
    }

    pub fn write_addr(&mut self, val: u8) {
        self.set_last_bus_value(val);
        if self.w == 0 {
            self.t = ((self.t & 0x00FF) | ((val as u16) << 8)) & 0x3FFF;
            self.w = 1;
        } else {
            self.t = (self.t & 0xFF00) | val as u16;
            self.v = self.t;
            self.w = 0;
            // $2006 second write sets v = t immediately.
            // Sync render_v so on-the-fly renderer sees the new scroll.
            // fine_x is unchanged (only $2005 first write sets fine_x).
            self.render_v = self.v;
        }
    }

    pub fn write_data(&mut self, val: u8, mapper: &mut Mapper) {
        self.set_last_bus_value(val);
        let addr = self.v & 0x3FFF;
        if addr < 0x2000 {
            mapper.ppu_write(addr, val);
        } else {
            self.ppu_write_nt(addr, val, mapper);
        }
        self.v = self
            .v
            .wrapping_add(if self.ctrl & 0x04 != 0 { 32 } else { 1 });
    }
}
