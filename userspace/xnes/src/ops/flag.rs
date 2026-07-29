use crate::bus::Bus;
use crate::cpu::{CpuRp2a03, FLAG_CARRY, FLAG_DECIMAL, FLAG_INTERRUPT, FLAG_OVERFLOW};

pub fn clc(cpu: &mut CpuRp2a03, _bus: &mut Bus) -> u8 {
    cpu.set_flag(FLAG_CARRY, false);
    2
}

pub fn sec(cpu: &mut CpuRp2a03, _bus: &mut Bus) -> u8 {
    cpu.set_flag(FLAG_CARRY, true);
    2
}

pub fn cld(cpu: &mut CpuRp2a03, _bus: &mut Bus) -> u8 {
    cpu.set_flag(FLAG_DECIMAL, false);
    2
}

pub fn sed(cpu: &mut CpuRp2a03, _bus: &mut Bus) -> u8 {
    cpu.set_flag(FLAG_DECIMAL, true);
    2
}

pub fn cli(cpu: &mut CpuRp2a03, _bus: &mut Bus) -> u8 {
    cpu.set_flag(FLAG_INTERRUPT, false);
    2
}

pub fn sei(cpu: &mut CpuRp2a03, _bus: &mut Bus) -> u8 {
    cpu.set_flag(FLAG_INTERRUPT, true);
    2
}

pub fn clv(cpu: &mut CpuRp2a03, _bus: &mut Bus) -> u8 {
    cpu.set_flag(FLAG_OVERFLOW, false);
    2
}
