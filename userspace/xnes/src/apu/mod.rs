// APU module - Audio Processing Unit
//
// Organized by channel:
// - pulse.rs: Pulse 1 & 2 channels (envelope, sweep, timer, duty)
// - triangle.rs: Triangle channel (linear counter, sequencer)
// - noise.rs: Noise channel (LFSR, envelope)
// - dmc.rs: DMC channel (delta modulation, DMA)

mod dmc;
mod noise;
mod pulse;
mod triangle;

const LENGTH_TABLE: [u8; 32] = [
    10, 254, 20, 2, 40, 4, 80, 6, 160, 8, 60, 10, 14, 12, 26, 14, 12, 16, 24, 18, 48, 20, 96, 22,
    192, 24, 72, 26, 16, 28, 32, 30,
];

// DMC rate table: period in CPU cycles between DMA reads
// Indexed by bits 3-0 of $4010
const DMC_RATES: [u16; 16] = [
    428, 380, 340, 320, 286, 254, 226, 214, 190, 160, 144, 128, 106, 96, 54, 32,
];

// LSB-first bit patterns for duty_step 0-7
// Duty 0: 0b00000010 -> 0,1,0,0,0,0,0,0  (12.5%)
// Duty 1: 0b00000110 -> 0,1,1,0,0,0,0,0  (25%)
// Duty 2: 0b00011110 -> 0,1,1,1,1,0,0,0  (50%)
// Duty 3: 0b11001111 -> 1,0,0,1,1,1,1,1  (25% negated)
const DUTY_SEQUENCES: [u8; 4] = [0x02, 0x06, 0x1E, 0xCF];

// ---- Fixed-point audio constants (Q16.16 format) ----
/// Precomputed NES DAC pulse output (Q16.16, indexed by `pulse_sum` 0..30).
/// `pulse_part_q16` = (9588 * s * 65536) / (812800 + 10000 * s)
const PULSE_TABLE: [i32; 31] = {
    let mut t = [0i32; 31];
    let mut s = 0usize;
    while s < 31 {
        let num = (9588 * s) as i64;
        let den = (812_800 + 10_000 * s) as i64;
        t[s] = ((num * 65536) / den) as i32;
        s += 1;
    }
    t
};

/// Triangle contribution to TND sum (Q16.16).
const TRI_TABLE: [i32; 16] = {
    let mut t = [0i32; 16];
    let mut i = 0usize;
    while i < 16 {
        t[i] = ((i as i64 * 65536) / 8227) as i32;
        i += 1;
    }
    t
};

/// Noise contribution to TND sum (Q16.16).
const NOI_TABLE: [i32; 16] = {
    let mut t = [0i32; 16];
    let mut i = 0usize;
    while i < 16 {
        t[i] = ((i as i64 * 65536) / 12241) as i32;
        i += 1;
    }
    t
};

/// DMC contribution to TND sum (Q16.16).
const DMC_TABLE: [i32; 128] = {
    let mut t = [0i32; 128];
    let mut i = 0usize;
    while i < 128 {
        t[i] = ((i as i64 * 65536) / 22638) as i32;
        i += 1;
    }
    t
};

/// Low-pass filter coefficient in Q16.16 (0.45 * 65536).
/// Cutoff ~4.6 kHz @ 44.1 kHz — removes high-frequency aliasing
/// from the noise channel's rapid LFSR clocking while preserving
/// the core NES audio bandwidth.
const FILTER_ALPHA: i32 = 29491;

/// Q16.16 representation of 1.0.
const ONE_Q16: u32 = 65536;

/// NES CPU clock rate.
const CPU_HZ: u64 = 1_789_773;

// Noise period table (CPU cycles)
const NOISE_PERIODS: [u16; 16] = [
    4, 8, 16, 32, 64, 96, 128, 160, 202, 254, 380, 508, 762, 1016, 2034, 4068,
];

#[derive(Default, Clone)]
pub struct Pulse {
    pub enabled: bool,
    pub duty: u8,
    pub vol: u8,
    pub timer_load: u16,
    pub timer_val: u16,
    pub duty_step: u8,

    // Envelope
    pub env_start: bool,
    pub env_disable: bool, // constant volume flag (bit 4 of $4000)
    pub env_divider: u8,
    pub env_decay: u8,

    // Length counter (halt flag is bit 5 of $4000, also envelope loop flag)
    pub length_counter: u8,
    pub length_halt: bool,

