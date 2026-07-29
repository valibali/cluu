#![cfg_attr(not(any(test, feature = "std")), no_std)]
#![deny(
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    unsafe_op_in_unsafe_fn,
    dead_code,
    unused_imports
)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_lossless,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::module_name_repetitions,
    clippy::similar_names,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::return_self_not_must_use,
    clippy::items_after_statements,
    clippy::wildcard_enum_match_arm,
    clippy::inline_always,
    clippy::wildcard_imports,
    clippy::missing_const_for_fn,
    clippy::large_stack_arrays,
    clippy::struct_excessive_bools
)]

extern crate alloc;

pub mod apu;
pub mod bus;
pub mod controller;
pub mod cpu;

pub mod mapper;
pub mod ops;
pub mod ppu;
pub mod rom;
pub mod util;

use bus::Bus;
use cpu::{CpuRp2a03, FLAG_BREAK, FLAG_INTERRUPT};
use mapper::Mapper;
use ops::{BASE_CYCLES, TABLE};

#[allow(clippy::too_many_lines)]
#[must_use]
pub fn tick(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    bus.dmc_tick();

    let start_cycle = bus.cpu_cycle;
    let mut cycles_extra = 0u8;

    let opcode = bus.read(cpu.pc());
    cpu.set_pc(cpu.pc().wrapping_add(1));
    let base_cycles = BASE_CYCLES[opcode as usize] as u64;
    bus.penultimate_sample_cycle = start_cycle + base_cycles.saturating_sub(2);

    let is_cli_sei = opcode == 0x58 || opcode == 0x78;
    let i_flag_for_irq = if is_cli_sei {
        cpu.get_flag(FLAG_INTERRUPT)
    } else {
        false
    };

    let is_sh =
        opcode == 0x93 || opcode == 0x9B || opcode == 0x9C || opcode == 0x9E || opcode == 0x9F;
    let dmc_saved = if is_sh {
        Some(bus.apu.save_dmc())
    } else {
        None
    };
    bus.apu.tick(base_cycles as u16);
    if bus.apu.dmc_dma_pending() {
        bus.dmc_just_fired = true;
    }

    if let Some(ref saved) = dmc_saved {
        bus.apu.restore_dmc(saved);
        bus.dmc_ticks = 0;
    }

    let cycles = TABLE[opcode as usize](cpu, bus) as u64;

    bus.cpu_cycle = start_cycle + cycles;
    bus.catch_up_ppu();

    if cycles > base_cycles {
        bus.apu.tick_without_dmc((cycles - base_cycles) as u16);
    }

    if is_sh {
        bus.apu.tick_dmc();
        let applied = 1u64 + bus.dmc_ticks as u64;
        let total = 1u64 + cycles;
        if total > applied {
            for _ in 0..(total - applied) as u16 {
                bus.apu.tick_dmc();
            }
        }
    }

    if bus.ppu.nmi_from_vblank || bus.ppu.nmi_deferred_pending {
        bus.ppu.nmi_from_vblank = false;
        bus.ppu.nmi_deferred_pending = false;
        bus.ppu.nmi_latched = false;
        let svc_start = bus.cpu_cycle;
        nmi(cpu, bus);
        bus.cpu_cycle = svc_start + 7;
        bus.catch_up_ppu();
        cycles_extra += 7;
    } else if !(if is_cli_sei {
        i_flag_for_irq
    } else {
        cpu.get_flag(FLAG_INTERRUPT)
    }) && bus.poll_irq()
    {
        let svc_start = bus.cpu_cycle;
        irq(cpu, bus);
        bus.cpu_cycle = svc_start + 7;
        bus.catch_up_ppu();
        cycles_extra += 7;
    }

    (cycles + cycles_extra as u64) as u8
}

pub fn nmi(cpu: &mut CpuRp2a03, bus: &mut Bus) {
    crate::ops::push(cpu, bus, (cpu.pc() >> 8) as u8);
    crate::ops::push(cpu, bus, cpu.pc() as u8);
    let sr = (cpu.sr() & !FLAG_BREAK) | 0x20;
    crate::ops::push(cpu, bus, sr);
    cpu.set_flag(FLAG_INTERRUPT, true);
    let lo = bus.read(0xFFFA) as u16;
    let hi = bus.read(0xFFFB) as u16;
    cpu.set_pc(lo | (hi << 8));
}

pub fn irq(cpu: &mut CpuRp2a03, bus: &mut Bus) {
    crate::ops::push(cpu, bus, (cpu.pc() >> 8) as u8);
    crate::ops::push(cpu, bus, cpu.pc() as u8);
    let sr = (cpu.sr() & !FLAG_BREAK) | 0x20;
    crate::ops::push(cpu, bus, sr);
    cpu.set_flag(FLAG_INTERRUPT, true);
    let lo = bus.read(0xFFFE) as u16;
    let hi = bus.read(0xFFFF) as u16;
    cpu.set_pc(lo | (hi << 8));
}

pub fn reset(cpu: &mut CpuRp2a03, bus: &mut Bus) {
    let lo = bus.read(0xFFFC) as u16;
    let hi = bus.read(0xFFFD) as u16;
    *cpu = CpuRp2a03::new(lo | (hi << 8));
}

/// High-level NES emulator that owns the CPU and Bus together.
///
/// This is the recommended entry point for most users.
/// The free functions (`tick`, `reset`, `nmi`, `irq`) are still available
/// for scenarios that require separate management of CPU and bus state.
pub struct Emulator {
    pub cpu: CpuRp2a03,
    pub bus: Bus,
}

impl Emulator {
    /// Create a new emulator from a ROM's mapper.
    pub fn new(mapper: Mapper) -> Self {
        let mut cpu = CpuRp2a03::new(0);
        let mut bus = Bus::new(mapper);
        reset(&mut cpu, &mut bus);
        Self { cpu, bus }
    }

    /// Execute one CPU instruction, advancing the system state.
    /// Returns the number of CPU cycles consumed by this instruction.
    #[inline]
    #[must_use]
    pub fn tick(&mut self) -> u8 {
        tick(&mut self.cpu, &mut self.bus)
    }

    /// Reset the CPU (re-reads the reset vector).
    pub fn reset(&mut self) {
        reset(&mut self.cpu, &mut self.bus);
    }

    /// Trigger an NMI.
    pub fn nmi(&mut self) {
        nmi(&mut self.cpu, &mut self.bus);
    }

    /// Trigger an IRQ.
    pub fn irq(&mut self) {
        irq(&mut self.cpu, &mut self.bus);
    }
}
