use crate::mapper::Mapper;

use super::Ppu;

impl Ppu {
    // ---- On-the-fly background pixel computation ----
    fn compute_bg_pixel(&mut self, x: u16, y: u16, mapper: &mut Mapper) -> (u8, u8) {
        // Use render_v (snapshot of v at last sync point) instead of t.
        let coarse_x = self.render_v & 0x001F;
        let coarse_y = (self.render_v >> 5) & 0x001F;
        let fine_y = (self.render_v >> 12) & 0x0007;
        let nt = (self.render_v >> 10) & 0x0003;

        let world_x = (coarse_x << 3) + self.render_fine_x as u16 + x;
        let world_y = (coarse_y << 3) + fine_y + y;

        let mut actual_nt = nt;
        if (world_x >> 8) & 1 != 0 {
            actual_nt ^= 1;
        }
        let coarse_y_calc = world_y >> 3;
        if (coarse_y_calc / 30) & 1 != 0 {
            actual_nt ^= 2;
        }

        let tile_x = (world_x >> 3) & 31;
        let tile_y = (coarse_y_calc % 30) & 31;
        let pixel_x = world_x & 7;
        let pixel_y = world_y & 7;

        let nt_base = 0x2000 | (actual_nt << 10);
        let nt_addr = nt_base | (tile_y << 5) | tile_x;
        let tile_index = self.ppu_read_nt(nt_addr, mapper);

        // MMC5 ExRAM mode 1: extended attributes for background tiles
        // The ExRAM byte at the tile's nametable address provides:
        //   bits 0-5: 4KB CHR page
        //   bits 6-7: palette (replaces attribute table)
        let ex_ram_mode = mapper.get_ex_ram_mode();
        if ex_ram_mode == 1 {
            self.compute_bg_pixel_exram_mode1(nt_addr, tile_index, pixel_x, pixel_y, mapper)
        } else {
            self.compute_bg_pixel_standard(
                tile_index, pixel_x, pixel_y, nt_base, tile_x, tile_y, mapper,
            )
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn compute_bg_pixel_exram_mode1(
        &self,
        nt_addr: u16,
        tile_index: u8,
        pixel_x: u16,
        pixel_y: u16,
        mapper: &mut Mapper,
    ) -> (u8, u8) {
        let exram_offset = nt_addr & 0x03FF;
        let exram_byte = mapper.read_ex_ram_byte(exram_offset);
        mapper.set_extended_chr_bank(exram_byte);
        mapper.set_chr_fetch_bg();

        let tile_addr = ((tile_index as u16) << 4) | pixel_y;
        let low = mapper.ppu_read(tile_addr);
        let high = mapper.ppu_read(tile_addr | 0x0008);

        let shift = 7 - pixel_x;
        let pixel = ((high >> shift) & 1) << 1 | ((low >> shift) & 1);

        if pixel == 0 {
            return (0, self.palette[0]);
        }
        // ExRAM mode 1: palette comes from ExRAM bits 6-7, not attribute table
        let pal_group = (exram_byte >> 6) & 3;
        (
            pixel,
            self.palette[((pal_group as usize) << 2) | pixel as usize],
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn compute_bg_pixel_standard(
        &mut self,
        tile_index: u8,
        pixel_x: u16,
        pixel_y: u16,
        nt_base: u16,
        tile_x: u16,
        tile_y: u16,
        mapper: &mut Mapper,
    ) -> (u8, u8) {
        mapper.set_chr_fetch_bg();
        let bg_table = if self.ctrl & 0x10 != 0 {
            0x1000
        } else {
            0x0000
        };
        let tile_addr = bg_table | ((tile_index as u16) << 4) | pixel_y;
        let low = mapper.ppu_read(tile_addr);
        let high = mapper.ppu_read(tile_addr | 0x0008);

        let shift = 7 - pixel_x;
        let pixel = ((high >> shift) & 1) << 1 | ((low >> shift) & 1);

        if pixel == 0 {
            (0, self.palette[0])
        } else {
            let attr_addr = nt_base | 0x03C0 | ((tile_y >> 2) << 3) | (tile_x >> 2);
            let attr = self.ppu_read_nt(attr_addr, mapper);
            let s = ((tile_x >> 1) & 1) << 1 | ((tile_y >> 1) & 1) << 2;
            let pal_group = (attr >> s) & 3;
            (
                pixel,
                self.palette[((pal_group as usize) << 2) | pixel as usize],
            )
        }
    }

    fn render_sprite_pixel(
        &self,
        x: u16,
        bg_pixel: u8,
        bg_color: u8,
        mapper: &mut Mapper,
    ) -> (u8, u8) {
        mapper.set_chr_fetch_sprite();
        let use_16 = self.ctrl & 0x20 != 0;
        let sprite_h = if use_16 { 16 } else { 8 };
        let sl = self.scanline;
        for si in 0..self.sprite_count {
            let idx = self.sprite_indices[si as usize] as usize;
            let oi = idx * 4;
            let sy = self.oam[oi] as u16;
            let tile = self.oam[oi + 1] as u16;
            let attr = self.oam[oi + 2];
            let sx = self.oam[oi + 3] as u16;
            if x < sx || x >= sx + 8 {
                continue;
            }
            let palette_bits = attr & 0x03;
            let behind = attr & 0x20 != 0;
            let flip_x = attr & 0x40 != 0;
            let flip_y = attr & 0x80 != 0;
            let sy_off = sl.wrapping_sub(sy) as u8;
            let pixel_y = if flip_y {
                (sprite_h as u8).wrapping_sub(1).wrapping_sub(sy_off)
            } else {
                sy_off
            };
            let pixel_x = if flip_x {
                7u8.wrapping_sub(x.wrapping_sub(sx) as u8)
            } else {
                x.wrapping_sub(sx) as u8
            };
            let tile_addr = if use_16 {
                let bank = if tile & 1 != 0 { 0x1000 } else { 0x0000 };
                let base_tile = tile & 0xFE;
                let bottom = u16::from(pixel_y >= 8);
                let fine_y = pixel_y & 7;
                bank | ((base_tile + bottom) << 4) | fine_y as u16
            } else {
                let bank = if self.ctrl & 0x08 != 0 {
                    0x1000
                } else {
                    0x0000
                };
                bank | (tile << 4) | pixel_y as u16
            };
            let low = mapper.ppu_read(tile_addr);
            let high = mapper.ppu_read(tile_addr | 8);
            let shift = 7 - pixel_x;
            let pixel = ((high >> shift) & 1) << 1 | ((low >> shift) & 1);
            if pixel == 0 {
                continue;
            }
            let sp0_here = if idx == 0 { pixel } else { 0 };
            if behind && bg_pixel != 0 {
                return (bg_color, sp0_here);
            }
            return (
                self.palette[0x10 | ((palette_bits as usize) << 2) | pixel as usize],
                sp0_here,
            );
        }
        (bg_color, 0)
    }

    pub(super) fn render_pixel(&mut self, x: u16, y: u16, mapper: &mut Mapper) {
        let bg_enabled = self.mask & 0x08 != 0;
        let show_left = self.mask & 0x02 != 0;
        let (bg_pixel, bg_colour) = if !bg_enabled || (!show_left && x < 8) {
            (0, self.palette[0])
        } else {
            self.compute_bg_pixel(x, y, mapper)
        };
        let colour = if self.mask & 0x10 != 0 && (self.mask & 0x04 != 0 || x >= 8) {
            let (sprite_colour, sp0_pixel) =
                self.render_sprite_pixel(x, bg_pixel, bg_colour, mapper);
            if sp0_pixel != 0 && bg_pixel != 0 && x != 255 && self.sprite_zero_hit_possible {
                self.status |= 0x40;
                self.sprite_zero_hit_possible = false;
            }
            sprite_colour
        } else {
            bg_colour
        };
        self.frame[(y as usize) * 256 + (x as usize)] = colour;
    }

    pub(super) fn evaluate_sprites_for(&mut self, sl: u16, mapper: &mut Mapper) {
        self.sprite_count = 0;
        if self.mask & 0x10 == 0 {
            return;
        }
        let sprite_h = if self.ctrl & 0x20 != 0 { 16 } else { 8 };
        let use_16 = self.ctrl & 0x20 != 0;
        for i in (0..0x100).step_by(4) {
            let sy = self.oam[i] as u16;
            if sy <= sl && sl < sy + sprite_h {
                if self.sprite_count < 8 {
                    self.sprite_indices[self.sprite_count as usize] = (i >> 2) as u8;
                    self.sprite_count += 1;
                } else {
                    self.status |= 0x20;
                }
            }
        }

        mapper.set_chr_fetch_sprite();
        for si in 0..8 {
            let tile_addr = if si < self.sprite_count {
                let idx = self.sprite_indices[si as usize] as usize;
                let oi = idx * 4;
                let tile = self.oam[oi + 1] as u16;
                let attr = self.oam[oi + 2];
                let flip_y = attr & 0x80 != 0;
                let pixel_y = if flip_y {
                    (sprite_h as u8).wrapping_sub(1)
                } else {
                    0
                };
                if use_16 {
                    let bank = if tile & 1 != 0 { 0x1000 } else { 0x0000 };
                    let base_tile = tile & 0xFE;
                    bank | (base_tile << 4) | pixel_y as u16
                } else {
                    let bank = if self.ctrl & 0x08 != 0 {
                        0x1000
                    } else {
                        0x0000
                    };
                    bank | (tile << 4) | pixel_y as u16
                }
            } else {
                // Dummy sprite fetch: tile $FF from sprite pattern table
                (((self.ctrl & 0x08 != 0) as u16) * 0x1000) | (0xFFu16 << 4)
            };
            self.chr_read(tile_addr, mapper);
            self.chr_read(tile_addr | 8, mapper);
        }
    }

    /// Fetch next background tile pattern from VRAM.
    pub(super) fn fetch_bg_tile(&mut self, mapper: &mut Mapper) {
        mapper.set_chr_fetch_bg();

        let tile_x = self.v & 0x001F;
        let tile_y = (self.v >> 5) & 0x001F;
        let fine_y = (self.v >> 12) & 0x0007;
        let nt = (self.v >> 10) & 0x0003;
        self.fetch_tile_pattern(tile_x, tile_y, fine_y, nt, mapper);
    }

    #[allow(clippy::too_many_arguments)]
    fn fetch_tile_pattern(
        &mut self,
        tile_x: u16,
        tile_y: u16,
        fine_y: u16,
        nt: u16,
        mapper: &mut Mapper,
    ) -> (u8, u8, u8, u8) {
        let nt_base = 0x2000 | (nt << 10);
        let nt_addr = nt_base | (tile_y << 5) | tile_x;
        // Real PPU bus: NT read puts A12=1 on the address bus.
        // This matters for MMC3 A12 edge detection (it generates the rising edge
        // that the MMC3 counts to clock its scanline counter).
        self.notify_mapper_a12(nt_addr, mapper);
        let nt_byte = self.ppu_read_nt(nt_addr, mapper);

        // MMC5 ExRAM mode 1: set extended CHR bank for background tile fetches
        let ex_ram_mode = mapper.get_ex_ram_mode();
        if ex_ram_mode == 1 {
            let exram_offset = nt_addr & 0x03FF;
            let exram_byte = mapper.read_ex_ram_byte(exram_offset);
            mapper.set_extended_chr_bank(exram_byte);
        }

        let attr_addr = nt_base | 0x03C0 | ((tile_y >> 2) << 3) | (tile_x >> 2);
        // Attribute read: also A12=1 (same as NT, no new rising edge).
        // Still notify to let the MMC3 glitch filter track A12 properly.
        self.notify_mapper_a12(attr_addr, mapper);
        let attr = self.ppu_read_nt(attr_addr, mapper);
        let attr_shift = ((tile_x & 2) >> 1) | ((tile_y & 2) << 1);
        let attr_bits = (attr >> attr_shift) & 3;
        let bg_table = if self.ctrl & 0x10 != 0 {
            0x1000
        } else {
            0x0000
        };
        let tile_addr = bg_table | ((nt_byte as u16) << 4) | fine_y;
        let pat_low = self.chr_read(tile_addr, mapper);
        let pat_high = self.chr_read(tile_addr | 0x0008, mapper);
        (nt_byte, attr_bits, pat_low, pat_high)
    }
}