    // Sweep ($4001 / $4005)
    pub sweep_enabled: bool,
    pub sweep_period: u8, // P (bits 6-4), divider period is P + 1 half-frames
    pub sweep_negate: bool,
    pub sweep_shift: u8,
    pub sweep_divider: u8,
    pub sweep_reload: bool,
    /// True for pulse 1 (uses one's complement in negate sweep), false for pulse 2
    pub is_pulse1: bool,
}

#[derive(Default, Clone)]
pub struct Triangle {
    pub enabled: bool,
    pub linear_counter_reload: u8, // bits 6-0 of $4008
    pub control_flag: bool,        // bit 7 of $4008 (halt / linear counter flag)
    pub linear_counter: u8,
    pub length_counter: u8,
    pub length_halt: bool,
    pub timer_load: u16,
    pub timer_val: u16,
    pub sequencer: u8, // 0..31, generates triangle waveform
    pub linear_reload: bool,
}

#[derive(Clone)]
pub struct Noise {
    pub enabled: bool,
    pub vol: u8,
    pub env_disable: bool,
    pub env_start: bool,
    pub env_divider: u8,
    pub env_decay: u8,
    pub length_counter: u8,
    pub length_halt: bool,
    pub mode: bool,       // bit 7 of $400E
    pub period_index: u8, // bits 3-0 of $400E
    pub timer_load: u16,
    pub timer_val: u16,
    pub lfsr: u16, // 15-bit shift register
}

impl Default for Noise {
    fn default() -> Self {
        Self {
            enabled: false,
            vol: 0,
            env_disable: false,
            env_start: false,
            env_divider: 0,
            env_decay: 0,
            length_counter: 0,
            length_halt: false,
            mode: false,
            period_index: 0,
            timer_load: 0,
            timer_val: 0,
            lfsr: 1, // LFSR starts at 1
        }
    }
}

#[derive(Default, Clone)]
pub struct Dmc {
    // Registers
    pub rate_index: u8,        // bits 3-0 of $4010
    pub loop_flag: bool,       // bit 4 of $4010
    pub irq_enable: bool,      // bit 7 of $4010
    pub dac: u8,               // 7-bit output level written to $4011
    pub sample_addr: u16,      // $4012: current sample address
    pub sample_addr_load: u16, // $4012 configured address = $C000 + val * 64
    pub sample_len: u16,       // $4013: sample length = val * 16 + 1

    // Internal state
    pub enabled: bool,
    pub sample_buffer: u8, // 1-byte buffer filled by DMA
    pub buffer_empty: bool,
    pub bits_remaining: u8, // bits left to shift out of sample_buffer (0-8)
    pub output_level: u8,   // 7-bit current output level

    // DMA state
    pub dma_needed: bool,     // DMC needs to read a byte from memory
    pub bytes_remaining: u16, // bytes left to read from sample

    // Timer
    pub timer: u16,      // counts down in CPU cycles
    pub timer_load: u16, // reload value from rate table

    // Status
    pub irq: bool,
}

pub struct Apu {
    pub cycles: u64,
    /// Accumulates sub-cycle timing for timer/FC stepping.
    /// The APU clocks timers and frame counter at half the CPU rate.
    /// This accumulator tracks CPU cycles and steps them every 2 cycles,
    /// carrying forward fractional cycles for accurate long-term timing.
    phase: u64,
    pub p1: Pulse,
    pub p2: Pulse,
    triangle: Triangle,
    noise: Noise,
    dmc: Dmc,
    pub audio_samples: [i16; 4096],
    pub sample_count: usize,
    /// Sample timing accumulator (Q16.16 fixed-point).
    /// Incremented by `ONE_Q16` per CPU cycle. Sample captured when >= `sample_period`.
    sample_timer: u32,
    /// Sample period in Q16.16 fixed-point.
    sample_period: u32,
    /// Low-pass filter state in Q16.16 fixed-point (emulates ~14KHz cutoff).
    filtered_sample: i32,
    /// DC blocking high-pass filter state.
    /// y[n] = 0.979 * y[n-1] + x[n] - x[n-1]  (cutoff ~150 Hz @ 44.1 kHz)
    hp_x: i64,
    hp_y: i64,
    /// Accumulated mixer output over the current sample period.
    /// Averaged at sample time to act as a crude anti-aliasing filter,
    /// reducing aliasing from noise LFSR and square wave harmonics.
    /// See Mesen's `blip_buf` band-limited synthesis for the gold standard.
    mix_accum: i64,
    mix_accum_count: u32,

