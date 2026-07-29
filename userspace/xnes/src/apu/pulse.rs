use crate::apu::{DUTY_SEQUENCES, Pulse};

impl Pulse {
    pub(super) fn step_timer(&mut self) {
        if self.timer_val == 0 {
            self.timer_val = self.timer_load;
            self.duty_step = self.duty_step.wrapping_sub(1) & 7;
        } else {
            self.timer_val -= 1;
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

    pub(super) fn clock_length(&mut self) {
        if self.length_counter > 0 && !self.length_halt {
            self.length_counter -= 1;
        }
    }

    pub(super) fn clock_sweep(&mut self) {
        // Decrement divider (wrapping for initial 0→255 behavior)
        self.sweep_divider = self.sweep_divider.wrapping_sub(1);

        if self.sweep_divider == 0 {
            // Apply sweep when divider reaches 0
            if self.sweep_enabled && self.sweep_shift > 0 && !self.is_sweep_muted() {
                self.timer_load = self.sweep_calc_target();
            }
            // Reset divider (always reset when it hits 0, even if sweep not applied)
            self.sweep_divider = self.sweep_period;
        }

        // Handle reload flag (set by $4001/$4005 write)
        if self.sweep_reload {
            self.sweep_divider = self.sweep_period;
            self.sweep_reload = false;
        }
    }

    fn is_sweep_muted(&self) -> bool {
        self.timer_load < 8 || self.sweep_calc_target() > 0x7FF
    }

    fn sweep_calc_target(&self) -> u16 {
        let shift = self.sweep_shift;
        let change = self.timer_load >> shift;
        if self.sweep_negate {
            if self.is_pulse1 {
                // Pulse 1 uses one's complement: timer_load - change - 1
                self.timer_load.saturating_sub(change).saturating_sub(1)
            } else {
                // Pulse 2 uses two's complement: timer_load - change
                self.timer_load - change
            }
        } else {
            self.timer_load + change
        }
    }

    pub fn volume_output(&self) -> u8 {
        if !self.enabled
            || self.length_counter == 0
            || self.timer_load < 8
            || (self.sweep_enabled && self.sweep_shift > 0 && self.sweep_calc_target() > 0x7FF)
        {
            return 0;
        }
        let bit = (DUTY_SEQUENCES[self.duty as usize] >> self.duty_step) & 1;
        if bit == 0 {
            return 0;
        }
        if self.env_disable {
            self.vol
        } else {
            self.env_decay
        }
    }
}
