use crate::apu::Triangle;

impl Triangle {
    pub(super) fn clock_length(&mut self) {
        if self.length_counter > 0 && !self.length_halt {
            self.length_counter -= 1;
        }
    }

    pub(super) fn clock_linear_counter(&mut self) {
        if self.linear_reload {
            self.linear_counter = self.linear_counter_reload;
        } else if self.linear_counter > 0 {
            self.linear_counter -= 1;
        }
        if !self.control_flag {
            self.linear_reload = false;
        }
    }

    pub(super) fn step_timer(&mut self) {
        if self.timer_val == 0 {
            self.timer_val = self.timer_load;
            self.sequencer = self.sequencer.wrapping_add(1) & 31;
        } else {
            self.timer_val -= 1;
        }
    }

    pub(super) fn output(&self) -> u8 {
        if !self.enabled || self.length_counter == 0 || self.linear_counter == 0 {
            return 0;
        }
        // Triangle sequencer: 15,14,...,0,0,1,...,15
        // Matches Mesen's _sequence array
        if self.sequencer < 16 {
            15 - self.sequencer
        } else {
            self.sequencer - 16
        }
    }
}