    // Frame counter
    frame_cycles: u32,
    frame_mode: bool, // false = 4-step, true = 5-step
    interrupt_inhibit: bool,
    pub frame_irq: bool,
    // Write delay for $4017 (3 or 4 CPU cycles)
    fc_delay: u8,
    fc_write_pending: bool,
    fc_pending_mode: bool,
    fc_pending_inhibit: bool,
}

impl Default for Apu {
    fn default() -> Self {
        Self::new()
    }
}

impl Apu {
    pub fn new() -> Self {
        Self {
            cycles: 0,
            phase: 0,
            p1: Pulse {
                is_pulse1: true,
                ..Pulse::default()
            },
            p2: Pulse::default(),
            triangle: Triangle::default(),
            noise: Noise::default(),
            dmc: Dmc {
                buffer_empty: true,
                timer: 1,
                timer_load: DMC_RATES[0],
                ..Dmc::default()
            },
            audio_samples: [0; 4096],
            sample_count: 0,
            sample_timer: 0,
            sample_period: (((CPU_HZ) << 16) / 44100) as u32,
            filtered_sample: 0,
            hp_x: 0,
            hp_y: 0,
            mix_accum: 0,
            mix_accum_count: 0,
            frame_cycles: 0,
            frame_mode: false,
            interrupt_inhibit: false,
            frame_irq: false,
            fc_delay: 0,
            fc_write_pending: false,
            fc_pending_mode: false,
            fc_pending_inhibit: false,
        }
    }

    pub fn set_sample_rate(&mut self, rate: u32) {
        self.sample_period = (((CPU_HZ) << 16) / rate as u64) as u32;
    }

    pub fn dmc_dma_pending(&self) -> bool {
        self.dmc.dma_needed
    }

    /// Check if a DMC DMA would fire within `cpu_cycles` CPU cycles
    /// (used by SHA/SHS/SHY/SHX for `IgnoreH` behavior - when a DMA occurs
    ///  just before the write cycle, the H value is ignored)
    pub fn dmc_dma_imminent(&self, cpu_cycles: u16) -> bool {
        if self.dmc.dma_needed {
            return true;
        }
        if self.dmc.enabled {
            // DMC is enabled. A DMA is imminent if:
            // 1. There are still bytes to read AND timer is low, OR
            // 2. Looping is enabled (sample auto-restarts) AND timer is low
            let will_have_bytes = self.dmc.bytes_remaining > 0 || self.dmc.loop_flag;
            if will_have_bytes && self.dmc.timer <= cpu_cycles {
                return true;
            }
        }
        false
    }

    /// Complete a DMC DMA read. `val` is the byte read from the sample address.
    pub fn dmc_complete_dma(&mut self, val: u8) {
        self.dmc.dma_read(val);
    }

    pub fn apu_irq_pending(&self) -> bool {
        self.frame_irq
    }

    pub fn dmc_sample_address(&self) -> u16 {
        self.dmc.sample_addr
    }

    /// Tick only the DMC unit by one CPU cycle.
    /// Used during bus accesses so DMC DMA can fire mid-instruction
    /// (required by SHA/SHS/SHY/SHX for correct H computation).
    #[inline(always)]
    pub fn tick_dmc(&mut self) {
        self.dmc.step();
    }

    /// Save DMC state for snapshot/restore around SH instructions.
    pub fn save_dmc(&self) -> Dmc {
        self.dmc.clone()
    }

    /// Restore DMC state (used for SH instruction per-access DMC ticking).
    pub fn restore_dmc(&mut self, saved: &Dmc) {
        self.dmc = saved.clone();
    }

    fn clock_quarter_frame(&mut self) {
        self.p1.clock_envelope();
        self.p2.clock_envelope();
        self.noise.clock_envelope();
        self.triangle.clock_linear_counter();
    }

    fn clock_half_frame(&mut self) {
        self.p1.clock_length();
        self.p2.clock_length();
        self.p1.clock_sweep();
        self.p2.clock_sweep();
        self.triangle.clock_length();
        self.noise.clock_length();
    }

