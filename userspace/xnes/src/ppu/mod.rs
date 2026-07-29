mod bus;
mod registers;
mod render;

use crate::mapper::Mapper;

pub(crate) const VISIBLE_SCANLINES: u16 = 240;
pub(crate) const VBLANK_START: u16 = 241;
pub(crate) const PRERENDER_SCANLINE: u16 = 261;
const TOTAL_SCANLINES: u16 = 262;
const CYCLES_PER_LINE: u16 = 341;
const HORIZONTAL_COPY_CYCLE: u16 = 257;
const STATUS_VBLANK: u8 = 0x80;
const STATUS_SPRITE0_HIT: u8 = 0x40;
const STATUS_SPRITE_OVERFLOW: u8 = 0x20;
const MASK_BG_ENABLE: u8 = 0x08;
const MASK_SPRITE_ENABLE: u8 = 0x10;
const MASK_RENDERING_BITS: u8 = MASK_BG_ENABLE | MASK_SPRITE_ENABLE;
const CTRL_NMI_ENABLE: u8 = 0x80;
const V_COARSE_X_MASK: u16 = 0x001F;
const V_COARSE_Y_MASK: u16 = 0x03E0;
const V_HORIZONTAL_MASK: u16 = 0x041F;
const V_VERTICAL_MASK: u16 = 0x7BE0;
const V_COARSE_Y_WRAP: u16 = 0x03C0;
const V_COARSE_Y_CLAMP: u16 = 0x03E0;
const V_FINE_Y_OVERFLOW: u16 = 0x7000;

pub struct Ppu {
    pub vram: [u8; 0x1000],
    pub palette: [u8; 0x20],
    pub oam: [u8; 0x100],

    pub ctrl: u8,
    pub mask: u8,
    pub status: u8,
    pub oam_addr: u8,

    pub v: u16, // current VRAM address (loopy-v)
    pub t: u16, // temporary VRAM address (loopy-t)
    pub fine_x: u8,
    pub w: u8, // write toggle (0 = first write, 1 = second)

    pub data_buffer: u8,
    pub last_bus_value: u8,
    pub tick_count: u64,
    pub last_bus_write_tick: u64,

    pub scanline: u16,
    pub cycle: u16,

    pub nmi_latched: bool,
    pub nmi_output: bool,
    pub nmi_from_vblank: bool,
    pub nmi_deferred_pending: bool,

    pub frame_complete: bool,
    pub frame: [u8; 61440],
    pub odd_frame: bool,

    sprite_count: u8,
    sprite_indices: [u8; 8],
    sprite_zero_hit_possible: bool,

    vbl_suppressed: bool,

    pub render_v: u16,
    pub render_fine_x: u8,

    prev_a12: bool,
    a12_low_cycles: u8,
}

impl Default for Ppu {
    fn default() -> Self {
        Self::new()
    }
}

impl Ppu {
    pub fn new() -> Self {
        Self {
            vram: [0; 0x1000],
            palette: [0; 0x20],
            oam: [0; 0x100],
            ctrl: 0,
            mask: 0,
            status: 0,
            oam_addr: 0,
            v: 0,
            t: 0,
            fine_x: 0,
            w: 0,
            data_buffer: 0,
            last_bus_value: 0,
            tick_count: 0,
            last_bus_write_tick: 0,
            scanline: 0,
            cycle: 0,
            nmi_latched: false,
            nmi_output: false,
            nmi_from_vblank: false,
            nmi_deferred_pending: false,
            frame_complete: false,
            frame: [0; 61440],
            odd_frame: true,
            sprite_count: 0,
            sprite_indices: [0; 8],
            sprite_zero_hit_possible: true,
            vbl_suppressed: false,
            render_v: 0,
            render_fine_x: 0,
            prev_a12: false,
            a12_low_cycles: 0,
        }
    }

    fn rendering_enabled(&self) -> bool {
        self.mask & MASK_RENDERING_BITS != 0
    }

    fn rendering_or_prerender(&self) -> bool {
        self.scanline < VISIBLE_SCANLINES || self.scanline == PRERENDER_SCANLINE
    }

    fn increment_coarse_x(&mut self) {
        if (self.v & V_COARSE_X_MASK) == 31 {
            self.v = (self.v & !V_COARSE_X_MASK) ^ 0x0400;
        } else {
            self.v += 1;
        }
    }

    fn increment_coarse_y(&mut self) {
        let fine_y = (self.v >> 12) & 7;
        if fine_y < 7 {
            self.v += 0x1000;
        } else {
            self.v &= !V_FINE_Y_OVERFLOW;
            let y = self.v & V_COARSE_Y_MASK;
            self.v = if y == V_COARSE_Y_WRAP {
                (self.v & !V_COARSE_Y_MASK) ^ 0x0800
            } else if y == V_COARSE_Y_CLAMP {
                (self.v & !V_COARSE_Y_MASK) ^ 0x0400
            } else {
                self.v + 0x0020
            };
        }
    }

    fn copy_horizontal(&mut self) {
        self.v = (self.v & !V_HORIZONTAL_MASK) | (self.t & V_HORIZONTAL_MASK);
    }

    fn copy_vertical(&mut self) {
        self.v = (self.v & !V_VERTICAL_MASK) | (self.t & V_VERTICAL_MASK);
    }

