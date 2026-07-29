use crate::apu::Dmc;

impl Dmc {
    pub(super) fn restart(&mut self) {
        self.sample_addr = self.sample_addr_load;
        self.bytes_remaining = self.sample_len;
        self.buffer_empty = true;
        self.bits_remaining = 0;
        self.dma_needed = false;
        // Reset timer so DMA fires after timer_load CPU cycles
        // (real NES also has a 2-3 CPU cycle startup delay)
        self.timer = if self.timer_load == 0 {
            1
        } else {
            self.timer_load
        };
    }

    pub(super) fn step(&mut self) -> bool {
        // Returns true if this step requires a DMA read (stealing CPU cycles)
        if self.timer > 0 {
            self.timer -= 1;
            return false;
        }

        // Reload timer (minimum 1 to avoid instant re-fire)
        self.timer = if self.timer_load == 0 {
            1
        } else {
            self.timer_load
        };

        if self.buffer_empty {
            // Need to read a byte into the buffer
            if self.bytes_remaining > 0 && !self.dma_needed {
                self.dma_needed = true;
            }
            return false;
        }

        // If buffer is not empty but no bits remain, it's empty
        if self.bits_remaining == 0 {
            self.buffer_empty = true;
            if self.bytes_remaining > 0 {
                self.dma_needed = true;
            } else if self.loop_flag {
                self.restart();
                self.dma_needed = true;
            } else if self.irq_enable {
                self.irq = true;
            }
            return false;
        }

        // Shift out one bit (LSB first)
        let bit = self.sample_buffer & 1;
        self.sample_buffer >>= 1;
        self.bits_remaining -= 1;

        // Adjust output level: +2 for 1, -2 for 0
        if bit == 1 {
            if self.output_level <= 125 {
                self.output_level += 2;
            }
        } else if self.output_level >= 2 {
            self.output_level -= 2;
        }

        if self.bits_remaining == 0 {
            self.buffer_empty = true;
            if self.bytes_remaining > 0 {
                self.dma_needed = true;
            } else if self.loop_flag {
                self.restart();
                self.dma_needed = true;
            } else if self.irq_enable {
                self.irq = true;
            }
        }

        false
    }

    /// Check if DMC DMA needs to read a byte, and return the address to read from.
    /// Returns None if no DMA is needed.
    pub fn poll_dma(&self) -> Option<u16> {
        if self.dma_needed && self.bytes_remaining > 0 {
            Some(self.sample_addr)
        } else {
            None
        }
    }

    /// Called when DMC DMA reads a byte from memory
    pub(super) fn dma_read(&mut self, val: u8) {
        self.sample_buffer = val;
        self.bits_remaining = 8;
        self.buffer_empty = false;
        self.dma_needed = false;
        self.bytes_remaining = self.bytes_remaining.saturating_sub(1);
        // Advance sample address, wrapping from $FFFF to $8000
        self.sample_addr = if self.sample_addr == 0xFFFF {
            0x8000
        } else {
            self.sample_addr.wrapping_add(1)
        };
    }

    pub(super) fn output(&self) -> u8 {
        if !self.enabled {
            return 0;
        }
        self.output_level
    }
}