    fn clock_frame_counter(&mut self) {
        // Events happen at the END of each APU cycle.
        // Check at current frame_cycles (pre-increment), then increment.

        if self.frame_mode {
            // Mode 1: 5-step sequence (18641 APU cycles)
            // Events at end of cycles: 3728, 7456, 11185, 18640
            match self.frame_cycles {
                3728 | 7456 | 11185 | 18640 => self.clock_quarter_frame(),
                _ => {}
            }
            match self.frame_cycles {
                7456 | 18640 => self.clock_half_frame(),
                _ => {}
            }
        } else {
            // Mode 0: 4-step sequence (14914 APU cycles)
            // Events at end of cycles: 3728, 7456, 11185, 14913
            match self.frame_cycles {
                3728 | 7456 | 11185 | 14913 => self.clock_quarter_frame(),
                _ => {}
            }
            match self.frame_cycles {
                7456 | 14913 => self.clock_half_frame(),
                _ => {}
            }
            // IRQ fires at end of APU cycle 14913
            if self.frame_cycles == 14913 && !self.interrupt_inhibit {
                self.frame_irq = true;
            }
        }

        self.frame_cycles += 1;
        if (self.frame_mode && self.frame_cycles >= 18641)
            || (!self.frame_mode && self.frame_cycles >= 14914)
        {
            self.frame_cycles = 0;
        }
    }

    /// Common inner loop for both `tick` and `tick_without_dmc`.
    /// `dmc_enabled` controls whether `self.dmc.step()` is called.
    fn tick_inner(&mut self, cpu_cycles: u16, dmc_enabled: bool) {
        for _ in 0..cpu_cycles {
            self.cycles += 1;

            // Handle $4017 write delay (count down in CPU cycles)
            if self.fc_delay > 0 {
                self.fc_delay -= 1;
                if self.fc_delay == 0 && self.fc_write_pending {
                    self.frame_mode = self.fc_pending_mode;
                    self.interrupt_inhibit = self.fc_pending_inhibit;
                    if self.interrupt_inhibit {
                        self.frame_irq = false;
                    }
                    self.frame_cycles = 0;
                    if self.frame_mode {
                        self.clock_quarter_frame();
                        self.clock_half_frame();
                    }
                    self.fc_write_pending = false;
                }
            }

            if dmc_enabled {
                self.dmc.step();
            }

            // Triangle and Noise timers run at CPU rate (every cycle)
            self.triangle.step_timer();
            self.noise.step_timer();

            // Pulse timers and frame counter run at APU rate (every 2 CPU cycles)
            self.phase += 1;
            if self.phase >= 2 {
                self.phase -= 2;
                self.p1.step_timer();
                self.p2.step_timer();
                self.clock_frame_counter();
            }

            // Accumulate the current mixer output every CPU cycle.
            // Using the average over the sample period (instead of a single
            // snapshot) acts as a boxcar anti-aliasing filter — it
            // attenuates high-frequency content like the noise channel's
            // rapid LFSR clocking before sampling.
            self.mix_accum += self.mixer_output() as i64;
            self.mix_accum_count += 1;

            self.sample_timer = self.sample_timer.wrapping_add(ONE_Q16);
            if self.sample_timer >= self.sample_period {
                self.sample_timer -= self.sample_period;
                if self.sample_count < self.audio_samples.len() {
                    // Use the accumulated average over the sample period
                    // instead of a single instantaneous snapshot.
                    let raw = (self.mix_accum / self.mix_accum_count as i64) as i32;
                    self.mix_accum = 0;
                    self.mix_accum_count = 0;
                    let diff = raw as i64 - self.filtered_sample as i64;
                    let delta = (diff * FILTER_ALPHA as i64) >> 16;
                    self.filtered_sample = (self.filtered_sample as i64 + delta) as i32;
                    // DC blocking high-pass filter (cutoff ~150 Hz @ 44.1 kHz)
                    // Removes low-frequency hum and DC offset that accumulates
                    // over long play sessions from imperfect sample timing.
                    // y[n] = 0.979 * y[n-1] + x[n] - x[n-1]
                    const HP_COEFF: i64 = 64150; // 0.979 in Q16.16
                    let x = self.filtered_sample as i64;
                    // Prime hp_x on first non-zero sample to avoid startup pop
                    if self.hp_x == 0 && x != 0 {
                        self.hp_x = x;
                    }
                    self.hp_y = ((self.hp_y * HP_COEFF) >> 16) + x - self.hp_x;
                    self.hp_x = x;
                    self.audio_samples[self.sample_count] = (self.hp_y >> 1) as i16;
                    self.sample_count += 1;
                }
            }
        }
    }

    pub fn tick(&mut self, cpu_cycles: u16) {
        self.tick_inner(cpu_cycles, true);
    }