    #[inline(always)]
    pub fn update_nmi_edge(&mut self, from_vblank: bool) {
        let new_output = (self.status & STATUS_VBLANK != 0) && (self.ctrl & CTRL_NMI_ENABLE != 0);
        let was_active = self.nmi_output;
        let edge = !was_active && new_output;
        if edge {
            self.nmi_latched = true;
            if from_vblank {
                self.nmi_from_vblank = true;
            }
        }
        self.nmi_output = new_output;
    }

    fn advance_scanline(&mut self, mapper: &mut Mapper) {
        self.cycle = 0;
        let ns = self.scanline.wrapping_add(1);
        if ns >= TOTAL_SCANLINES {
            self.scanline = 0;
            self.odd_frame = !self.odd_frame;
            self.frame_complete = true;
        } else {
            self.scanline = ns;
        }
        mapper.notify_scanline(self.scanline);
        if self.scanline == PRERENDER_SCANLINE {
            self.sprite_zero_hit_possible = true;
            if self.rendering_enabled() {
                self.copy_vertical();
            }
            self.render_v = self.v;
            self.render_fine_x = self.fine_x;
        }
    }

    fn handle_odd_frame_skip(&mut self, mapper: &mut Mapper) -> bool {
        if self.scanline == PRERENDER_SCANLINE
            && self.cycle == CYCLES_PER_LINE - 1 // 340
            && self.odd_frame
            && self.rendering_enabled()
        {
            self.cycle = 0;
            self.scanline = 0;
            self.odd_frame = !self.odd_frame;
            self.frame_complete = true;
            self.sprite_zero_hit_possible = true;
            mapper.notify_scanline(0);
            return true;
        }
        false
    }

    fn cycle1_vblank_set(&mut self) {
        if !self.vbl_suppressed {
            self.status = (self.status | STATUS_VBLANK) & !STATUS_SPRITE_OVERFLOW;
            self.update_nmi_edge(true);
        }
        self.vbl_suppressed = false;
    }

    fn cycle1_prerender_clear(&mut self) {
        self.status &= !(STATUS_VBLANK | STATUS_SPRITE0_HIT | STATUS_SPRITE_OVERFLOW);
        self.update_nmi_edge(false);
    }

    fn cycles_1_to_256(&mut self, cy: u16, mapper: &mut Mapper) {
        if self.rendering_enabled() && (cy & 7) == 1 {
            self.fetch_bg_tile(mapper);
            self.increment_coarse_x();
        }

        if self.scanline < VISIBLE_SCANLINES {
            let pixel_x = cy - 1;
            self.render_pixel(pixel_x, self.scanline, mapper);
        }

        if cy == 256 && self.rendering_enabled() {
            self.increment_coarse_y();
        }
    }

    fn cycle257_copy_and_eval(&mut self, mapper: &mut Mapper) {
        if self.rendering_enabled() {
            self.copy_horizontal();
            self.render_v = (self.render_v & !V_HORIZONTAL_MASK) | (self.v & V_HORIZONTAL_MASK);
            self.render_fine_x = self.fine_x;
        }
        let next_sl = if self.scanline == PRERENDER_SCANLINE {
            0
        } else {
            self.scanline + 1
        };
        self.evaluate_sprites_for(next_sl, mapper);
    }

    fn notify_mapper_a12(&mut self, addr: u16, mapper: &mut Mapper) {
        let a12_new = (addr & 0x1000) != 0;
        if a12_new && !self.prev_a12 {
            if self.a12_low_cycles >= 3 {
                mapper.clock_scanline();
            }
            self.a12_low_cycles = 0;
        } else if !a12_new && self.prev_a12 {
            self.a12_low_cycles = 0;
        }
        self.prev_a12 = a12_new;
    }

    pub fn chr_read(&mut self, addr: u16, mapper: &mut Mapper) -> u8 {
        self.notify_mapper_a12(addr, mapper);
        mapper.ppu_read(addr)
    }

    pub fn tick(&mut self, mapper: &mut Mapper) {
        self.tick_count += 1;

        if self.tick_count == 1 {
            mapper.notify_scanline(0);
        }
        if !self.prev_a12 && self.a12_low_cycles < 255 {
            self.a12_low_cycles += 1;
        }

        let cy = self.cycle;
        let sl = self.scanline;

        if cy > CYCLES_PER_LINE - 2 {
            self.advance_scanline(mapper);
            return;
        }
        self.cycle += 1;

        if self.handle_odd_frame_skip(mapper) {
            return;
        }

        if cy == 1 {
            match sl {
                VBLANK_START => self.cycle1_vblank_set(),
                PRERENDER_SCANLINE => self.cycle1_prerender_clear(),
                _ => {}
            }
        }

        if self.rendering_or_prerender() {
            if (1..=256).contains(&cy) {
                self.cycles_1_to_256(cy, mapper);
            }
            // --- Phase 5: Cycle 257 — horizontal copy + sprite evaluation ---
            if cy == HORIZONTAL_COPY_CYCLE {
                self.cycle257_copy_and_eval(mapper);
            }
        }
    }

    pub fn tick_batch(&mut self, mut count: u16, mapper: &mut Mapper) {
        while count > 0 {
            self.tick(mapper);
            count -= 1;
        }
    }
}
