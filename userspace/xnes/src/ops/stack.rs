use crate::bus::Bus;
use crate::cpu::{CpuRp2a03, FLAG_BREAK};
use crate::ops::{pull, push};

pub fn pha(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    push(cpu, bus, cpu.a());
    3
}

pub fn pla(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let val = pull(cpu, bus);
    cpu.set_a(val);
    cpu.set_sign(val);
    cpu.set_zero(val);
    4
}

pub fn php(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    // PHP pushes SR with B flag SET and bit 5 always SET
    push(cpu, bus, (cpu.sr() | FLAG_BREAK) | 0x20);
    3
}

pub fn plp(cpu: &mut CpuRp2a03, bus: &mut Bus) -> u8 {
    let sr = (pull(cpu, bus) & !FLAG_BREAK) | 0x20;
    cpu.set_sr(sr);
    4
}