    /// Tick all APU components except DMC (DMC is ticked per bus access).
    pub fn tick_without_dmc(&mut self, cpu_cycles: u16) {
        self.tick_inner(cpu_cycles, false);
    }

    fn mixer_output(&self) -> i32 {
        let pulse_sum = (self.p1.volume_output() + self.p2.volume_output()) as usize;
        let pulse_part = PULSE_TABLE[pulse_sum.min(30)]; // Q16.16

        let tri = self.triangle.output() as usize;
        let noi = self.noise.output() as usize;
        let dmc = self.dmc.output() as usize;
        let tnd_sum_q16 = TRI_TABLE[tri] + NOI_TABLE[noi] + DMC_TABLE[dmc]; // Q16.16

        let tnd_part = if tnd_sum_q16 == 0 {
            0
        } else {
            let s = tnd_sum_q16 as i64;
            // tnd_part = 159.79 * tnd_sum / (1.0 + 100.0 * tnd_sum)
            // tnd_part_q16 = (15979 * s * 65536) / (6553600 + 10000 * s)
            let num = 15979i64 * s * 65536;
            let den = 6_553_600i64 + 10_000 * s;
            (num / den) as i32
        };

        (pulse_part + tnd_part).clamp(0, 65535)
    }

    pub fn read(&mut self, addr: u16) -> u8 {
        match addr {
            0x4015 => {
                let mut status = 0u8;
                if self.p1.length_counter > 0 {
                    status |= 1;
                }
                if self.p2.length_counter > 0 {
                    status |= 2;
                }
                if self.triangle.length_counter > 0 {
                    status |= 4;
                }
                if self.noise.length_counter > 0 {
                    status |= 8;
                }
                if self.dmc.enabled && self.dmc.bytes_remaining > 0 {
                    status |= 0x10;
                }
                if self.dmc.irq {
                    status |= 0x80;
                }
                if self.frame_irq {
                    status |= 0x40;
                }
                self.frame_irq = false;
                status
            }
            // $4016 and $4017 reads go through the controller, but APU catches
            // reads when they come through the bus. Return open bus.
            _ => 0,
        }
    }

