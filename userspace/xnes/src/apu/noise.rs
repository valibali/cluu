use crate::apu::Noise;

impl Noise {
    pub(super) fn clock_length(&mut self) {
        if self.length_counter > 0 && !self.length_halt {
            self.length_counter -= 1;
        }
    }

    pub(super) fn clock_envelope(&mut self) {
        if self.env_start {
            self.env_start = false;
            self.env_decay = 15;
            self.env_divider = self.vol;
        } else if self.env_divider == 0 {
            self.env_divider = self.vol;
            if self.env_decay == 0 {
                if self.length_halt {
                    self.env_decay = 15;
                }
            } else {
                self.env_decay -= 1;
            }
        } else {
            self.env_divider -= 1;
        }
    }

    pub(super) fn step_timer(&mut self) {
        if self.timer_val == 0 {
            self.timer_val = self.timer_load;
            // Clock LFSR
            let feedback = if self.mode {
                // Mode 1: feedback from bits 0 and 6
                (self.lfsr ^ (self.lfsr >> 6)) & 1
            } else {
                // Mode 0: feedback from bits 0 and 1
                (self.lfsr ^ (self.lfsr >> 1)) & 1
            };
            self.lfsr = (self.lfsr >> 1) | (feedback << 14);
            // LFSR all-zeros check: if 0, set to 1
            if self.lfsr == 0 {
                self.lfsr = 1;
            }
        } else {
            self.timer_val -= 1;
        }
    }

    pub(super) fn output(&self) -> u8 {
        if !self.enabled || self.length_counter == 0 || (self.lfsr & 1) != 0 {
            return 0;
        }
        if self.env_disable {
            self.vol
        } else {
            self.env_decay
        }
    }
}