    #[allow(clippy::too_many_lines)]
    pub fn write(&mut self, addr: u16, val: u8) {
        match addr {
            0x4000 => {
                self.p1.duty = val >> 6;
                self.p1.length_halt = val & 0x20 != 0;
                self.p1.env_disable = val & 0x10 != 0;
                self.p1.vol = val & 0x0F;
            }
            0x4001 => {
                self.p1.sweep_enabled = val & 0x80 != 0;
                self.p1.sweep_period = ((val >> 4) & 7) + 1;
                self.p1.sweep_negate = val & 0x08 != 0;
                self.p1.sweep_shift = val & 0x07;
                self.p1.sweep_reload = true;
            }
            0x4002 => {
                self.p1.timer_load = (self.p1.timer_load & 0xFF00) | val as u16;
            }
            0x4003 => {
                self.p1.timer_load = (self.p1.timer_load & 0x00FF) | ((val as u16 & 7) << 8);
                self.p1.timer_val = self.p1.timer_load; // reload timer immediately
                self.p1.duty_step = 0;
                self.p1.env_start = true;
                if self.p1.enabled {
                    let idx = (val >> 3) as usize;
                    self.p1.length_counter = LENGTH_TABLE[idx.min(31)];
                }
            }
            0x4004 => {
                self.p2.duty = val >> 6;
                self.p2.length_halt = val & 0x20 != 0;
                self.p2.env_disable = val & 0x10 != 0;
                self.p2.vol = val & 0x0F;
            }
            0x4005 => {
                self.p2.sweep_enabled = val & 0x80 != 0;
                self.p2.sweep_period = ((val >> 4) & 7) + 1;
                self.p2.sweep_negate = val & 0x08 != 0;
                self.p2.sweep_shift = val & 0x07;
                self.p2.sweep_reload = true;
            }
            0x4006 => {
                self.p2.timer_load = (self.p2.timer_load & 0xFF00) | val as u16;
            }
            0x4007 => {
                self.p2.timer_load = (self.p2.timer_load & 0x00FF) | ((val as u16 & 7) << 8);
                self.p2.timer_val = self.p2.timer_load; // reload timer immediately
                self.p2.duty_step = 0;
                self.p2.env_start = true;
                if self.p2.enabled {
                    let idx = (val >> 3) as usize;
                    self.p2.length_counter = LENGTH_TABLE[idx.min(31)];
                }
            }
            0x4008 => {
                // Triangle control
                // Bit 7 controls both linear counter AND length counter halting
                let control = val & 0x80 != 0;
                self.triangle.control_flag = control;
                self.triangle.length_halt = control;
                self.triangle.linear_counter_reload = val & 0x7F;
                // Writing $4008 triggers a linear counter reload
                self.triangle.linear_reload = true;
            }
            0x400A => {
                // Triangle timer low
                self.triangle.timer_load = (self.triangle.timer_load & 0xFF00) | val as u16;
            }
            0x400B => {
                // Triangle timer high + length counter
                self.triangle.timer_load =
                    (self.triangle.timer_load & 0x00FF) | ((val as u16 & 7) << 8);
                self.triangle.timer_val = self.triangle.timer_load; // reload timer immediately
                self.triangle.linear_reload = true;
                if self.triangle.enabled {
                    let idx = (val >> 3) as usize;
                    self.triangle.length_counter = LENGTH_TABLE[idx.min(31)];
                }
            }
            0x400C => {
                // Noise volume/envelope
                self.noise.length_halt = val & 0x20 != 0;
                self.noise.env_disable = val & 0x10 != 0;
                self.noise.vol = val & 0x0F;
            }
            0x400E => {
                // Noise mode/period
                self.noise.mode = val & 0x80 != 0;
                self.noise.period_index = val & 0x0F;
                // Period in CPU cycles (timer runs at CPU rate)
                self.noise.timer_load =
                    NOISE_PERIODS[self.noise.period_index as usize].saturating_sub(1);
            }
            0x400F => {
                // Noise length counter
                self.noise.env_start = true;
                if self.noise.enabled {
                    let idx = (val >> 3) as usize;
                    self.noise.length_counter = LENGTH_TABLE[idx.min(31)];
                }
            }
            0x4010 => {
                // DMC control
                self.dmc.irq_enable = val & 0x80 != 0;
                self.dmc.loop_flag = val & 0x40 != 0;
                self.dmc.rate_index = val & 0x0F;
                self.dmc.timer_load = DMC_RATES[self.dmc.rate_index as usize];
                if !self.dmc.irq_enable {
                    self.dmc.irq = false;
                }
            }
            0x4011 => {
                // DMC DAC (7-bit output level)
                self.dmc.dac = val & 0x7F;
                self.dmc.output_level = self.dmc.dac;
            }
            0x4012 => {
                // DMC sample address: $C000 + val * 64
                self.dmc.sample_addr_load = 0xC000 | ((val as u16) << 6);
            }
            0x4013 => {
                // DMC sample length: val * 16 + 1
                self.dmc.sample_len = (val as u16) * 16 + 1;
            }
            0x4015 => {
                self.p1.enabled = val & 1 != 0;
                self.p2.enabled = val & 2 != 0;
                self.triangle.enabled = val & 4 != 0;
                self.noise.enabled = val & 8 != 0;
                if !self.p1.enabled {
                    self.p1.length_counter = 0;
                }
                if !self.p2.enabled {
                    self.p2.length_counter = 0;
                }
                if !self.triangle.enabled {
                    self.triangle.length_counter = 0;
                }
                if !self.noise.enabled {
                    self.noise.length_counter = 0;
                }
                // DMC enable (bit 4)
                let prev_enabled = self.dmc.enabled;
                self.dmc.enabled = val & 0x10 != 0;
                if self.dmc.enabled && !prev_enabled {
                    // Restart DMC sample
                    self.dmc.restart();
                } else if !self.dmc.enabled {
                    // Stop DMC
                    self.dmc.bytes_remaining = 0;
                    self.dmc.bits_remaining = 0;
                    self.dmc.buffer_empty = true;
                    self.dmc.dma_needed = false;
                }
                // Clear DMC IRQ
                self.dmc.irq = false;
            }
            0x4017 => {
                // Frame counter write with delay
                // Delay is 3 CPU cycles for write (put) cycles, 4 for read (get) cycles
                // Since $4017 is always written during a put cycle, use 3
                // Store the pending values but don't apply yet
                self.fc_delay = 3;
                self.fc_write_pending = true;
                self.fc_pending_mode = val & 0x80 != 0;
                self.fc_pending_inhibit = val & 0x40 != 0;
            }
            _ => {}
        }
    }
}
